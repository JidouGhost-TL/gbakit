//! Game Boy Advance (AGB) PPU graphics primitives: BGR555 palettes, 8x8 4bpp/8bpp tiles,
//! SCR tilemap entries, OAM sprites & affine matrices, text/bitmap renderers, and 1bpp/2bpp font glyphs.

#![deny(unsafe_code)]

mod font;
mod oam;
mod render;
pub mod replace;
pub mod ripper;
pub mod tilemap_replace;

pub use font::{decode_glyph_1bpp, decode_glyph_2bpp};
pub use formatkit_core::Image;
pub use oam::{ObjAffine, ObjAttr, ObjColorMode, ObjMode, ObjShape};
pub use render::{
    decode_mode3_image, decode_mode3_rgba, decode_mode4_image, decode_mode4_rgba,
    decode_mode5_image, decode_mode5_rgba, render_bg_text_4bpp_rgba, screen_block_entry_index,
    GBA_MODE5_HEIGHT, GBA_MODE5_WIDTH, GBA_SCREEN_HEIGHT, GBA_SCREEN_WIDTH,
};
pub use replace::{
    compress_gfx, decode_image_rgba, decode_ppm_p6_rgba, detect_stream_at, encode_image_to_tiles,
    encode_palette_bgr555, find_free_space, patch_rom_pointers, replace_palette_in_rom,
    replace_tileset_bytes_in_rom, replace_tileset_in_rom, write_asset_in_rom, DetectedStream,
    EncodedTileset, GfxCompression, PaletteMode, ReplaceOptions, ReplaceReport, TileBpp,
    DEFAULT_FREE_SPACE_ALIGN, DEFAULT_MAX_IMAGE_PIXELS,
};
pub use ripper::{
    generate_tmx, generate_tsx, render_palette_swatch_rgba, render_sprite_rgba,
    render_tilemap_rgba, render_tileset_sheet_rgba, rip_graphics, scan_graphics_and_maps,
    scan_palettes, scan_sprites, CompressionKind, PaletteManifestEntry, RawPngEncoder,
    RipManifest, RipOptions, RipSummary, ScannedPalette, ScannedSprite, ScannedTilemap,
    ScannedTileset, SpriteManifestEntry, TilemapManifestEntry, TilesetManifestEntry,
};
pub use tilemap_replace::{
    encode_tilemap_scr, parse_tmx_csv, parse_tmx_to_scr, prepare_tilemap_scr,
    replace_tilemap_in_rom, EncodedTilemap, ParsedTmx, TilemapInputFormat, TilemapReplaceOptions,
    TILED_FLIP_D, TILED_FLIP_H, TILED_FLIP_V, TILEMAP_FREE_SPACE_START,
};

use formatkit_core::{Error, Result};
use formatkit_pixel::{
    apply_palette, bgr555_to_rgba, pack_4bpp_into, tilemap::TextTilemapEntry, unpack_4bpp_into,
    Alpha555Policy, IndexPolicy, NibbleOrder, PackError,
};

/// Convert a 15-bit GBA `BGR555` color (`0bbbbbgggggrrrrr`) to opaque `[R, G, B, A]`.
pub fn bgr555_to_rgba8(color: u16) -> [u8; 4] {
    bgr555_to_rgba(color, Alpha555Policy::Opaque)
}

/// Convert `[R, G, B]` to a 15-bit GBA `BGR555` value.
pub fn rgb8_to_bgr555(r: u8, g: u8, b: u8) -> u16 {
    formatkit_pixel::rgb8_to_bgr555(r, g, b)
}

/// Decode a little-endian GBA palette slice (`2 * N` bytes) into RGBA8 colors.
pub fn decode_palette(bytes: &[u8]) -> Result<Vec<[u8; 4]>> {
    if !bytes.len().is_multiple_of(2) {
        return Err(Error::InvalidField {
            what: "palette_byte_len",
            value: bytes.len() as u64,
        });
    }
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| bgr555_to_rgba8(u16::from_le_bytes(c)))
        .collect())
}

/// Decode one 8x8 GBA 4bpp linear tile (32 bytes) into 64 palette indices (`0..=15`) without allocation.
pub fn decode_tile_4bpp(tile: &[u8]) -> Result<[u8; 64]> {
    if tile.len() < 32 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 32,
            available: tile.len(),
        });
    }
    let mut out = [0u8; 64];
    unpack_4bpp_into(&tile[..32], NibbleOrder::LowFirst, &mut out).map_err(|_| Error::Truncated {
        offset: 0,
        needed: 32,
        available: tile.len(),
    })?;
    Ok(out)
}

/// Render one 8x8 GBA 4bpp linear tile (32 bytes) to 64 RGBA8 pixels using `formatkit_pixel::apply_palette`.
pub fn render_tile_4bpp_rgba(tile: &[u8], palette: &[[u8; 4]]) -> Result<Vec<u8>> {
    let indices = decode_tile_4bpp(tile)?;
    let mut rgba = Vec::with_capacity(64 * 4);
    apply_palette(indices.into_iter().map(usize::from), &mut rgba, |idx| {
        palette.get(idx).copied().ok_or(Error::InvalidField {
            what: "palette_index_4bpp",
            value: idx as u64,
        })
    })?;
    Ok(rgba)
}

/// Encode 64 palette indices (`0..=15`) into one 8x8 GBA 4bpp linear tile (32 bytes) without allocation.
pub fn encode_tile_4bpp(pixels: &[u8; 64]) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    pack_4bpp_into(pixels, NibbleOrder::LowFirst, IndexPolicy::Reject, &mut out).map_err(|err| {
        match err {
            PackError::IndexOutOfRange { value, .. } => Error::InvalidField {
                what: "pixel_index_4bpp",
                value: value as u64,
            },
            PackError::LengthMismatch { actual, .. } => Error::InvalidField {
                what: "pixel_tile_4bpp_len",
                value: actual as u64,
            },
            other => Error::Malformed(other.to_string()),
        }
    })?;
    Ok(out)
}

/// Decode one 8x8 GBA 8bpp linear tile (64 bytes) into 64 palette indices without allocation.
pub fn decode_tile_8bpp(tile: &[u8]) -> Result<[u8; 64]> {
    if tile.len() < 64 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 64,
            available: tile.len(),
        });
    }
    let mut out = [0u8; 64];
    out.copy_from_slice(&tile[..64]);
    Ok(out)
}

/// Decoded GBA text-mode `SCR` tilemap entry (16-bit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrEntry {
    pub tile_index: u16,
    pub h_flip: bool,
    pub v_flip: bool,
    pub palette_bank: u8,
}

impl ScrEntry {
    pub fn from_u16(raw: u16) -> Self {
        let e = TextTilemapEntry::from_raw(raw);
        Self {
            tile_index: e.tile,
            h_flip: e.h_flip,
            v_flip: e.v_flip,
            palette_bank: e.palette,
        }
    }

    pub fn to_u16(self) -> u16 {
        TextTilemapEntry {
            tile: self.tile_index,
            h_flip: self.h_flip,
            v_flip: self.v_flip,
            palette: self.palette_bank,
        }
        .to_raw()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgr555_round_trip() {
        let pure_red = rgb8_to_bgr555(255, 0, 0);
        assert_eq!(pure_red, 0x001F);
        assert_eq!(bgr555_to_rgba8(pure_red), [255, 0, 0, 255]);

        let pure_blue = rgb8_to_bgr555(0, 0, 255);
        assert_eq!(pure_blue, 0x7C00);
        assert_eq!(bgr555_to_rgba8(pure_blue), [0, 0, 255, 255]);
    }

    #[test]
    fn tile_4bpp_round_trip() {
        let mut pixels = [0u8; 64];
        for (i, p) in pixels.iter_mut().enumerate() {
            *p = (i % 16) as u8;
        }
        let encoded = encode_tile_4bpp(&pixels).unwrap();
        let decoded = decode_tile_4bpp(&encoded).unwrap();
        assert_eq!(decoded, pixels);

        let palette: Vec<[u8; 4]> = (0..16).map(|i| [i as u8 * 16, 0, 0, 255]).collect();
        let rgba = render_tile_4bpp_rgba(&encoded, &palette).unwrap();
        assert_eq!(rgba.len(), 64 * 4);
        assert_eq!(&rgba[0..8], &[0, 0, 0, 255, 16, 0, 0, 255]);
    }

    #[test]
    fn scr_entry_round_trip() {
        let e = ScrEntry {
            tile_index: 0x1AB,
            h_flip: true,
            v_flip: false,
            palette_bank: 7,
        };
        assert_eq!(ScrEntry::from_u16(e.to_u16()), e);
    }

    #[test]
    fn oam_all_twelve_dimensions() {
        let cases = [
            (ObjShape::Square, 0, (8, 8)),
            (ObjShape::Square, 1, (16, 16)),
            (ObjShape::Square, 2, (32, 32)),
            (ObjShape::Square, 3, (64, 64)),
            (ObjShape::Horizontal, 0, (16, 8)),
            (ObjShape::Horizontal, 1, (32, 8)),
            (ObjShape::Horizontal, 2, (32, 16)),
            (ObjShape::Horizontal, 3, (64, 32)),
            (ObjShape::Vertical, 0, (8, 16)),
            (ObjShape::Vertical, 1, (8, 32)),
            (ObjShape::Vertical, 2, (16, 32)),
            (ObjShape::Vertical, 3, (32, 64)),
        ];

        for (shape, size, expected) in cases {
            let attr = ObjAttr {
                y: 0,
                affine: false,
                double_size_or_disable: false,
                mode: ObjMode::Normal,
                mosaic: false,
                color_mode: ObjColorMode::Bpp4,
                shape,
                x: 0,
                affine_param: 0,
                h_flip: false,
                v_flip: false,
                size,
                tile_index: 0,
                priority: 0,
                palette_bank: 0,
            };
            assert_eq!(attr.dimensions(), Some(expected));
        }

        let invalid_size = ObjAttr {
            y: 0,
            affine: false,
            double_size_or_disable: false,
            mode: ObjMode::Normal,
            mosaic: false,
            color_mode: ObjColorMode::Bpp4,
            shape: ObjShape::Square,
            x: 0,
            affine_param: 0,
            h_flip: false,
            v_flip: false,
            size: 4,
            tile_index: 0,
            priority: 0,
            palette_bank: 0,
        };
        assert_eq!(invalid_size.dimensions(), None);
    }

    #[test]
    fn oam_attr_round_trip_regular_and_affine() {
        let regular = ObjAttr {
            y: 120,
            affine: false,
            double_size_or_disable: true,
            mode: ObjMode::SemiTransparent,
            mosaic: true,
            color_mode: ObjColorMode::Bpp4,
            shape: ObjShape::Horizontal,
            x: 300,
            affine_param: 0,
            h_flip: true,
            v_flip: true,
            size: 2,
            tile_index: 0x2A5,
            priority: 2,
            palette_bank: 11,
        };
        assert!(regular.is_disabled());
        assert!(!regular.is_double_size());
        let raw_regular = regular.to_attrs();
        assert_eq!(ObjAttr::from_attrs(raw_regular).unwrap(), regular);

        let mut oam_entry = [0u8; 8];
        oam_entry[0..2].copy_from_slice(&raw_regular[0].to_le_bytes());
        oam_entry[2..4].copy_from_slice(&raw_regular[1].to_le_bytes());
        oam_entry[4..6].copy_from_slice(&raw_regular[2].to_le_bytes());
        oam_entry[6..8].copy_from_slice(&[0xDE, 0xAD]); // affine padding ignored
        assert_eq!(ObjAttr::from_oam_entry(&oam_entry).unwrap(), regular);
        assert_eq!(ObjAttr::from_bytes(&oam_entry[..6]).unwrap(), regular);

        let affine = ObjAttr {
            y: 44,
            affine: true,
            double_size_or_disable: true,
            mode: ObjMode::ObjWindow,
            mosaic: false,
            color_mode: ObjColorMode::Bpp8,
            shape: ObjShape::Vertical,
            x: 511,
            affine_param: 27,
            h_flip: false,
            v_flip: false,
            size: 3,
            tile_index: 0x3FF,
            priority: 3,
            palette_bank: 4,
        };
        assert!(!affine.is_disabled());
        assert!(affine.is_double_size());
        let raw_affine = affine.to_attrs();
        assert_eq!(ObjAttr::from_attrs(raw_affine).unwrap(), affine);

        // Prohibited mode (3) and shape (3) must be rejected.
        assert!(ObjAttr::from_attrs([3 << 10, 0, 0]).is_err());
        assert!(ObjAttr::from_attrs([3 << 14, 0, 0]).is_err());
        assert!(ObjAttr::from_bytes(&[0u8; 5]).is_err());
    }

    #[test]
    fn oam_affine_interleaved_extraction() {
        let mut block = [0xAAu8; 32];
        // pa = 256 (1.0), pb = -128 (-0.5), pc = 64 (0.25), pd = -512 (-2.0)
        block[6..8].copy_from_slice(&256i16.to_le_bytes());
        block[14..16].copy_from_slice(&(-128i16).to_le_bytes());
        block[22..24].copy_from_slice(&64i16.to_le_bytes());
        block[30..32].copy_from_slice(&(-512i16).to_le_bytes());

        let m = ObjAffine::from_oam_block(&block);
        assert_eq!(
            m,
            ObjAffine {
                pa: 256,
                pb: -128,
                pc: 64,
                pd: -512,
            }
        );
        assert_eq!(m.to_f32(), [1.0, -0.5, 0.25, -2.0]);
    }

    #[test]
    fn bg_text_4bpp_render_flips_banks_and_transparency() {
        // Tile 0: asymmetric marker pattern so flips are unambiguous.
        // (0,0) = 1, (7,0) = 2, (0,7) = 3, (7,7) = 4, rest = 0.
        let mut tile0_px = [0u8; 64];
        tile0_px[0] = 1;
        tile0_px[7] = 2;
        tile0_px[7 * 8] = 3;
        tile0_px[7 * 8 + 7] = 4;
        let tile0 = encode_tile_4bpp(&tile0_px).unwrap();

        // Tile 1: solid color index 5.
        let tile1 = encode_tile_4bpp(&[5u8; 64]).unwrap();

        let mut tiles = Vec::new();
        tiles.extend_from_slice(&tile0);
        tiles.extend_from_slice(&tile1);

        // 2x2 tilemap:
        // (0,0): tile 0, normal, bank 0
        // (1,0): tile 0, h_flip, bank 0
        // (0,1): tile 0, v_flip, bank 1
        // (1,1): tile 1, h_flip+v_flip, bank 1
        let entries = [
            ScrEntry {
                tile_index: 0,
                h_flip: false,
                v_flip: false,
                palette_bank: 0,
            },
            ScrEntry {
                tile_index: 0,
                h_flip: true,
                v_flip: false,
                palette_bank: 0,
            },
            ScrEntry {
                tile_index: 0,
                h_flip: false,
                v_flip: true,
                palette_bank: 1,
            },
            ScrEntry {
                tile_index: 1,
                h_flip: true,
                v_flip: true,
                palette_bank: 1,
            },
        ];
        let mut scr_map = Vec::new();
        for e in entries {
            scr_map.extend_from_slice(&e.to_u16().to_le_bytes());
        }

        // 2 palette banks (32 colors): [bank * 100 + idx, idx, bank, 255].
        let mut palette = Vec::with_capacity(32);
        for bank in 0u8..2 {
            for idx in 0u8..16 {
                palette.push([bank * 100 + idx, idx, bank, 255]);
            }
        }

        let rgba = render_bg_text_4bpp_rgba(&tiles, &scr_map, &palette, 2, 2, true).unwrap();
        assert_eq!(rgba.len(), 16 * 16 * 4);

        let pixel_at = |buf: &[u8], x: usize, y: usize| -> [u8; 4] {
            let i = (y * 16 + x) * 4;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };

        // Top-left tile (0,0): unflipped, bank 0
        assert_eq!(pixel_at(&rgba, 0, 0), palette[1]);
        assert_eq!(pixel_at(&rgba, 7, 0), palette[2]);
        assert_eq!(pixel_at(&rgba, 0, 7), palette[3]);
        assert_eq!(pixel_at(&rgba, 7, 7), palette[4]);
        // Index 0 with transparent_zero = true -> [0, 0, 0, 0]
        assert_eq!(pixel_at(&rgba, 1, 0), [0, 0, 0, 0]);

        // Top-right tile (1,0): h_flip, bank 0 -> x=8 has original x=7 (color 2), x=15 has original x=0 (color 1)
        assert_eq!(pixel_at(&rgba, 8, 0), palette[2]);
        assert_eq!(pixel_at(&rgba, 15, 0), palette[1]);
        assert_eq!(pixel_at(&rgba, 8, 7), palette[4]);
        assert_eq!(pixel_at(&rgba, 15, 7), palette[3]);

        // Bottom-left tile (0,1): v_flip, bank 1 -> y=8 has original y=7 (colors 3, 4), y=15 has original y=0 (colors 1, 2)
        assert_eq!(pixel_at(&rgba, 0, 8), palette[16 + 3]);
        assert_eq!(pixel_at(&rgba, 7, 8), palette[16 + 4]);
        assert_eq!(pixel_at(&rgba, 0, 15), palette[16 + 1]);
        assert_eq!(pixel_at(&rgba, 7, 15), palette[16 + 2]);

        // Bottom-right tile (1,1): tile 1 (all 5s), bank 1
        assert_eq!(pixel_at(&rgba, 8, 8), palette[16 + 5]);
        assert_eq!(pixel_at(&rgba, 15, 15), palette[16 + 5]);

        // With transparent_zero = false, index 0 uses bank's entry 0.
        let opaque = render_bg_text_4bpp_rgba(&tiles, &scr_map, &palette, 2, 2, false).unwrap();
        assert_eq!(pixel_at(&opaque, 1, 0), palette[0]);
        assert_eq!(pixel_at(&opaque, 1, 8), palette[16]);

        // Error paths: truncated map, missing tile, missing palette bank.
        assert!(render_bg_text_4bpp_rgba(&tiles, &scr_map[..6], &palette, 2, 2, true).is_err());
        assert!(render_bg_text_4bpp_rgba(&tile0, &scr_map, &palette, 2, 2, true).is_err());
        assert!(render_bg_text_4bpp_rgba(&tiles, &scr_map, &palette[..16], 2, 2, true).is_err());
    }

    #[test]
    fn bg_text_4bpp_render_64x32_screenblock_quadrants() {
        // Tile 0: solid color 1, Tile 1: solid color 2
        let tile0 = encode_tile_4bpp(&[1u8; 64]).unwrap();
        let tile1 = encode_tile_4bpp(&[2u8; 64]).unwrap();
        let mut tiles = Vec::new();
        tiles.extend_from_slice(&tile0);
        tiles.extend_from_slice(&tile1);

        let mut palette = vec![[0u8; 4]; 16];
        palette[1] = [10, 20, 30, 255];
        palette[2] = [200, 100, 50, 255];

        // 64x32 BG = 2 horizontal 32x32 screen blocks (2048 entries = 4096 bytes).
        // Put tile 1 at screen block 1 entry 0 (index 1024 -> (tx=32, ty=0)).
        let mut scr_map = vec![0u8; 64 * 32 * 2];
        let entry1 = ScrEntry {
            tile_index: 1,
            h_flip: false,
            v_flip: false,
            palette_bank: 0,
        }
        .to_u16()
        .to_le_bytes();
        scr_map[1024 * 2..1024 * 2 + 2].copy_from_slice(&entry1);

        let rgba = render_bg_text_4bpp_rgba(&tiles, &scr_map, &palette, 64, 32, false).unwrap();
        let width_px = 64 * 8;
        let px_at = |x: usize, y: usize| -> [u8; 4] {
            let i = (y * width_px + x) * 4;
            [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
        };
        // (tx=0, ty=0) -> tile 0 -> palette[1]
        assert_eq!(px_at(0, 0), palette[1]);
        // (tx=32, ty=0) -> pixel x=256, y=0 -> SCR index 1024 -> tile 1 -> palette[2]
        assert_eq!(px_at(32 * 8, 0), palette[2]);
        // (tx=0, ty=1) -> SCR index 32 -> tile 0 -> palette[1]
        assert_eq!(px_at(0, 8), palette[1]);
    }

    #[test]
    fn mode3_and_mode4_framebuffers() {
        let pixel_count = GBA_SCREEN_WIDTH * GBA_SCREEN_HEIGHT;

        // Mode 3: direct BGR555
        let red = rgb8_to_bgr555(255, 0, 0);
        let green = rgb8_to_bgr555(0, 255, 0);
        let blue = rgb8_to_bgr555(0, 0, 255);
        let mut mode3_vram = vec![0u8; pixel_count * 2];
        mode3_vram[0..2].copy_from_slice(&red.to_le_bytes());
        mode3_vram[2..4].copy_from_slice(&green.to_le_bytes());
        mode3_vram[(pixel_count - 1) * 2..pixel_count * 2].copy_from_slice(&blue.to_le_bytes());

        let m3 = decode_mode3_rgba(&mode3_vram).unwrap();
        assert_eq!(m3.len(), pixel_count * 4);
        assert_eq!(&m3[0..4], &[255, 0, 0, 255]);
        assert_eq!(&m3[4..8], &[0, 255, 0, 255]);
        assert_eq!(&m3[(pixel_count - 1) * 4..], &[0, 0, 255, 255]);
        assert!(decode_mode3_rgba(&mode3_vram[..pixel_count * 2 - 1]).is_err());

        // Mode 4: 8bpp indexed
        let palette: Vec<[u8; 4]> = (0..=255u8).map(|i| [i, 255 - i, i / 2, 255]).collect();
        let mut mode4_vram = vec![0u8; pixel_count];
        mode4_vram[0] = 10;
        mode4_vram[1] = 200;
        mode4_vram[pixel_count - 1] = 255;

        let m4 = decode_mode4_rgba(&mode4_vram, &palette).unwrap();
        assert_eq!(m4.len(), pixel_count * 4);
        assert_eq!(&m4[0..4], &palette[10]);
        assert_eq!(&m4[4..8], &palette[200]);
        assert_eq!(&m4[(pixel_count - 1) * 4..], &palette[255]);
        assert!(decode_mode4_rgba(&mode4_vram[..pixel_count - 1], &palette).is_err());
        assert!(decode_mode4_rgba(&mode4_vram, &palette[..16]).is_err());
    }

    #[test]
    fn font_glyph_1bpp_and_2bpp() {
        // 1bpp 8x2: row 0 = 0b1011_0010, row 1 = 0b0100_1101
        let data_1bpp = [0b1011_0010u8, 0b0100_1101u8];
        assert_eq!(
            decode_glyph_1bpp(&data_1bpp, 8, 2, true).unwrap(),
            vec![1, 0, 1, 1, 0, 0, 1, 0, 0, 1, 0, 0, 1, 1, 0, 1]
        );
        assert_eq!(
            decode_glyph_1bpp(&data_1bpp, 8, 2, false).unwrap(),
            vec![0, 1, 0, 0, 1, 1, 0, 1, 1, 0, 1, 1, 0, 0, 1, 0]
        );

        // Non-byte-aligned width (5x2 -> 1 byte per row)
        assert_eq!(
            decode_glyph_1bpp(&data_1bpp, 5, 2, true).unwrap(),
            vec![1, 0, 1, 1, 0, 0, 1, 0, 0, 1]
        );
        assert!(decode_glyph_1bpp(&data_1bpp[..1], 5, 2, true).is_err());

        // 2bpp 4x2: row 0 = 0b11_10_01_00 (0xE4), row 1 = 0b00_01_10_11 (0x1B)
        let data_2bpp = [0b11_10_01_00u8, 0b00_01_10_11u8];
        assert_eq!(
            decode_glyph_2bpp(&data_2bpp, 4, 2, true).unwrap(),
            vec![3, 2, 1, 0, 0, 1, 2, 3]
        );
        assert_eq!(
            decode_glyph_2bpp(&data_2bpp, 4, 2, false).unwrap(),
            vec![0, 1, 2, 3, 3, 2, 1, 0]
        );

        // Non-byte-aligned width (5x2 -> 2 bytes per row)
        let wide_2bpp = [0b11_10_01_00u8, 0b01_00_00_00u8, 0b00_01_10_11u8, 0b10_00_00_00u8];
        assert_eq!(
            decode_glyph_2bpp(&wide_2bpp, 5, 2, true).unwrap(),
            vec![3, 2, 1, 0, 1, 0, 1, 2, 3, 2]
        );
        assert!(decode_glyph_2bpp(&wide_2bpp[..3], 5, 2, true).is_err());
    }
}
