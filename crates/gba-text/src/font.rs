//! 1bpp and 2bpp bitmap / VWF font sheet rip (PNG/PPM) and in-place ROM replacement.

#![deny(unsafe_code)]

use formatkit_core::{Error, Image, Result};
use gba_gfx::{decode_glyph_1bpp, decode_glyph_2bpp, decode_image_rgba, RawPngEncoder};
use serde::{Deserialize, Serialize};

/// Bit depth of a GBA bitmap/VWF font glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontBpp {
    /// 1 bit per pixel (2 shades: background, ink).
    Bpp1,
    /// 2 bits per pixel (4 shades: background, dark gray, light gray, white).
    Bpp2,
}

impl FontBpp {
    /// Numeric bit depth (`1` or `2`).
    #[must_use]
    pub const fn bits(self) -> u8 {
        match self {
            Self::Bpp1 => 1,
            Self::Bpp2 => 2,
        }
    }

    /// Number of pixels packed per byte.
    #[must_use]
    pub const fn pixels_per_byte(self) -> usize {
        match self {
            Self::Bpp1 => 8,
            Self::Bpp2 => 4,
        }
    }
}

/// Output image format for [`dump_font_sheet`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontImageFormat {
    /// 32-bit RGBA PNG.
    Png,
    /// Binary PPM (`P6`).
    Ppm,
}

/// Geometry and packing configuration for a GBA bitmap font sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FontSheetConfig {
    /// Number of glyphs to dump or replace.
    pub glyph_count: usize,
    /// Width of each glyph in pixels.
    pub glyph_width: usize,
    /// Height of each glyph in pixels.
    pub glyph_height: usize,
    /// Bit depth (`1` or `2`).
    pub bpp: FontBpp,
    /// `true` for MSB-first bit packing, `false` for LSB-first (`--lsb`).
    pub msb_first: bool,
    /// Number of glyph columns in the rendered sheet grid.
    pub cols: usize,
}

impl FontSheetConfig {
    /// Validate configuration fields and return the number of bytes per glyph.
    pub fn bytes_per_glyph(&self) -> Result<usize> {
        if self.glyph_count == 0 {
            return Err(Error::InvalidField {
                what: "font_glyph_count",
                value: 0,
            });
        }
        if self.glyph_width == 0 || self.glyph_height == 0 {
            return Err(Error::InvalidField {
                what: "font_glyph_dimensions",
                value: 0,
            });
        }
        if self.cols == 0 {
            return Err(Error::InvalidField {
                what: "font_sheet_cols",
                value: 0,
            });
        }
        let row_bytes = self.glyph_width.div_ceil(self.bpp.pixels_per_byte());
        row_bytes
            .checked_mul(self.glyph_height)
            .ok_or(Error::InvalidField {
                what: "font_glyph_byte_size",
                value: self.glyph_width as u64,
            })
    }

    /// Total byte size of all glyphs in ROM.
    pub fn total_rom_bytes(&self) -> Result<usize> {
        let per_glyph = self.bytes_per_glyph()?;
        per_glyph
            .checked_mul(self.glyph_count)
            .ok_or(Error::InvalidField {
                what: "font_total_rom_bytes",
                value: self.glyph_count as u64,
            })
    }

    /// Rendered sheet dimensions `(width_px, height_px)`.
    pub fn sheet_dimensions(&self) -> Result<(u32, u32)> {
        let _ = self.bytes_per_glyph()?;
        let cols = self.cols.min(self.glyph_count).max(1);
        let rows = self.glyph_count.div_ceil(cols);
        let width = cols
            .checked_mul(self.glyph_width)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or(Error::InvalidField {
                what: "font_sheet_width",
                value: cols as u64,
            })?;
        let height = rows
            .checked_mul(self.glyph_height)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or(Error::InvalidField {
                what: "font_sheet_height",
                value: rows as u64,
            })?;
        Ok((width, height))
    }
}

/// 1bpp RGBA palette: background black, foreground white.
const PALETTE_1BPP: [[u8; 4]; 2] = [[0, 0, 0, 255], [255, 255, 255, 255]];

/// 2bpp RGBA palette: 4 distinct shades (black, dark gray, light gray, white) for easy editing.
const PALETTE_2BPP: [[u8; 4]; 4] = [
    [0, 0, 0, 255],
    [85, 85, 85, 255],
    [170, 170, 170, 255],
    [255, 255, 255, 255],
];

/// Encode row-major 1bpp pixel indices (`0` or `1`) into byte-aligned glyph rows.
///
/// Exact inverse of [`gba_gfx::decode_glyph_1bpp`].
pub fn encode_glyph_1bpp(
    pixels: &[u8],
    width: usize,
    height: usize,
    msb_first: bool,
) -> Result<Vec<u8>> {
    let expected = width.checked_mul(height).ok_or(Error::InvalidField {
        what: "glyph_1bpp_pixels",
        value: width as u64,
    })?;
    if pixels.len() != expected {
        return Err(Error::InvalidField {
            what: "glyph_1bpp_pixels_len",
            value: pixels.len() as u64,
        });
    }
    let row_bytes = width.div_ceil(8);
    let mut out = vec![0u8; row_bytes * height];

    for y in 0..height {
        let row_out = &mut out[y * row_bytes..(y + 1) * row_bytes];
        for x in 0..width {
            let v = pixels[y * width + x];
            if v > 1 {
                return Err(Error::InvalidField {
                    what: "glyph_1bpp_pixel_value",
                    value: u64::from(v),
                });
            }
            if v != 0 {
                let byte_idx = x / 8;
                let bit = if msb_first { 7 - (x % 8) } else { x % 8 };
                row_out[byte_idx] |= 1u8 << bit;
            }
        }
    }

    Ok(out)
}

/// Encode row-major 2bpp pixel indices (`0..=3`) into byte-aligned glyph rows.
///
/// Exact inverse of [`gba_gfx::decode_glyph_2bpp`].
pub fn encode_glyph_2bpp(
    pixels: &[u8],
    width: usize,
    height: usize,
    msb_first: bool,
) -> Result<Vec<u8>> {
    let expected = width.checked_mul(height).ok_or(Error::InvalidField {
        what: "glyph_2bpp_pixels",
        value: width as u64,
    })?;
    if pixels.len() != expected {
        return Err(Error::InvalidField {
            what: "glyph_2bpp_pixels_len",
            value: pixels.len() as u64,
        });
    }
    let row_bytes = width.div_ceil(4);
    let mut out = vec![0u8; row_bytes * height];

    for y in 0..height {
        let row_out = &mut out[y * row_bytes..(y + 1) * row_bytes];
        for x in 0..width {
            let v = pixels[y * width + x];
            if v > 3 {
                return Err(Error::InvalidField {
                    what: "glyph_2bpp_pixel_value",
                    value: u64::from(v),
                });
            }
            let byte_idx = x / 4;
            let shift = if msb_first {
                6 - (x % 4) * 2
            } else {
                (x % 4) * 2
            };
            row_out[byte_idx] |= v << shift;
        }
    }

    Ok(out)
}

/// Render a 1bpp or 2bpp font sheet from `rom` at `offset` into an RGBA8 [`Image`].
pub fn dump_font_sheet_rgba(
    rom: &[u8],
    offset: usize,
    config: &FontSheetConfig,
) -> Result<Image> {
    let bytes_per_glyph = config.bytes_per_glyph()?;
    let total_bytes = config.total_rom_bytes()?;
    if offset
        .checked_add(total_bytes)
        .is_none_or(|end| end > rom.len())
    {
        return Err(Error::Truncated {
            offset,
            needed: total_bytes,
            available: rom.len().saturating_sub(offset),
        });
    }

    let (sheet_w, sheet_h) = config.sheet_dimensions()?;
    let cols = config.cols.min(config.glyph_count).max(1);
    let mut rgba = vec![0u8; (sheet_w as usize) * (sheet_h as usize) * 4];
    // Fill background with opaque black so unused trailing grid cells are clean.
    for px in rgba.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }

    for glyph_idx in 0..config.glyph_count {
        let g_start = offset + glyph_idx * bytes_per_glyph;
        let g_slice = &rom[g_start..g_start + bytes_per_glyph];
        let pixels = match config.bpp {
            FontBpp::Bpp1 => decode_glyph_1bpp(
                g_slice,
                config.glyph_width,
                config.glyph_height,
                config.msb_first,
            )?,
            FontBpp::Bpp2 => decode_glyph_2bpp(
                g_slice,
                config.glyph_width,
                config.glyph_height,
                config.msb_first,
            )?,
        };

        let col = glyph_idx % cols;
        let row = glyph_idx / cols;
        let base_x = col * config.glyph_width;
        let base_y = row * config.glyph_height;

        for py in 0..config.glyph_height {
            for px in 0..config.glyph_width {
                let val = pixels[py * config.glyph_width + px] as usize;
                let color = match config.bpp {
                    FontBpp::Bpp1 => PALETTE_1BPP[val.min(1)],
                    FontBpp::Bpp2 => PALETTE_2BPP[val.min(3)],
                };
                let dst = ((base_y + py) * (sheet_w as usize) + (base_x + px)) * 4;
                rgba[dst..dst + 4].copy_from_slice(&color);
            }
        }
    }

    Ok(Image {
        width: sheet_w,
        height: sheet_h,
        rgba,
    })
}

/// Render a 1bpp or 2bpp font sheet from `rom` at `offset` as PNG or binary PPM (`P6`) bytes.
pub fn dump_font_sheet(
    rom: &[u8],
    offset: usize,
    config: &FontSheetConfig,
    format: FontImageFormat,
) -> Result<Vec<u8>> {
    let img = dump_font_sheet_rgba(rom, offset, config)?;
    match format {
        FontImageFormat::Png => RawPngEncoder::encode_rgba8(img.width, img.height, &img.rgba),
        FontImageFormat::Ppm => {
            let mut out = format!("P6\n{} {}\n255\n", img.width, img.height).into_bytes();
            out.reserve((img.width as usize) * (img.height as usize) * 3);
            for px in img.rgba.as_chunks::<4>().0 {
                out.extend_from_slice(&px[..3]);
            }
            Ok(out)
        }
    }
}

/// Structured report returned by [`replace_font_sheet_in_rom`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FontReplaceReport {
    /// ROM file offset where the font glyphs were written.
    pub offset: usize,
    /// Number of glyphs replaced.
    pub glyph_count: usize,
    /// Width of each glyph in pixels.
    pub glyph_width: usize,
    /// Height of each glyph in pixels.
    pub glyph_height: usize,
    /// Bit depth (`1` or `2`).
    pub bpp: u8,
    /// `true` if MSB-first, `false` if LSB-first.
    pub msb_first: bool,
    /// Grid column count used to slice the sheet.
    pub cols: usize,
    /// Total ROM bytes written.
    pub bytes_written: usize,
    /// Number of ROM bytes that actually changed compared to the original ROM.
    pub bytes_changed: usize,
}

/// Decode an edited PNG or PPM (`P6`) font sheet and write its 1bpp/2bpp glyphs in-place into `rom`.
pub fn replace_font_sheet_in_rom(
    rom: &mut [u8],
    offset: usize,
    image_bytes: &[u8],
    config: &FontSheetConfig,
) -> Result<FontReplaceReport> {
    let bytes_per_glyph = config.bytes_per_glyph()?;
    let total_bytes = config.total_rom_bytes()?;
    if offset
        .checked_add(total_bytes)
        .is_none_or(|end| end > rom.len())
    {
        return Err(Error::Truncated {
            offset,
            needed: total_bytes,
            available: rom.len().saturating_sub(offset),
        });
    }

    let (expected_w, expected_h) = config.sheet_dimensions()?;
    let img = decode_image_rgba(image_bytes)?;
    if img.width != expected_w || img.height != expected_h {
        return Err(Error::Malformed(format!(
            "font sheet image dimensions {}x{} do not match expected {}x{} ({} glyphs of {}x{}, {} cols)",
            img.width,
            img.height,
            expected_w,
            expected_h,
            config.glyph_count,
            config.glyph_width,
            config.glyph_height,
            config.cols.min(config.glyph_count).max(1),
        )));
    }

    let cols = config.cols.min(config.glyph_count).max(1);
    let mut encoded_all = Vec::with_capacity(total_bytes);
    let mut glyph_px = vec![0u8; config.glyph_width * config.glyph_height];

    for glyph_idx in 0..config.glyph_count {
        let col = glyph_idx % cols;
        let row = glyph_idx / cols;
        let base_x = col * config.glyph_width;
        let base_y = row * config.glyph_height;

        for py in 0..config.glyph_height {
            for px in 0..config.glyph_width {
                let src = ((base_y + py) * (img.width as usize) + (base_x + px)) * 4;
                let r = img.rgba[src];
                let g = img.rgba[src + 1];
                let b = img.rgba[src + 2];
                let a = img.rgba[src + 3];
                glyph_px[py * config.glyph_width + px] = rgba_to_glyph_index(r, g, b, a, config.bpp);
            }
        }

        let glyph_bytes = match config.bpp {
            FontBpp::Bpp1 => encode_glyph_1bpp(
                &glyph_px,
                config.glyph_width,
                config.glyph_height,
                config.msb_first,
            )?,
            FontBpp::Bpp2 => encode_glyph_2bpp(
                &glyph_px,
                config.glyph_width,
                config.glyph_height,
                config.msb_first,
            )?,
        };
        debug_assert_eq!(glyph_bytes.len(), bytes_per_glyph);
        encoded_all.extend_from_slice(&glyph_bytes);
    }

    let mut bytes_changed = 0usize;
    for (dst, &src) in rom[offset..offset + total_bytes]
        .iter_mut()
        .zip(encoded_all.iter())
    {
        if *dst != src {
            *dst = src;
            bytes_changed += 1;
        }
    }

    Ok(FontReplaceReport {
        offset,
        glyph_count: config.glyph_count,
        glyph_width: config.glyph_width,
        glyph_height: config.glyph_height,
        bpp: config.bpp.bits(),
        msb_first: config.msb_first,
        cols,
        bytes_written: total_bytes,
        bytes_changed,
    })
}

#[inline]
fn rgba_to_glyph_index(r: u8, g: u8, b: u8, a: u8, bpp: FontBpp) -> u8 {
    if a < 128 {
        return 0;
    }
    let luma = (77u32 * u32::from(r) + 150u32 * u32::from(g) + 29u32 * u32::from(b)) >> 8;
    match bpp {
        FontBpp::Bpp1 => u8::from(luma >= 128),
        FontBpp::Bpp2 => {
            if luma < 43 {
                0
            } else if luma < 128 {
                1
            } else if luma < 213 {
                2
            } else {
                3
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_sheet_1bpp_png_and_ppm_roundtrip() {
        let mut rom = vec![0u8; 0x400];
        let font_off = 0x100usize;
        // 4 glyphs of 8x8 1bpp (8 bytes each = 32 bytes)
        let original_glyphs: [u8; 32] = [
            0x3C, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00, // 'A'
            0x7C, 0x66, 0x66, 0x7C, 0x66, 0x66, 0x7C, 0x00, // 'B'
            0x3C, 0x66, 0x60, 0x60, 0x60, 0x66, 0x3C, 0x00, // 'C'
            0x78, 0x6C, 0x66, 0x66, 0x66, 0x6C, 0x78, 0x00, // 'D'
        ];
        rom[font_off..font_off + 32].copy_from_slice(&original_glyphs);

        let cfg = FontSheetConfig {
            glyph_count: 4,
            glyph_width: 8,
            glyph_height: 8,
            bpp: FontBpp::Bpp1,
            msb_first: true,
            cols: 2,
        };

        // PNG round-trip
        let png = dump_font_sheet(&rom, font_off, &cfg, FontImageFormat::Png).unwrap();
        assert_eq!(&png[..8], &[137, 80, 78, 71, 13, 10, 26, 10]);

        let mut rom2 = vec![0xAAu8; 0x400];
        let report = replace_font_sheet_in_rom(&mut rom2, font_off, &png, &cfg).unwrap();
        assert_eq!(report.bytes_written, 32);
        assert_eq!(&rom2[font_off..font_off + 32], &original_glyphs);

        // PPM round-trip
        let ppm = dump_font_sheet(&rom, font_off, &cfg, FontImageFormat::Ppm).unwrap();
        assert!(ppm.starts_with(b"P6\n16 16\n255\n"));

        let mut rom3 = rom.clone();
        let report3 = replace_font_sheet_in_rom(&mut rom3, font_off, &ppm, &cfg).unwrap();
        assert_eq!(report3.bytes_changed, 0);
        assert_eq!(&rom3[font_off..font_off + 32], &original_glyphs);
    }

    #[test]
    fn font_sheet_2bpp_lsb_edit_roundtrip() {
        let mut rom = vec![0u8; 0x400];
        let font_off = 0x100usize;
        // 2 glyphs of 8x8 2bpp (16 bytes each = 32 bytes)
        let original_glyphs: Vec<u8> = (0..32u8).map(|i| i.wrapping_mul(37)).collect();
        rom[font_off..font_off + 32].copy_from_slice(&original_glyphs);

        let cfg = FontSheetConfig {
            glyph_count: 2,
            glyph_width: 8,
            glyph_height: 8,
            bpp: FontBpp::Bpp2,
            msb_first: false,
            cols: 2,
        };

        let mut img = dump_font_sheet_rgba(&rom, font_off, &cfg).unwrap();
        assert_eq!((img.width, img.height), (16, 8));

        // Edit pixel (0, 0) of glyph 0 to shade 3 (white) and pixel (1, 0) to shade 2 (light gray).
        img.rgba[0..4].copy_from_slice(&[255, 255, 255, 255]);
        img.rgba[4..8].copy_from_slice(&[170, 170, 170, 255]);
        let edited_png = RawPngEncoder::encode_rgba8(img.width, img.height, &img.rgba).unwrap();

        let report = replace_font_sheet_in_rom(&mut rom, font_off, &edited_png, &cfg).unwrap();
        assert_eq!(report.bytes_written, 32);
        assert!(report.bytes_changed >= 1);

        // Verify glyph 0 pixel (0,0) == 3, (1,0) == 2 in LSB-first 2bpp: bits 1..0 = 3, bits 3..2 = 2
        let decoded = decode_glyph_2bpp(&rom[font_off..font_off + 16], 8, 8, false).unwrap();
        assert_eq!(decoded[0], 3);
        assert_eq!(decoded[1], 2);
        // Glyph 1 untouched
        assert_eq!(&rom[font_off + 16..font_off + 32], &original_glyphs[16..32]);
    }
}
