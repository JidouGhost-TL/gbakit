//! GBA PPU Graphics Asset Ripper engine.
//!
//! Extracts 4bpp/8bpp tilesets, Mode 0–5 background tilemaps, OAM sprite sheets,
//! and BGR555 palettes directly from retail ROMs without live memory dumps.

#![deny(unsafe_code)]

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;

use formatkit_core::{Error, Image, Result};
use gba_compress::{
    decode_hudson_lz, decode_huffman, decode_konami_dict, decode_lz11, decode_lz77,
    decode_shinen_mode7,
};
use gba_rom::{bus_to_rom_offset, rom_offset_to_bus, scan_pointer_tables, AgbHeader};
use serde::{Deserialize, Serialize};

use crate::{
    bgr555_to_rgba8, decode_tile_4bpp, decode_tile_8bpp, oam::ObjAttr, render::screen_block_entry_index,
    ScrEntry,
};

// -----------------------------------------------------------------------------
// PNG encoding (delegated to FormatKit)
// -----------------------------------------------------------------------------

/// PNG chunk CRC-32 and zlib Adler-32, re-exported from FormatKit.
pub use formatkit_core::checksum::{adler32, crc32_iso_hdlc as crc32};

/// Deterministic stored-zlib PNG encoder backed by `formatkit_codec::encode_rgba_png`.
pub struct RawPngEncoder;

impl RawPngEncoder {
    /// Encode an RGBA8 buffer (`width * height * 4` bytes) into a valid 32-bit RGBA PNG.
    pub fn encode_rgba8(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
        let len = Image::rgba_len(width, height)?;
        Self::encode(width, height, prefix(rgba, len)?.to_vec())
    }

    /// Encode an RGB8 buffer (`width * height * 3` bytes) as an opaque RGBA PNG.
    pub fn encode_rgb8(width: u32, height: u32, rgb: &[u8]) -> Result<Vec<u8>> {
        let len = Image::rgba_len(width, height)? / 4 * 3;
        let (pixels, _) = prefix(rgb, len)?.as_chunks::<3>();
        let rgba = pixels.iter().flat_map(|&[r, g, b]| [r, g, b, 0xFF]).collect();
        Self::encode(width, height, rgba)
    }

    fn encode(width: u32, height: u32, rgba: Vec<u8>) -> Result<Vec<u8>> {
        formatkit_codec::encode_rgba_png(&Image { width, height, rgba }, usize::MAX)
    }
}

fn prefix(pixels: &[u8], len: usize) -> Result<&[u8]> {
    pixels.get(..len).ok_or(Error::Truncated {
        offset: 0,
        needed: len,
        available: pixels.len(),
    })
}

// -----------------------------------------------------------------------------
// Adobe Color Table (.act) & Palette Helpers
// -----------------------------------------------------------------------------

/// Encode a palette into standard 768-byte Adobe Color Table (`.act`) format (256 RGB triples).
pub fn encode_act_palette(palette: &[[u8; 4]]) -> Vec<u8> {
    let mut act = vec![0u8; 768];
    for (i, col) in palette.iter().take(256).enumerate() {
        act[i * 3] = col[0];
        act[i * 3 + 1] = col[1];
        act[i * 3 + 2] = col[2];
    }
    act
}

/// Render a palette swatch image as an RGBA buffer.
///
/// For a 16-color palette, renders a 256x16 swatch image (16 swatches of 16x16 pixels).
/// For a 256-color palette, renders a 256x256 swatch image (16x16 grid of 16x16 pixels).
pub fn render_palette_swatch_rgba(palette: &[[u8; 4]]) -> (u32, u32, Vec<u8>) {
    let is_16 = palette.len() <= 16;
    let (width, height, cols, rows) = if is_16 {
        (256u32, 16u32, 16usize, 1usize)
    } else {
        (256u32, 256u32, 16usize, 16usize)
    };

    let swatch_size = 16usize;
    let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];

    for r in 0..rows {
        for c in 0..cols {
            let color_idx = r * cols + c;
            let col = palette.get(color_idx).copied().unwrap_or([0, 0, 0, 255]);
            for sy in 0..swatch_size {
                let py = r * swatch_size + sy;
                for sx in 0..swatch_size {
                    let px = c * swatch_size + sx;
                    let out_idx = (py * (width as usize) + px) * 4;
                    rgba[out_idx..out_idx + 4].copy_from_slice(&col);
                }
            }
        }
    }

    (width, height, rgba)
}

// -----------------------------------------------------------------------------
// Data Structures for Scanned Assets
// -----------------------------------------------------------------------------

/// Compression method of a graphics or map block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompressionKind {
    Raw,
    Lz77,
    Lz11,
    HudsonLz,
    KonamiDict,
    ShinenMode7,
    Huffman,
}

/// Scanned BGR555 palette metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScannedPalette {
    pub id: String,
    pub offset: usize,
    pub bus_address: u32,
    pub color_count: usize,
    pub is_xref: bool,
    pub contrast_score: f32,
    #[serde(skip)]
    pub colors: Vec<[u8; 4]>,
}

/// Scanned tileset metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScannedTileset {
    pub id: String,
    pub offset: usize,
    pub bus_address: u32,
    pub compression: CompressionKind,
    pub bpp: u8,
    pub tile_count: usize,
    pub decompressed_bytes: usize,
    pub compressed_bytes: usize,
    pub palette_offset: Option<usize>,
    #[serde(skip)]
    pub raw_data: Vec<u8>,
}

/// Scanned background tilemap metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScannedTilemap {
    pub id: String,
    pub offset: usize,
    pub bus_address: u32,
    pub compression: CompressionKind,
    pub width_tiles: usize,
    pub height_tiles: usize,
    pub decompressed_bytes: usize,
    pub compressed_bytes: usize,
    pub tileset_offset: Option<usize>,
    pub palette_offset: Option<usize>,
    #[serde(skip)]
    pub entries: Vec<ScrEntry>,
}

/// Scanned OAM sprite metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScannedSprite {
    pub id: String,
    pub offset: usize,
    pub bus_address: u32,
    pub width: u16,
    pub height: u16,
    pub bpp: u8,
    pub tile_index: u16,
    pub palette_bank: u8,
    pub h_flip: bool,
    pub v_flip: bool,
}

/// Summary counts of all ripped assets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RipSummary {
    pub palette_count: usize,
    pub tileset_count: usize,
    pub tilemap_count: usize,
    pub sprite_count: usize,
}

/// Manifest of all extracted graphics assets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RipManifest {
    pub rom_title: String,
    pub game_code: String,
    pub maker_code: String,
    pub rom_size: usize,
    pub summary: RipSummary,
    pub palettes: Vec<PaletteManifestEntry>,
    pub tilesets: Vec<TilesetManifestEntry>,
    pub tilemaps: Vec<TilemapManifestEntry>,
    pub sprites: Vec<SpriteManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaletteManifestEntry {
    pub id: String,
    pub offset: String,
    pub bus_address: String,
    pub colors: usize,
    pub bytes: usize,
    pub act_file: String,
    pub png_file: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TilesetManifestEntry {
    pub id: String,
    pub offset: String,
    pub bus_address: String,
    pub compression: CompressionKind,
    pub bpp: u8,
    pub tile_count: usize,
    pub decompressed_bytes: usize,
    pub compressed_bytes: usize,
    pub palette_offset: Option<String>,
    pub png_file: String,
    pub tsx_file: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TilemapManifestEntry {
    pub id: String,
    pub offset: String,
    pub bus_address: String,
    pub compression: CompressionKind,
    pub width_tiles: usize,
    pub height_tiles: usize,
    pub width_px: usize,
    pub height_px: usize,
    pub decompressed_bytes: usize,
    pub tileset_offset: Option<String>,
    pub palette_offset: Option<String>,
    pub png_file: String,
    pub tmx_file: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpriteManifestEntry {
    pub id: String,
    pub offset: String,
    pub bus_address: String,
    pub width: u16,
    pub height: u16,
    pub bpp: u8,
    pub tile_index: u16,
    pub palette_bank: u8,
    pub png_file: String,
}

// -----------------------------------------------------------------------------
// Palette Scanner: Heuristics & Pointer Xrefs
// -----------------------------------------------------------------------------

/// Identify candidate 16-color (32B) and 256-color (512B) BGR555 palettes.
pub fn scan_palettes(rom: &[u8]) -> Vec<ScannedPalette> {
    let mut results = Vec::new();
    let mut seen_offsets = HashSet::new();

    // 1. Gather pointer table cross references
    let mut xref_offsets = HashSet::new();
    let ptr_tables = scan_pointer_tables(rom, 4);
    for pt in &ptr_tables {
        for &entry in &pt.entries {
            if let Some(off) = bus_to_rom_offset(entry, rom.len()) {
                if off % 2 == 0 && off + 32 <= rom.len() {
                    xref_offsets.insert(off);
                }
            }
        }
    }

    // 2. Scan xref targets first
    for &off in &xref_offsets {
        if seen_offsets.contains(&off) {
            continue;
        }

        // Try 256-color palette
        if off + 512 <= rom.len() {
            if let Some((colors, score)) = evaluate_bgr555_palette(&rom[off..off + 512], 256) {
                seen_offsets.insert(off);
                results.push(ScannedPalette {
                    id: format!("pal_0x{:08X}", off),
                    offset: off,
                    bus_address: rom_offset_to_bus(off).unwrap_or(0),
                    color_count: 256,
                    is_xref: true,
                    contrast_score: score,
                    colors,
                });
                continue;
            }
        }

        // Try 16-color palette
        if off + 32 <= rom.len() {
            if let Some((colors, score)) = evaluate_bgr555_palette(&rom[off..off + 32], 16) {
                seen_offsets.insert(off);
                results.push(ScannedPalette {
                    id: format!("pal_0x{:08X}", off),
                    offset: off,
                    bus_address: rom_offset_to_bus(off).unwrap_or(0),
                    color_count: 16,
                    is_xref: true,
                    contrast_score: score,
                    colors,
                });
            }
        }
    }

    // 3. Scan aligned offsets for strong contrast palettes (skip 0x000..0x200 ROM header)
    let mut off = 0x200usize;
    let max_off = rom.len().saturating_sub(32);
    while off < max_off {
        if seen_offsets.contains(&off) {
            off += 32;
            continue;
        }

        // 256-color check (require higher distinct colors if non-xref)
        if off + 512 <= rom.len() && off.is_multiple_of(64) {
            if let Some((colors, score)) = evaluate_bgr555_palette(&rom[off..off + 512], 256) {
                if score >= 12.0 {
                    seen_offsets.insert(off);
                    results.push(ScannedPalette {
                        id: format!("pal_0x{:08X}", off),
                        offset: off,
                        bus_address: rom_offset_to_bus(off).unwrap_or(0),
                        color_count: 256,
                        is_xref: false,
                        contrast_score: score,
                        colors,
                    });
                    off += 512;
                    continue;
                }
            }
        }

        // 16-color check
        if off + 32 <= rom.len() {
            if let Some((colors, score)) = evaluate_bgr555_palette(&rom[off..off + 32], 16) {
                if score >= 10.0 {
                    seen_offsets.insert(off);
                    results.push(ScannedPalette {
                        id: format!("pal_0x{:08X}", off),
                        offset: off,
                        bus_address: rom_offset_to_bus(off).unwrap_or(0),
                        color_count: 16,
                        is_xref: false,
                        contrast_score: score,
                        colors,
                    });
                    off += 32;
                    continue;
                }
            }
        }
        off += 4;
    }

    results.sort_by_key(|p| p.offset);
    results
}

fn evaluate_bgr555_palette(bytes: &[u8], color_count: usize) -> Option<(Vec<[u8; 4]>, f32)> {
    let byte_len = color_count * 2;
    if bytes.len() < byte_len {
        return None;
    }
    let slice = &bytes[..byte_len];

    // Reject printable ASCII text blocks (all ASCII bytes 0x00..=0x7F have bit 15 == 0)
    let ascii_count = slice
        .iter()
        .filter(|&&b| (0x20..=0x7E).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t')
        .count();
    if ascii_count * 100 / byte_len >= 55 {
        return None;
    }

    let mut words = Vec::with_capacity(color_count);
    let mut lums = Vec::with_capacity(color_count);
    let mut colors = Vec::with_capacity(color_count);

    for chunk in slice.as_chunks::<2>().0 {
        let w = u16::from_le_bytes(*chunk);
        // Bit 15 must be 0 for standard GBA BGR555 colors
        if (w & 0x8000) != 0 {
            return None;
        }
        words.push(w);
        let r = (w & 0x1F) as u32;
        let g = ((w >> 5) & 0x1F) as u32;
        let b = ((w >> 10) & 0x1F) as u32;
        let lum = (r * 299 + g * 587 + b * 114) / 1000;
        lums.push(lum);
        colors.push(bgr555_to_rgba8(w));
    }

    let unique_words: HashSet<u16> = words.iter().copied().collect();
    let min_unique = if color_count == 16 { 4 } else { 16 };
    if unique_words.len() < min_unique {
        return None;
    }

    // Check for constant arithmetic progressions (e.g., opcodes or counters)
    let mut is_progression = true;
    if words.len() >= 4 {
        let delta = words[1].wrapping_sub(words[0]);
        for i in 1..words.len() - 1 {
            if words[i + 1].wrapping_sub(words[i]) != delta {
                is_progression = false;
                break;
            }
        }
    }
    if is_progression && unique_words.len() > 1 {
        return None;
    }

    let min_lum = lums.iter().copied().min().unwrap_or(0);
    let max_lum = lums.iter().copied().max().unwrap_or(0);
    let contrast = (max_lum - min_lum) as f32;

    if color_count == 16 && contrast < 5.0 {
        return None;
    }
    if color_count == 256 && contrast < 8.0 {
        return None;
    }

    Some((colors, contrast))
}

// -----------------------------------------------------------------------------
// Tileset Decoder & Tilemap Decoder
// -----------------------------------------------------------------------------

/// Scan ROM for LZ77 and Huffman compressed blocks and classify them into
/// tilesets and background tilemaps.
pub fn scan_graphics_and_maps(
    rom: &[u8],
    palettes: &[ScannedPalette],
) -> (Vec<ScannedTileset>, Vec<ScannedTilemap>) {
    let mut tilesets = Vec::new();
    let mut tilemaps = Vec::new();

    let mut scanned_ranges: Vec<(usize, usize)> = Vec::new();

    // Scan for 4-byte aligned compression headers (LZ77: 0x10, Huffman: 0x24/0x28)
    for off in (0..rom.len().saturating_sub(8)).step_by(4) {
        // Skip if inside an already decompressed block
        if scanned_ranges.iter().any(|(s, e)| off >= *s && off < *e) {
            continue;
        }

        let magic = rom[off];
        let maybe_decompressed = if magic == 0x10 {
            // LZ77
            let sz = (rom[off + 1] as usize)
                | ((rom[off + 2] as usize) << 8)
                | ((rom[off + 3] as usize) << 16);
            if (32..=262_144).contains(&sz) {
                decode_lz77(&rom[off..], 262_144)
                    .ok()
                    .map(|out| (CompressionKind::Lz77, out.consumed, out.bytes))
            } else {
                None
            }
        } else if magic == 0x11 {
            // Capcom / Nintendo Extended LZ11
            let sz = (rom[off + 1] as usize)
                | ((rom[off + 2] as usize) << 8)
                | ((rom[off + 3] as usize) << 16);
            if (32..=262_144).contains(&sz) || sz == 0 {
                decode_lz11(&rom[off..], 262_144)
                    .ok()
                    .map(|out| (CompressionKind::Lz11, out.consumed, out.bytes))
            } else {
                None
            }
        } else if magic == 0x48 || rom[off..].starts_with(b"HLZ\x01") {
            // Hudson Soft variant LZ: validate size range before attempting decode
            let sz = if rom[off..].starts_with(b"HLZ\x01") {
                if off + 8 <= rom.len() {
                    u32::from_le_bytes([rom[off + 4], rom[off + 5], rom[off + 6], rom[off + 7]])
                        as usize
                } else {
                    0
                }
            } else if off + 4 <= rom.len() {
                (rom[off + 1] as usize)
                    | ((rom[off + 2] as usize) << 8)
                    | ((rom[off + 3] as usize) << 16)
            } else {
                0
            };
            if (32..=262_144).contains(&sz) {
                decode_hudson_lz(&rom[off..], 262_144)
                    .ok()
                    .map(|out| (CompressionKind::HudsonLz, out.consumed, out.bytes))
            } else {
                None
            }
        } else if magic == 0x4B {
            // Konami Dictionary / Huffman
            let sz = (rom[off + 1] as usize)
                | ((rom[off + 2] as usize) << 8)
                | ((rom[off + 3] as usize) << 16);
            if (32..=262_144).contains(&sz) {
                decode_konami_dict(&rom[off..], 262_144)
                    .ok()
                    .map(|out| (CompressionKind::KonamiDict, out.consumed, out.bytes))
            } else {
                None
            }
        } else if magic == 0x53 {
            // Shin'en Mode 7 / Affine Delta RLE
            let sz = (rom[off + 1] as usize)
                | ((rom[off + 2] as usize) << 8)
                | ((rom[off + 3] as usize) << 16);
            if (32..=262_144).contains(&sz) {
                decode_shinen_mode7(&rom[off..], 262_144)
                    .ok()
                    .map(|out| (CompressionKind::ShinenMode7, out.consumed, out.bytes))
            } else {
                None
            }
        } else if magic == 0x24 || magic == 0x28 {
            // Huffman
            let sz = (rom[off + 1] as usize)
                | ((rom[off + 2] as usize) << 8)
                | ((rom[off + 3] as usize) << 16);
            if (32..=262_144).contains(&sz) {
                decode_huffman(&rom[off..], 262_144)
                    .ok()
                    .map(|out| (CompressionKind::Huffman, out.consumed, out.bytes))
            } else {
                None
            }
        } else {
            None
        };

        if let Some((comp, consumed, decompressed)) = maybe_decompressed {
            let decomp_len = decompressed.len();
            // Reject expanding decodes on niche/Huffman codecs (common Thumb opcode false positive),
            // while permitting normal literal-block overhead (~25%) on standard BIOS LZ77/LZ11.
            let max_consumed = match comp {
                CompressionKind::Lz77 | CompressionKind::Lz11 => decomp_len + (decomp_len / 4) + 8,
                _ => decomp_len.saturating_sub(1),
            };
            if consumed > max_consumed {
                continue;
            }
            scanned_ranges.push((off, off + consumed));

            // Check if it matches a standard background tilemap size:
            // 2048 bytes (32x32), 4096 bytes (64x32), or 8192 bytes (64x64)
            if decomp_len == 2048 || decomp_len == 4096 || decomp_len == 8192 {
                if let Some(map) = try_decode_tilemap(off, comp, consumed, &decompressed) {
                    tilemaps.push(map);
                    continue;
                }
            }

            // Otherwise, if it is a multiple of 32 bytes and looks like graphics, classify as a tileset
            if decomp_len >= 64 && decomp_len.is_multiple_of(32) {
                // Basic entropy check: ensure it is not all identical bytes
                let first_byte = decompressed[0];
                let all_same = decompressed.iter().all(|&b| b == first_byte);
                if !all_same {
                    let bpp = classify_tileset_bpp(off, off + consumed, comp, &decompressed, palettes);
                    let pal_off = find_nearest_palette_for_bpp(off, off + consumed, bpp, palettes);
                    let tile_count = if bpp == 8 { decomp_len / 64 } else { decomp_len / 32 };

                    tilesets.push(ScannedTileset {
                        id: format!("tiles_0x{:08X}", off),
                        offset: off,
                        bus_address: rom_offset_to_bus(off).unwrap_or(0),
                        compression: comp,
                        bpp,
                        tile_count,
                        decompressed_bytes: decomp_len,
                        compressed_bytes: consumed,
                        palette_offset: pal_off,
                        raw_data: decompressed,
                    });
                }
            }
        }
    }

    // Scan for uncompressed tilesets referenced by ROM pointer tables
    let tables = scan_pointer_tables(rom, 8);
    let mut candidate_raw_offsets: Vec<usize> = Vec::new();
    let mut seen_raw_offsets = HashSet::new();

    for table in tables {
        for &bus_ptr in &table.entries {
            if let Some(raw_off) = bus_to_rom_offset(bus_ptr, rom.len()) {
                if raw_off >= 0x200
                    && raw_off.is_multiple_of(32)
                    && raw_off + 256 <= rom.len()
                    && seen_raw_offsets.insert(raw_off)
                {
                    candidate_raw_offsets.push(raw_off);
                }
            }
        }
    }
    candidate_raw_offsets.sort_unstable();

    for (idx, &raw_off) in candidate_raw_offsets.iter().enumerate() {
        if scanned_ranges.iter().any(|(s, e)| raw_off >= *s && raw_off < *e) {
            continue;
        }
        // Bound upper limit using next sorted pointer and next scanned range to avoid overlaps
        let mut upper_bound = (raw_off + 32_768).min(rom.len());
        if let Some(&next_ptr_off) = candidate_raw_offsets.get(idx + 1) {
            if next_ptr_off > raw_off {
                upper_bound = upper_bound.min(next_ptr_off);
            }
        }
        for &(s, _) in &scanned_ranges {
            if s > raw_off {
                upper_bound = upper_bound.min(s);
            }
        }
        let max_scan = (upper_bound.saturating_sub(raw_off)) & !31;
        if max_scan < 256 {
            continue;
        }
        let block = &rom[raw_off..raw_off + max_scan];

        // Reject ASCII text, pointer tables, ARM/Thumb machine code, or flat padding
        if !looks_like_raw_tileset(block) {
            continue;
        }

        let mut valid_len = 256usize;
        while valid_len + 32 <= max_scan {
            let next_tile = &block[valid_len..valid_len + 32];
            if next_tile.iter().all(|&b| b == 0) || next_tile.iter().all(|&b| b == 0xFF) {
                break;
            }
            valid_len += 32;
        }

        if valid_len >= 256 {
            let raw_slice = &block[..valid_len];
            let bpp = classify_tileset_bpp(raw_off, raw_off + valid_len, CompressionKind::Raw, raw_slice, palettes);
            let pal_off = find_nearest_palette_for_bpp(raw_off, raw_off + valid_len, bpp, palettes);
            let tile_count = if bpp == 8 { valid_len / 64 } else { valid_len / 32 };

            scanned_ranges.push((raw_off, raw_off + valid_len));
            tilesets.push(ScannedTileset {
                id: format!("tiles_raw_0x{:08X}", raw_off),
                offset: raw_off,
                bus_address: rom_offset_to_bus(raw_off).unwrap_or(0),
                compression: CompressionKind::Raw,
                bpp,
                tile_count,
                decompressed_bytes: valid_len,
                compressed_bytes: valid_len,
                palette_offset: pal_off,
                raw_data: raw_slice.to_vec(),
            });
        }
    }

    tilesets.sort_by_key(|t| t.offset);

    // Associate nearest tilesets and palettes to tilemaps
    for map in &mut tilemaps {
        if map.tileset_offset.is_none() && !tilesets.is_empty() {
            let best_tileset = tilesets
                .iter()
                .min_by_key(|t| (t.offset as isize - map.offset as isize).abs())
                .map(|t| t.offset);
            map.tileset_offset = best_tileset;
        }
        if map.palette_offset.is_none() && !palettes.is_empty() {
            map.palette_offset = find_nearest_palette_offset(map.offset, palettes);
        }
    }

    (tilesets, tilemaps)
}

fn try_decode_tilemap(
    offset: usize,
    compression: CompressionKind,
    consumed: usize,
    bytes: &[u8],
) -> Option<ScannedTilemap> {
    let (w, h) = match bytes.len() {
        2048 => (32, 32),
        4096 => (64, 32), // Standard 64x32 horizontal paging
        8192 => (64, 64),
        _ => return None,
    };

    let entry_count = w * h;
    let mut entries = Vec::with_capacity(entry_count);
    let mut plausible_tile_indices = 0usize;

    for chunk in bytes[..entry_count * 2].as_chunks::<2>().0 {
        let raw = u16::from_le_bytes(*chunk);
        let entry = ScrEntry::from_u16(raw);
        if entry.tile_index < 1024 {
            plausible_tile_indices += 1;
        }
        entries.push(entry);
    }

    // Require plausible tile indices for at least 95% of entries
    if plausible_tile_indices * 100 / entry_count < 95 {
        return None;
    }

    Some(ScannedTilemap {
        id: format!("map_0x{:08X}", offset),
        offset,
        bus_address: rom_offset_to_bus(offset).unwrap_or(0),
        compression,
        width_tiles: w,
        height_tiles: h,
        decompressed_bytes: bytes.len(),
        compressed_bytes: consumed,
        tileset_offset: None,
        palette_offset: None,
        entries,
    })
}

fn find_nearest_palette_offset(offset: usize, palettes: &[ScannedPalette]) -> Option<usize> {
    palettes
        .iter()
        .min_by_key(|p| (p.offset as isize - offset as isize).abs())
        .map(|p| p.offset)
}

/// Pair a tileset with the nearest compatible palette (16-color for 4bpp, 256-color for 8bpp),
/// excluding any palette offsets that fall inside the tileset's own compressed byte range.
fn find_nearest_palette_for_bpp(
    offset: usize,
    range_end: usize,
    bpp: u8,
    palettes: &[ScannedPalette],
) -> Option<usize> {
    let target_colors = if bpp == 8 { 256 } else { 16 };
    let exact = palettes
        .iter()
        .filter(|p| p.color_count == target_colors && !(p.offset >= offset && p.offset < range_end))
        .min_by_key(|p| (p.offset as isize - offset as isize).abs())
        .map(|p| p.offset);
    if exact.is_some() {
        return exact;
    }
    palettes
        .iter()
        .filter(|p| !(p.offset >= offset && p.offset < range_end))
        .min_by_key(|p| (p.offset as isize - offset as isize).abs())
        .map(|p| p.offset)
}

/// Content-based 4bpp vs 8bpp classifier combining tile row autocorrelation (stride 4 vs stride 8),
/// compression kind, and nearby 256-color palette evidence.
fn classify_tileset_bpp(
    offset: usize,
    range_end: usize,
    comp: CompressionKind,
    data: &[u8],
    palettes: &[ScannedPalette],
) -> u8 {
    if !data.len().is_multiple_of(64) {
        return 4;
    }
    if comp == CompressionKind::ShinenMode7 {
        return 8;
    }

    // Check if nearest non-overlapping palette within 64 KB is 256-color
    let nearest_outside = palettes
        .iter()
        .filter(|p| !(p.offset >= offset && p.offset < range_end))
        .min_by_key(|p| (p.offset as isize - offset as isize).abs());
    if let Some(p) = nearest_outside {
        if p.color_count == 256 && (p.offset as isize - offset as isize).unsigned_abs() <= 65_536 {
            return 8;
        }
    }

    // Content heuristic: compare vertical row-to-row correlation at stride 4 (4bpp) vs stride 8 (8bpp)
    let sample = &data[..data.len().min(4096)];
    if sample.len() >= 128 {
        let mut stride4_eq = 0usize;
        let mut stride8_eq = 0usize;
        let mut distinct = [false; 256];
        for &b in sample {
            distinct[b as usize] = true;
        }
        for i in 0..sample.len() - 8 {
            if sample[i] != 0 && sample[i] == sample[i + 4] {
                stride4_eq += 1;
            }
            if sample[i] != 0 && sample[i] == sample[i + 8] {
                stride8_eq += 1;
            }
        }
        let distinct_count = distinct.iter().filter(|&&v| v).count();
        if distinct_count >= 48 && stride8_eq > stride4_eq.saturating_mul(2) && stride8_eq >= 32 {
            return 8;
        }
    }

    4
}

/// Reject ASCII strings, pointer tables, ARM/Thumb machine code, and flat runs when carving raw tiles.
fn looks_like_raw_tileset(block: &[u8]) -> bool {
    let probe = &block[..block.len().min(256)];
    let non_pad = probe.iter().filter(|&&b| b != 0 && b != 0xFF).count();
    if non_pad < 32 {
        return false;
    }

    // 1. Reject printable ASCII text blocks
    let ascii = probe
        .iter()
        .filter(|&&b| (0x20..=0x7E).contains(&b) || b == b'\n' || b == b'\r')
        .count();
    if ascii * 100 / probe.len() >= 50 {
        return false;
    }

    // 2. Reject ROM pointer tables (>25% Wait State 0 ROM bus pointers)
    let (words, _) = probe.as_chunks::<4>();
    let rom_ptrs = words
        .iter()
        .filter(|w| {
            let v = u32::from_le_bytes(**w);
            (0x0800_0000..0x0A00_0000).contains(&v)
        })
        .count();
    if !words.is_empty() && rom_ptrs * 100 / words.len() >= 25 {
        return false;
    }

    // 3. Reject ARM code (>45% unconditional 0xE* top nibble) and Thumb function prologues/epilogues
    let arm_cond_e = words.iter().filter(|w| (w[3] & 0xF0) == 0xE0).count();
    if !words.is_empty() && arm_cond_e * 100 / words.len() >= 45 {
        return false;
    }
    let (halfwords, _) = probe.as_chunks::<2>();
    let thumb_markers = halfwords
        .iter()
        .filter(|hw| {
            let v = u16::from_le_bytes(**hw);
            v == 0x4770 || (v & 0xFF00) == 0xB500 || (v & 0xFF00) == 0xBD00
        })
        .count();
    if thumb_markers >= 3 {
        return false;
    }

    true
}

// -----------------------------------------------------------------------------
// Rendering & Composite Engine
// -----------------------------------------------------------------------------

/// Render a 4bpp or 8bpp tileset into a 16-tile wide RGBA8 tile sheet.
pub fn render_tileset_sheet_rgba(
    tileset: &ScannedTileset,
    palette: &[[u8; 4]],
) -> Result<(u32, u32, Vec<u8>)> {
    let tile_count = tileset.tile_count;
    if tile_count == 0 {
        return Err(Error::InvalidField {
            what: "tile_count_zero",
            value: 0,
        });
    }

    let columns = 16usize;
    let rows = tile_count.div_ceil(columns);
    let width_px = columns * 8;
    let height_px = rows * 8;
    let mut out = vec![0u8; width_px * height_px * 4];

    for idx in 0..tile_count {
        let col = idx % columns;
        let row = idx / columns;
        let tile_pixels = if tileset.bpp == 8 {
            let tile_bytes_start = idx * 64;
            if tile_bytes_start + 64 > tileset.raw_data.len() {
                break;
            }
            decode_tile_8bpp(&tileset.raw_data[tile_bytes_start..tile_bytes_start + 64])?
        } else {
            let tile_bytes_start = idx * 32;
            if tile_bytes_start + 32 > tileset.raw_data.len() {
                break;
            }
            decode_tile_4bpp(&tileset.raw_data[tile_bytes_start..tile_bytes_start + 32])?
        };

        for ty in 0..8 {
            for tx in 0..8 {
                let px = col * 8 + tx;
                let py = row * 8 + ty;
                let out_offset = (py * width_px + px) * 4;
                let color_idx = tile_pixels[ty * 8 + tx] as usize;
                let color = palette.get(color_idx).copied().unwrap_or_else(|| {
                    let g = (color_idx * 17).min(255) as u8;
                    [g, g, g, 255]
                });
                out[out_offset..out_offset + 4].copy_from_slice(&color);
            }
        }
    }

    Ok((width_px as u32, height_px as u32, out))
}

/// Render a GBA background tilemap with hardware screen-block quadrant addressing,
/// tile horizontal/vertical flips, and 16 palette banks into an RGBA8 buffer.
pub fn render_tilemap_rgba(
    map: &ScannedTilemap,
    tiles: &[u8],
    palette: &[[u8; 4]],
    transparent_zero: bool,
) -> Result<(u32, u32, Vec<u8>)> {
    let width_px = map.width_tiles * 8;
    let height_px = map.height_tiles * 8;
    let mut out = vec![0u8; width_px * height_px * 4];

    for ty in 0..map.height_tiles {
        for tx in 0..map.width_tiles {
            let entry_idx = screen_block_entry_index(tx, ty, map.width_tiles, map.height_tiles);
            let entry = map
                .entries
                .get(entry_idx)
                .copied()
                .unwrap_or(ScrEntry {
                    tile_index: 0,
                    h_flip: false,
                    v_flip: false,
                    palette_bank: 0,
                });

            let tile_offset = (entry.tile_index as usize) * 32;
            let tile_pixels = if tile_offset + 32 <= tiles.len() {
                decode_tile_4bpp(&tiles[tile_offset..tile_offset + 32])?
            } else {
                [0u8; 64]
            };

            let bank_base = (entry.palette_bank as usize) * 16;

            for y in 0..8 {
                let src_y = if entry.v_flip { 7 - y } else { y };
                for x in 0..8 {
                    let src_x = if entry.h_flip { 7 - x } else { x };
                    let color_idx = tile_pixels[src_y * 8 + src_x] as usize;

                    let px = tx * 8 + x;
                    let py = ty * 8 + y;
                    let out_offset = (py * width_px + px) * 4;

                    let color = if color_idx == 0 && transparent_zero {
                        [0, 0, 0, 0]
                    } else {
                        let pal_idx = bank_base + color_idx;
                        palette.get(pal_idx).copied().unwrap_or_else(|| {
                            let g = (color_idx * 17).min(255) as u8;
                            [g, g, g, 255]
                        })
                    };

                    out[out_offset..out_offset + 4].copy_from_slice(&color);
                }
            }
        }
    }

    Ok((width_px as u32, height_px as u32, out))
}

// -----------------------------------------------------------------------------
// Tiled TSX & TMX Export
// -----------------------------------------------------------------------------

/// Generate a Tiled Tileset (`.tsx`) XML document.
pub fn generate_tsx(
    name: &str,
    tile_count: usize,
    columns: usize,
    image_rel_path: &str,
    img_w: u32,
    img_h: u32,
) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <tileset version=\"1.10\" tiledversion=\"1.10.2\" name=\"{name}\" tilewidth=\"8\" tileheight=\"8\" tilecount=\"{tile_count}\" columns=\"{columns}\">\n \
         <image source=\"{image_rel_path}\" width=\"{img_w}\" height=\"{img_h}\"/>\n\
         </tileset>\n"
    )
}

/// Generate a Tiled Map (`.tmx`) XML document referencing a `.tsx` tileset.
///
/// Encodes tile indices and GBA flip flags into standard Tiled 32-bit GIDs:
/// - Bit 31 (`0x8000_0000`): Horizontal flip
/// - Bit 30 (`0x4000_0000`): Vertical flip
pub fn generate_tmx(
    map: &ScannedTilemap,
    tsx_rel_path: &str,
) -> String {
    let mut csv = String::with_capacity(map.width_tiles * map.height_tiles * 6);

    for ty in 0..map.height_tiles {
        for tx in 0..map.width_tiles {
            let entry_idx = screen_block_entry_index(tx, ty, map.width_tiles, map.height_tiles);
            let entry = map
                .entries
                .get(entry_idx)
                .copied()
                .unwrap_or(ScrEntry {
                    tile_index: 0,
                    h_flip: false,
                    v_flip: false,
                    palette_bank: 0,
                });

            let mut gid = (entry.tile_index as u32) + 1; // 1-based local GID
            if entry.h_flip {
                gid |= 0x8000_0000;
            }
            if entry.v_flip {
                gid |= 0x4000_0000;
            }

            csv.push_str(&gid.to_string());
            if tx + 1 < map.width_tiles || ty + 1 < map.height_tiles {
                csv.push(',');
            }
        }
        csv.push('\n');
    }

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <map version=\"1.10\" tiledversion=\"1.10.2\" orientation=\"orthogonal\" renderorder=\"right-down\" width=\"{w}\" height=\"{h}\" tilewidth=\"8\" tileheight=\"8\" infinite=\"0\" nextlayerid=\"2\" nextobjectid=\"1\">\n \
         <tileset firstgid=\"1\" source=\"{tsx_rel_path}\"/>\n \
         <layer id=\"1\" name=\"Background Layer\" width=\"{w}\" height=\"{h}\">\n  \
         <data encoding=\"csv\">\n{csv}\
         </data>\n \
         </layer>\n\
         </map>\n",
        w = map.width_tiles,
        h = map.height_tiles,
    )
}

// -----------------------------------------------------------------------------
// OAM Sprite Rendering & Scanning
// -----------------------------------------------------------------------------

/// Render an OAM sprite (composite multi-tile shape) into an RGBA8 buffer.
pub fn render_sprite_rgba(
    attr: &ObjAttr,
    tiles: &[u8],
    palette: &[[u8; 4]],
    mapping_1d: bool,
) -> Result<(u16, u16, Vec<u8>)> {
    let (w, h) = attr.dimensions().ok_or(Error::InvalidField {
        what: "invalid_sprite_dimensions",
        value: attr.size as u64,
    })?;

    let tiles_x = (w / 8) as usize;
    let tiles_y = (h / 8) as usize;
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];

    let is_8bpp = attr.color_mode == crate::oam::ObjColorMode::Bpp8;
    let bank_base = if is_8bpp { 0 } else { (attr.palette_bank as usize) * 16 };

    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let local_tile = if mapping_1d {
                if is_8bpp {
                    (attr.tile_index as usize) + (ty * tiles_x + tx) * 2
                } else {
                    (attr.tile_index as usize) + (ty * tiles_x + tx)
                }
            } else {
                (attr.tile_index as usize) + ty * 32 + tx
            };

            let tile_bytes_start = if is_8bpp {
                local_tile * 64
            } else {
                local_tile * 32
            };

            let tile_pixels = if is_8bpp {
                if tile_bytes_start + 64 <= tiles.len() {
                    decode_tile_8bpp(&tiles[tile_bytes_start..tile_bytes_start + 64])?
                } else {
                    [0u8; 64]
                }
            } else {
                if tile_bytes_start + 32 <= tiles.len() {
                    decode_tile_4bpp(&tiles[tile_bytes_start..tile_bytes_start + 32])?
                } else {
                    [0u8; 64]
                }
            };

            for y in 0..8 {
                for x in 0..8 {
                    let color_idx = tile_pixels[y * 8 + x] as usize;
                    if color_idx == 0 {
                        continue; // Transparent color 0
                    }
                    let col = palette.get(bank_base + color_idx).copied().unwrap_or([255, 255, 255, 255]);

                    // Hardware sprite flip applies to entire composite sprite
                    let final_x = if attr.h_flip {
                        (w as usize) - 1 - (tx * 8 + x)
                    } else {
                        tx * 8 + x
                    };
                    let final_y = if attr.v_flip {
                        (h as usize) - 1 - (ty * 8 + y)
                    } else {
                        ty * 8 + y
                    };

                    let out_offset = (final_y * (w as usize) + final_x) * 4;
                    out[out_offset..out_offset + 4].copy_from_slice(&col);
                }
            }
        }
    }

    Ok((w, h, out))
}

/// Scan ROM for valid OAM sprite tables.
pub fn scan_sprites(rom: &[u8]) -> Vec<ScannedSprite> {
    let mut sprites = Vec::new();
    // Scan at 8-byte aligned offsets for valid OAM attribute entries
    for off in (0..rom.len().saturating_sub(8)).step_by(8) {
        let entry_bytes: [u8; 8] = match rom[off..off + 8].try_into() {
            Ok(b) => b,
            Err(_) => continue,
        };
        if let Ok(attr) = ObjAttr::from_oam_entry(&entry_bytes) {
            if let Some((w, h)) = attr.dimensions() {
                // Must have plausible attributes and non-disabled status
                if !attr.is_disabled() && attr.tile_index < 512 {
                    sprites.push(ScannedSprite {
                        id: format!("sprite_0x{:08X}", off),
                        offset: off,
                        bus_address: rom_offset_to_bus(off).unwrap_or(0),
                        width: w,
                        height: h,
                        bpp: if attr.color_mode == crate::oam::ObjColorMode::Bpp8 { 8 } else { 4 },
                        tile_index: attr.tile_index,
                        palette_bank: attr.palette_bank,
                        h_flip: attr.h_flip,
                        v_flip: attr.v_flip,
                    });
                }
            }
        }
    }
    sprites
}

// -----------------------------------------------------------------------------
// Rip Orchestration & Manifest Export
// -----------------------------------------------------------------------------

/// Configuration options for the graphics asset ripper.
#[derive(Debug, Clone)]
pub struct RipOptions {
    pub extract_palettes: bool,
    pub extract_tilesets: bool,
    pub extract_tilemaps: bool,
    pub extract_sprites: bool,
    pub output_dir: PathBuf,
}

impl Default for RipOptions {
    fn default() -> Self {
        Self {
            extract_palettes: true,
            extract_tilesets: true,
            extract_tilemaps: true,
            extract_sprites: true,
            output_dir: PathBuf::from("gfx_rip"),
        }
    }
}

/// Execute the high-performance graphics asset ripper across a ROM buffer.
pub fn rip_graphics(rom: &[u8], options: &RipOptions) -> Result<RipManifest> {
    let hdr = AgbHeader::parse(rom).unwrap_or_else(|_| AgbHeader {
        entry_opcode: 0,
        title: "UNKNOWN".into(),
        game_code: "AGB".into(),
        maker_code: "00".into(),
        fixed_value: 0,
        unit_code: 0,
        device_type: 0,
        software_version: 0,
        header_checksum: 0,
        computed_checksum: 0,
    });

    let palettes = scan_palettes(rom);
    let (tilesets, tilemaps) = scan_graphics_and_maps(rom, &palettes);
    let sprites = if options.extract_sprites {
        scan_sprites(rom)
    } else {
        Vec::new()
    };

    fs::create_dir_all(&options.output_dir).map_err(|e| Error::Malformed(e.to_string()))?;

    let pal_dir = options.output_dir.join("palettes");
    let tiles_dir = options.output_dir.join("tiles");
    let maps_dir = options.output_dir.join("maps");
    let sprites_dir = options.output_dir.join("sprites");

    if options.extract_palettes && !palettes.is_empty() {
        fs::create_dir_all(&pal_dir).map_err(|e| Error::Malformed(e.to_string()))?;
    }
    if options.extract_tilesets && !tilesets.is_empty() {
        fs::create_dir_all(&tiles_dir).map_err(|e| Error::Malformed(e.to_string()))?;
    }
    if options.extract_tilemaps && !tilemaps.is_empty() {
        fs::create_dir_all(&maps_dir).map_err(|e| Error::Malformed(e.to_string()))?;
    }
    if options.extract_sprites && !sprites.is_empty() {
        fs::create_dir_all(&sprites_dir).map_err(|e| Error::Malformed(e.to_string()))?;
    }

    let mut palette_entries = Vec::new();
    let mut tileset_entries = Vec::new();
    let mut tilemap_entries = Vec::new();
    let mut sprite_entries = Vec::new();

    // Map palettes by offset for lookup
    let pal_map: BTreeMap<usize, &ScannedPalette> = palettes.iter().map(|p| (p.offset, p)).collect();
    let tiles_map: BTreeMap<usize, &ScannedTileset> = tilesets.iter().map(|t| (t.offset, t)).collect();

    // Default clean 256-color palette
    let default_palette: Vec<[u8; 4]> = (0..=255u8).map(|i| [i, i, i, 255]).collect();

    // 1. Export Palettes
    if options.extract_palettes {
        for pal in &palettes {
            let act_bytes = encode_act_palette(&pal.colors);
            let act_path = pal_dir.join(format!("{}.act", pal.id));
            fs::write(&act_path, &act_bytes).map_err(|e| Error::Malformed(e.to_string()))?;

            let (w, h, swatch_rgba) = render_palette_swatch_rgba(&pal.colors);
            let png_bytes = RawPngEncoder::encode_rgba8(w, h, &swatch_rgba)?;
            let png_path = pal_dir.join(format!("{}.png", pal.id));
            fs::write(&png_path, &png_bytes).map_err(|e| Error::Malformed(e.to_string()))?;

            palette_entries.push(PaletteManifestEntry {
                id: pal.id.clone(),
                offset: format!("0x{:08X}", pal.offset),
                bus_address: format!("0x{:08X}", pal.bus_address),
                colors: pal.color_count,
                bytes: pal.color_count * 2,
                act_file: format!("palettes/{}.act", pal.id),
                png_file: format!("palettes/{}.png", pal.id),
            });
        }
    }

    // 2. Export Tilesets
    if options.extract_tilesets {
        for ts in &tilesets {
            let pal = ts
                .palette_offset
                .and_then(|off| pal_map.get(&off))
                .map(|p| p.colors.as_slice())
                .unwrap_or(default_palette.as_slice());

            let (w, h, sheet_rgba) = render_tileset_sheet_rgba(ts, pal)?;
            let png_bytes = RawPngEncoder::encode_rgba8(w, h, &sheet_rgba)?;
            let png_filename = format!("{}.png", ts.id);
            let png_path = tiles_dir.join(&png_filename);
            fs::write(&png_path, &png_bytes).map_err(|e| Error::Malformed(e.to_string()))?;

            let tsx_filename = format!("{}.tsx", ts.id);
            let tsx_xml = generate_tsx(&ts.id, ts.tile_count, 16, &png_filename, w, h);
            let tsx_path = tiles_dir.join(&tsx_filename);
            fs::write(&tsx_path, tsx_xml.as_bytes()).map_err(|e| Error::Malformed(e.to_string()))?;

            tileset_entries.push(TilesetManifestEntry {
                id: ts.id.clone(),
                offset: format!("0x{:08X}", ts.offset),
                bus_address: format!("0x{:08X}", ts.bus_address),
                compression: ts.compression,
                bpp: ts.bpp,
                tile_count: ts.tile_count,
                decompressed_bytes: ts.decompressed_bytes,
                compressed_bytes: ts.compressed_bytes,
                palette_offset: ts.palette_offset.map(|off| format!("0x{:08X}", off)),
                png_file: format!("tiles/{}", png_filename),
                tsx_file: format!("tiles/{}", tsx_filename),
            });
        }
    }

    // 3. Export Tilemaps
    if options.extract_tilemaps {
        for map in &tilemaps {
            let tiles_bytes = map
                .tileset_offset
                .and_then(|off| tiles_map.get(&off))
                .map(|t| t.raw_data.as_slice())
                .unwrap_or(&[]);

            let pal = map
                .palette_offset
                .and_then(|off| pal_map.get(&off))
                .map(|p| p.colors.as_slice())
                .unwrap_or(default_palette.as_slice());

            let (w, h, map_rgba) = render_tilemap_rgba(map, tiles_bytes, pal, false)?;
            let png_bytes = RawPngEncoder::encode_rgba8(w, h, &map_rgba)?;
            let png_filename = format!("{}.png", map.id);
            let png_path = maps_dir.join(&png_filename);
            fs::write(&png_path, &png_bytes).map_err(|e| Error::Malformed(e.to_string()))?;

            // Generate Tiled TMX referencing tileset
            let tsx_rel = match map.tileset_offset {
                Some(t_off) => format!("../tiles/tiles_0x{:08X}.tsx", t_off),
                None => "../tiles/default.tsx".to_string(),
            };
            let tmx_filename = format!("{}.tmx", map.id);
            let tmx_xml = generate_tmx(map, &tsx_rel);
            let tmx_path = maps_dir.join(&tmx_filename);
            fs::write(&tmx_path, tmx_xml.as_bytes()).map_err(|e| Error::Malformed(e.to_string()))?;

            tilemap_entries.push(TilemapManifestEntry {
                id: map.id.clone(),
                offset: format!("0x{:08X}", map.offset),
                bus_address: format!("0x{:08X}", map.bus_address),
                compression: map.compression,
                width_tiles: map.width_tiles,
                height_tiles: map.height_tiles,
                width_px: map.width_tiles * 8,
                height_px: map.height_tiles * 8,
                decompressed_bytes: map.decompressed_bytes,
                tileset_offset: map.tileset_offset.map(|off| format!("0x{:08X}", off)),
                palette_offset: map.palette_offset.map(|off| format!("0x{:08X}", off)),
                png_file: format!("maps/{}", png_filename),
                tmx_file: format!("maps/{}", tmx_filename),
            });
        }
    }

    // 4. Export Sprites (first 32 unique sprites)
    if options.extract_sprites && !sprites.is_empty() {
        let first_tiles = tilesets.first().map(|t| t.raw_data.as_slice()).unwrap_or(&[]);
        for sp in sprites.iter().take(32) {
            let attr = ObjAttr {
                y: 0,
                affine: false,
                double_size_or_disable: false,
                mode: crate::oam::ObjMode::Normal,
                mosaic: false,
                color_mode: if sp.bpp == 8 {
                    crate::oam::ObjColorMode::Bpp8
                } else {
                    crate::oam::ObjColorMode::Bpp4
                },
                shape: match (sp.width, sp.height) {
                    (w, h) if w == h => crate::oam::ObjShape::Square,
                    (w, h) if w > h => crate::oam::ObjShape::Horizontal,
                    _ => crate::oam::ObjShape::Vertical,
                },
                x: 0,
                affine_param: 0,
                h_flip: sp.h_flip,
                v_flip: sp.v_flip,
                size: 0,
                tile_index: sp.tile_index,
                priority: 0,
                palette_bank: sp.palette_bank,
            };

            if let Ok((w, h, rgba)) = render_sprite_rgba(&attr, first_tiles, &default_palette, true) {
                if let Ok(png_bytes) = RawPngEncoder::encode_rgba8(w as u32, h as u32, &rgba) {
                    let filename = format!("{}.png", sp.id);
                    let path = sprites_dir.join(&filename);
                    let _ = fs::write(&path, &png_bytes);

                    sprite_entries.push(SpriteManifestEntry {
                        id: sp.id.clone(),
                        offset: format!("0x{:08X}", sp.offset),
                        bus_address: format!("0x{:08X}", sp.bus_address),
                        width: sp.width,
                        height: sp.height,
                        bpp: sp.bpp,
                        tile_index: sp.tile_index,
                        palette_bank: sp.palette_bank,
                        png_file: format!("sprites/{}", filename),
                    });
                }
            }
        }
    }

    let manifest = RipManifest {
        rom_title: hdr.title,
        game_code: hdr.game_code,
        maker_code: hdr.maker_code,
        rom_size: rom.len(),
        summary: RipSummary {
            palette_count: palette_entries.len(),
            tileset_count: tileset_entries.len(),
            tilemap_count: tilemap_entries.len(),
            sprite_count: sprite_entries.len(),
        },
        palettes: palette_entries,
        tilesets: tileset_entries,
        tilemaps: tilemap_entries,
        sprites: sprite_entries,
    };

    let manifest_json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| Error::Malformed(e.to_string()))?;
    let manifest_path = options.output_dir.join("manifest.json");
    fs::write(&manifest_path, manifest_json.as_bytes())
        .map_err(|e| Error::Malformed(e.to_string()))?;

    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gba_compress::encode_lz77;
    use crate::{rgb8_to_bgr555, decode_mode5_rgba, decode_mode5_image, GBA_MODE5_WIDTH, GBA_MODE5_HEIGHT};

    #[test]
    fn test_raw_png_encoder_rgba_and_rgb() {
        // 4x4 RGBA
        let rgba = vec![255u8; 4 * 4 * 4];
        let png = RawPngEncoder::encode_rgba8(4, 4, &rgba).unwrap();
        assert_eq!(&png[0..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

        // Check chunk names
        let ihdr_type = &png[12..16];
        assert_eq!(ihdr_type, b"IHDR");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");

        // 4x4 RGB
        let rgb = vec![128u8; 4 * 4 * 3];
        let png_rgb = RawPngEncoder::encode_rgb8(4, 4, &rgb).unwrap();
        assert_eq!(&png_rgb[0..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);

        // Decoded pixels round-trip exactly (RGB input decodes as opaque RGBA).
        assert_eq!(formatkit_codec::decode_png_rgba(&png, 16).unwrap().rgba, rgba);
        let opaque: Vec<u8> = [128, 128, 128, 255].repeat(16);
        assert_eq!(formatkit_codec::decode_png_rgba(&png_rgb, 16).unwrap().rgba, opaque);
    }

    #[test]
    fn test_act_palette_encoder_exactly_768_bytes() {
        let palette: Vec<[u8; 4]> = vec![
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 0, 255],
        ];
        let act = encode_act_palette(&palette);
        assert_eq!(act.len(), 768);
        assert_eq!(&act[0..3], &[255, 0, 0]);
        assert_eq!(&act[3..6], &[0, 255, 0]);
        assert_eq!(&act[6..9], &[0, 0, 255]);
        assert_eq!(&act[9..12], &[255, 255, 0]);
        assert_eq!(&act[12..15], &[0, 0, 0]);
    }

    #[test]
    fn test_screen_block_entry_index_quadrants() {
        // 32x32
        assert_eq!(screen_block_entry_index(0, 0, 32, 32), 0);
        assert_eq!(screen_block_entry_index(31, 31, 32, 32), 1023);

        // 64x32
        assert_eq!(screen_block_entry_index(0, 0, 64, 32), 0);
        assert_eq!(screen_block_entry_index(31, 31, 64, 32), 1023);
        assert_eq!(screen_block_entry_index(32, 0, 64, 32), 1024);
        assert_eq!(screen_block_entry_index(63, 31, 64, 32), 2047);

        // 32x64
        assert_eq!(screen_block_entry_index(0, 0, 32, 64), 0);
        assert_eq!(screen_block_entry_index(31, 31, 32, 64), 1023);
        assert_eq!(screen_block_entry_index(0, 32, 32, 64), 1024);
        assert_eq!(screen_block_entry_index(31, 63, 32, 64), 2047);

        // 64x64
        assert_eq!(screen_block_entry_index(0, 0, 64, 64), 0);
        assert_eq!(screen_block_entry_index(32, 0, 64, 64), 1024);
        assert_eq!(screen_block_entry_index(0, 32, 64, 64), 2048);
        assert_eq!(screen_block_entry_index(32, 32, 64, 64), 3072);
        assert_eq!(screen_block_entry_index(63, 63, 64, 64), 4095);
    }

    #[test]
    fn test_screen_map_render_composite_accuracy() {
        // Create an asymmetric 4bpp tile (tile 0):
        // top-left pixel = 1, bottom-right pixel = 2, others = 0
        let mut tile0 = [0u8; 32];
        tile0[0] = 0x01; // row 0, pixel 0 = 1, pixel 1 = 0
        tile0[31] = 0x20; // row 7, pixel 6 = 0, pixel 7 = 2

        let mut tiles = Vec::new();
        tiles.extend_from_slice(&tile0);

        let mut palette = vec![[0u8; 4]; 256];
        palette[1] = [200, 10, 10, 255]; // Red
        palette[2] = [10, 10, 200, 255]; // Blue

        // Create 32x32 map with 4 entries at (0,0), (1,0), (0,1), (1,1):
        // (0,0): normal
        // (1,0): h_flip
        // (0,1): v_flip
        // (1,1): h_flip + v_flip
        let mut map_entries = vec![ScrEntry {
            tile_index: 0,
            h_flip: false,
            v_flip: false,
            palette_bank: 0,
        }; 1024];

        map_entries[0] = ScrEntry {
            tile_index: 0,
            h_flip: false,
            v_flip: false,
            palette_bank: 0,
        };
        map_entries[1] = ScrEntry {
            tile_index: 0,
            h_flip: true,
            v_flip: false,
            palette_bank: 0,
        };
        map_entries[32] = ScrEntry {
            tile_index: 0,
            h_flip: false,
            v_flip: true,
            palette_bank: 0,
        };
        map_entries[33] = ScrEntry {
            tile_index: 0,
            h_flip: true,
            v_flip: true,
            palette_bank: 0,
        };

        let map = ScannedTilemap {
            id: "map_test".into(),
            offset: 0,
            bus_address: 0x08000000,
            compression: CompressionKind::Raw,
            width_tiles: 32,
            height_tiles: 32,
            decompressed_bytes: 2048,
            compressed_bytes: 2048,
            tileset_offset: Some(0),
            palette_offset: Some(0),
            entries: map_entries,
        };

        let (w, h, rgba) = render_tilemap_rgba(&map, &tiles, &palette, false).unwrap();
        assert_eq!(w, 256);
        assert_eq!(h, 256);

        let get_px = |x: usize, y: usize| -> [u8; 4] {
            let idx = (y * 256 + x) * 4;
            [rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3]]
        };

        // (0,0) normal: (0,0) has color 1, (7,7) has color 2
        assert_eq!(get_px(0, 0), palette[1]);
        assert_eq!(get_px(7, 7), palette[2]);

        // (1,0) h_flip: (8,0) tile spans x=8..15. Pixel 0 is flipped to x=15, Pixel 7 flipped to x=8
        assert_eq!(get_px(15, 0), palette[1]);
        assert_eq!(get_px(8, 7), palette[2]);

        // (0,1) v_flip: tile spans y=8..15. Pixel 0 is flipped to y=15, Pixel 7 flipped to y=8
        assert_eq!(get_px(0, 15), palette[1]);
        assert_eq!(get_px(7, 8), palette[2]);

        // (1,1) h_flip + v_flip: flipped both. Pixel 0 at (15, 15), Pixel 7 at (8, 8)
        assert_eq!(get_px(15, 15), palette[1]);
        assert_eq!(get_px(8, 8), palette[2]);
    }

    #[test]
    fn test_tmx_and_tsx_serialization() {
        let tsx = generate_tsx("test_tiles", 64, 16, "test_tiles.png", 128, 32);
        assert!(tsx.contains("<tileset"));
        assert!(tsx.contains("tilewidth=\"8\""));
        assert!(tsx.contains("tileheight=\"8\""));
        assert!(tsx.contains("columns=\"16\""));
        assert!(tsx.contains("image source=\"test_tiles.png\""));

        let mut entries = vec![ScrEntry {
            tile_index: 0,
            h_flip: false,
            v_flip: false,
            palette_bank: 0,
        }; 1024];

        // Entry 0: tile 5, h_flip = true -> GID = (5 + 1) | 0x80000000 = 2147483654
        entries[0] = ScrEntry {
            tile_index: 5,
            h_flip: true,
            v_flip: false,
            palette_bank: 0,
        };
        // Entry 1: tile 7, v_flip = true -> GID = (7 + 1) | 0x40000000 = 1073741832
        entries[1] = ScrEntry {
            tile_index: 7,
            h_flip: false,
            v_flip: true,
            palette_bank: 0,
        };

        let map = ScannedTilemap {
            id: "map_01".into(),
            offset: 0,
            bus_address: 0x08000000,
            compression: CompressionKind::Lz77,
            width_tiles: 32,
            height_tiles: 32,
            decompressed_bytes: 2048,
            compressed_bytes: 500,
            tileset_offset: None,
            palette_offset: None,
            entries,
        };

        let tmx = generate_tmx(&map, "../tiles/test_tiles.tsx");
        assert!(tmx.contains("<map"));
        assert!(tmx.contains("width=\"32\""));
        assert!(tmx.contains("height=\"32\""));
        assert!(tmx.contains("<tileset firstgid=\"1\" source=\"../tiles/test_tiles.tsx\"/>"));
        assert!(tmx.contains("2147483654,1073741832"));
    }

    #[test]
    fn test_sprite_multi_tile_render_flips() {
        // Create 2x2 tiles (16x16 sprite) in 4bpp
        let mut tiles = vec![0u8; 4 * 32];
        // Tile 0 at (0,0): pixel (0,0) = 1
        tiles[0] = 0x01;
        // Tile 3 at (1,1): pixel (7,7) = 2
        tiles[3 * 32 + 31] = 0x20;

        let mut palette = vec![[0u8; 4]; 256];
        palette[1] = [255, 0, 0, 255];
        palette[2] = [0, 0, 255, 255];

        let mut attr = ObjAttr {
            y: 0,
            affine: false,
            double_size_or_disable: false,
            mode: crate::oam::ObjMode::Normal,
            mosaic: false,
            color_mode: crate::oam::ObjColorMode::Bpp4,
            shape: crate::oam::ObjShape::Square,
            x: 0,
            affine_param: 0,
            h_flip: false,
            v_flip: false,
            size: 1, // 16x16
            tile_index: 0,
            priority: 0,
            palette_bank: 0,
        };

        // Normal render
        let (w, h, rgba) = render_sprite_rgba(&attr, &tiles, &palette, true).unwrap();
        assert_eq!(w, 16);
        assert_eq!(h, 16);
        let get_px = |x: usize, y: usize| -> [u8; 4] {
            let idx = (y * 16 + x) * 4;
            [rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3]]
        };
        assert_eq!(get_px(0, 0), palette[1]);
        assert_eq!(get_px(15, 15), palette[2]);

        // Horizontal flip
        attr.h_flip = true;
        let (_, _, rgba_h) = render_sprite_rgba(&attr, &tiles, &palette, true).unwrap();
        let get_px_h = |x: usize, y: usize| -> [u8; 4] {
            let idx = (y * 16 + x) * 4;
            [rgba_h[idx], rgba_h[idx + 1], rgba_h[idx + 2], rgba_h[idx + 3]]
        };
        assert_eq!(get_px_h(15, 0), palette[1]);
        assert_eq!(get_px_h(0, 15), palette[2]);

        // Vertical flip
        attr.h_flip = false;
        attr.v_flip = true;
        let (_, _, rgba_v) = render_sprite_rgba(&attr, &tiles, &palette, true).unwrap();
        let get_px_v = |x: usize, y: usize| -> [u8; 4] {
            let idx = (y * 16 + x) * 4;
            [rgba_v[idx], rgba_v[idx + 1], rgba_v[idx + 2], rgba_v[idx + 3]]
        };
        assert_eq!(get_px_v(0, 15), palette[1]);
        assert_eq!(get_px_v(15, 0), palette[2]);
    }

    #[test]
    fn test_mode5_framebuffer_decoding() {
        let pixel_count = GBA_MODE5_WIDTH * GBA_MODE5_HEIGHT;
        let mut vram = vec![0u8; pixel_count * 2];
        let red = rgb8_to_bgr555(255, 0, 0);
        let green = rgb8_to_bgr555(0, 255, 0);
        vram[0..2].copy_from_slice(&red.to_le_bytes());
        vram[2..4].copy_from_slice(&green.to_le_bytes());

        let rgba = decode_mode5_rgba(&vram).unwrap();
        assert_eq!(rgba.len(), pixel_count * 4);
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[0, 255, 0, 255]);

        let img = decode_mode5_image(&vram).unwrap();
        assert_eq!(img.width, GBA_MODE5_WIDTH as u32);
        assert_eq!(img.height, GBA_MODE5_HEIGHT as u32);
    }

    #[test]
    fn test_synthetic_rom_graphics_rip_pipeline() {
        // Build a synthetic GBA ROM
        let mut rom = vec![0u8; 32768];

        // Valid AgbHeader at offset 0
        rom[0..4].copy_from_slice(&0xEA00002Eu32.to_le_bytes());
        rom[0xA0..0xAC].copy_from_slice(b"TEST TITLE  ");
        rom[0xAC..0xB0].copy_from_slice(b"TSTE");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xB2] = 0x96;
        let mut chk = 0u8;
        for b in &rom[0xA0..0xBD] {
            chk = chk.wrapping_sub(*b);
        }
        rom[0xBD] = chk.wrapping_sub(0x19);

        // Place a pointer table at 0x200 pointing to:
        // - Palette at 0x400
        // - Tileset at 0x600
        // - Tilemap at 0x800
        let pal_addr = 0x08000400u32;
        let tiles_addr = 0x08000600u32;
        let map_addr = 0x08000800u32;
        rom[0x200..0x204].copy_from_slice(&pal_addr.to_le_bytes());
        rom[0x204..0x208].copy_from_slice(&tiles_addr.to_le_bytes());
        rom[0x208..0x20C].copy_from_slice(&map_addr.to_le_bytes());
        rom[0x20C..0x210].copy_from_slice(&0x08000A00u32.to_le_bytes());

        // 16-color Palette at 0x400 with high contrast
        for i in 0..16 {
            let color = rgb8_to_bgr555((i * 16) as u8, 0, ((15 - i) * 16) as u8);
            rom[0x400 + i * 2..0x400 + i * 2 + 2].copy_from_slice(&color.to_le_bytes());
        }

        // LZ77 Tileset at 0x600 (4 tiles of 32 bytes = 128 bytes)
        let mut raw_tiles = vec![0u8; 128];
        for (i, b) in raw_tiles.iter_mut().enumerate() {
            *b = (i % 250) as u8;
        }
        let lz_tiles = encode_lz77(&raw_tiles).unwrap();
        rom[0x600..0x600 + lz_tiles.len()].copy_from_slice(&lz_tiles);

        // LZ77 32x32 Tilemap at 0x800 (1024 entries = 2048 bytes)
        let mut raw_map = vec![0u8; 2048];
        for i in 0..1024 {
            let entry = ScrEntry {
                tile_index: (i % 4) as u16,
                h_flip: (i % 2) != 0,
                v_flip: (i % 3) == 0,
                palette_bank: (i % 2) as u8,
            };
            raw_map[i * 2..i * 2 + 2].copy_from_slice(&entry.to_u16().to_le_bytes());
        }
        let lz_map = encode_lz77(&raw_map).unwrap();
        rom[0x800..0x800 + lz_map.len()].copy_from_slice(&lz_map);

        // OAM sprite table at 0x1000
        let obj = ObjAttr {
            y: 30,
            affine: false,
            double_size_or_disable: false,
            mode: crate::oam::ObjMode::Normal,
            mosaic: false,
            color_mode: crate::oam::ObjColorMode::Bpp4,
            shape: crate::oam::ObjShape::Square,
            x: 50,
            affine_param: 0,
            h_flip: false,
            v_flip: false,
            size: 1, // 16x16
            tile_index: 0,
            priority: 1,
            palette_bank: 0,
        };
        let obj_attrs = obj.to_attrs();
        rom[0x1000..0x1002].copy_from_slice(&obj_attrs[0].to_le_bytes());
        rom[0x1002..0x1004].copy_from_slice(&obj_attrs[1].to_le_bytes());
        rom[0x1004..0x1006].copy_from_slice(&obj_attrs[2].to_le_bytes());

        // Run ripper in isolated temp dir
        let test_dir = std::env::temp_dir().join(format!("gbakit_rip_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&test_dir);

        let options = RipOptions {
            extract_palettes: true,
            extract_tilesets: true,
            extract_tilemaps: true,
            extract_sprites: true,
            output_dir: test_dir.clone(),
        };

        let manifest = rip_graphics(&rom, &options).expect("rip_graphics succeeds");
        assert_eq!(manifest.game_code, "TSTE");
        assert!(manifest.summary.palette_count >= 1);
        assert!(manifest.summary.tileset_count >= 1);
        assert!(manifest.summary.tilemap_count >= 1);

        // Check generated files
        assert!(test_dir.join("manifest.json").is_file());
        assert!(test_dir.join(&manifest.palettes[0].act_file).is_file());
        assert!(test_dir.join(&manifest.palettes[0].png_file).is_file());
        assert!(test_dir.join(&manifest.tilesets[0].png_file).is_file());
        assert!(test_dir.join(&manifest.tilesets[0].tsx_file).is_file());
        assert!(test_dir.join(&manifest.tilemaps[0].png_file).is_file());
        assert!(test_dir.join(&manifest.tilemaps[0].tmx_file).is_file());

        // Cleanup
        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_8bpp_and_uncompressed_tileset_ripping() {
        let mut rom = vec![0u8; 65536];

        // Valid AgbHeader at offset 0
        rom[0..4].copy_from_slice(&0xEA00002Eu32.to_le_bytes());
        rom[0xA0..0xAC].copy_from_slice(b"8BPP TEST   ");
        rom[0xAC..0xB0].copy_from_slice(b"8BPE");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xB2] = 0x96;

        // 256-color Palette at 0x400 (512 bytes)
        for i in 0..256 {
            let color = rgb8_to_bgr555((i % 32 * 8) as u8, ((i / 32) * 32) as u8, 128);
            rom[0x400 + i * 2..0x400 + i * 2 + 2].copy_from_slice(&color.to_le_bytes());
        }

        // 8bpp LZ77 Tileset at 0x800 (4 tiles * 64 bytes = 256 bytes)
        let mut raw_8bpp_tiles = vec![0u8; 256];
        for (i, b) in raw_8bpp_tiles.iter_mut().enumerate() {
            *b = (i % 255) as u8;
        }
        let lz_8bpp = encode_lz77(&raw_8bpp_tiles).unwrap();
        rom[0x800..0x800 + lz_8bpp.len()].copy_from_slice(&lz_8bpp);

        // Raw uncompressed tileset at 0x2000 (8 tiles * 32 bytes = 256 bytes)
        for i in 0..256 {
            rom[0x2000 + i] = (i % 15 * 17) as u8;
        }

        // Pointer table at 0x1000 with 8 pointers pointing to 0x2000 and onwards
        let raw_bus_ptr = 0x08002000u32;
        for i in 0..8 {
            let entry_addr = raw_bus_ptr + (i as u32) * 32;
            let off = 0x1000 + i * 4;
            rom[off..off + 4].copy_from_slice(&entry_addr.to_le_bytes());
        }

        let test_dir = std::env::temp_dir().join(format!("gbakit_rip_8bpp_{}", std::process::id()));
        let _ = fs::remove_dir_all(&test_dir);

        let options = RipOptions {
            extract_palettes: true,
            extract_tilesets: true,
            extract_tilemaps: false,
            extract_sprites: false,
            output_dir: test_dir.clone(),
        };

        let manifest = rip_graphics(&rom, &options).expect("rip_graphics succeeds for 8bpp/raw");
        assert!(manifest.summary.palette_count >= 1);
        assert!(manifest.summary.tileset_count >= 1);

        // Check that 8bpp tileset was correctly detected with bpp: 8 and tile_count: 4
        let ts_8bpp = manifest.tilesets.iter().find(|t| t.offset == "0x00000800");
        assert!(ts_8bpp.is_some(), "8bpp tileset at 0x800 detected");
        let ts = ts_8bpp.unwrap();
        assert_eq!(ts.bpp, 8, "Expected bpp 8");
        assert_eq!(ts.tile_count, 4, "Expected 4 tiles of 64 bytes");

        // Cleanup
        let _ = fs::remove_dir_all(&test_dir);
    }
}

