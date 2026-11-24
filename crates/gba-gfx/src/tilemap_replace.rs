//! GBA PPU tilemap (`.tmx` CSV / raw `.scr`) parsing, Screen Block encoding, and safe ROM write-back.
//!
//! Supports:
//! - Parsing Tiled `.tmx` XML maps with `<data encoding="csv">` (including 32-bit GID H-flip
//!   `0x8000_0000`, V-flip `0x4000_0000`, stripping diagonal/hex flags, subtracting `firstgid`,
//!   and applying a configurable 4bpp `palette_bank` `0..=15`).
//! - Encoding `(width_tiles, height_tiles)` grids of [`ScrEntry`] into hardware GBA 256×256
//!   (`32×32` tile = 2,048-byte) Screen Block quadrant order (`32×32`, `64×32`, `32×64`, `64×64`),
//!   or linear row-major order for non-standard dimensions.
//! - Accepting raw little-endian `.scr` binary tilemaps directly.
//! - Capacity-checked ROM write-back with BIOS codec auto-detection (`LZ77`, `RLE`, `Huffman 4/8`)
//!   and optional relocation to `0xFF` (then `0x00`) free space at `>= 0x200` with ROM pointer patching.

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::{
    render::screen_block_entry_index,
    replace::{write_asset_in_rom, ReplaceOptions, ReplaceReport},
    ScrEntry,
};

/// Minimum ROM offset for tilemap free-space relocation (`0x200`).
pub const TILEMAP_FREE_SPACE_START: usize = 0x200;

/// Tiled 32-bit GID horizontal flip bit (bit 31).
pub const TILED_FLIP_H: u32 = 0x8000_0000;
/// Tiled 32-bit GID vertical flip bit (bit 30).
pub const TILED_FLIP_V: u32 = 0x4000_0000;
/// Tiled 32-bit GID diagonal flip bit (bit 29, stripped for GBA text-mode BG).
pub const TILED_FLIP_D: u32 = 0x2000_0000;
/// Mask for the lower 28-bit raw tile GID (strips bits 31..28 flip/rotation flags).
const TILED_GID_MASK: u32 = 0x0FFF_FFFF;

/// Input format for tilemap write-back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TilemapInputFormat {
    /// Auto-detect: Tiled `.tmx` XML if input looks like XML `<map ...>`, otherwise raw `.scr`.
    Auto,
    /// Tiled `.tmx` XML document with `<data encoding="csv">`.
    Tmx,
    /// Raw little-endian 16-bit GBA Screen Block (`.scr`) binary bytes.
    RawScr,
}

/// Parsed Tiled `.tmx` CSV tilemap in row-major `(tx, ty)` order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTmx {
    /// Map width in 8×8 tiles.
    pub width_tiles: usize,
    /// Map height in 8×8 tiles.
    pub height_tiles: usize,
    /// Tileset `firstgid` attribute (defaults to `1` if omitted).
    pub firstgid: u32,
    /// Decoded [`ScrEntry`] grid in row-major order (`ty * width_tiles + tx`).
    pub entries: Vec<ScrEntry>,
}

/// Encoded GBA Screen Block (`.scr`) tilemap ready for ROM write-back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedTilemap {
    /// Raw little-endian 16-bit GBA SCR bytes (in hardware screen-block order for standard BG sizes).
    pub scr_bytes: Vec<u8>,
    /// Number of 16-bit tilemap entries (`scr_bytes.len() / 2`).
    pub entry_count: usize,
    /// Map width in 8×8 tiles (present when encoded from `.tmx`).
    pub width_tiles: Option<usize>,
    /// Map height in 8×8 tiles (present when encoded from `.tmx`).
    pub height_tiles: Option<usize>,
    /// Resolved input format (`Tmx` or `RawScr`).
    pub input_format: TilemapInputFormat,
}

/// Options controlling tilemap parsing, encoding, and ROM write-back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TilemapReplaceOptions {
    /// Default 4bpp palette bank (`0..=15`) applied to `.tmx` entries.
    pub palette_bank: u8,
    /// Underlying ROM replacement options (compression, capacity, relocation, padding).
    pub replace: ReplaceOptions,
}

impl Default for TilemapReplaceOptions {
    fn default() -> Self {
        Self {
            palette_bank: 0,
            replace: ReplaceOptions {
                free_space_start: TILEMAP_FREE_SPACE_START,
                ..ReplaceOptions::default()
            },
        }
    }
}

/// Parse a Tiled `.tmx` XML document containing a `<data encoding="csv">` tile layer
/// into a row-major [`ParsedTmx`] of GBA [`ScrEntry`] values.
///
/// Handles Tiled's 32-bit GID flags:
/// - Bit 31 (`0x8000_0000`): horizontal flip (`h_flip`)
/// - Bit 30 (`0x4000_0000`): vertical flip (`v_flip`)
/// - Bit 29 (`0x2000_0000`): diagonal flip (stripped cleanly)
/// - Subtracts `<tileset firstgid="N">` (default `1`) to produce 10-bit GBA tile index `0..=1023`.
/// - Applies `palette_bank` (`0..=15`) to all entries.
pub fn parse_tmx_csv(tmx_xml: &str, palette_bank: u8) -> Result<ParsedTmx> {
    if palette_bank > 15 {
        return Err(Error::InvalidField {
            what: "palette_bank",
            value: u64::from(palette_bank),
        });
    }

    let xml = tmx_xml.strip_prefix('\u{FEFF}').unwrap_or(tmx_xml);

    // 1) Extract <map ...> opening tag attributes.
    let map_tag = find_opening_tag(xml, "map")
        .ok_or_else(|| Error::Malformed("missing <map> element in TMX document".into()))?;
    let width_tiles = parse_xml_attr_usize(map_tag, "width")
        .ok_or_else(|| Error::Malformed("missing or invalid width attribute on <map>".into()))?;
    let height_tiles = parse_xml_attr_usize(map_tag, "height")
        .ok_or_else(|| Error::Malformed("missing or invalid height attribute on <map>".into()))?;
    if width_tiles == 0 || height_tiles == 0 {
        return Err(Error::InvalidField {
            what: "tmx_dimensions",
            value: ((width_tiles as u64) << 32) | (height_tiles as u64),
        });
    }

    let expected_entries = width_tiles
        .checked_mul(height_tiles)
        .ok_or(Error::InvalidField {
            what: "tmx_tile_count",
            value: width_tiles as u64,
        })?;

    // 2) Extract optional <tileset firstgid="N" ...> (default 1).
    let firstgid = match find_opening_tag(xml, "tileset") {
        Some(ts_tag) => match parse_xml_attr_str(ts_tag, "firstgid") {
            Some(val) => {
                let n = val.parse::<u32>().map_err(|_| {
                    Error::Malformed(format!("invalid tileset firstgid attribute: {val:?}"))
                })?;
                if n == 0 {
                    return Err(Error::InvalidField {
                        what: "tmx_firstgid",
                        value: 0,
                    });
                }
                n
            }
            None => 1,
        },
        None => 1,
    };

    // 3) Locate <data ...>...</data> and verify encoding="csv".
    let (data_tag, data_body) = find_tag_with_body(xml, "data")
        .ok_or_else(|| Error::Malformed("missing <data>...</data> element in TMX document".into()))?;
    let encoding = parse_xml_attr_str(data_tag, "encoding")
        .ok_or_else(|| Error::Malformed("missing encoding attribute on <data>".into()))?;
    if !encoding.eq_ignore_ascii_case("csv") {
        return Err(Error::Malformed(format!(
            "unsupported TMX <data> encoding {encoding:?}; expected \"csv\""
        )));
    }

    // 4) Parse comma/whitespace-separated 32-bit GIDs.
    let mut entries = Vec::with_capacity(expected_entries);
    for raw_token in data_body.split(',') {
        let token = raw_token.trim();
        if token.is_empty() {
            continue;
        }
        let gid = token
            .parse::<u32>()
            .map_err(|_| Error::Malformed(format!("invalid TMX CSV GID token: {token:?}")))?;

        let h_flip = (gid & TILED_FLIP_H) != 0;
        let v_flip = (gid & TILED_FLIP_V) != 0;
        // Strip bit 31 (H), bit 30 (V), bit 29 (D), bit 28 (hex) cleanly.
        let raw_id = gid & TILED_GID_MASK;

        let entry = if raw_id == 0 {
            ScrEntry {
                tile_index: 0,
                h_flip: false,
                v_flip: false,
                palette_bank,
            }
        } else {
            if raw_id < firstgid {
                return Err(Error::InvalidField {
                    what: "tmx_gid_below_firstgid",
                    value: u64::from(raw_id),
                });
            }
            let local_tile = raw_id - firstgid;
            if local_tile > 1023 {
                return Err(Error::InvalidField {
                    what: "tmx_tile_index",
                    value: u64::from(local_tile),
                });
            }
            ScrEntry {
                tile_index: local_tile as u16,
                h_flip,
                v_flip,
                palette_bank,
            }
        };
        entries.push(entry);
    }

    if entries.len() != expected_entries {
        return Err(Error::Malformed(format!(
            "TMX CSV entry count mismatch: expected {expected_entries} ({width_tiles}x{height_tiles}), found {}",
            entries.len()
        )));
    }

    Ok(ParsedTmx {
        width_tiles,
        height_tiles,
        firstgid,
        entries,
    })
}

/// Encode a row-major `(width_tiles, height_tiles)` grid of [`ScrEntry`] values into GBA
/// hardware Screen Block (`.scr`) little-endian bytes.
///
/// For standard GBA text-mode BG dimensions (`32×32`, `64×32`, `32×64`, `64×64`), entries are
/// laid out in hardware 256×256 (`32×32` tile = 2,048-byte) screen-block quadrant order matching
/// [`screen_block_entry_index`]. For non-standard dimensions, entries are written in linear
/// row-major order.
pub fn encode_tilemap_scr(
    width_tiles: usize,
    height_tiles: usize,
    entries_row_major: &[ScrEntry],
) -> Result<Vec<u8>> {
    if width_tiles == 0 || height_tiles == 0 {
        return Err(Error::InvalidField {
            what: "tilemap_dimensions",
            value: ((width_tiles as u64) << 32) | (height_tiles as u64),
        });
    }
    let total = width_tiles
        .checked_mul(height_tiles)
        .ok_or(Error::InvalidField {
            what: "tilemap_entry_count",
            value: width_tiles as u64,
        })?;
    if entries_row_major.len() != total {
        return Err(Error::Malformed(format!(
            "tilemap entry count mismatch: expected {total} ({width_tiles}x{height_tiles}), got {}",
            entries_row_major.len()
        )));
    }

    let mut out = vec![0u8; total * 2];
    for ty in 0..height_tiles {
        for tx in 0..width_tiles {
            let src_idx = ty * width_tiles + tx;
            let dst_idx = screen_block_entry_index(tx, ty, width_tiles, height_tiles);
            let raw = entries_row_major[src_idx].to_u16().to_le_bytes();
            out[dst_idx * 2..dst_idx * 2 + 2].copy_from_slice(&raw);
        }
    }

    Ok(out)
}

/// Parse a Tiled `.tmx` CSV document and encode it directly to GBA Screen Block (`.scr`) bytes.
pub fn parse_tmx_to_scr(tmx_xml: &str, palette_bank: u8) -> Result<EncodedTilemap> {
    let parsed = parse_tmx_csv(tmx_xml, palette_bank)?;
    let scr_bytes = encode_tilemap_scr(parsed.width_tiles, parsed.height_tiles, &parsed.entries)?;
    Ok(EncodedTilemap {
        entry_count: parsed.entries.len(),
        scr_bytes,
        width_tiles: Some(parsed.width_tiles),
        height_tiles: Some(parsed.height_tiles),
        input_format: TilemapInputFormat::Tmx,
    })
}

/// Decode or validate input tilemap bytes (`.tmx` CSV or raw `.scr`) into an [`EncodedTilemap`].
pub fn prepare_tilemap_scr(
    map_bytes: &[u8],
    format: TilemapInputFormat,
    palette_bank: u8,
) -> Result<EncodedTilemap> {
    let resolved = match format {
        TilemapInputFormat::Auto => detect_tilemap_input_format(map_bytes),
        other => other,
    };

    match resolved {
        TilemapInputFormat::Tmx => {
            let xml = std::str::from_utf8(map_bytes)
                .map_err(|_| Error::Malformed("TMX input is not valid UTF-8".into()))?;
            parse_tmx_to_scr(xml, palette_bank)
        }
        TilemapInputFormat::RawScr => {
            if map_bytes.is_empty() || !map_bytes.len().is_multiple_of(2) {
                return Err(Error::InvalidField {
                    what: "raw_scr_byte_len",
                    value: map_bytes.len() as u64,
                });
            }
            Ok(EncodedTilemap {
                entry_count: map_bytes.len() / 2,
                scr_bytes: map_bytes.to_vec(),
                width_tiles: None,
                height_tiles: None,
                input_format: TilemapInputFormat::RawScr,
            })
        }
        TilemapInputFormat::Auto => unreachable!("resolved format is never Auto"),
    }
}

/// Replace a GBA background tilemap in `rom` at `rom_offset` from Tiled `.tmx` CSV or raw `.scr` bytes.
///
/// Encodes the tilemap to GBA Screen Block bytes, optionally recompresses with the target or
/// auto-detected BIOS codec (`LZ77` VRAM/WRAM, `RLE`, `Huffman 4/8`, or `Raw`), and writes
/// back in-place if it fits or relocates to `0xFF` (then `0x00`) free space at `>= 0x200` and
/// patches all 4-byte-aligned `0x08xxxxxx` ROM pointers when `options.replace.allow_relocate` is set.
pub fn replace_tilemap_in_rom(
    rom: &mut [u8],
    rom_offset: usize,
    map_bytes: &[u8],
    format: TilemapInputFormat,
    options: &TilemapReplaceOptions,
) -> Result<(ReplaceReport, EncodedTilemap)> {
    let encoded = prepare_tilemap_scr(map_bytes, format, options.palette_bank)?;
    let mut replace_opts = options.replace;
    if replace_opts.free_space_start < TILEMAP_FREE_SPACE_START {
        replace_opts.free_space_start = TILEMAP_FREE_SPACE_START;
    }
    let report = write_asset_in_rom(rom, rom_offset, &encoded.scr_bytes, &replace_opts)?;
    Ok((report, encoded))
}

fn detect_tilemap_input_format(bytes: &[u8]) -> TilemapInputFormat {
    let stripped = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    if let Ok(s) = std::str::from_utf8(stripped) {
        let trimmed = s.trim_start();
        if trimmed.starts_with('<') && (trimmed.contains("<map") || trimmed.starts_with("<?xml")) {
            return TilemapInputFormat::Tmx;
        }
    }
    TilemapInputFormat::RawScr
}

// -----------------------------------------------------------------------------
// Minimal Zero-Dependency XML Helpers for TMX
// -----------------------------------------------------------------------------

/// Find the first opening tag `<tag_name ...>` in `xml` and return the slice inside `<...>`.
fn find_opening_tag<'a>(xml: &'a str, tag_name: &str) -> Option<&'a str> {
    let needle = format!("<{tag_name}");
    let mut search_from = 0usize;
    while let Some(rel) = xml[search_from..].find(&needle) {
        let start = search_from + rel;
        let after = start + needle.len();
        let next_byte = xml.as_bytes().get(after).copied();
        if matches!(
            next_byte,
            Some(b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/')
        ) {
            let end_rel = xml[after..].find('>')?;
            return Some(&xml[start + 1..after + end_rel]);
        }
        search_from = after;
    }
    None
}

/// Find the first `<tag_name ...>body</tag_name>` and return `(opening_tag_inner, body)`.
fn find_tag_with_body<'a>(xml: &'a str, tag_name: &str) -> Option<(&'a str, &'a str)> {
    let needle = format!("<{tag_name}");
    let close_needle = format!("</{tag_name}>");
    let mut search_from = 0usize;
    while let Some(rel) = xml[search_from..].find(&needle) {
        let start = search_from + rel;
        let after = start + needle.len();
        let next_byte = xml.as_bytes().get(after).copied();
        if matches!(next_byte, Some(b' ' | b'\t' | b'\r' | b'\n' | b'>')) {
            let open_end_rel = xml[after..].find('>')?;
            let open_end = after + open_end_rel;
            let open_inner = &xml[start + 1..open_end];
            if open_inner.trim_end().ends_with('/') {
                search_from = open_end + 1;
                continue;
            }
            let body_start = open_end + 1;
            let close_rel = xml[body_start..].find(&close_needle)?;
            return Some((open_inner, &xml[body_start..body_start + close_rel]));
        }
        search_from = after;
    }
    None
}

/// Extract an attribute value `attr="value"` or `attr='value'` from an XML tag's interior slice.
fn parse_xml_attr_str<'a>(tag_inner: &'a str, attr: &str) -> Option<&'a str> {
    let bytes = tag_inner.as_bytes();
    let attr_bytes = attr.as_bytes();
    let mut i = 0usize;

    // Skip tag name.
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }

    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'/' {
            break;
        }
        let name_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' {
            i += 1;
        }
        let name = &bytes[name_start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let quote = bytes[i];
        if quote != b'"' && quote != b'\'' {
            continue;
        }
        i += 1;
        let val_start = i;
        while i < bytes.len() && bytes[i] != quote {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let val_end = i;
        i += 1;

        if name == attr_bytes {
            return Some(&tag_inner[val_start..val_end]);
        }
    }
    None
}

fn parse_xml_attr_usize(tag_inner: &str, attr: &str) -> Option<usize> {
    parse_xml_attr_str(tag_inner, attr)?.parse::<usize>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        generate_tmx,
        replace::{compress_gfx, GfxCompression},
        CompressionKind, ScannedTilemap,
    };
    use gba_compress::{decode_lz77, DEFAULT_MAX_OUTPUT};
    use gba_rom::rom_offset_to_bus;

    fn make_synthetic_rom(size: usize) -> Vec<u8> {
        let mut rom = vec![0xAAu8; size];
        rom[0xB2] = gba_rom::FIXED_VALUE;
        rom
    }

    #[test]
    fn parses_tmx_csv_with_flips_firstgid_and_palette_bank() {
        let tmx = r#"<?xml version="1.0" encoding="UTF-8"?>
<map version="1.10" orientation="orthogonal" width="2" height="2" tilewidth="8" tileheight="8">
 <tileset firstgid="10" source="tiles.tsx"/>
 <layer id="1" name="BG0" width="2" height="2">
  <data encoding="csv">
10,2147483659,
1073741836,3758096397
</data>
 </layer>
</map>"#;
        // 10 -> tile 0
        // 2147483659 = 0x8000_0000 | 11 -> tile 1, H-flip
        // 1073741836 = 0x4000_0000 | 12 -> tile 2, V-flip
        // 3758096397 = 0xE000_0000 | 13 -> tile 3, H-flip + V-flip (bit 29 diagonal stripped cleanly)
        let parsed = parse_tmx_csv(tmx, 5).unwrap();
        assert_eq!(parsed.width_tiles, 2);
        assert_eq!(parsed.height_tiles, 2);
        assert_eq!(parsed.firstgid, 10);
        assert_eq!(
            parsed.entries,
            vec![
                ScrEntry {
                    tile_index: 0,
                    h_flip: false,
                    v_flip: false,
                    palette_bank: 5,
                },
                ScrEntry {
                    tile_index: 1,
                    h_flip: true,
                    v_flip: false,
                    palette_bank: 5,
                },
                ScrEntry {
                    tile_index: 2,
                    h_flip: false,
                    v_flip: true,
                    palette_bank: 5,
                },
                ScrEntry {
                    tile_index: 3,
                    h_flip: true,
                    v_flip: true,
                    palette_bank: 5,
                },
            ]
        );

        // Reject invalid palette_bank (> 15).
        assert!(parse_tmx_csv(tmx, 16).is_err());

        // Reject tile index > 1023.
        let overflow_tmx = r#"<map width="1" height="1"><tileset firstgid="1"/><data encoding="csv">1025</data></map>"#;
        assert!(parse_tmx_csv(overflow_tmx, 0).is_err());

        // Reject non-CSV encoding.
        let b64_tmx = r#"<map width="1" height="1"><data encoding="base64">AAAA</data></map>"#;
        assert!(parse_tmx_csv(b64_tmx, 0).is_err());
    }

    #[test]
    fn screenblock_64x32_quadrant_ordering_matches_generate_tmx_roundtrip() {
        // Construct a 64x32 GBA tilemap (2 horizontal 32x32 screen blocks = 2048 entries = 4096 bytes).
        let width = 64usize;
        let height = 32usize;
        let mut orig_scr_entries = vec![
            ScrEntry {
                tile_index: 0,
                h_flip: false,
                v_flip: false,
                palette_bank: 0,
            };
            width * height
        ];
        for ty in 0..height {
            for tx in 0..width {
                let idx = screen_block_entry_index(tx, ty, width, height);
                orig_scr_entries[idx] = ScrEntry {
                    tile_index: ((ty * 17 + tx) % 1024) as u16,
                    h_flip: (tx & 1) != 0,
                    v_flip: (ty & 1) != 0,
                    palette_bank: 3,
                };
            }
        }

        let scanned = ScannedTilemap {
            id: "map_0".into(),
            offset: 0x1000,
            bus_address: 0x0800_1000,
            compression: CompressionKind::Lz77,
            width_tiles: width,
            height_tiles: height,
            decompressed_bytes: width * height * 2,
            compressed_bytes: 0,
            tileset_offset: None,
            palette_offset: None,
            entries: orig_scr_entries.clone(),
        };

        // Generate TMX using ripper, then parse and re-encode back to SCR.
        let tmx = generate_tmx(&scanned, "tiles.tsx");
        let encoded = parse_tmx_to_scr(&tmx, 3).unwrap();
        assert_eq!(encoded.width_tiles, Some(64));
        assert_eq!(encoded.height_tiles, Some(32));
        assert_eq!(encoded.entry_count, 2048);
        assert_eq!(encoded.scr_bytes.len(), 4096);

        // Verify byte-exact screenblock quadrant order!
        for (i, expected) in orig_scr_entries.iter().enumerate() {
            let actual = u16::from_le_bytes([encoded.scr_bytes[i * 2], encoded.scr_bytes[i * 2 + 1]]);
            assert_eq!(actual, expected.to_u16(), "mismatch at SCR index {i}");
        }

        // Specifically verify (tx=32, ty=0) lands at entry 1024 (start of second 32x32 screenblock).
        let sb1_first = u16::from_le_bytes([encoded.scr_bytes[1024 * 2], encoded.scr_bytes[1024 * 2 + 1]]);
        assert_eq!(ScrEntry::from_u16(sb1_first).tile_index, 32);
    }

    #[test]
    fn replace_tilemap_in_rom_inplace_lz77_and_relocated_pointer_patching() {
        let mut rom = make_synthetic_rom(8192);
        let map_off = 0x400usize;

        // 1) In-place LZ77 replacement: start with noisy 32x32 SCR (2048 bytes) compressed with LZ77.
        let mut noisy_scr = vec![0u8; 2048];
        for (i, b) in noisy_scr.iter_mut().enumerate() {
            *b = ((i * 73 + 19) & 0xFF) as u8;
        }
        let orig_lz = compress_gfx(&noisy_scr, GfxCompression::Lz77Vram).unwrap();
        rom[map_off..map_off + orig_lz.len()].copy_from_slice(&orig_lz);

        // Build a uniform 32x32 TMX (compresses much smaller than noisy_scr).
        let mut csv_vals = Vec::with_capacity(1024);
        for i in 0..1024u32 {
            let gid = if i == 0 { 1 | TILED_FLIP_H } else { 1 };
            csv_vals.push(gid.to_string());
        }
        let small_tmx = format!(
            "<map width=\"32\" height=\"32\"><tileset firstgid=\"1\"/><layer><data encoding=\"csv\">{}</data></layer></map>",
            csv_vals.join(",")
        );

        let opts = TilemapReplaceOptions {
            palette_bank: 2,
            ..TilemapReplaceOptions::default()
        };
        let (rep, enc) = replace_tilemap_in_rom(
            &mut rom,
            map_off,
            small_tmx.as_bytes(),
            TilemapInputFormat::Auto,
            &opts,
        )
        .unwrap();
        assert!(!rep.relocated);
        assert_eq!(rep.written_offset, map_off);
        assert_eq!(rep.compression, GfxCompression::Lz77Vram);
        assert_eq!(enc.input_format, TilemapInputFormat::Tmx);
        assert_eq!(enc.scr_bytes.len(), 2048);

        let dec = decode_lz77(&rom[map_off..], DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec.bytes, enc.scr_bytes);
        let first_entry = ScrEntry::from_u16(u16::from_le_bytes([dec.bytes[0], dec.bytes[1]]));
        assert_eq!(first_entry.tile_index, 0);
        assert!(first_entry.h_flip);
        assert_eq!(first_entry.palette_bank, 2);

        // 2) Relocated replacement: write pointers to map_off and carve 0xFF free space at 0x1000..0x1F00.
        let old_bus = rom_offset_to_bus(map_off).unwrap();
        let ptr_locs = [0x100usize, 0x120];
        for &loc in &ptr_locs {
            rom[loc..loc + 4].copy_from_slice(&old_bus.to_le_bytes());
        }
        rom[0x1000..0x1F00].fill(0xFF);

        // Replace the now-tiny compressed slot with noisy raw SCR bytes -> overflows -> must relocate!
        let mut reloc_opts = TilemapReplaceOptions::default();
        reloc_opts.replace.allow_relocate = true;
        let (reloc_rep, reloc_enc) = replace_tilemap_in_rom(
            &mut rom,
            map_off,
            &noisy_scr,
            TilemapInputFormat::RawScr,
            &reloc_opts,
        )
        .unwrap();
        assert!(reloc_rep.relocated);
        assert_eq!(reloc_rep.written_offset, 0x1000);
        assert_eq!(reloc_rep.written_bus_address, 0x0800_1000);
        assert_eq!(reloc_rep.patched_pointers, 2);
        assert_eq!(reloc_rep.patched_pointer_offsets, ptr_locs.to_vec());
        assert_eq!(reloc_enc.input_format, TilemapInputFormat::RawScr);

        for &loc in &ptr_locs {
            let ptr = u32::from_le_bytes(rom[loc..loc + 4].try_into().unwrap());
            assert_eq!(ptr, 0x0800_1000);
        }
        let dec_reloc = decode_lz77(&rom[0x1000..], DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec_reloc.bytes, noisy_scr);
    }
}
