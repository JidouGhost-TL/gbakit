//! 1bpp and 2bpp bitmap font glyph decoders backed by `formatkit_pixel`.

use formatkit_core::{Error, Result};
use formatkit_pixel::{unpack_glyph_1bpp, unpack_glyph_2bpp, BitOrder, PackError};

#[derive(Clone, Copy)]
enum GlyphBpp {
    Bpp1,
    Bpp2,
}

fn decode_glyph(
    bytes: &[u8],
    width: usize,
    height: usize,
    msb_first: bool,
    bpp: GlyphBpp,
) -> Result<Vec<u8>> {
    let (pixels_per_byte, what_len, what_pixels) = match bpp {
        GlyphBpp::Bpp1 => (8, "glyph_1bpp_len", "glyph_1bpp_pixels"),
        GlyphBpp::Bpp2 => (4, "glyph_2bpp_len", "glyph_2bpp_pixels"),
    };
    let row_bytes = width.div_ceil(pixels_per_byte);
    let needed = row_bytes.checked_mul(height).ok_or(Error::InvalidField {
        what: what_len,
        value: width as u64,
    })?;
    let pixel_count = width.checked_mul(height).ok_or(Error::InvalidField {
        what: what_pixels,
        value: width as u64,
    })?;
    if bytes.len() < needed {
        return Err(Error::Truncated {
            offset: 0,
            needed,
            available: bytes.len(),
        });
    }

    let order = if msb_first {
        BitOrder::MsbFirst
    } else {
        BitOrder::LsbFirst
    };
    let mut out = vec![0u8; pixel_count];
    let res = match bpp {
        GlyphBpp::Bpp1 => unpack_glyph_1bpp(bytes, width, height, order, &mut out),
        GlyphBpp::Bpp2 => unpack_glyph_2bpp(bytes, width, height, order, &mut out),
    };
    res.map_err(|err| match err {
        PackError::LengthMismatch { expected, actual } => Error::Truncated {
            offset: 0,
            needed: expected,
            available: actual,
        },
        _ => Error::InvalidField {
            what: what_len,
            value: width as u64,
        },
    })?;
    Ok(out)
}

/// Decode a 1bpp bitmap font glyph into row-major pixel values (`0` or `1`).
///
/// Each row occupies `width.div_ceil(8)` bytes (byte-aligned rows).
pub fn decode_glyph_1bpp(
    bytes: &[u8],
    width: usize,
    height: usize,
    msb_first: bool,
) -> Result<Vec<u8>> {
    decode_glyph(bytes, width, height, msb_first, GlyphBpp::Bpp1)
}

/// Decode a 2bpp bitmap font glyph into row-major pixel values (`0..=3`).
///
/// Each row occupies `width.div_ceil(4)` bytes (byte-aligned rows, 4 pixels per byte).
pub fn decode_glyph_2bpp(
    bytes: &[u8],
    width: usize,
    height: usize,
    msb_first: bool,
) -> Result<Vec<u8>> {
    decode_glyph(bytes, width, height, msb_first, GlyphBpp::Bpp2)
}
