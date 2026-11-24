//! GBA PPU graphics modding and safe capacity-checked ROM write-back.
//!
//! Supports ingesting PNG (via `formatkit-codec`) and binary PPM (`P6`) images into RGBA8,
//! matching or quantizing against 16-color (4bpp) or 256-color (8bpp) GBA `BGR555` palettes,
//! packing 8×8 linear tiles, recompressing with GBA BIOS codecs (`LZ77` WRAM/VRAM, `RLE`,
//! `Huffman` 4-bit/8-bit, or `Raw`), and writing back into ROM either in-place (with
//! consumed-stream capacity checking and slack padding) or via free-space relocation and
//! automatic 4-byte-aligned ROM pointer-table patching.

#![deny(unsafe_code)]

use formatkit_core::{Error, Image, Result};
use formatkit_pixel::{nearest_palette_index, quantize_rgba, PaletteDistanceMetric};
use gba_compress::{
    decode_huffman, decode_lz77, decode_rle, encode_huffman_4bit, encode_huffman_8bit, encode_lz77,
    encode_lz77_vram, encode_rle, DEFAULT_MAX_OUTPUT,
};
use gba_rom::{find_free_space_prefer_ff, rom_offset_to_bus, HEADER_SIZE};
pub use gba_rom::{find_free_space, patch_rom_pointers};
use serde::{Deserialize, Serialize};

use crate::{bgr555_to_rgba8, decode_palette, encode_tile_4bpp, rgb8_to_bgr555};

/// Default upper bound on decoded image pixels (16 megapixels).
pub const DEFAULT_MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;

/// Default alignment (in bytes) for relocated graphics streams in GBA ROMs.
pub const DEFAULT_FREE_SPACE_ALIGN: usize = 4;

const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

// -----------------------------------------------------------------------------
// Image Ingestion (PNG + PPM P6)
// -----------------------------------------------------------------------------

/// Decode a PNG or binary PPM (`P6`) byte slice into an RGBA8 [`Image`].
pub fn decode_image_rgba(bytes: &[u8]) -> Result<Image> {
    if bytes.starts_with(&PNG_SIGNATURE) {
        return formatkit_codec::decode_png_rgba(bytes, DEFAULT_MAX_IMAGE_PIXELS);
    }
    if bytes.len() >= 2 && &bytes[..2] == b"P6" {
        return decode_ppm_p6_rgba(bytes);
    }
    Err(Error::Malformed(
        "unsupported image format: expected PNG or binary PPM (P6)".into(),
    ))
}

/// Decode a binary PPM (`P6`) byte slice into an opaque RGBA8 [`Image`].
pub fn decode_ppm_p6_rgba(bytes: &[u8]) -> Result<Image> {
    if bytes.len() < 3 || &bytes[..2] != b"P6" {
        return Err(Error::Malformed("invalid PPM P6 signature".into()));
    }
    let mut pos = 2usize;
    if !bytes[pos].is_ascii_whitespace() && bytes[pos] != b'#' {
        return Err(Error::Malformed(
            "PPM P6 magic must be followed by whitespace".into(),
        ));
    }

    let width = read_ppm_u32_token(bytes, &mut pos, "ppm_width")?;
    let height = read_ppm_u32_token(bytes, &mut pos, "ppm_height")?;
    let maxval = read_ppm_u32_token(bytes, &mut pos, "ppm_maxval")?;

    if width == 0 || height == 0 {
        return Err(Error::InvalidField {
            what: "ppm_dimensions",
            value: (u64::from(width) << 32) | u64::from(height),
        });
    }
    let pixel_count = u64::from(width) * u64::from(height);
    if pixel_count > DEFAULT_MAX_IMAGE_PIXELS {
        return Err(Error::ResourceLimit {
            resource: "ppm_pixels",
            requested: pixel_count,
            limit: DEFAULT_MAX_IMAGE_PIXELS,
        });
    }
    if maxval == 0 || maxval > 65535 {
        return Err(Error::InvalidField {
            what: "ppm_maxval",
            value: u64::from(maxval),
        });
    }

    // Standard PPM P6 has exactly one single whitespace byte separating maxval from raster data.
    if pos >= bytes.len() || !bytes[pos].is_ascii_whitespace() {
        return Err(Error::Malformed(
            "expected single whitespace separator after PPM maxval".into(),
        ));
    }
    pos += 1;

    let pixels = pixel_count as usize;
    let bytes_per_sample = if maxval <= 255 { 1usize } else { 2usize };
    let needed = pixels
        .checked_mul(3)
        .and_then(|n| n.checked_mul(bytes_per_sample))
        .ok_or(Error::ResourceLimit {
            resource: "ppm_raster_bytes",
            requested: u64::MAX,
            limit: usize::MAX as u64,
        })?;
    let available = bytes.len().saturating_sub(pos);
    if available < needed {
        return Err(Error::Truncated {
            offset: pos,
            needed,
            available,
        });
    }

    let raster = &bytes[pos..pos + needed];
    let mut rgba = Vec::with_capacity(pixels * 4);
    if maxval == 255 {
        for rgb in raster.as_chunks::<3>().0 {
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 0xFF]);
        }
    } else if maxval < 255 {
        let half = maxval / 2;
        for rgb in raster.as_chunks::<3>().0 {
            let r = ((u32::from(rgb[0]) * 255 + half) / maxval).min(255) as u8;
            let g = ((u32::from(rgb[1]) * 255 + half) / maxval).min(255) as u8;
            let b = ((u32::from(rgb[2]) * 255 + half) / maxval).min(255) as u8;
            rgba.extend_from_slice(&[r, g, b, 0xFF]);
        }
    } else {
        let half = maxval / 2;
        for rgb in raster.as_chunks::<6>().0 {
            let r16 = u32::from(u16::from_be_bytes([rgb[0], rgb[1]]));
            let g16 = u32::from(u16::from_be_bytes([rgb[2], rgb[3]]));
            let b16 = u32::from(u16::from_be_bytes([rgb[4], rgb[5]]));
            let r = ((r16 * 255 + half) / maxval).min(255) as u8;
            let g = ((g16 * 255 + half) / maxval).min(255) as u8;
            let b = ((b16 * 255 + half) / maxval).min(255) as u8;
            rgba.extend_from_slice(&[r, g, b, 0xFF]);
        }
    }

    Ok(Image {
        width,
        height,
        rgba,
    })
}

fn read_ppm_u32_token(bytes: &[u8], pos: &mut usize, what: &'static str) -> Result<u32> {
    loop {
        while *pos < bytes.len() && bytes[*pos].is_ascii_whitespace() {
            *pos += 1;
        }
        if *pos < bytes.len() && bytes[*pos] == b'#' {
            while *pos < bytes.len() && bytes[*pos] != b'\n' {
                *pos += 1;
            }
            continue;
        }
        break;
    }

    let start = *pos;
    while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
        *pos += 1;
    }
    if *pos == start {
        return Err(Error::Malformed(format!("missing PPM header token: {what}")));
    }
    let token = std::str::from_utf8(&bytes[start..*pos])
        .map_err(|_| Error::Malformed(format!("non-UTF8 PPM header token: {what}")))?;
    token
        .parse::<u32>()
        .map_err(|_| Error::Malformed(format!("invalid PPM integer for {what}: {token}")))
}

// -----------------------------------------------------------------------------
// Palette & Tile Encoding
// -----------------------------------------------------------------------------

/// Bit depth of a GBA 8×8 linear tileset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TileBpp {
    /// 4 bits per pixel (16 colors, 32 bytes per 8×8 tile).
    Bpp4,
    /// 8 bits per pixel (256 colors, 64 bytes per 8×8 tile).
    Bpp8,
}

impl TileBpp {
    /// Number of bytes per 8×8 GBA linear tile.
    pub const fn bytes_per_tile(self) -> usize {
        match self {
            Self::Bpp4 => 32,
            Self::Bpp8 => 64,
        }
    }

    /// Maximum number of palette entries supported at this bit depth.
    pub const fn max_colors(self) -> usize {
        match self {
            Self::Bpp4 => 16,
            Self::Bpp8 => 256,
        }
    }

    /// Numeric bit depth (`4` or `8`).
    pub const fn bits(self) -> u8 {
        match self {
            Self::Bpp4 => 4,
            Self::Bpp8 => 8,
        }
    }
}

/// Strategy for resolving palette indices when encoding an RGBA image into GBA tiles.
#[derive(Debug, Clone, Copy)]
pub enum PaletteMode<'a> {
    /// Match image pixels against an existing RGBA8 palette (1..=16 for 4bpp, 1..=256 for 8bpp).
    Existing(&'a [[u8; 4]]),
    /// Match image pixels against an existing little-endian GBA `BGR555` palette slice.
    ExistingBgr555(&'a [u8]),
    /// Generate a fresh `BGR555`-quantized palette from the image.
    Generate,
}

/// Encoded GBA linear tileset and its associated `BGR555` palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedTileset {
    /// Uncompressed GBA 8×8 linear tile bytes (`32 * tile_count` for 4bpp, `64 * tile_count` for 8bpp).
    pub tile_bytes: Vec<u8>,
    /// Resolved RGBA8 palette (length `16` for 4bpp or `256` for 8bpp when generated).
    pub palette_rgba: Vec<[u8; 4]>,
    /// Little-endian 16-bit GBA `BGR555` palette bytes (`2 * palette_rgba.len()`).
    pub palette_bgr555: Vec<u8>,
    /// Bit depth of the encoded tiles.
    pub bpp: TileBpp,
    /// Total number of 8×8 tiles.
    pub tile_count: usize,
    /// Tileset width in 8×8 tiles.
    pub width_tiles: u32,
    /// Tileset height in 8×8 tiles.
    pub height_tiles: u32,
}

/// Encode an RGBA8 palette into little-endian 15-bit GBA `BGR555` bytes (`2 * N` bytes).
pub fn encode_palette_bgr555(palette: &[[u8; 4]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(palette.len() * 2);
    for &[r, g, b, _] in palette {
        out.extend_from_slice(&rgb8_to_bgr555(r, g, b).to_le_bytes());
    }
    out
}

/// Quantize or palette-match an RGBA8 [`Image`] and pack it into GBA 8×8 linear `4bpp` or `8bpp` tiles.
///
/// The image dimensions must be positive multiples of 8. When `transparent_index0` is `true`,
/// pixels with `alpha < 128` are mapped to palette index `0` (GBA hardware transparency), and
/// opaque pixels are matched against indices `1..`.
pub fn encode_image_to_tiles(
    image: &Image,
    bpp: TileBpp,
    palette_mode: PaletteMode<'_>,
    transparent_index0: bool,
) -> Result<EncodedTileset> {
    image.validate()?;
    if image.width == 0
        || image.height == 0
        || !image.width.is_multiple_of(8)
        || !image.height.is_multiple_of(8)
    {
        return Err(Error::InvalidField {
            what: "tile_image_dimensions",
            value: (u64::from(image.width) << 32) | u64::from(image.height),
        });
    }

    let max_colors = bpp.max_colors();
    let (palette_rgba, indices) = match palette_mode {
        PaletteMode::Existing(pal) => {
            map_image_to_existing_palette(image, pal, max_colors, transparent_index0)?
        }
        PaletteMode::ExistingBgr555(bgr_bytes) => {
            let pal = decode_palette(bgr_bytes)?;
            map_image_to_existing_palette(image, &pal, max_colors, transparent_index0)?
        }
        PaletteMode::Generate => {
            quantize_image_to_bgr555_palette(image, max_colors, transparent_index0)?
        }
    };

    let width_tiles = image.width / 8;
    let height_tiles = image.height / 8;
    let tile_count = (width_tiles as usize) * (height_tiles as usize);
    let stride = image.width as usize;

    let mut tile_bytes = Vec::with_capacity(tile_count * bpp.bytes_per_tile());
    let mut tile_px = [0u8; 64];

    for ty in 0..height_tiles as usize {
        for tx in 0..width_tiles as usize {
            for py in 0..8usize {
                let row_start = (ty * 8 + py) * stride + tx * 8;
                tile_px[py * 8..py * 8 + 8].copy_from_slice(&indices[row_start..row_start + 8]);
            }
            match bpp {
                TileBpp::Bpp4 => {
                    let packed = encode_tile_4bpp(&tile_px)?;
                    tile_bytes.extend_from_slice(&packed);
                }
                TileBpp::Bpp8 => {
                    tile_bytes.extend_from_slice(&tile_px);
                }
            }
        }
    }

    let palette_bgr555 = encode_palette_bgr555(&palette_rgba);
    Ok(EncodedTileset {
        tile_bytes,
        palette_rgba,
        palette_bgr555,
        bpp,
        tile_count,
        width_tiles,
        height_tiles,
    })
}

fn map_image_to_existing_palette(
    image: &Image,
    palette: &[[u8; 4]],
    max_colors: usize,
    transparent_index0: bool,
) -> Result<(Vec<[u8; 4]>, Vec<u8>)> {
    if palette.is_empty() || palette.len() > max_colors {
        return Err(Error::InvalidField {
            what: "palette_color_count",
            value: palette.len() as u64,
        });
    }

    let pixel_count = (image.width as usize) * (image.height as usize);
    let mut indices = Vec::with_capacity(pixel_count);
    let opaque_slice = if transparent_index0 && palette.len() > 1 {
        &palette[1..]
    } else {
        palette
    };
    let opaque_offset = if transparent_index0 && palette.len() > 1 {
        1usize
    } else {
        0usize
    };

    for &color in image.rgba.as_chunks::<4>().0 {
        if transparent_index0 && color[3] < 0x80 {
            indices.push(0);
        } else {
            let best = nearest_palette_index(color, opaque_slice, PaletteDistanceMetric::Rgb)
                .unwrap_or(0);
            indices.push((opaque_offset + best) as u8);
        }
    }

    Ok((palette.to_vec(), indices))
}

fn quantize_image_to_bgr555_palette(
    image: &Image,
    max_colors: usize,
    transparent_index0: bool,
) -> Result<(Vec<[u8; 4]>, Vec<u8>)> {
    let pixel_count = (image.width as usize) * (image.height as usize);
    let target_opaque_colors = if transparent_index0 {
        max_colors.saturating_sub(1).max(1)
    } else {
        max_colors
    };

    // Snap opaque pixels to GBA BGR555 space before quantizing so colors that map to
    // the same 15-bit hardware word never waste distinct palette entries.
    let mut opaque_snapped = Vec::with_capacity(image.rgba.len());
    for px in image.rgba.as_chunks::<4>().0 {
        if transparent_index0 && px[3] < 0x80 {
            continue;
        }
        let snapped = bgr555_to_rgba8(rgb8_to_bgr555(px[0], px[1], px[2]));
        opaque_snapped.extend_from_slice(&snapped);
    }

    let active_colors: Vec<[u8; 4]> = if opaque_snapped.is_empty() {
        Vec::new()
    } else {
        let (pal, _) = quantize_rgba(
            &opaque_snapped,
            target_opaque_colors,
            PaletteDistanceMetric::Rgb,
        )
        .map_err(|e| Error::Malformed(e.to_string()))?;
        // Snap averaged median-cut centroids back to exact BGR555-representable RGBA.
        let mut snapped_pal: Vec<[u8; 4]> = pal
            .into_iter()
            .map(|c| bgr555_to_rgba8(rgb8_to_bgr555(c[0], c[1], c[2])))
            .collect();
        snapped_pal.sort_unstable();
        snapped_pal.dedup();
        snapped_pal
    };

    let mut full_palette = Vec::with_capacity(max_colors);
    let opaque_start = if transparent_index0 {
        full_palette.push([0, 0, 0, 0]);
        1usize
    } else {
        0usize
    };
    full_palette.extend_from_slice(&active_colors);
    let active_len = active_colors.len();
    while full_palette.len() < max_colors {
        full_palette.push([0, 0, 0, 255]);
    }

    let active_slice = &full_palette[opaque_start..opaque_start + active_len];
    let mut indices = Vec::with_capacity(pixel_count);
    for px in image.rgba.as_chunks::<4>().0 {
        if (transparent_index0 && px[3] < 0x80) || active_slice.is_empty() {
            indices.push(0);
        } else {
            let snapped = bgr555_to_rgba8(rgb8_to_bgr555(px[0], px[1], px[2]));
            let best = nearest_palette_index(snapped, active_slice, PaletteDistanceMetric::Rgb)
                .unwrap_or(0);
            indices.push((opaque_start + best) as u8);
        }
    }

    Ok((full_palette, indices))
}

// -----------------------------------------------------------------------------
// Compression Detection & Recompression
// -----------------------------------------------------------------------------

/// GBA graphics compression codec for write-back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GfxCompression {
    /// Uncompressed raw tile or palette bytes.
    Raw,
    /// GBA BIOS LZ77 (`0x10`, `SWI 0x11` WRAM-safe, `disp >= 1`).
    Lz77Wram,
    /// GBA BIOS LZ77 (`0x10`, `SWI 0x12` VRAM-safe, `disp >= 2`).
    Lz77Vram,
    /// GBA BIOS RLE (`0x30`, `SWI 0x14` / `0x15`).
    Rle,
    /// GBA BIOS 4-bit Huffman (`0x24`, `SWI 0x13`).
    Huffman4,
    /// GBA BIOS 8-bit Huffman (`0x28`, `SWI 0x13`).
    Huffman8,
}

/// Result of probing a ROM offset for a self-delimiting GBA BIOS compressed stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedStream {
    /// Detected compression codec (`Lz77Vram` for `0x10` to guarantee VRAM safety).
    pub compression: GfxCompression,
    /// Exact number of compressed bytes consumed in the ROM.
    pub consumed_bytes: usize,
    /// Decompressed payload size in bytes.
    pub decompressed_bytes: usize,
}

/// Compress `data` using the requested [`GfxCompression`] codec.
pub fn compress_gfx(data: &[u8], compression: GfxCompression) -> Result<Vec<u8>> {
    match compression {
        GfxCompression::Raw => Ok(data.to_vec()),
        GfxCompression::Lz77Wram => encode_lz77(data),
        GfxCompression::Lz77Vram => encode_lz77_vram(data),
        GfxCompression::Rle => encode_rle(data),
        GfxCompression::Huffman4 => encode_huffman_4bit(data),
        GfxCompression::Huffman8 => encode_huffman_8bit(data),
    }
}

/// Probe `rom` at `rom_offset` for a valid GBA BIOS compressed stream (`LZ77`, `RLE`,
/// or `Huffman` 4-bit/8-bit) and return its exact `consumed_bytes` and `decompressed_bytes`.
///
/// Returns `Ok(None)` if the bytes at `rom_offset` do not form a valid BIOS compressed stream.
pub fn detect_stream_at(
    rom: &[u8],
    rom_offset: usize,
    max_output: usize,
) -> Result<Option<DetectedStream>> {
    if rom_offset >= rom.len() {
        return Err(Error::Truncated {
            offset: rom_offset,
            needed: 1,
            available: 0,
        });
    }
    let slice = &rom[rom_offset..];
    if slice.len() < 4 {
        return Ok(None);
    }

    let detected = match slice[0] {
        0x10 => decode_lz77(slice, max_output).ok().map(|out| DetectedStream {
            compression: GfxCompression::Lz77Vram,
            consumed_bytes: out.consumed,
            decompressed_bytes: out.bytes.len(),
        }),
        0x30 => decode_rle(slice, max_output).ok().map(|out| DetectedStream {
            compression: GfxCompression::Rle,
            consumed_bytes: out.consumed,
            decompressed_bytes: out.bytes.len(),
        }),
        0x24 => decode_huffman(slice, max_output).ok().map(|out| DetectedStream {
            compression: GfxCompression::Huffman4,
            consumed_bytes: out.consumed,
            decompressed_bytes: out.bytes.len(),
        }),
        0x28 => decode_huffman(slice, max_output).ok().map(|out| DetectedStream {
            compression: GfxCompression::Huffman8,
            consumed_bytes: out.consumed,
            decompressed_bytes: out.bytes.len(),
        }),
        _ => None,
    };

    Ok(detected)
}

// -----------------------------------------------------------------------------
// Safe ROM Write-Back API
// -----------------------------------------------------------------------------

/// Options controlling safe capacity-checked ROM asset replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceOptions {
    /// Target compression codec. `None` auto-detects the original stream's codec at `rom_offset`.
    pub compression: Option<GfxCompression>,
    /// Explicit byte capacity of the original asset at `rom_offset`.
    /// Required for uncompressed (`Raw`) tilesets; optional for self-delimiting BIOS streams.
    pub original_capacity: Option<usize>,
    /// Fill byte written to any slack bytes when the new stream is smaller than `original_capacity`.
    pub pad_byte: u8,
    /// When `true`, relocate overflowing streams to free space and rewrite ROM pointers.
    pub allow_relocate: bool,
    /// Alignment (in bytes) for relocated streams in free space (default: `4`).
    pub relocate_alignment: usize,
    /// Minimum ROM offset to begin scanning for free space (default: `0xC0`).
    pub free_space_start: usize,
    /// Treat pixels with `alpha < 128` as palette index `0` (GBA transparency).
    pub transparent_index0: bool,
}

impl Default for ReplaceOptions {
    fn default() -> Self {
        Self {
            compression: None,
            original_capacity: None,
            pad_byte: 0x00,
            allow_relocate: false,
            relocate_alignment: DEFAULT_FREE_SPACE_ALIGN,
            free_space_start: HEADER_SIZE,
            transparent_index0: true,
        }
    }
}

/// Structured report describing a completed ROM graphics replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceReport {
    /// Original ROM offset of the replaced asset.
    pub original_offset: usize,
    /// Final ROM offset where the new asset was written (equals `original_offset` if in-place).
    pub written_offset: usize,
    /// GBA Wait State 0 bus address (`0x0800_0000 + written_offset`).
    pub written_bus_address: u32,
    /// Compression codec used to encode the written asset.
    pub compression: GfxCompression,
    /// Measured or configured capacity of the original slot in bytes.
    pub original_capacity: usize,
    /// Uncompressed payload size in bytes.
    pub uncompressed_bytes: usize,
    /// Final written payload size in bytes (after compression).
    pub written_bytes: usize,
    /// Number of slack bytes padded with `pad_byte` (0 if relocated or exact fit).
    pub slack_bytes: usize,
    /// Whether the asset was relocated to free space.
    pub relocated: bool,
    /// Number of 4-byte-aligned ROM pointers rewritten to the relocated address.
    pub patched_pointers: usize,
    /// ROM offsets of all rewritten 32-bit ROM pointers.
    pub patched_pointer_offsets: Vec<usize>,
}

/// Write a raw (uncompressed) graphics or palette payload back into `rom` at `rom_offset` with
/// codec auto-detection, exact consumed-stream capacity checking, and optional free-space
/// relocation + pointer-table patching.
pub fn write_asset_in_rom(
    rom: &mut [u8],
    rom_offset: usize,
    raw_payload: &[u8],
    options: &ReplaceOptions,
) -> Result<ReplaceReport> {
    if rom_offset >= rom.len() {
        return Err(Error::Truncated {
            offset: rom_offset,
            needed: 1,
            available: 0,
        });
    }

    let (target_compression, original_capacity) =
        resolve_codec_and_capacity(rom, rom_offset, options)?;

    if rom_offset
        .checked_add(original_capacity)
        .is_none_or(|end| end > rom.len())
    {
        return Err(Error::Truncated {
            offset: rom_offset,
            needed: original_capacity,
            available: rom.len().saturating_sub(rom_offset),
        });
    }

    let orig_bus_address = rom_offset_to_bus(rom_offset).ok_or(Error::InvalidField {
        what: "written_rom_offset",
        value: rom_offset as u64,
    })?;

    let encoded = compress_gfx(raw_payload, target_compression)?;
    let written_bytes = encoded.len();

    if written_bytes <= original_capacity {
        rom[rom_offset..rom_offset + written_bytes].copy_from_slice(&encoded);
        let slack_bytes = original_capacity - written_bytes;
        if slack_bytes > 0 {
            rom[rom_offset + written_bytes..rom_offset + original_capacity].fill(options.pad_byte);
        }
        return Ok(ReplaceReport {
            original_offset: rom_offset,
            written_offset: rom_offset,
            written_bus_address: orig_bus_address,
            compression: target_compression,
            original_capacity,
            uncompressed_bytes: raw_payload.len(),
            written_bytes,
            slack_bytes,
            relocated: false,
            patched_pointers: 0,
            patched_pointer_offsets: Vec::new(),
        });
    }

    if !options.allow_relocate {
        return Err(Error::ResourceLimit {
            resource: "rom_asset_capacity",
            requested: written_bytes as u64,
            limit: original_capacity as u64,
        });
    }

    let new_offset = find_free_space_prefer_ff(
        rom,
        written_bytes,
        options.relocate_alignment,
        options.free_space_start,
        Some((rom_offset, rom_offset + original_capacity)),
    )
    .ok_or(Error::ResourceLimit {
        resource: "rom_free_space",
        requested: written_bytes as u64,
        limit: 0,
    })?;

    let written_bus_address = rom_offset_to_bus(new_offset).ok_or(Error::InvalidField {
        what: "relocated_rom_offset",
        value: new_offset as u64,
    })?;

    rom[new_offset..new_offset + written_bytes].copy_from_slice(&encoded);
    let patched_pointer_offsets = patch_rom_pointers(
        rom,
        rom_offset,
        new_offset,
        Some((new_offset, new_offset + written_bytes)),
    )?;

    Ok(ReplaceReport {
        original_offset: rom_offset,
        written_offset: new_offset,
        written_bus_address,
        compression: target_compression,
        original_capacity,
        uncompressed_bytes: raw_payload.len(),
        written_bytes,
        slack_bytes: 0,
        relocated: true,
        patched_pointers: patched_pointer_offsets.len(),
        patched_pointer_offsets,
    })
}

fn resolve_codec_and_capacity(
    rom: &[u8],
    rom_offset: usize,
    options: &ReplaceOptions,
) -> Result<(GfxCompression, usize)> {
    let detected = match (options.compression, options.original_capacity) {
        (Some(GfxCompression::Raw), Some(cap)) => return Ok((GfxCompression::Raw, cap)),
        (Some(GfxCompression::Raw), None) => {
            return Err(Error::Malformed(
                "raw asset replacement requires explicit original_capacity".into(),
            ));
        }
        (Some(codec), Some(cap)) => return Ok((codec, cap)),
        _ => detect_stream_at(rom, rom_offset, DEFAULT_MAX_OUTPUT)?,
    };

    match (options.compression, options.original_capacity, detected) {
        (Some(codec), Some(cap), _) => Ok((codec, cap)),
        (Some(codec), None, Some(det)) => Ok((codec, det.consumed_bytes)),
        (Some(_), None, None) => Err(Error::Malformed(
            "could not auto-measure original stream capacity at rom_offset; specify original_capacity"
                .into(),
        )),
        (None, Some(cap), Some(det)) => Ok((det.compression, cap)),
        (None, None, Some(det)) => Ok((det.compression, det.consumed_bytes)),
        (None, Some(cap), None) => Ok((GfxCompression::Raw, cap)),
        (None, None, None) => Err(Error::Malformed(
            "no BIOS compressed stream detected at rom_offset; specify compression and original_capacity"
                .into(),
        )),
    }
}

/// Replace a tileset in `rom` at `rom_offset` from an RGBA8 [`Image`].
///
/// Quantizes or palette-matches `image`, packs GBA 8×8 linear `4bpp`/`8bpp` tiles, recompresses
/// with the target or auto-detected codec, and writes back in-place or via free-space relocation.
pub fn replace_tileset_in_rom(
    rom: &mut [u8],
    rom_offset: usize,
    image: &Image,
    bpp: TileBpp,
    palette_mode: PaletteMode<'_>,
    options: &ReplaceOptions,
) -> Result<(ReplaceReport, EncodedTileset)> {
    let encoded_tileset =
        encode_image_to_tiles(image, bpp, palette_mode, options.transparent_index0)?;
    let report = write_asset_in_rom(rom, rom_offset, &encoded_tileset.tile_bytes, options)?;
    Ok((report, encoded_tileset))
}

/// Decode a PNG or binary PPM (`P6`) image from `image_bytes` and replace a tileset in `rom`
/// at `rom_offset`.
pub fn replace_tileset_bytes_in_rom(
    rom: &mut [u8],
    rom_offset: usize,
    image_bytes: &[u8],
    bpp: TileBpp,
    palette_mode: PaletteMode<'_>,
    options: &ReplaceOptions,
) -> Result<(ReplaceReport, EncodedTileset)> {
    let image = decode_image_rgba(image_bytes)?;
    replace_tileset_in_rom(rom, rom_offset, &image, bpp, palette_mode, options)
}

/// Replace a GBA `BGR555` palette in `rom` at `rom_offset` from an RGBA8 palette slice.
///
/// Supports both uncompressed raw palettes (16-color / 32B or 256-color / 512B, or any 1..=256
/// color slice) and BIOS-compressed palettes.
pub fn replace_palette_in_rom(
    rom: &mut [u8],
    rom_offset: usize,
    palette_rgba: &[[u8; 4]],
    options: &ReplaceOptions,
) -> Result<ReplaceReport> {
    if palette_rgba.is_empty() || palette_rgba.len() > 256 {
        return Err(Error::InvalidField {
            what: "palette_color_count",
            value: palette_rgba.len() as u64,
        });
    }
    let bgr555 = encode_palette_bgr555(palette_rgba);

    let mut effective = *options;
    if effective.compression.is_none() {
        let detected_pal = detect_stream_at(rom, rom_offset, 512)?.filter(|d| {
            d.decompressed_bytes >= 2
                && d.decompressed_bytes <= 512
                && d.decompressed_bytes.is_multiple_of(2)
        });
        if let Some(det) = detected_pal {
            effective.compression = Some(det.compression);
            if effective.original_capacity.is_none() {
                effective.original_capacity = Some(det.consumed_bytes);
            }
        } else {
            effective.compression = Some(GfxCompression::Raw);
            if effective.original_capacity.is_none() {
                effective.original_capacity = Some(bgr555.len());
            }
        }
    } else if effective.compression == Some(GfxCompression::Raw)
        && effective.original_capacity.is_none()
    {
        effective.original_capacity = Some(bgr555.len());
    }

    write_asset_in_rom(rom, rom_offset, &bgr555, &effective)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_tile_4bpp, decode_tile_8bpp, RawPngEncoder};
    use formatkit_codec::{encode_rgba_png_with, PngCompression, PngEncodeOptions};

    fn make_ppm_p6(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
        let mut out = format!("P6\n# synthetic test ppm\n{width} {height}\n255\n").into_bytes();
        out.extend_from_slice(rgb);
        out
    }

    fn make_test_image_16x8() -> Image {
        // 2x1 tiles (16x8) using 5 exact BGR555-representable colors + transparent pixels.
        let colors = [
            [0, 0, 0, 0],       // transparent -> index 0
            [255, 0, 0, 255],   // red
            [0, 255, 0, 255],   // green
            [0, 0, 255, 255],   // blue
            [255, 255, 0, 255], // yellow
        ];
        let mut rgba = Vec::with_capacity(16 * 8 * 4);
        for y in 0..8usize {
            for x in 0..16usize {
                let c = colors[(x + y) % colors.len()];
                rgba.extend_from_slice(&c);
            }
        }
        Image {
            width: 16,
            height: 8,
            rgba,
        }
    }

    fn make_synthetic_rom(size: usize) -> Vec<u8> {
        // Non-free-space background (0xAA) so free-space regions (0xFF/0x00) are explicit.
        let mut rom = vec![0xAAu8; size];
        rom[0xB2] = gba_rom::FIXED_VALUE;
        rom
    }

    #[test]
    fn ppm_and_png_round_trip_4bpp_and_8bpp() {
        let img = make_test_image_16x8();

        // 1) Deflated PNG -> decode_image_rgba -> 4bpp tiles + generated BGR555 palette
        let png_deflated = encode_rgba_png_with(
            &img,
            usize::MAX,
            &PngEncodeOptions {
                compression: PngCompression::Deflate { level: 6 },
                ..PngEncodeOptions::default()
            },
        )
        .unwrap();
        let decoded_png = decode_image_rgba(&png_deflated).unwrap();
        assert_eq!(decoded_png, img);

        let enc4 = encode_image_to_tiles(&decoded_png, TileBpp::Bpp4, PaletteMode::Generate, true)
            .unwrap();
        assert_eq!(enc4.tile_count, 2);
        assert_eq!(enc4.tile_bytes.len(), 64);
        assert_eq!(enc4.palette_rgba.len(), 16);
        assert_eq!(enc4.palette_bgr555.len(), 32);

        // Decode tiles + palette back to RGBA and verify exact pixel round-trip.
        let decoded_pal = decode_palette(&enc4.palette_bgr555).unwrap();
        for tile_idx in 0..2usize {
            let px_indices = decode_tile_4bpp(&enc4.tile_bytes[tile_idx * 32..(tile_idx + 1) * 32])
                .unwrap();
            for py in 0..8usize {
                for px in 0..8usize {
                    let idx = px_indices[py * 8 + px] as usize;
                    let src_off = (py * 16 + tile_idx * 8 + px) * 4;
                    let expected = &img.rgba[src_off..src_off + 4];
                    if expected[3] == 0 {
                        assert_eq!(idx, 0);
                    } else {
                        assert_ne!(idx, 0);
                        assert_eq!(&decoded_pal[idx], expected);
                    }
                }
            }
        }

        // 2) Stored PNG -> 8bpp tiles with existing BGR555 palette
        let png_stored = RawPngEncoder::encode_rgba8(img.width, img.height, &img.rgba).unwrap();
        let decoded_stored = decode_image_rgba(&png_stored).unwrap();
        let enc8 = encode_image_to_tiles(
            &decoded_stored,
            TileBpp::Bpp8,
            PaletteMode::ExistingBgr555(&enc4.palette_bgr555),
            true,
        )
        .unwrap();
        assert_eq!(enc8.tile_count, 2);
        assert_eq!(enc8.tile_bytes.len(), 128);
        for tile_idx in 0..2usize {
            let t4 =
                decode_tile_4bpp(&enc4.tile_bytes[tile_idx * 32..(tile_idx + 1) * 32]).unwrap();
            let t8 =
                decode_tile_8bpp(&enc8.tile_bytes[tile_idx * 64..(tile_idx + 1) * 64]).unwrap();
            assert_eq!(t4, t8);
        }

        // 3) Binary PPM P6 -> decode_image_rgba -> 4bpp opaque round-trip
        let mut rgb = Vec::with_capacity(8 * 8 * 3);
        for y in 0..8usize {
            for x in 0..8usize {
                if (x + y) % 2 == 0 {
                    rgb.extend_from_slice(&[255, 0, 0]);
                } else {
                    rgb.extend_from_slice(&[0, 0, 255]);
                }
            }
        }
        let ppm_bytes = make_ppm_p6(8, 8, &rgb);
        let decoded_ppm = decode_image_rgba(&ppm_bytes).unwrap();
        assert_eq!(decoded_ppm.width, 8);
        assert_eq!(decoded_ppm.height, 8);
        let enc_ppm =
            encode_image_to_tiles(&decoded_ppm, TileBpp::Bpp4, PaletteMode::Generate, false)
                .unwrap();
        let ppm_pal = decode_palette(&enc_ppm.palette_bgr555).unwrap();
        let t0 = decode_tile_4bpp(&enc_ppm.tile_bytes).unwrap();
        for i in 0..64usize {
            let actual = ppm_pal[t0[i] as usize];
            assert_eq!(&actual[..3], &rgb[i * 3..i * 3 + 3]);
        }
    }

    #[test]
    fn inplace_lz77_rle_raw_replacement_and_overflow_rejection() {
        let mut rom = make_synthetic_rom(2048);

        // Build an original high-entropy 4bpp tileset (128 bytes) compressed with LZ77 at 0x200.
        let orig_raw: Vec<u8> = (0..128u8)
            .map(|i| i.wrapping_mul(37).wrapping_add(13))
            .collect();
        let orig_lz = encode_lz77_vram(&orig_raw).unwrap();
        let lz_off = 0x200usize;
        rom[lz_off..lz_off + orig_lz.len()].copy_from_slice(&orig_lz);

        // Replace with a highly compressible solid-color 16x16 (4 tiles = 128B raw) image.
        let solid_img = Image {
            width: 16,
            height: 16,
            rgba: [255u8, 0, 0, 255].repeat(16 * 16),
        };
        let opts = ReplaceOptions {
            pad_byte: 0x00,
            allow_relocate: false,
            ..ReplaceOptions::default()
        };
        let (report, enc) = replace_tileset_in_rom(
            &mut rom,
            lz_off,
            &solid_img,
            TileBpp::Bpp4,
            PaletteMode::Generate,
            &opts,
        )
        .unwrap();

        assert!(!report.relocated);
        assert_eq!(report.written_offset, lz_off);
        assert_eq!(report.compression, GfxCompression::Lz77Vram);
        assert_eq!(report.original_capacity, orig_lz.len());
        assert_eq!(report.uncompressed_bytes, 128);
        assert!(report.written_bytes < report.original_capacity);
        assert_eq!(
            report.slack_bytes,
            report.original_capacity - report.written_bytes
        );
        // Verify slack bytes were padded with 0x00.
        assert!(rom[lz_off + report.written_bytes..lz_off + orig_lz.len()]
            .iter()
            .all(|&b| b == 0x00));
        // Verify ROM stream decodes back to exact encoded tile bytes.
        let dec = decode_lz77(&rom[lz_off..], DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec.bytes, enc.tile_bytes);

        // Now try to replace the tiny compressed slot with a much larger noisy image with allow_relocate = false -> must fail!
        let mut noisy_rgba = Vec::with_capacity(32 * 32 * 4);
        for i in 0..(32 * 32usize) {
            noisy_rgba.extend_from_slice(&[
                ((i * 53) & 0xF8) as u8,
                ((i * 97) & 0xF8) as u8,
                ((i * 193) & 0xF8) as u8,
                255,
            ]);
        }
        let noisy_img = Image {
            width: 32,
            height: 32,
            rgba: noisy_rgba,
        };
        let err = replace_tileset_in_rom(
            &mut rom,
            lz_off,
            &noisy_img,
            TileBpp::Bpp4,
            PaletteMode::Generate,
            &opts,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            Error::ResourceLimit {
                resource: "rom_asset_capacity",
                ..
            }
        ));

        // RLE in-place replacement at 0x400.
        let rle_off = 0x400usize;
        let orig_rle_raw: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(11)).collect();
        let orig_rle = encode_rle(&orig_rle_raw).unwrap();
        rom[rle_off..rle_off + orig_rle.len()].copy_from_slice(&orig_rle);

        let rle_img = Image {
            width: 8,
            height: 8,
            rgba: [0u8, 255, 0, 255].repeat(64),
        };
        let (rle_rep, rle_enc) = replace_tileset_in_rom(
            &mut rom,
            rle_off,
            &rle_img,
            TileBpp::Bpp4,
            PaletteMode::Generate,
            &ReplaceOptions {
                pad_byte: 0xFF,
                ..ReplaceOptions::default()
            },
        )
        .unwrap();
        assert_eq!(rle_rep.compression, GfxCompression::Rle);
        assert!(!rle_rep.relocated);
        let dec_rle = decode_rle(&rom[rle_off..], DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec_rle.bytes, rle_enc.tile_bytes);

        // Raw in-place replacement at 0x600 with explicit capacity.
        let raw_off = 0x600usize;
        let (raw_rep, raw_enc) = replace_tileset_in_rom(
            &mut rom,
            raw_off,
            &rle_img,
            TileBpp::Bpp4,
            PaletteMode::Generate,
            &ReplaceOptions {
                compression: Some(GfxCompression::Raw),
                original_capacity: Some(64),
                pad_byte: 0x00,
                ..ReplaceOptions::default()
            },
        )
        .unwrap();
        assert_eq!(raw_rep.compression, GfxCompression::Raw);
        assert_eq!(raw_rep.written_bytes, 32);
        assert_eq!(raw_rep.slack_bytes, 32);
        assert_eq!(&rom[raw_off..raw_off + 32], raw_enc.tile_bytes.as_slice());
        assert!(rom[raw_off + 32..raw_off + 64].iter().all(|&b| b == 0x00));
    }

    #[test]
    fn relocation_and_rom_pointer_table_patching() {
        let mut rom = make_synthetic_rom(4096);

        // Place a tiny original LZ77 stream at 0x200.
        let orig_off = 0x200usize;
        let tiny_orig = encode_lz77_vram(&[0u8; 32]).unwrap();
        rom[orig_off..orig_off + tiny_orig.len()].copy_from_slice(&tiny_orig);

        // Write three 4-byte-aligned ROM pointers to 0x0800_0200 in pointer tables.
        let old_bus = rom_offset_to_bus(orig_off).unwrap();
        let ptr_locs = [0x100usize, 0x108, 0x140];
        for &loc in &ptr_locs {
            rom[loc..loc + 4].copy_from_slice(&old_bus.to_le_bytes());
        }

        // Carve a 0xFF free-space block at 0x800..0xF00.
        rom[0x800..0xF00].fill(0xFF);

        // Build a larger noisy 32x16 (8 tiles = 256B raw) PNG that overflows tiny_orig.
        let mut rgba = Vec::with_capacity(32 * 16 * 4);
        for i in 0..(32 * 16usize) {
            rgba.extend_from_slice(&[
                ((i * 29) & 0xF8) as u8,
                ((i * 71) & 0xF8) as u8,
                ((i * 113) & 0xF8) as u8,
                255,
            ]);
        }
        let png = RawPngEncoder::encode_rgba8(32, 16, &rgba).unwrap();

        let opts = ReplaceOptions {
            allow_relocate: true,
            relocate_alignment: 4,
            ..ReplaceOptions::default()
        };
        let (report, enc) = replace_tileset_bytes_in_rom(
            &mut rom,
            orig_off,
            &png,
            TileBpp::Bpp4,
            PaletteMode::Generate,
            &opts,
        )
        .unwrap();

        assert!(report.relocated);
        assert_eq!(report.original_offset, orig_off);
        assert_eq!(report.written_offset, 0x800);
        assert_eq!(report.written_bus_address, 0x0800_0800);
        assert_eq!(report.patched_pointers, 3);
        assert_eq!(report.patched_pointer_offsets, ptr_locs.to_vec());

        // Verify all 3 pointer slots now point to 0x0800_0800.
        for &loc in &ptr_locs {
            let ptr = u32::from_le_bytes(rom[loc..loc + 4].try_into().unwrap());
            assert_eq!(ptr, 0x0800_0800);
        }

        // Verify relocated compressed stream decodes back to exact tile bytes.
        let dec = decode_lz77(&rom[report.written_offset..], DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec.bytes, enc.tile_bytes);
    }

    #[test]
    fn palette_replacement_in_rom_raw_and_compressed() {
        let mut rom = make_synthetic_rom(2048);

        let new_pal: Vec<[u8; 4]> = (0..16u8)
            .map(|i| [i.saturating_mul(16), 255 - i.saturating_mul(16), 128, 255])
            .collect();

        // 1) Raw 16-color palette at 0x300
        let raw_pal_off = 0x300usize;
        let report = replace_palette_in_rom(
            &mut rom,
            raw_pal_off,
            &new_pal,
            &ReplaceOptions::default(),
        )
        .unwrap();
        assert!(!report.relocated);
        assert_eq!(report.compression, GfxCompression::Raw);
        assert_eq!(report.written_bytes, 32);
        let read_back = decode_palette(&rom[raw_pal_off..raw_pal_off + 32]).unwrap();
        for (expected, actual) in new_pal.iter().zip(read_back.iter()) {
            assert_eq!(
                *actual,
                bgr555_to_rgba8(rgb8_to_bgr555(expected[0], expected[1], expected[2]))
            );
        }

        // 2) Huffman-compressed palette at 0x500
        let huff_pal_off = 0x500usize;
        let orig_pal_bytes = encode_palette_bgr555(&new_pal);
        let orig_huff = encode_huffman_8bit(&orig_pal_bytes).unwrap();
        rom[huff_pal_off..huff_pal_off + orig_huff.len()].copy_from_slice(&orig_huff);

        let solid_pal = vec![[255, 0, 0, 255]; 16];
        let huff_report = replace_palette_in_rom(
            &mut rom,
            huff_pal_off,
            &solid_pal,
            &ReplaceOptions::default(),
        )
        .unwrap();
        assert!(!huff_report.relocated);
        assert_eq!(huff_report.compression, GfxCompression::Huffman8);
        let dec_huff = decode_huffman(&rom[huff_pal_off..], 512).unwrap();
        assert_eq!(dec_huff.bytes, encode_palette_bgr555(&solid_pal));
    }
}
