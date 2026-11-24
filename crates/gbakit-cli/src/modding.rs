//! CLI subcommands for ROM modding round-trips: `gfx replace` and `audio replace`.
//!
//! Both commands are strictly non-destructive: the input ROM is read into memory, modified
//! there, and written to a separate `-o <out.gba>` path. Writing over the input file is refused.

use std::fs;
use std::path::Path;

use gbakit::gfx::{
    replace_palette_in_rom, replace_tilemap_in_rom, replace_tileset_bytes_in_rom, GfxCompression,
    PaletteMode, ReplaceOptions, TileBpp, TilemapInputFormat, TilemapReplaceOptions,
};
use gbakit::sound::{
    build_m4a_wavedata_from_i16, encode_ima_adpcm4, encode_m4a_dpcm, encode_pcm8_signed,
    encode_pcm8_unsigned, quantize_i16_to_i8, read_wav_pcm_mono_i16_resampled,
    replace_m4a_wavedata_in_rom, replace_pcm8_in_rom, AudioReplaceMode, AudioReplaceOptions,
    ImaAdpcmVariant, M4aWaveBuildOptions, M4aWaveCodec, WaveDataHeader,
};

use super::{parse_flag_value, parse_offset, parse_positional_arg};

/// Flags taking a value for `gfx replace` (used to skip values when finding the positional ROM).
const GFX_VALUE_FLAGS: &[&[&str]] = &[
    &["--offset"],
    &["--image"],
    &["-o", "--output"],
    &["--bpp"],
    &["--palette-offset"],
    &["--compression"],
    &["--pad-byte"],
    &["--capacity"],
];

/// Flags taking a value for `audio replace`.
const AUDIO_VALUE_FLAGS: &[&[&str]] = &[
    &["--offset"],
    &["--wav"],
    &["-o", "--output"],
    &["--codec"],
    &["--rate"],
    &["--loop-start"],
    &["--raw-capacity"],
];

pub const GFX_REPLACE_USAGE: &str = "usage: gbakit gfx replace <rom.gba> --offset <hex|dec> --image <in.png|ppm> -o <out.gba> \
[--bpp 4|8] [--palette-offset <hex|dec>] [--write-palette] \
[--compression auto|raw|lz77-vram|lz77-wram|rle|huff4|huff8] [--capacity <bytes>] \
[--allow-relocate] [--pad-byte <n>] [--json]";

pub const AUDIO_REPLACE_USAGE: &str = "usage: gbakit audio replace <rom.gba> --offset <hex|dec> --wav <in.wav> -o <out.gba> \
[--codec pcm8|dpcm|pcm8-unsigned|ima-adpcm|ima-adpcm-arm] [--rate <hz>] [--loop-start <samples>] \
[--raw-capacity <bytes>] [--allow-relocate] [--json]";

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn required<'a>(args: &'a [String], names: &[&str], what: &str) -> Result<&'a str, String> {
    parse_flag_value(args, names).ok_or_else(|| format!("missing required {what}"))
}

fn parse_u32_arg(s: &str, what: &str) -> Result<u32, String> {
    let v = parse_offset(s).map_err(|e| format!("invalid {what} {s:?}: {e}"))?;
    u32::try_from(v).map_err(|_| format!("{what} {v} exceeds u32 range"))
}

/// Refuse to write the output over the input ROM (never modify the input in place).
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

fn parse_gfx_compression(s: &str) -> Result<Option<GfxCompression>, String> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "auto" => None,
        "raw" | "none" => Some(GfxCompression::Raw),
        "lz77-vram" | "lz77" | "lz10" => Some(GfxCompression::Lz77Vram),
        "lz77-wram" => Some(GfxCompression::Lz77Wram),
        "rle" => Some(GfxCompression::Rle),
        "huff4" | "huffman4" => Some(GfxCompression::Huffman4),
        "huff8" | "huffman8" => Some(GfxCompression::Huffman8),
        other => {
            return Err(format!(
                "unknown --compression {other:?} (expected auto|raw|lz77-vram|lz77-wram|rle|huff4|huff8)"
            ))
        }
    })
}

fn gfx_compression_id(c: GfxCompression) -> &'static str {
    match c {
        GfxCompression::Raw => "raw",
        GfxCompression::Lz77Wram => "lz77-wram",
        GfxCompression::Lz77Vram => "lz77-vram",
        GfxCompression::Rle => "rle",
        GfxCompression::Huffman4 => "huff4",
        GfxCompression::Huffman8 => "huff8",
    }
}

/// `gbakit gfx replace <rom.gba> --offset <n> --image <in.png|ppm> -o <out.gba> [...]`
pub fn cmd_gfx_replace(args: &[String]) -> Result<(), String> {
    let rom_path = parse_positional_arg(args, GFX_VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <rom.gba>\n{GFX_REPLACE_USAGE}"))?;
    let rom_path = Path::new(rom_path);
    let offset = parse_offset(required(args, &["--offset"], "--offset <hex|dec>")?)
        .map_err(|e| format!("invalid --offset: {e}"))?;
    let image_path = Path::new(required(args, &["--image"], "--image <in.png|ppm>")?);
    let out_path = Path::new(required(args, &["-o", "--output"], "-o <out.gba>")?);
    let as_json = has_flag(args, "--json");
    let write_palette = has_flag(args, "--write-palette");
    let allow_relocate = has_flag(args, "--allow-relocate");

    let bpp = match parse_flag_value(args, &["--bpp"]).unwrap_or("4") {
        "4" => TileBpp::Bpp4,
        "8" => TileBpp::Bpp8,
        other => return Err(format!("invalid --bpp {other:?} (expected 4 or 8)")),
    };
    let compression = parse_gfx_compression(parse_flag_value(args, &["--compression"]).unwrap_or("auto"))?;
    let pad_byte = match parse_flag_value(args, &["--pad-byte"]) {
        Some(s) => {
            let v = parse_offset(s).map_err(|e| format!("invalid --pad-byte: {e}"))?;
            u8::try_from(v).map_err(|_| format!("--pad-byte {v} exceeds 0xFF"))?
        }
        None => 0x00,
    };
    let capacity = match parse_flag_value(args, &["--capacity"]) {
        Some(s) => Some(parse_offset(s).map_err(|e| format!("invalid --capacity: {e}"))?),
        None => None,
    };
    let palette_offset = match parse_flag_value(args, &["--palette-offset"]) {
        Some(s) => Some(parse_offset(s).map_err(|e| format!("invalid --palette-offset: {e}"))?),
        None => None,
    };
    if write_palette && palette_offset.is_none() {
        return Err("--write-palette requires --palette-offset <hex|dec>".into());
    }
    ensure_distinct_paths(rom_path, out_path)?;

    let mut rom = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let image_bytes =
        fs::read(image_path).map_err(|e| format!("read {}: {e}", image_path.display()))?;
    if offset >= rom.len() {
        return Err(format!("--offset 0x{offset:X} exceeds ROM length {}", rom.len()));
    }

    // Auto mode: probe the original stream so we can report a clear error instead of a
    // generic failure when no BIOS stream exists and no explicit capacity was supplied.
    if compression.is_none() && capacity.is_none() {
        let detected = gbakit::gfx::detect_stream_at(&rom, offset, gbakit::compress::DEFAULT_MAX_OUTPUT)
            .map_err(|e| format!("probe stream at 0x{offset:X}: {e}"))?;
        if detected.is_none() {
            return Err(format!(
                "--compression auto: no LZ77/RLE/Huffman stream detected at 0x{offset:X}; \
                 the asset is treated as raw, which requires an explicit --capacity <bytes> \
                 (or pass an explicit --compression codec)"
            ));
        }
    }
    if compression == Some(GfxCompression::Raw) && capacity.is_none() {
        return Err("--compression raw requires an explicit --capacity <bytes> for the original slot".into());
    }

    // Resolve palette strategy.
    let palette_bytes_len = bpp.max_colors() * 2;
    let existing_palette: Option<Vec<u8>> = match (palette_offset, write_palette) {
        (Some(pal_off), false) => {
            let end = pal_off
                .checked_add(palette_bytes_len)
                .filter(|&e| e <= rom.len())
                .ok_or_else(|| {
                    format!(
                        "--palette-offset 0x{pal_off:X} + {palette_bytes_len} bytes exceeds ROM length {}",
                        rom.len()
                    )
                })?;
            Some(rom[pal_off..end].to_vec())
        }
        _ => None,
    };
    let palette_mode = match &existing_palette {
        Some(bytes) => PaletteMode::ExistingBgr555(bytes),
        None => PaletteMode::Generate,
    };

    let options = ReplaceOptions {
        compression,
        original_capacity: capacity,
        pad_byte,
        allow_relocate,
        ..ReplaceOptions::default()
    };

    let (report, encoded) =
        replace_tileset_bytes_in_rom(&mut rom, offset, &image_bytes, bpp, palette_mode, &options)
            .map_err(|e| format!("replace tileset at 0x{offset:X}: {e}"))?;

    let palette_report = match (palette_offset, write_palette) {
        (Some(pal_off), true) => {
            let pal_opts = ReplaceOptions {
                pad_byte,
                allow_relocate,
                ..ReplaceOptions::default()
            };
            Some(
                replace_palette_in_rom(&mut rom, pal_off, &encoded.palette_rgba, &pal_opts)
                    .map_err(|e| format!("write palette at 0x{pal_off:X}: {e}"))?,
            )
        }
        _ => None,
    };

    fs::write(out_path, &rom).map_err(|e| format!("write {}: {e}", out_path.display()))?;

    let palette_mode_id = match (palette_offset, write_palette) {
        (Some(_), true) => "generated_written",
        (Some(_), false) => "matched_existing",
        (None, _) => "generated_not_written",
    };

    if as_json {
        let payload = serde_json::json!({
            "input": rom_path.display().to_string(),
            "output": out_path.display().to_string(),
            "image": image_path.display().to_string(),
            "bpp": bpp.bits(),
            "tile_count": encoded.tile_count,
            "width_tiles": encoded.width_tiles,
            "height_tiles": encoded.height_tiles,
            "palette_mode": palette_mode_id,
            "compression": gfx_compression_id(report.compression),
            "original_offset": format!("0x{:X}", report.original_offset),
            "written_offset": format!("0x{:X}", report.written_offset),
            "written_bus_address": format!("0x{:08X}", report.written_bus_address),
            "original_capacity": report.original_capacity,
            "uncompressed_bytes": report.uncompressed_bytes,
            "written_bytes": report.written_bytes,
            "slack_bytes": report.slack_bytes,
            "relocated": report.relocated,
            "patched_pointers": report.patched_pointers,
            "patched_pointer_offsets": report
                .patched_pointer_offsets
                .iter()
                .map(|o| format!("0x{o:X}"))
                .collect::<Vec<_>>(),
            "palette": palette_report.as_ref().map(|p| serde_json::json!({
                "written_offset": format!("0x{:X}", p.written_offset),
                "compression": gfx_compression_id(p.compression),
                "written_bytes": p.written_bytes,
                "relocated": p.relocated,
                "patched_pointers": p.patched_pointers,
            })),
        });
        println!("{}", serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?);
    } else {
        println!("Graphics Replace: {} -> {}", rom_path.display(), out_path.display());
        println!("  Image:              {}", image_path.display());
        println!(
            "  Tiles:              {} ({}x{} tiles, {}bpp)",
            encoded.tile_count,
            encoded.width_tiles,
            encoded.height_tiles,
            bpp.bits()
        );
        println!("  Palette Mode:       {palette_mode_id}");
        println!("  Compression:        {}", gfx_compression_id(report.compression));
        println!("  Original Offset:    0x{:X}", report.original_offset);
        println!(
            "  Written Offset:     0x{:X} (bus 0x{:08X})",
            report.written_offset, report.written_bus_address
        );
        println!(
            "  Size:               {} bytes written / {} capacity ({} uncompressed)",
            report.written_bytes, report.original_capacity, report.uncompressed_bytes
        );
        println!(
            "  Mode:               {}",
            if report.relocated { "relocated" } else { "in-place" }
        );
        if report.relocated {
            println!("  Patched Pointers:   {}", report.patched_pointers);
        }
        if let Some(p) = &palette_report {
            println!(
                "  Palette Written:    0x{:X} ({} bytes, {})",
                p.written_offset,
                p.written_bytes,
                gfx_compression_id(p.compression)
            );
        }
    }
    Ok(())
}

/// Flags taking a value for `gfx replace-map`.
const GFX_MAP_VALUE_FLAGS: &[&[&str]] = &[
    &["--offset"],
    &["--map"],
    &["-o", "--output"],
    &["--palette-bank"],
    &["--compression"],
    &["--pad-byte"],
    &["--capacity"],
];

pub const GFX_REPLACE_MAP_USAGE: &str = "usage: gbakit gfx replace-map <rom.gba> --offset <hex|dec> --map <in.tmx|scr> -o <out.gba> \
[--palette-bank <0..15>] [--compression auto|raw|lz77-vram|lz77-wram|rle|huff4|huff8] \
[--capacity <bytes>] [--allow-relocate] [--pad-byte <n>] [--json]";

/// `gbakit gfx replace-map <rom.gba> --offset <n> --map <in.tmx|scr> -o <out.gba> [...]`
pub fn cmd_gfx_replace_map(args: &[String]) -> Result<(), String> {
    let rom_path = parse_positional_arg(args, GFX_MAP_VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <rom.gba>\n{GFX_REPLACE_MAP_USAGE}"))?;
    let rom_path = Path::new(rom_path);
    let offset = parse_offset(required(args, &["--offset"], "--offset <hex|dec>")?)
        .map_err(|e| format!("invalid --offset: {e}"))?;
    let map_path = Path::new(required(args, &["--map"], "--map <in.tmx|scr>")?);
    let out_path = Path::new(required(args, &["-o", "--output"], "-o <out.gba>")?);
    let as_json = has_flag(args, "--json");
    let allow_relocate = has_flag(args, "--allow-relocate");

    let palette_bank = match parse_flag_value(args, &["--palette-bank"]) {
        Some(s) => {
            let v = parse_offset(s).map_err(|e| format!("invalid --palette-bank: {e}"))?;
            if v > 15 {
                return Err(format!("--palette-bank {v} out of range (expected 0..=15)"));
            }
            v as u8
        }
        None => 0,
    };
    let compression =
        parse_gfx_compression(parse_flag_value(args, &["--compression"]).unwrap_or("auto"))?;
    let pad_byte = match parse_flag_value(args, &["--pad-byte"]) {
        Some(s) => {
            let v = parse_offset(s).map_err(|e| format!("invalid --pad-byte: {e}"))?;
            u8::try_from(v).map_err(|_| format!("--pad-byte {v} exceeds 0xFF"))?
        }
        None => 0x00,
    };
    let capacity = match parse_flag_value(args, &["--capacity"]) {
        Some(s) => Some(parse_offset(s).map_err(|e| format!("invalid --capacity: {e}"))?),
        None => None,
    };
    ensure_distinct_paths(rom_path, out_path)?;

    let mut rom = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let map_bytes = fs::read(map_path).map_err(|e| format!("read {}: {e}", map_path.display()))?;
    if offset >= rom.len() {
        return Err(format!(
            "--offset 0x{offset:X} exceeds ROM length {}",
            rom.len()
        ));
    }

    if compression.is_none() && capacity.is_none() {
        let detected =
            gbakit::gfx::detect_stream_at(&rom, offset, gbakit::compress::DEFAULT_MAX_OUTPUT)
                .map_err(|e| format!("probe stream at 0x{offset:X}: {e}"))?;
        if detected.is_none() {
            return Err(format!(
                "--compression auto: no LZ77/RLE/Huffman stream detected at 0x{offset:X}; \
                 the tilemap is treated as raw, which requires an explicit --capacity <bytes> \
                 (or pass an explicit --compression codec)"
            ));
        }
    }
    if compression == Some(GfxCompression::Raw) && capacity.is_none() {
        return Err(
            "--compression raw requires an explicit --capacity <bytes> for the original slot"
                .into(),
        );
    }

    let input_format = match map_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("tmx") | Some("xml") => TilemapInputFormat::Tmx,
        Some("scr") | Some("bin") | Some("map") => TilemapInputFormat::RawScr,
        _ => TilemapInputFormat::Auto,
    };

    let options = TilemapReplaceOptions {
        palette_bank,
        replace: ReplaceOptions {
            compression,
            original_capacity: capacity,
            pad_byte,
            allow_relocate,
            free_space_start: gbakit::gfx::TILEMAP_FREE_SPACE_START,
            ..ReplaceOptions::default()
        },
    };

    let (report, encoded) =
        replace_tilemap_in_rom(&mut rom, offset, &map_bytes, input_format, &options)
            .map_err(|e| format!("replace tilemap at 0x{offset:X}: {e}"))?;

    fs::write(out_path, &rom).map_err(|e| format!("write {}: {e}", out_path.display()))?;

    let fmt_id = match encoded.input_format {
        TilemapInputFormat::Tmx => "tmx",
        TilemapInputFormat::RawScr => "scr",
        TilemapInputFormat::Auto => "auto",
    };

    if as_json {
        let payload = serde_json::json!({
            "input": rom_path.display().to_string(),
            "output": out_path.display().to_string(),
            "map": map_path.display().to_string(),
            "input_format": fmt_id,
            "palette_bank": palette_bank,
            "entry_count": encoded.entry_count,
            "width_tiles": encoded.width_tiles,
            "height_tiles": encoded.height_tiles,
            "compression": gfx_compression_id(report.compression),
            "original_offset": format!("0x{:X}", report.original_offset),
            "written_offset": format!("0x{:X}", report.written_offset),
            "written_bus_address": format!("0x{:08X}", report.written_bus_address),
            "original_capacity": report.original_capacity,
            "uncompressed_bytes": report.uncompressed_bytes,
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
            "Tilemap Replace: {} -> {}",
            rom_path.display(),
            out_path.display()
        );
        println!("  Map Input:          {} ({fmt_id})", map_path.display());
        if let (Some(w), Some(h)) = (encoded.width_tiles, encoded.height_tiles) {
            println!(
                "  Dimensions:         {w}x{h} tiles ({} entries, palette bank {palette_bank})",
                encoded.entry_count
            );
        } else {
            println!("  Entries:            {}", encoded.entry_count);
        }
        println!(
            "  Compression:        {}",
            gfx_compression_id(report.compression)
        );
        println!("  Original Offset:    0x{:X}", report.original_offset);
        println!(
            "  Written Offset:     0x{:X} (bus 0x{:08X})",
            report.written_offset, report.written_bus_address
        );
        println!(
            "  Size:               {} bytes written / {} capacity ({} uncompressed)",
            report.written_bytes, report.original_capacity, report.uncompressed_bytes
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum AudioCodec {
    Pcm8,
    Dpcm,
    Pcm8Unsigned,
    ImaAdpcm,
    ImaAdpcmArm,
}

impl AudioCodec {
    fn parse(s: &str) -> Result<Self, String> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "pcm8" => Self::Pcm8,
            "dpcm" => Self::Dpcm,
            "pcm8-unsigned" => Self::Pcm8Unsigned,
            "ima-adpcm" => Self::ImaAdpcm,
            "ima-adpcm-arm" => Self::ImaAdpcmArm,
            other => {
                return Err(format!(
                    "unknown --codec {other:?} (expected pcm8|dpcm|pcm8-unsigned|ima-adpcm|ima-adpcm-arm)"
                ))
            }
        })
    }

    fn id(self) -> &'static str {
        match self {
            Self::Pcm8 => "pcm8",
            Self::Dpcm => "dpcm",
            Self::Pcm8Unsigned => "pcm8-unsigned",
            Self::ImaAdpcm => "ima-adpcm",
            Self::ImaAdpcmArm => "ima-adpcm-arm",
        }
    }
}

/// `gbakit audio replace <rom.gba> --offset <n> --wav <in.wav> -o <out.gba> [...]`
pub fn cmd_audio_replace(args: &[String]) -> Result<(), String> {
    let rom_path = parse_positional_arg(args, AUDIO_VALUE_FLAGS)
        .ok_or_else(|| format!("missing required <rom.gba>\n{AUDIO_REPLACE_USAGE}"))?;
    let rom_path = Path::new(rom_path);
    let offset = parse_offset(required(args, &["--offset"], "--offset <hex|dec>")?)
        .map_err(|e| format!("invalid --offset: {e}"))?;
    let wav_path = Path::new(required(args, &["--wav"], "--wav <in.wav>")?);
    let out_path = Path::new(required(args, &["-o", "--output"], "-o <out.gba>")?);
    let as_json = has_flag(args, "--json");
    let allow_relocate = has_flag(args, "--allow-relocate");
    let codec = AudioCodec::parse(parse_flag_value(args, &["--codec"]).unwrap_or("pcm8"))?;
    let rate = match parse_flag_value(args, &["--rate"]) {
        Some(s) => {
            let r = parse_u32_arg(s, "--rate")?;
            if r == 0 {
                return Err("--rate must be greater than 0".into());
            }
            Some(r)
        }
        None => None,
    };
    let loop_start = match parse_flag_value(args, &["--loop-start"]) {
        Some(s) => Some(parse_u32_arg(s, "--loop-start")?),
        None => None,
    };
    let raw_capacity = match parse_flag_value(args, &["--raw-capacity"]) {
        Some(s) => Some(parse_offset(s).map_err(|e| format!("invalid --raw-capacity: {e}"))?),
        None => None,
    };
    ensure_distinct_paths(rom_path, out_path)?;

    let mut rom = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let wav_bytes = fs::read(wav_path).map_err(|e| format!("read {}: {e}", wav_path.display()))?;
    if offset >= rom.len() {
        return Err(format!("--offset 0x{offset:X} exceeds ROM length {}", rom.len()));
    }
    let opts = AudioReplaceOptions {
        allow_relocate,
        ..AudioReplaceOptions::default()
    };

    let m4a_path = matches!(codec, AudioCodec::Pcm8 | AudioCodec::Dpcm) && raw_capacity.is_none();
    let (target_rate, sample_count, report, container) = if m4a_path {
        // Preserve the original WaveData playback rate unless --rate overrides it.
        let target_rate = match rate {
            Some(r) => r,
            None => {
                let hdr = WaveDataHeader::parse(&rom[offset..]).map_err(|e| {
                    format!(
                        "no valid M4A WaveData header at 0x{offset:X} ({e:?}); \
                         pass --raw-capacity <bytes> to replace a raw sample buffer instead"
                    )
                })?;
                hdr.base_sample_rate_hz().max(1)
            }
        };
        let (_, samples) = read_wav_pcm_mono_i16_resampled(&wav_bytes, Some(target_rate))
            .map_err(|e| format!("decode {}: {e}", wav_path.display()))?;
        let build = M4aWaveBuildOptions {
            sample_rate_hz: target_rate,
            loop_start,
            codec: if codec == AudioCodec::Dpcm {
                M4aWaveCodec::PokemonGen3Dpcm
            } else {
                M4aWaveCodec::Pcm8Signed
            },
        };
        let wavedata = build_m4a_wavedata_from_i16(&samples, build)
            .map_err(|e| format!("build M4A WaveData: {e}"))?;
        let report = replace_m4a_wavedata_in_rom(&mut rom, offset, &wavedata, opts)
            .map_err(|e| format!("replace M4A WaveData at 0x{offset:X}: {e}"))?;
        (target_rate, samples.len(), report, "m4a_wavedata")
    } else {
        let capacity = raw_capacity.ok_or_else(|| {
            format!(
                "--codec {} writes a raw sample buffer and requires --raw-capacity <bytes>",
                codec.id()
            )
        })?;
        if loop_start.is_some() {
            return Err("--loop-start is only supported for M4A WaveData replacement (omit --raw-capacity)".into());
        }
        let (src_rate, samples) = read_wav_pcm_mono_i16_resampled(&wav_bytes, rate)
            .map_err(|e| format!("decode {}: {e}", wav_path.display()))?;
        let encoded = match codec {
            AudioCodec::Pcm8 => encode_pcm8_signed(&samples),
            AudioCodec::Dpcm => {
                let pcm_i8: Vec<i8> = samples.iter().map(|&s| quantize_i16_to_i8(s)).collect();
                encode_m4a_dpcm(&pcm_i8)
            }
            AudioCodec::Pcm8Unsigned => encode_pcm8_unsigned(&samples),
            AudioCodec::ImaAdpcm => encode_ima_adpcm4(&samples, ImaAdpcmVariant::StandardLowNibbleFirst),
            AudioCodec::ImaAdpcmArm => {
                encode_ima_adpcm4(&samples, ImaAdpcmVariant::LogikArmHighNibbleFirst)
            }
        };
        let report = replace_pcm8_in_rom(&mut rom, offset, capacity, &encoded, opts)
            .map_err(|e| format!("replace raw sample at 0x{offset:X}: {e}"))?;
        (src_rate, samples.len(), report, "raw")
    };

    fs::write(out_path, &rom).map_err(|e| format!("write {}: {e}", out_path.display()))?;

    let mode_id = match report.mode {
        AudioReplaceMode::InPlace => "in_place",
        AudioReplaceMode::Relocated => "relocated",
    };
    if as_json {
        let payload = serde_json::json!({
            "input": rom_path.display().to_string(),
            "output": out_path.display().to_string(),
            "wav": wav_path.display().to_string(),
            "codec": codec.id(),
            "container": container,
            "sample_rate_hz": target_rate,
            "samples": sample_count,
            "loop_start": loop_start,
            "mode": mode_id,
            "original_offset": format!("0x{:X}", report.original_offset),
            "written_offset": format!("0x{:X}", report.written_offset),
            "original_capacity": report.original_capacity,
            "written_bytes": report.written_bytes,
            "patched_pointers": report.patched_pointer_count,
            "patched_pointer_offsets": report
                .patched_pointer_offsets
                .iter()
                .map(|o| format!("0x{o:X}"))
                .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?);
    } else {
        println!("Audio Replace: {} -> {}", rom_path.display(), out_path.display());
        println!("  WAV:                {}", wav_path.display());
        println!("  Codec:              {} ({container})", codec.id());
        println!("  Sample Rate:        {target_rate} Hz");
        println!("  Samples:            {sample_count}");
        if let Some(ls) = loop_start {
            println!("  Loop Start:         {ls}");
        }
        println!("  Original Offset:    0x{:X}", report.original_offset);
        println!("  Written Offset:     0x{:X}", report.written_offset);
        println!(
            "  Size:               {} bytes written / {} capacity",
            report.written_bytes, report.original_capacity
        );
        println!("  Mode:               {mode_id}");
        if report.mode == AudioReplaceMode::Relocated {
            println!("  Patched Pointers:   {}", report.patched_pointer_count);
        }
    }
    Ok(())
}
