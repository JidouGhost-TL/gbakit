//! CLI subcommands for GBA studio archives (`PACK`, `WAYFORWARD`, `SFCD`):
//! `gbakit archive scan|extract|repack|replace`.

use std::fs;
use std::path::Path;

use gbakit::rom::{
    extract_archive_entries, pack_archive, parse_archive_at, replace_archive_in_rom, scan_archives,
    ArchiveFormat, ArchiveManifest, ArchiveManifestEntry, ArchiveReplaceOptions,
};

use super::{parse_flag_value, parse_offset, parse_positional_arg};

const ARCHIVE_USAGE: &str = "usage:\n  \
  gbakit archive scan    <rom.gba> [--json]\n  \
  gbakit archive extract <rom_or_archive> [--offset <hex|dec>] -o <out_dir/> [--json]\n  \
  gbakit archive repack  <in_dir/> -o <out.bin> [--format pack|wayforward|sfcd] [--json]\n  \
  gbakit archive replace <rom.gba> --offset <hex|dec> --input <dir_or_bin> -o <out.gba> [--format pack|wayforward|sfcd] [--capacity <bytes>] [--allow-relocate] [--pad-byte <n>] [--json]";

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

fn parse_archive_format(s: &str) -> Result<ArchiveFormat, String> {
    ArchiveFormat::from_name(s)
        .ok_or_else(|| format!("unknown --format {s:?} (expected pack|wayforward|sfcd)"))
}

pub fn dispatch_archive(args: &[String]) -> Result<(), String> {
    let Some(sub) = args.first().map(String::as_str) else {
        return Err(ARCHIVE_USAGE.into());
    };
    match sub {
        "scan" => cmd_archive_scan(&args[1..]),
        "extract" => cmd_archive_extract(&args[1..]),
        "repack" => cmd_archive_repack(&args[1..]),
        "replace" => cmd_archive_replace(&args[1..]),
        _ => Err(format!("unknown archive subcommand {sub:?}\n{ARCHIVE_USAGE}")),
    }
}

fn cmd_archive_scan(args: &[String]) -> Result<(), String> {
    let as_json = has_flag(args, "--json");
    let rom_path = parse_positional_arg(args, &[])
        .ok_or_else(|| format!("missing required <rom.gba>\n{ARCHIVE_USAGE}"))?;
    let rom_path = Path::new(rom_path);
    let bytes = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let archives = scan_archives(&bytes);

    if as_json {
        let items: Vec<serde_json::Value> = archives
            .iter()
            .map(|a| {
                serde_json::json!({
                    "format": a.format.as_str(),
                    "offset": format!("0x{:X}", a.rom_offset),
                    "total_size": a.total_size,
                    "has_end_sentinel": a.has_end_sentinel,
                    "entry_count": a.entries.len(),
                    "entries": a.entries.iter().map(|e| serde_json::json!({
                        "index": e.index,
                        "name": e.name,
                        "offset": format!("0x{:X}", e.offset),
                        "size": e.size,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        let payload = serde_json::json!({
            "input": rom_path.display().to_string(),
            "archive_count": archives.len(),
            "archives": items,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else if archives.is_empty() {
        println!("No PACK / WAYFORWARD / SFCD archives detected in {}.", rom_path.display());
    } else {
        println!(
            "Detected {} studio archive(s) in {}:",
            archives.len(),
            rom_path.display()
        );
        for (idx, a) in archives.iter().enumerate() {
            println!(
                "  [{}] {:<10} @ 0x{:06X} | {} entries | {} bytes",
                idx,
                a.format.as_str().to_uppercase(),
                a.rom_offset,
                a.entries.len(),
                a.total_size
            );
        }
    }
    Ok(())
}

fn cmd_archive_extract(args: &[String]) -> Result<(), String> {
    const VALUE_FLAGS: &[&[&str]] = &[&["--offset"], &["-o", "--output"]];
    let as_json = has_flag(args, "--json");
    let input_path = parse_positional_arg(args, VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <rom_or_archive>\n{ARCHIVE_USAGE}"))?;
    let input_path = Path::new(input_path);
    let out_dir = Path::new(required(args, &["-o", "--output"], "-o <out_dir/>")?);
    let offset_opt = match parse_flag_value(args, &["--offset"]) {
        Some(s) => Some(parse_offset(s).map_err(|e| format!("invalid --offset: {e}"))?),
        None => None,
    };

    let bytes = fs::read(input_path).map_err(|e| format!("read {}: {e}", input_path.display()))?;
    let archive = match offset_opt {
        Some(off) => parse_archive_at(&bytes, off)
            .map_err(|e| format!("parse archive at 0x{off:X}: {e}"))?,
        None => match parse_archive_at(&bytes, 0) {
            Ok(a) => a,
            Err(_) => {
                let scanned = scan_archives(&bytes);
                match scanned.len() {
                    0 => {
                        return Err(format!(
                            "no PACK / WAYFORWARD / SFCD archive found in {}",
                            input_path.display()
                        ));
                    }
                    1 => scanned.into_iter().next().expect("checked len == 1"),
                    n => {
                        return Err(format!(
                            "found {n} archives in {}; specify --offset <hex|dec> to choose one",
                            input_path.display()
                        ));
                    }
                }
            }
        },
    };

    let extracted = extract_archive_entries(&bytes, &archive)
        .map_err(|e| format!("extract archive entries: {e}"))?;
    fs::create_dir_all(out_dir)
        .map_err(|e| format!("create output dir {}: {e}", out_dir.display()))?;

    let mut manifest_entries = Vec::with_capacity(extracted.len());
    for (entry, data) in &extracted {
        let filename = match archive.format {
            ArchiveFormat::Sfcd => {
                let safe: String = entry
                    .name
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                            c
                        } else {
                            '_'
                        }
                    })
                    .collect();
                format!("{:04}_{safe}.bin", entry.index)
            }
            ArchiveFormat::Pack | ArchiveFormat::WayForward => {
                format!("entry_{:04}.bin", entry.index)
            }
        };
        let entry_path = out_dir.join(&filename);
        fs::write(&entry_path, data)
            .map_err(|e| format!("write {}: {e}", entry_path.display()))?;
        manifest_entries.push(ArchiveManifestEntry {
            index: entry.index,
            name: entry.name.clone(),
            file: filename,
            original_offset: entry.offset,
            size: entry.size,
        });
    }

    let manifest = ArchiveManifest {
        format: archive.format,
        source_offset: archive.rom_offset,
        total_size: archive.total_size,
        entries: manifest_entries,
    };
    let manifest_path = out_dir.join("manifest.json");
    let manifest_json = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    fs::write(&manifest_path, &manifest_json)
        .map_err(|e| format!("write {}: {e}", manifest_path.display()))?;

    if as_json {
        let payload = serde_json::json!({
            "input": input_path.display().to_string(),
            "output_dir": out_dir.display().to_string(),
            "manifest": manifest_path.display().to_string(),
            "format": archive.format.as_str(),
            "offset": format!("0x{:X}", archive.rom_offset),
            "total_size": archive.total_size,
            "entry_count": archive.entries.len(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "Extracted {} archive ({} entries, {} bytes) from 0x{:X} -> {}",
            archive.format.as_str().to_uppercase(),
            archive.entries.len(),
            archive.total_size,
            archive.rom_offset,
            out_dir.display()
        );
    }
    Ok(())
}

fn build_archive_from_dir(
    in_dir: &Path,
    format_override: Option<ArchiveFormat>,
) -> Result<(ArchiveFormat, usize, Vec<u8>), String> {
    let manifest_path = in_dir.join("manifest.json");
    let (format, owned_entries): (ArchiveFormat, Vec<(String, Vec<u8>)>) =
        if manifest_path.is_file() {
            let text = fs::read_to_string(&manifest_path)
                .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
            let manifest: ArchiveManifest = serde_json::from_str(&text)
                .map_err(|e| format!("parse {}: {e}", manifest_path.display()))?;
            let fmt = format_override.unwrap_or(manifest.format);
            let mut items = Vec::with_capacity(manifest.entries.len());
            for entry in &manifest.entries {
                let file_path = in_dir.join(&entry.file);
                let data = fs::read(&file_path)
                    .map_err(|e| format!("read {}: {e}", file_path.display()))?;
                items.push((entry.name.clone(), data));
            }
            (fmt, items)
        } else {
            let fmt = format_override.ok_or_else(|| {
                format!(
                    "no manifest.json in {}; specify --format pack|wayforward|sfcd",
                    in_dir.display()
                )
            })?;
            let mut paths = Vec::new();
            let rd = fs::read_dir(in_dir)
                .map_err(|e| format!("read dir {}: {e}", in_dir.display()))?;
            for entry in rd {
                let entry = entry.map_err(|e| e.to_string())?;
                let path = entry.path();
                if path.is_file()
                    && path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n != "manifest.json")
                {
                    paths.push(path);
                }
            }
            paths.sort();
            let mut items = Vec::with_capacity(paths.len());
            for path in paths {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("ENTRY")
                    .to_string();
                let data =
                    fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
                items.push((name, data));
            }
            (fmt, items)
        };

    let refs: Vec<(&str, &[u8])> = owned_entries
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    let packed = pack_archive(format, &refs).map_err(|e| format!("repack archive: {e}"))?;
    Ok((format, owned_entries.len(), packed))
}

fn cmd_archive_repack(args: &[String]) -> Result<(), String> {
    const VALUE_FLAGS: &[&[&str]] = &[&["-o", "--output"], &["--format"]];
    let as_json = has_flag(args, "--json");
    let in_dir = parse_positional_arg(args, VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <in_dir/>\n{ARCHIVE_USAGE}"))?;
    let in_dir = Path::new(in_dir);
    let out_path = Path::new(required(args, &["-o", "--output"], "-o <out.bin>")?);
    let format_override = match parse_flag_value(args, &["--format"]) {
        Some(s) => Some(parse_archive_format(s)?),
        None => None,
    };

    let (format, entry_count, packed) = build_archive_from_dir(in_dir, format_override)?;
    fs::write(out_path, &packed).map_err(|e| format!("write {}: {e}", out_path.display()))?;

    if as_json {
        let payload = serde_json::json!({
            "input_dir": in_dir.display().to_string(),
            "output": out_path.display().to_string(),
            "format": format.as_str(),
            "entry_count": entry_count,
            "written_bytes": packed.len(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "Repacked {} archive ({} entries, {} bytes) -> {}",
            format.as_str().to_uppercase(),
            entry_count,
            packed.len(),
            out_path.display()
        );
    }
    Ok(())
}

fn cmd_archive_replace(args: &[String]) -> Result<(), String> {
    const VALUE_FLAGS: &[&[&str]] = &[
        &["--offset"],
        &["--input"],
        &["-o", "--output"],
        &["--format"],
        &["--capacity"],
        &["--pad-byte"],
    ];
    let as_json = has_flag(args, "--json");
    let allow_relocate = has_flag(args, "--allow-relocate");
    let rom_path = parse_positional_arg(args, VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <rom.gba>\n{ARCHIVE_USAGE}"))?;
    let rom_path = Path::new(rom_path);
    let offset = parse_offset(required(args, &["--offset"], "--offset <hex|dec>")?)
        .map_err(|e| format!("invalid --offset: {e}"))?;
    let input_path = Path::new(required(args, &["--input"], "--input <dir_or_bin>")?);
    let out_path = Path::new(required(args, &["-o", "--output"], "-o <out.gba>")?);
    let format_override = match parse_flag_value(args, &["--format"]) {
        Some(s) => Some(parse_archive_format(s)?),
        None => None,
    };
    let capacity = match parse_flag_value(args, &["--capacity"]) {
        Some(s) => Some(parse_offset(s).map_err(|e| format!("invalid --capacity: {e}"))?),
        None => None,
    };
    let pad_byte = match parse_flag_value(args, &["--pad-byte"]) {
        Some(s) => {
            let v = parse_offset(s).map_err(|e| format!("invalid --pad-byte: {e}"))?;
            u8::try_from(v).map_err(|_| format!("--pad-byte {v} exceeds 0xFF"))?
        }
        None => 0x00,
    };
    ensure_distinct_paths(rom_path, out_path)?;

    let mut rom = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    if offset >= rom.len() {
        return Err(format!(
            "--offset 0x{offset:X} exceeds ROM length {}",
            rom.len()
        ));
    }

    let archive_bytes = if input_path.is_dir() {
        build_archive_from_dir(input_path, format_override)?.2
    } else {
        fs::read(input_path).map_err(|e| format!("read {}: {e}", input_path.display()))?
    };

    let options = ArchiveReplaceOptions {
        original_capacity: capacity,
        pad_byte,
        allow_relocate,
        ..ArchiveReplaceOptions::default()
    };
    let report = replace_archive_in_rom(&mut rom, offset, &archive_bytes, &options)
        .map_err(|e| format!("replace archive at 0x{offset:X}: {e}"))?;

    fs::write(out_path, &rom).map_err(|e| format!("write {}: {e}", out_path.display()))?;

    if as_json {
        let payload = serde_json::json!({
            "input": rom_path.display().to_string(),
            "output": out_path.display().to_string(),
            "archive_input": input_path.display().to_string(),
            "format": report.format.as_str(),
            "entry_count": report.entry_count,
            "original_offset": format!("0x{:X}", report.original_offset),
            "written_offset": format!("0x{:X}", report.written_offset),
            "written_bus_address": format!("0x{:08X}", report.written_bus_address),
            "original_capacity": report.original_capacity,
            "written_bytes": report.written_bytes,
            "slack_bytes": report.slack_bytes,
            "relocated": report.relocated,
            "patched_pointers": report.patched_pointers,
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
            "Archive Replace: {} -> {}",
            rom_path.display(),
            out_path.display()
        );
        println!(
            "  Format:             {} ({} entries)",
            report.format.as_str().to_uppercase(),
            report.entry_count
        );
        println!("  Original Offset:    0x{:X}", report.original_offset);
        println!(
            "  Written Offset:     0x{:X} (bus 0x{:08X})",
            report.written_offset, report.written_bus_address
        );
        println!(
            "  Size:               {} bytes written / {} capacity",
            report.written_bytes, report.original_capacity
        );
        println!(
            "  Mode:               {}",
            if report.relocated {
                "relocated"
            } else {
                "in-place"
            }
        );
        if report.relocated {
            println!("  Patched Pointers:   {}", report.patched_pointers);
        }
    }
    Ok(())
}
