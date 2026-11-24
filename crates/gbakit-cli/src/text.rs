//! CLI subcommands for GBA text localization:
//! - `gbakit text tables`
//! - `gbakit text extract` (raw and `--lz77`)
//! - `gbakit text scan`
//! - `gbakit text replace` (raw and `--lz77`)
//! - `gbakit text huffman-extract`
//! - `gbakit text huffman-replace`
//! - `gbakit text font-dump`
//! - `gbakit text font-replace`
//! - `gbakit text font-widths`
//!
//! Write commands (`replace`, `huffman-replace`, `font-replace`, `font-widths --from-image`)
//! are strictly non-destructive: the input ROM is read into memory, modified there, and
//! written to a separate `-o <out.gba>` path. Overwriting the input ROM in place is refused.

#![deny(unsafe_code)]

use std::fs;
use std::path::Path;

use gbakit::text::{
    compute_vwf_widths_from_sheet, dump_font_sheet, extract_lz77_string, extract_string,
    parse_huffman_text, read_vwf_widths, replace_font_sheet_in_rom, replace_huffman_text_in_rom,
    replace_lz77_string_in_rom, replace_string_in_rom, replace_vwf_widths_in_rom,
    scan_string_tables, Charmap, FontBpp, FontImageFormat, FontSheetConfig, HuffmanTree,
    TextReplaceMode, TextReplaceOptions, BUILTIN_CHARMAPS, DEFAULT_MAX_STRING_LEN,
};

use super::{parse_flag_value, parse_offset, parse_positional_arg};

const EXTRACT_VALUE_FLAGS: &[&[&str]] = &[&["--offset"], &["--table"], &["--max-len"]];
const SCAN_VALUE_FLAGS: &[&[&str]] = &[&["--table"], &["--min-entries"], &["--limit"]];
const REPLACE_VALUE_FLAGS: &[&[&str]] = &[
    &["--offset"],
    &["--text"],
    &["-o", "--output"],
    &["--table"],
    &["--capacity"],
    &["--pad-byte"],
];
const HUFFMAN_EXTRACT_VALUE_FLAGS: &[&[&str]] = &[
    &["--tree"],
    &["--root"],
    &["--offset"],
    &["--max-len"],
];
const HUFFMAN_REPLACE_VALUE_FLAGS: &[&[&str]] = &[
    &["--tree"],
    &["--root"],
    &["--offset"],
    &["--text"],
    &["-o", "--output"],
    &["--capacity"],
    &["--pad-byte"],
];
const FONT_DUMP_VALUE_FLAGS: &[&[&str]] = &[
    &["--offset"],
    &["--count"],
    &["--width"],
    &["--height"],
    &["--bpp"],
    &["-o", "--output"],
    &["--cols"],
];
const FONT_REPLACE_VALUE_FLAGS: &[&[&str]] = &[
    &["--offset"],
    &["--image"],
    &["--count"],
    &["--width"],
    &["--height"],
    &["--bpp"],
    &["-o", "--output"],
    &["--cols"],
];
const FONT_WIDTHS_VALUE_FLAGS: &[&[&str]] = &[
    &["--offset"],
    &["--count"],
    &["--from-image"],
    &["--width"],
    &["--height"],
    &["--cols"],
    &["--pad"],
    &["-o", "--output"],
];

pub const TEXT_USAGE: &str = "usage:\n  \
  gbakit text tables [--json]\n  \
  gbakit text extract <rom.gba> --offset <hex|dec> [--table <builtin|file.tbl>] [--lz77] [--max-len <n>] [--json]\n  \
  gbakit text scan <rom.gba> [--table <builtin|file.tbl>] [--min-entries <n>] [--limit <n>] [--json]\n  \
  gbakit text replace <rom.gba> --offset <hex|dec> --text <str> -o <out.gba> [--table <builtin|file.tbl>] [--lz77] [--capacity <n>] [--allow-relocate] [--pad-byte <n>] [--json]\n  \
  gbakit text huffman-extract <rom.gba> --tree <hex|dec> --root <hex|dec> --offset <hex|dec> [--max-len <n>] [--json]\n  \
  gbakit text huffman-replace <rom.gba> --tree <hex|dec> --root <hex|dec> --offset <hex|dec> --text <str> -o <out.gba> [--capacity <n>] [--allow-relocate] [--pad-byte <n>] [--json]\n  \
  gbakit text font-dump <rom.gba> --offset <hex|dec> --count <n> --width <px> --height <px> --bpp 1|2 -o <out.png|ppm> [--lsb] [--cols <n>] [--json]\n  \
  gbakit text font-replace <rom.gba> --offset <hex|dec> --image <in.png|ppm> --count <n> --width <px> --height <px> --bpp 1|2 -o <out.gba> [--lsb] [--cols <n>] [--json]\n  \
  gbakit text font-widths <rom.gba> --offset <hex|dec> --count <n> [--from-image <sheet.png|ppm> --width <px> --height <px> [--cols <n>] [--pad <px>] -o <out.gba>] [--json]";

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn required<'a>(args: &'a [String], names: &[&str], what: &str) -> Result<&'a str, String> {
    parse_flag_value(args, names).ok_or_else(|| format!("missing required {what}"))
}

fn ensure_distinct_paths(input: &Path, output: &Path) -> Result<(), String> {
    if input == output {
        return Err(format!(
            "refusing to overwrite input ROM {} in place; choose a different -o path",
            input.display()
        ));
    }
    if let (Ok(a), Ok(b)) = (fs::canonicalize(input), fs::canonicalize(output)) {
        if a == b {
            return Err(format!(
                "refusing to overwrite input ROM {} in place; choose a different -o path",
                input.display()
            ));
        }
    }
    Ok(())
}

fn load_charmap(table_opt: Option<&str>) -> Result<Charmap, String> {
    let Some(spec) = table_opt else {
        return Ok(Charmap::ascii());
    };
    let path = Path::new(spec);
    if path.is_file() {
        let content =
            fs::read_to_string(path).map_err(|e| format!("read charmap {}: {e}", path.display()))?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| spec.to_string());
        return Charmap::parse_tbl(name, &content)
            .map_err(|e| format!("parse .tbl {}: {e}", path.display()));
    }
    Charmap::builtin(spec).map_err(|e| format!("load charmap {spec:?}: {e}"))
}

fn parse_bpp(s: &str) -> Result<FontBpp, String> {
    match s {
        "1" => Ok(FontBpp::Bpp1),
        "2" => Ok(FontBpp::Bpp2),
        other => Err(format!("invalid --bpp {other:?} (expected 1 or 2)")),
    }
}

fn format_hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join("")
}

/// Dispatch `gbakit text <subcommand> ...`.
pub fn dispatch_text(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        return Err(TEXT_USAGE.into());
    }
    match args[0].as_str() {
        "tables" => cmd_text_tables(&args[1..]),
        "extract" => cmd_text_extract(&args[1..]),
        "scan" => cmd_text_scan(&args[1..]),
        "replace" => cmd_text_replace(&args[1..]),
        "huffman-extract" => cmd_text_huffman_extract(&args[1..]),
        "huffman-replace" => cmd_text_huffman_replace(&args[1..]),
        "font-dump" => cmd_text_font_dump(&args[1..]),
        "font-replace" => cmd_text_font_replace(&args[1..]),
        "font-widths" => cmd_text_font_widths(&args[1..]),
        other => Err(format!("unknown `text` subcommand {other:?}\n{TEXT_USAGE}")),
    }
}

/// `gbakit text tables [--json]`
pub fn cmd_text_tables(args: &[String]) -> Result<(), String> {
    let as_json = has_flag(args, "--json");
    if as_json {
        let payload = serde_json::json!({
            "tables": BUILTIN_CHARMAPS,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        println!("Built-in GBA Text Charmaps:");
        for info in BUILTIN_CHARMAPS {
            println!(
                "  {:<18} [END=0x{}]  {}",
                info.name, info.default_terminator_hex, info.description
            );
        }
        println!("\nAlso supports custom .tbl files (`--table path/to/game.tbl`) and `ascii-offset:<delta>`.");
    }
    Ok(())
}

/// `gbakit text extract <rom.gba> --offset <hex|dec> [--table <builtin|file.tbl>] [--lz77] [--max-len <n>] [--json]`
pub fn cmd_text_extract(args: &[String]) -> Result<(), String> {
    let rom_path = parse_positional_arg(args, EXTRACT_VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <rom.gba>\n{TEXT_USAGE}"))?;
    let rom_path = Path::new(rom_path);
    let offset = parse_offset(required(args, &["--offset"], "--offset <hex|dec>")?)
        .map_err(|e| format!("invalid --offset: {e}"))?;
    let lz77 = has_flag(args, "--lz77");
    let max_len = match parse_flag_value(args, &["--max-len"]) {
        Some(s) => parse_offset(s).map_err(|e| format!("invalid --max-len: {e}"))?,
        None => {
            if lz77 {
                65_536
            } else {
                DEFAULT_MAX_STRING_LEN
            }
        }
    };
    let charmap = load_charmap(parse_flag_value(args, &["--table"]))?;
    let as_json = has_flag(args, "--json");

    let rom = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;

    if lz77 {
        let extracted = extract_lz77_string(&rom, offset, &charmap, max_len)
            .map_err(|e| format!("extract LZ77 string at 0x{offset:X}: {e}"))?;
        if as_json {
            let payload = serde_json::json!({
                "table": charmap.name(),
                "lz77": true,
                "offset": format!("0x{:X}", extracted.offset),
                "bus_address": extracted.bus_address.map(|a| format!("0x{a:08X}")),
                "text": extracted.text,
                "compressed_bytes_len": extracted.compressed_bytes_len,
                "decompressed_bytes_len": extracted.decompressed_bytes_len,
                "payload_bytes_len": extracted.payload_bytes_len,
                "raw_bytes_len": extracted.raw_bytes_len,
                "terminated": extracted.terminated,
                "terminator_hex": extracted.terminator.as_deref().map(format_hex_bytes),
                "mapped_tokens": extracted.mapped_tokens,
                "unmapped_tokens": extracted.unmapped_tokens,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
            );
        } else {
            println!(
                "Extracted LZ77 String @ 0x{:X} (table: {}, {} compressed -> {} decompressed bytes, {}):",
                extracted.offset,
                charmap.name(),
                extracted.compressed_bytes_len,
                extracted.decompressed_bytes_len,
                if extracted.terminated {
                    "terminated"
                } else {
                    "unterminated"
                }
            );
            println!("{}", extracted.text);
        }
        return Ok(());
    }

    let extracted = extract_string(&rom, offset, &charmap, max_len)
        .map_err(|e| format!("extract string at 0x{offset:X}: {e}"))?;

    if as_json {
        let payload = serde_json::json!({
            "table": charmap.name(),
            "lz77": false,
            "offset": format!("0x{:X}", extracted.offset),
            "bus_address": extracted.bus_address.map(|a| format!("0x{a:08X}")),
            "text": extracted.text,
            "payload_bytes_len": extracted.payload_bytes_len,
            "raw_bytes_len": extracted.raw_bytes_len,
            "terminated": extracted.terminated,
            "terminator_hex": extracted.terminator.as_deref().map(format_hex_bytes),
            "mapped_tokens": extracted.mapped_tokens,
            "unmapped_tokens": extracted.unmapped_tokens,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "Extracted String @ 0x{:X} (table: {}, {} raw bytes, {}):",
            extracted.offset,
            charmap.name(),
            extracted.raw_bytes_len,
            if extracted.terminated {
                "terminated"
            } else {
                "unterminated"
            }
        );
        println!("{}", extracted.text);
    }
    Ok(())
}

/// `gbakit text scan <rom.gba> [--table <builtin|file.tbl>] [--min-entries <n>] [--limit <n>] [--json]`
pub fn cmd_text_scan(args: &[String]) -> Result<(), String> {
    let rom_path = parse_positional_arg(args, SCAN_VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <rom.gba>\n{TEXT_USAGE}"))?;
    let rom_path = Path::new(rom_path);
    let charmap = load_charmap(parse_flag_value(args, &["--table"]))?;
    let min_entries = match parse_flag_value(args, &["--min-entries"]) {
        Some(s) => parse_offset(s).map_err(|e| format!("invalid --min-entries: {e}"))?,
        None => 4,
    };
    let limit = match parse_flag_value(args, &["--limit"]) {
        Some(s) => Some(parse_offset(s).map_err(|e| format!("invalid --limit: {e}"))?),
        None => None,
    };
    let as_json = has_flag(args, "--json");

    let rom = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let mut tables = scan_string_tables(&rom, &charmap, min_entries);
    let total_detected = tables.len();
    if let Some(max_tables) = limit {
        tables.truncate(max_tables);
    }

    if as_json {
        let items: Vec<serde_json::Value> = tables
            .iter()
            .map(|t| {
                let entries: Vec<serde_json::Value> = t
                    .entries
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "pointer_offset": format!("0x{:X}", e.pointer_offset),
                            "target_offset": format!("0x{:X}", e.target_offset),
                            "bus_address": format!("0x{:08X}", e.bus_address),
                            "preview": e.preview,
                            "raw_bytes_len": e.raw_bytes_len,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "table_offset": format!("0x{:X}", t.table_offset),
                    "bus_address": format!("0x{:08X}", t.bus_address),
                    "entry_count": t.entry_count,
                    "valid_string_count": t.valid_string_count,
                    "entries": entries,
                })
            })
            .collect();
        let payload = serde_json::json!({
            "table": charmap.name(),
            "rom_bytes": rom.len(),
            "min_entries": min_entries,
            "total_tables_detected": total_detected,
            "tables_returned": tables.len(),
            "tables": items,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "String Table Scan: {} (table: {}, min_entries: {})",
            rom_path.display(),
            charmap.name(),
            min_entries
        );
        println!("  Detected Tables: {} (showing {})", total_detected, tables.len());
        for (idx, t) in tables.iter().enumerate() {
            println!(
                "  [{idx}] Table @ 0x{:X} (bus 0x{:08X}) — {}/{} valid strings",
                t.table_offset, t.bus_address, t.valid_string_count, t.entry_count
            );
            for e in t.entries.iter().take(5) {
                println!(
                    "        0x{:X} ({} B): {:?}",
                    e.target_offset, e.raw_bytes_len, e.preview
                );
            }
            if t.entries.len() > 5 {
                println!("        ... and {} more strings", t.entries.len() - 5);
            }
        }
    }
    Ok(())
}

/// `gbakit text replace <rom.gba> --offset <hex|dec> --text <str> -o <out.gba> [--table <builtin|file.tbl>] [--lz77] [...]`
pub fn cmd_text_replace(args: &[String]) -> Result<(), String> {
    let rom_path = parse_positional_arg(args, REPLACE_VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <rom.gba>\n{TEXT_USAGE}"))?;
    let rom_path = Path::new(rom_path);
    let offset = parse_offset(required(args, &["--offset"], "--offset <hex|dec>")?)
        .map_err(|e| format!("invalid --offset: {e}"))?;
    let new_text = required(args, &["--text"], "--text <str>")?;
    let out_path = Path::new(required(args, &["-o", "--output"], "-o <out.gba>")?);
    let charmap = load_charmap(parse_flag_value(args, &["--table"]))?;
    let lz77 = has_flag(args, "--lz77");
    let allow_relocate = has_flag(args, "--allow-relocate");
    let as_json = has_flag(args, "--json");

    let capacity = match parse_flag_value(args, &["--capacity"]) {
        Some(s) => Some(parse_offset(s).map_err(|e| format!("invalid --capacity: {e}"))?),
        None => None,
    };
    let pad_byte = match parse_flag_value(args, &["--pad-byte"]) {
        Some(s) => {
            let v = parse_offset(s).map_err(|e| format!("invalid --pad-byte: {e}"))?;
            Some(u8::try_from(v).map_err(|_| format!("--pad-byte {v} exceeds 0xFF"))?)
        }
        None => None,
    };

    ensure_distinct_paths(rom_path, out_path)?;

    let mut rom = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let options = TextReplaceOptions {
        capacity,
        pad_byte,
        allow_relocate,
        ..TextReplaceOptions::default()
    };

    let report = if lz77 {
        replace_lz77_string_in_rom(&mut rom, offset, new_text, &charmap, &options)
            .map_err(|e| format!("replace LZ77 string at 0x{offset:X}: {e}"))?
    } else {
        replace_string_in_rom(&mut rom, offset, new_text, &charmap, &options)
            .map_err(|e| format!("replace string at 0x{offset:X}: {e}"))?
    };

    fs::write(out_path, &rom).map_err(|e| format!("write {}: {e}", out_path.display()))?;

    let mode_id = match report.mode {
        TextReplaceMode::InPlace => "in_place",
        TextReplaceMode::Relocated => "relocated",
    };

    if as_json {
        let payload = serde_json::json!({
            "input": rom_path.display().to_string(),
            "output": out_path.display().to_string(),
            "table": charmap.name(),
            "lz77": lz77,
            "mode": mode_id,
            "original_offset": format!("0x{:X}", report.original_offset),
            "written_offset": format!("0x{:X}", report.written_offset),
            "written_bus_address": format!("0x{:08X}", report.written_bus_address),
            "original_bytes_len": report.original_bytes_len,
            "original_capacity": report.original_capacity,
            "new_bytes_len": report.new_bytes_len,
            "slack_bytes": report.slack_bytes,
            "patched_pointers": report.patched_pointer_count,
            "patched_pointer_offsets": report
                .patched_pointer_offsets
                .iter()
                .map(|o| format!("0x{o:X}"))
                .collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "{}Text Replace: {} -> {}",
            if lz77 { "LZ77 " } else { "" },
            rom_path.display(),
            out_path.display()
        );
        println!("  Charmap:          {}", charmap.name());
        println!("  Original Offset:  0x{:X}", report.original_offset);
        println!(
            "  Written Offset:   0x{:X} (bus 0x{:08X})",
            report.written_offset, report.written_bus_address
        );
        println!(
            "  Size:             {} bytes written / {} capacity (original {} bytes)",
            report.new_bytes_len, report.original_capacity, report.original_bytes_len
        );
        println!("  Mode:             {mode_id}");
        if report.mode == TextReplaceMode::Relocated {
            println!("  Patched Pointers: {}", report.patched_pointer_count);
        }
    }
    Ok(())
}

/// `gbakit text huffman-extract <rom.gba> --tree <off> --root <off> --offset <off> [--max-len <n>] [--json]`
pub fn cmd_text_huffman_extract(args: &[String]) -> Result<(), String> {
    let rom_path = parse_positional_arg(args, HUFFMAN_EXTRACT_VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <rom.gba>\n{TEXT_USAGE}"))?;
    let rom_path = Path::new(rom_path);
    let tree_offset = parse_offset(required(args, &["--tree"], "--tree <hex|dec>")?)
        .map_err(|e| format!("invalid --tree: {e}"))?;
    let root_offset = parse_offset(required(args, &["--root"], "--root <hex|dec>")?)
        .map_err(|e| format!("invalid --root: {e}"))?;
    let offset = parse_offset(required(args, &["--offset"], "--offset <hex|dec>")?)
        .map_err(|e| format!("invalid --offset: {e}"))?;
    let max_len = match parse_flag_value(args, &["--max-len"]) {
        Some(s) => parse_offset(s).map_err(|e| format!("invalid --max-len: {e}"))?,
        None => 4096,
    };
    let as_json = has_flag(args, "--json");

    let rom = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let tree = HuffmanTree::from_rom(&rom, tree_offset, root_offset)
        .map_err(|e| format!("parse Huffman tree (tree=0x{tree_offset:X}, root=0x{root_offset:X}): {e}"))?;
    let decoded = tree
        .decode_at(&rom, offset, max_len)
        .map_err(|e| format!("decode Huffman text at 0x{offset:X}: {e}"))?;

    if as_json {
        let payload = serde_json::json!({
            "tree_offset": format!("0x{tree_offset:X}"),
            "root_offset": format!("0x{root_offset:X}"),
            "tree_nodes": tree.node_count(),
            "tree_symbols": tree.symbol_count(),
            "offset": format!("0x{:X}", decoded.offset),
            "bus_address": decoded.bus_address.map(|a| format!("0x{a:08X}")),
            "text": decoded.text,
            "decoded_bytes_len": decoded.bytes.len(),
            "compressed_bytes_len": decoded.compressed_bytes_len,
            "bit_len": decoded.bit_len,
            "terminated": decoded.terminated,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "Extracted Huffman Text @ 0x{:X} ({} compressed bytes / {} bits -> {} bytes, {}):",
            decoded.offset,
            decoded.compressed_bytes_len,
            decoded.bit_len,
            decoded.bytes.len(),
            if decoded.terminated {
                "terminated"
            } else {
                "unterminated"
            }
        );
        println!("{}", decoded.text);
    }
    Ok(())
}

/// `gbakit text huffman-replace <rom.gba> --tree <off> --root <off> --offset <off> --text <str> -o <out.gba> [--allow-relocate] [--json]`
pub fn cmd_text_huffman_replace(args: &[String]) -> Result<(), String> {
    let rom_path = parse_positional_arg(args, HUFFMAN_REPLACE_VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <rom.gba>\n{TEXT_USAGE}"))?;
    let rom_path = Path::new(rom_path);
    let tree_offset = parse_offset(required(args, &["--tree"], "--tree <hex|dec>")?)
        .map_err(|e| format!("invalid --tree: {e}"))?;
    let root_offset = parse_offset(required(args, &["--root"], "--root <hex|dec>")?)
        .map_err(|e| format!("invalid --root: {e}"))?;
    let offset = parse_offset(required(args, &["--offset"], "--offset <hex|dec>")?)
        .map_err(|e| format!("invalid --offset: {e}"))?;
    let new_text = required(args, &["--text"], "--text <str>")?;
    let out_path = Path::new(required(args, &["-o", "--output"], "-o <out.gba>")?);
    let allow_relocate = has_flag(args, "--allow-relocate");
    let as_json = has_flag(args, "--json");

    let capacity = match parse_flag_value(args, &["--capacity"]) {
        Some(s) => Some(parse_offset(s).map_err(|e| format!("invalid --capacity: {e}"))?),
        None => None,
    };
    let pad_byte = match parse_flag_value(args, &["--pad-byte"]) {
        Some(s) => {
            let v = parse_offset(s).map_err(|e| format!("invalid --pad-byte: {e}"))?;
            Some(u8::try_from(v).map_err(|_| format!("--pad-byte {v} exceeds 0xFF"))?)
        }
        None => None,
    };

    ensure_distinct_paths(rom_path, out_path)?;

    let new_bytes =
        parse_huffman_text(new_text).map_err(|e| format!("parse Huffman --text: {e}"))?;
    let mut rom = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let options = TextReplaceOptions {
        capacity,
        pad_byte,
        allow_relocate,
        ..TextReplaceOptions::default()
    };

    let report = replace_huffman_text_in_rom(
        &mut rom,
        tree_offset,
        root_offset,
        offset,
        &new_bytes,
        &options,
    )
    .map_err(|e| format!("replace Huffman text at 0x{offset:X}: {e}"))?;

    fs::write(out_path, &rom).map_err(|e| format!("write {}: {e}", out_path.display()))?;

    let mode_id = match report.mode {
        TextReplaceMode::InPlace => "in_place",
        TextReplaceMode::Relocated => "relocated",
    };

    if as_json {
        let payload = serde_json::json!({
            "input": rom_path.display().to_string(),
            "output": out_path.display().to_string(),
            "tree_offset": format!("0x{tree_offset:X}"),
            "root_offset": format!("0x{root_offset:X}"),
            "mode": mode_id,
            "original_offset": format!("0x{:X}", report.original_offset),
            "written_offset": format!("0x{:X}", report.written_offset),
            "written_bus_address": format!("0x{:08X}", report.written_bus_address),
            "original_bytes_len": report.original_bytes_len,
            "original_capacity": report.original_capacity,
            "new_bytes_len": report.new_bytes_len,
            "slack_bytes": report.slack_bytes,
            "patched_pointers": report.patched_pointer_count,
            "patched_pointer_offsets": report
                .patched_pointer_offsets
                .iter()
                .map(|o| format!("0x{o:X}"))
                .collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "Huffman Text Replace: {} -> {}",
            rom_path.display(),
            out_path.display()
        );
        println!("  Original Offset:  0x{:X}", report.original_offset);
        println!(
            "  Written Offset:   0x{:X} (bus 0x{:08X})",
            report.written_offset, report.written_bus_address
        );
        println!(
            "  Size:             {} bytes written / {} capacity (original {} bytes)",
            report.new_bytes_len, report.original_capacity, report.original_bytes_len
        );
        println!("  Mode:             {mode_id}");
        if report.mode == TextReplaceMode::Relocated {
            println!("  Patched Pointers: {}", report.patched_pointer_count);
        }
    }
    Ok(())
}

/// `gbakit text font-dump <rom.gba> --offset <hex|dec> --count <n> --width <px> --height <px> --bpp 1|2 -o <out.png|ppm> [--lsb] [--cols <n>]`
pub fn cmd_text_font_dump(args: &[String]) -> Result<(), String> {
    let rom_path = parse_positional_arg(args, FONT_DUMP_VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <rom.gba>\n{TEXT_USAGE}"))?;
    let rom_path = Path::new(rom_path);
    let offset = parse_offset(required(args, &["--offset"], "--offset <hex|dec>")?)
        .map_err(|e| format!("invalid --offset: {e}"))?;
    let glyph_count = parse_offset(required(args, &["--count"], "--count <n>")?)
        .map_err(|e| format!("invalid --count: {e}"))?;
    let glyph_width = parse_offset(required(args, &["--width"], "--width <px>")?)
        .map_err(|e| format!("invalid --width: {e}"))?;
    let glyph_height = parse_offset(required(args, &["--height"], "--height <px>")?)
        .map_err(|e| format!("invalid --height: {e}"))?;
    let bpp = parse_bpp(required(args, &["--bpp"], "--bpp 1|2")?)?;
    let out_path = Path::new(required(args, &["-o", "--output"], "-o <out.png|ppm>")?);
    let msb_first = !has_flag(args, "--lsb");
    let as_json = has_flag(args, "--json");
    let cols = match parse_flag_value(args, &["--cols"]) {
        Some(s) => parse_offset(s).map_err(|e| format!("invalid --cols: {e}"))?,
        None => 16,
    };

    let format = match out_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("ppm") => FontImageFormat::Ppm,
        Some("png") | None => FontImageFormat::Png,
        Some(other) => {
            return Err(format!(
                "unsupported font sheet output extension '.{other}' (expected .png or .ppm)"
            ))
        }
    };

    let cfg = FontSheetConfig {
        glyph_count,
        glyph_width,
        glyph_height,
        bpp,
        msb_first,
        cols,
    };

    let rom = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let (sheet_w, sheet_h) = cfg
        .sheet_dimensions()
        .map_err(|e| format!("invalid font sheet geometry: {e}"))?;
    let image_bytes = dump_font_sheet(&rom, offset, &cfg, format)
        .map_err(|e| format!("dump font sheet at 0x{offset:X}: {e}"))?;

    fs::write(out_path, &image_bytes).map_err(|e| format!("write {}: {e}", out_path.display()))?;

    let fmt_str = match format {
        FontImageFormat::Png => "png",
        FontImageFormat::Ppm => "ppm",
    };

    if as_json {
        let payload = serde_json::json!({
            "input": rom_path.display().to_string(),
            "output": out_path.display().to_string(),
            "format": fmt_str,
            "offset": format!("0x{offset:X}"),
            "glyph_count": glyph_count,
            "glyph_width": glyph_width,
            "glyph_height": glyph_height,
            "bpp": bpp.bits(),
            "msb_first": msb_first,
            "cols": cols.min(glyph_count).max(1),
            "sheet_width": sheet_w,
            "sheet_height": sheet_h,
            "output_bytes": image_bytes.len(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "Font Sheet Dumped: {} @ 0x{:X} -> {} ({}x{} {}, {} glyphs of {}x{} {}bpp {})",
            rom_path.display(),
            offset,
            out_path.display(),
            sheet_w,
            sheet_h,
            fmt_str.to_uppercase(),
            glyph_count,
            glyph_width,
            glyph_height,
            bpp.bits(),
            if msb_first { "MSB-first" } else { "LSB-first" }
        );
    }
    Ok(())
}

/// `gbakit text font-replace <rom.gba> --offset <hex|dec> --image <in.png|ppm> --count <n> --width <px> --height <px> --bpp 1|2 -o <out.gba> [--lsb] [--cols <n>] [--json]`
pub fn cmd_text_font_replace(args: &[String]) -> Result<(), String> {
    let rom_path = parse_positional_arg(args, FONT_REPLACE_VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <rom.gba>\n{TEXT_USAGE}"))?;
    let rom_path = Path::new(rom_path);
    let offset = parse_offset(required(args, &["--offset"], "--offset <hex|dec>")?)
        .map_err(|e| format!("invalid --offset: {e}"))?;
    let image_path = Path::new(required(args, &["--image"], "--image <in.png|ppm>")?);
    let glyph_count = parse_offset(required(args, &["--count"], "--count <n>")?)
        .map_err(|e| format!("invalid --count: {e}"))?;
    let glyph_width = parse_offset(required(args, &["--width"], "--width <px>")?)
        .map_err(|e| format!("invalid --width: {e}"))?;
    let glyph_height = parse_offset(required(args, &["--height"], "--height <px>")?)
        .map_err(|e| format!("invalid --height: {e}"))?;
    let bpp = parse_bpp(required(args, &["--bpp"], "--bpp 1|2")?)?;
    let out_path = Path::new(required(args, &["-o", "--output"], "-o <out.gba>")?);
    let msb_first = !has_flag(args, "--lsb");
    let as_json = has_flag(args, "--json");
    let cols = match parse_flag_value(args, &["--cols"]) {
        Some(s) => parse_offset(s).map_err(|e| format!("invalid --cols: {e}"))?,
        None => 16,
    };

    ensure_distinct_paths(rom_path, out_path)?;

    let mut rom = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let image_bytes =
        fs::read(image_path).map_err(|e| format!("read {}: {e}", image_path.display()))?;

    let cfg = FontSheetConfig {
        glyph_count,
        glyph_width,
        glyph_height,
        bpp,
        msb_first,
        cols,
    };

    let report = replace_font_sheet_in_rom(&mut rom, offset, &image_bytes, &cfg)
        .map_err(|e| format!("replace font sheet at 0x{offset:X}: {e}"))?;

    fs::write(out_path, &rom).map_err(|e| format!("write {}: {e}", out_path.display()))?;

    if as_json {
        let payload = serde_json::json!({
            "input": rom_path.display().to_string(),
            "output": out_path.display().to_string(),
            "image": image_path.display().to_string(),
            "offset": format!("0x{:X}", report.offset),
            "glyph_count": report.glyph_count,
            "glyph_width": report.glyph_width,
            "glyph_height": report.glyph_height,
            "bpp": report.bpp,
            "msb_first": report.msb_first,
            "cols": report.cols,
            "bytes_written": report.bytes_written,
            "bytes_changed": report.bytes_changed,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        println!("Font Replace: {} -> {}", rom_path.display(), out_path.display());
        println!("  Image:          {}", image_path.display());
        println!("  Offset:         0x{:X}", report.offset);
        println!(
            "  Glyphs:         {} ({}x{}, {}bpp, {})",
            report.glyph_count,
            report.glyph_width,
            report.glyph_height,
            report.bpp,
            if report.msb_first { "MSB-first" } else { "LSB-first" }
        );
        println!(
            "  Bytes Written:  {} ({} changed)",
            report.bytes_written, report.bytes_changed
        );
    }
    Ok(())
}

/// `gbakit text font-widths <rom.gba> --offset <off> --count <n> [--from-image <sheet.png> --width <px> --height <px> [--cols <n>] [--pad <px>] -o <out.gba>] [--json]`
pub fn cmd_text_font_widths(args: &[String]) -> Result<(), String> {
    let rom_path = parse_positional_arg(args, FONT_WIDTHS_VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <rom.gba>\n{TEXT_USAGE}"))?;
    let rom_path = Path::new(rom_path);
    let offset = parse_offset(required(args, &["--offset"], "--offset <hex|dec>")?)
        .map_err(|e| format!("invalid --offset: {e}"))?;
    let count = parse_offset(required(args, &["--count"], "--count <n>")?)
        .map_err(|e| format!("invalid --count: {e}"))?;
    let as_json = has_flag(args, "--json");

    if let Some(image_spec) = parse_flag_value(args, &["--from-image"]) {
        let image_path = Path::new(image_spec);
        let glyph_w = parse_offset(required(args, &["--width"], "--width <px>")?)
            .map_err(|e| format!("invalid --width: {e}"))?;
        let glyph_h = parse_offset(required(args, &["--height"], "--height <px>")?)
            .map_err(|e| format!("invalid --height: {e}"))?;
        let cols = match parse_flag_value(args, &["--cols"]) {
            Some(s) => parse_offset(s).map_err(|e| format!("invalid --cols: {e}"))?,
            None => 16,
        };
        let pad = match parse_flag_value(args, &["--pad"]) {
            Some(s) => {
                let v = parse_offset(s).map_err(|e| format!("invalid --pad: {e}"))?;
                u8::try_from(v).map_err(|_| format!("--pad {v} exceeds 255"))?
            }
            None => 1,
        };
        let out_path = Path::new(required(args, &["-o", "--output"], "-o <out.gba>")?);
        ensure_distinct_paths(rom_path, out_path)?;

        let mut rom = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
        let image_bytes =
            fs::read(image_path).map_err(|e| format!("read {}: {e}", image_path.display()))?;
        let widths =
            compute_vwf_widths_from_sheet(&image_bytes, count, glyph_w, glyph_h, cols, pad)
                .map_err(|e| format!("compute VWF widths from {}: {e}", image_path.display()))?;
        let report = replace_vwf_widths_in_rom(&mut rom, offset, &widths)
            .map_err(|e| format!("replace VWF widths at 0x{offset:X}: {e}"))?;
        fs::write(out_path, &rom).map_err(|e| format!("write {}: {e}", out_path.display()))?;

        if as_json {
            let payload = serde_json::json!({
                "input": rom_path.display().to_string(),
                "output": out_path.display().to_string(),
                "image": image_path.display().to_string(),
                "offset": format!("0x{:X}", report.offset),
                "count": report.count,
                "padding": pad,
                "bytes_changed": report.bytes_changed,
                "widths": report.widths,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
            );
        } else {
            println!(
                "VWF Widths Updated: {} -> {} (@ 0x{:X}, {} glyphs, {} changed, pad={}px)",
                rom_path.display(),
                out_path.display(),
                report.offset,
                report.count,
                report.bytes_changed,
                pad
            );
            println!("  Widths: {:?}", report.widths);
        }
        return Ok(());
    }

    let rom = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let widths = read_vwf_widths(&rom, offset, count)
        .map_err(|e| format!("read VWF widths at 0x{offset:X}: {e}"))?;

    if as_json {
        let payload = serde_json::json!({
            "input": rom_path.display().to_string(),
            "offset": format!("0x{offset:X}"),
            "count": widths.len(),
            "widths": widths,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "VWF Widths @ 0x{:X} ({} glyphs): {:?}",
            offset,
            widths.len(),
            widths
        );
    }
    Ok(())
}
