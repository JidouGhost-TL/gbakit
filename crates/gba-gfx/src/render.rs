//! GBA background and bitmap mode framebuffers (text-mode 4bpp BG, Mode 3, Mode 4).

use formatkit_core::{Error, Image, Result};
use formatkit_pixel::{apply_palette, blit_rgba};

use crate::{bgr555_to_rgba8, decode_tile_4bpp, ScrEntry};

/// GBA LCD screen width in pixels.
pub const GBA_SCREEN_WIDTH: usize = 240;

/// GBA LCD screen height in pixels.
pub const GBA_SCREEN_HEIGHT: usize = 160;

/// GBA Mode 5 LCD screen width in pixels.
pub const GBA_MODE5_WIDTH: usize = 160;

/// GBA Mode 5 LCD screen height in pixels.
pub const GBA_MODE5_HEIGHT: usize = 128;

/// Compute the linear entry index in a GBA text-mode background Screen Block (SCB) array.
///
/// Handles hardware screen block paging for:
/// - 32x32: 1 SCB (1024 entries)
/// - 64x32: 2 horizontal SCBs (2048 entries)
/// - 32x64: 2 vertical SCBs (2048 entries)
/// - 64x64: 4 SCBs arranged in 2x2 grid (4096 entries)
///
/// For non-standard dimensions, falls back to standard row-major indexing (`ty * width_tiles + tx`).
pub fn screen_block_entry_index(
    tx: usize,
    ty: usize,
    width_tiles: usize,
    height_tiles: usize,
) -> usize {
    if width_tiles == 32 && height_tiles == 32 {
        ty * 32 + tx
    } else if width_tiles == 64 && height_tiles == 32 {
        let block = tx / 32;
        let lx = tx % 32;
        let ly = ty;
        block * 1024 + ly * 32 + lx
    } else if width_tiles == 32 && height_tiles == 64 {
        let block = ty / 32;
        let lx = tx;
        let ly = ty % 32;
        block * 1024 + ly * 32 + lx
    } else if width_tiles == 64 && height_tiles == 64 {
        let block = (ty / 32) * 2 + (tx / 32);
        let lx = tx % 32;
        let ly = ty % 32;
        block * 1024 + ly * 32 + lx
    } else {
        ty * width_tiles + tx
    }
}

/// Composite a GBA text-mode 4bpp background layer into an RGBA8 buffer.
///
/// Honors each [`ScrEntry`]'s `tile_index`, `h_flip`, `v_flip`, and `palette_bank`.
/// When `transparent_zero` is `true`, pixel index `0` within any tile is emitted as
/// `[0, 0, 0, 0]` without looking up the palette.
///
/// Returns `width_tiles * 8 * height_tiles * 8 * 4` RGBA8 bytes in row-major order.
pub fn render_bg_text_4bpp_rgba(
    tiles: &[u8],
    scr_map: &[u8],
    palette: &[[u8; 4]],
    width_tiles: usize,
    height_tiles: usize,
    transparent_zero: bool,
) -> Result<Vec<u8>> {
    let tile_count = width_tiles
        .checked_mul(height_tiles)
        .ok_or(Error::InvalidField {
            what: "bg_tile_count",
            value: width_tiles as u64,
        })?;
    let needed_scr = tile_count.checked_mul(2).ok_or(Error::InvalidField {
        what: "bg_scr_len",
        value: tile_count as u64,
    })?;
    if scr_map.len() < needed_scr {
        return Err(Error::Truncated {
            offset: 0,
            needed: needed_scr,
            available: scr_map.len(),
        });
    }

    let width_px = width_tiles.checked_mul(8).ok_or(Error::InvalidField {
        what: "bg_width_px",
        value: width_tiles as u64,
    })?;
    let height_px = height_tiles.checked_mul(8).ok_or(Error::InvalidField {
        what: "bg_height_px",
        value: height_tiles as u64,
    })?;
    let out_len = width_px
        .checked_mul(height_px)
        .and_then(|n| n.checked_mul(4))
        .ok_or(Error::InvalidField {
            what: "bg_rgba_len",
            value: width_px as u64,
        })?;

    let width_px_u32 = u32::try_from(width_px).map_err(|_| Error::InvalidField {
        what: "bg_width_px",
        value: width_px as u64,
    })?;
    let height_px_u32 = u32::try_from(height_px).map_err(|_| Error::InvalidField {
        what: "bg_height_px",
        value: height_px as u64,
    })?;

    let mut out = vec![0u8; out_len];
    let mut tile_rgba = Vec::with_capacity(64 * 4);

    for ty in 0..height_tiles {
        for tx in 0..width_tiles {
            let entry_offset = (ty * width_tiles + tx) * 2;
            let raw = u16::from_le_bytes([scr_map[entry_offset], scr_map[entry_offset + 1]]);
            let entry = ScrEntry::from_u16(raw);

            let tile_offset = usize::from(entry.tile_index) * 32;
            if tile_offset + 32 > tiles.len() {
                return Err(Error::Truncated {
                    offset: tile_offset,
                    needed: 32,
                    available: tiles.len().saturating_sub(tile_offset),
                });
            }

            let pixels = decode_tile_4bpp(&tiles[tile_offset..tile_offset + 32])?;
            let bank_base = usize::from(entry.palette_bank) * 16;

            tile_rgba.clear();
            apply_palette(
                pixels.iter().copied().map(usize::from),
                &mut tile_rgba,
                |color_idx| {
                    if color_idx == 0 && transparent_zero {
                        Ok([0, 0, 0, 0])
                    } else {
                        let pal_idx = bank_base + color_idx;
                        palette.get(pal_idx).copied().ok_or(Error::InvalidField {
                            what: "palette_index_4bpp",
                            value: pal_idx as u64,
                        })
                    }
                },
            )?;

            blit_rgba(
                &mut out,
                width_px_u32,
                height_px_u32,
                (tx * 8) as u32,
                (ty * 8) as u32,
                &tile_rgba,
                8,
                8,
                entry.h_flip,
                entry.v_flip,
                1,
                false,
            )
            .map_err(|_| Error::InvalidField {
                what: "bg_tile_blit",
                value: (ty * width_tiles + tx) as u64,
            })?;
        }
    }

    Ok(out)
}

/// Decode a GBA Mode 3 framebuffer (`240 * 160` little-endian BGR555 pixels) into RGBA8 bytes.
pub fn decode_mode3_rgba(vram: &[u8]) -> Result<Vec<u8>> {
    let pixel_count = GBA_SCREEN_WIDTH * GBA_SCREEN_HEIGHT;
    let needed = pixel_count * 2;
    if vram.len() < needed {
        return Err(Error::Truncated {
            offset: 0,
            needed,
            available: vram.len(),
        });
    }

    let mut out = Vec::with_capacity(pixel_count * 4);
    for &chunk in vram[..needed].as_chunks::<2>().0 {
        out.extend_from_slice(&bgr555_to_rgba8(u16::from_le_bytes(chunk)));
    }
    Ok(out)
}

/// Decode a GBA Mode 4 framebuffer (`240 * 160` 8bpp indexed pixels) into RGBA8 bytes
/// using [`formatkit_pixel::apply_palette`].
pub fn decode_mode4_rgba(vram: &[u8], palette: &[[u8; 4]]) -> Result<Vec<u8>> {
    let pixel_count = GBA_SCREEN_WIDTH * GBA_SCREEN_HEIGHT;
    if vram.len() < pixel_count {
        return Err(Error::Truncated {
            offset: 0,
            needed: pixel_count,
            available: vram.len(),
        });
    }

    let mut rgba = Vec::with_capacity(pixel_count * 4);
    apply_palette(
        vram[..pixel_count].iter().copied().map(usize::from),
        &mut rgba,
        |idx| {
            palette.get(idx).copied().ok_or(Error::InvalidField {
                what: "palette_index_8bpp",
                value: idx as u64,
            })
        },
    )?;
    Ok(rgba)
}

/// Decode a GBA Mode 3 framebuffer (`240 * 160` BGR555) into a formatkit [`Image`].
pub fn decode_mode3_image(vram: &[u8]) -> Result<Image> {
    let rgba = decode_mode3_rgba(vram)?;
    Image::try_new(GBA_SCREEN_WIDTH as u32, GBA_SCREEN_HEIGHT as u32, rgba)
}

/// Decode a GBA Mode 4 framebuffer (`240 * 160` 8bpp indexed pixels) into a formatkit [`Image`].
pub fn decode_mode4_image(vram: &[u8], palette: &[[u8; 4]]) -> Result<Image> {
    let rgba = decode_mode4_rgba(vram, palette)?;
    Image::try_new(GBA_SCREEN_WIDTH as u32, GBA_SCREEN_HEIGHT as u32, rgba)
}

/// Decode a GBA Mode 5 framebuffer (`160 * 128` little-endian BGR555 pixels) into RGBA8 bytes.
pub fn decode_mode5_rgba(vram: &[u8]) -> Result<Vec<u8>> {
    let pixel_count = GBA_MODE5_WIDTH * GBA_MODE5_HEIGHT;
    let needed = pixel_count * 2;
    if vram.len() < needed {
        return Err(Error::Truncated {
            offset: 0,
            needed,
            available: vram.len(),
        });
    }

    let mut out = Vec::with_capacity(pixel_count * 4);
    for &chunk in vram[..needed].as_chunks::<2>().0 {
        out.extend_from_slice(&bgr555_to_rgba8(u16::from_le_bytes(chunk)));
    }
    Ok(out)
}

/// Decode a GBA Mode 5 framebuffer (`160 * 128` BGR555) into a formatkit [`Image`].
pub fn decode_mode5_image(vram: &[u8]) -> Result<Image> {
    let rgba = decode_mode5_rgba(vram)?;
    Image::try_new(GBA_MODE5_WIDTH as u32, GBA_MODE5_HEIGHT as u32, rgba)
}
