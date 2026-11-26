//! `MAP\0` 4bpp packed-pixel map parser, symmetric writer, and ROM scanner.
//!
//! Layout:
//! - `+0x00`: `b"MAP\0"` (4 bytes)
//! - `+0x04`: `width: u16 LE` (`1..=2048`)
//! - `+0x06`: `height: u16 LE` (`1..=2048`)
//! - `+0x08`: `((width * height) + 1) / 2` bytes of GBA low-nibble-first 4bpp packed pixels.

use formatkit_core::{u16_at, Error, Result};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_MAP0};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_MAP0: &[u8; 4] = b"MAP\0";
const MAP0_HEADER_LEN: usize = 8;
const MAX_DIM: u16 = 2048;
const SCAN_MIN_DIM: u16 = 4;
const SCAN_MAX_DIM: u16 = 512;

/// Parsed `studio.map0` 4bpp packed-pixel map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Map0Asset {
    pub width: u16,
    pub height: u16,
    pub packed_4bpp: Vec<u8>,
    pub consumed_end: usize,
}

impl Map0Asset {
    /// Serialize this map back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Fast, panic-free structural detector for `MAP\0` 4bpp packed-pixel maps.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse a `MAP\0` 4bpp packed-pixel map from the start of `data`.
pub fn parse(data: &[u8]) -> Result<Map0Asset> {
    parse_with_limits(data, 1, MAX_DIM)
}

fn parse_with_limits(data: &[u8], min_dim: u16, max_dim: u16) -> Result<Map0Asset> {
    if data.len() < MAP0_HEADER_LEN {
        return Err(Error::Truncated {
            offset: 0,
            needed: MAP0_HEADER_LEN,
            available: data.len(),
        });
    }
    if &data[..4] != MAGIC_MAP0 {
        let found = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        return Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_MAP0),
            found,
        });
    }
    let width = u16_at(data, 4)?;
    let height = u16_at(data, 6)?;
    if width < min_dim || height < min_dim || width > max_dim || height > max_dim {
        return Err(Error::InvalidField {
            what: "MAP0 dimensions",
            value: (u64::from(width) << 16) | u64::from(height),
        });
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or(Error::InvalidField {
            what: "MAP0 pixel count",
            value: u64::from(width),
        })?;
    let body_len = pixels.checked_add(1).ok_or(Error::InvalidField {
        what: "MAP0 packed length",
        value: pixels as u64,
    })? / 2;
    let consumed_end = MAP0_HEADER_LEN
        .checked_add(body_len)
        .ok_or(Error::InvalidField {
            what: "MAP0 consumed_end",
            value: body_len as u64,
        })?;
    if consumed_end > data.len() {
        return Err(Error::Truncated {
            offset: 0,
            needed: consumed_end,
            available: data.len(),
        });
    }
    Ok(Map0Asset {
        width,
        height,
        packed_4bpp: data[MAP0_HEADER_LEN..consumed_end].to_vec(),
        consumed_end,
    })
}

/// Serialize a parsed `studio.map0` asset back into its exact binary representation.
pub fn pack(asset: &Map0Asset) -> Result<Vec<u8>> {
    if asset.width == 0 || asset.height == 0 || asset.width > MAX_DIM || asset.height > MAX_DIM {
        return Err(Error::InvalidField {
            what: "MAP0 dimensions",
            value: (u64::from(asset.width) << 16) | u64::from(asset.height),
        });
    }
    let pixels = (asset.width as usize)
        .checked_mul(asset.height as usize)
        .ok_or(Error::InvalidField {
            what: "MAP0 pixel count",
            value: u64::from(asset.width),
        })?;
    let expected_len = pixels.checked_add(1).ok_or(Error::InvalidField {
        what: "MAP0 packed length",
        value: pixels as u64,
    })? / 2;
    if asset.packed_4bpp.len() != expected_len {
        return Err(Error::InvalidField {
            what: "MAP0 body length mismatch",
            value: asset.packed_4bpp.len() as u64,
        });
    }
    let mut out = Vec::with_capacity(MAP0_HEADER_LEN.saturating_add(expected_len));
    out.extend_from_slice(MAGIC_MAP0);
    out.extend_from_slice(&asset.width.to_le_bytes());
    out.extend_from_slice(&asset.height.to_le_bytes());
    out.extend_from_slice(&asset.packed_4bpp);
    Ok(out)
}

/// Scan `rom` on 4-byte aligned offsets for verified `studio.map0` assets.
pub fn scan_map0(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective = &rom[..rom.len().min(MAX_ROM_SIZE)];
    if effective.len() < HEADER_SIZE + MAP0_HEADER_LEN {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + MAP0_HEADER_LEN <= effective.len() {
        if &effective[off..off + 4] == MAGIC_MAP0 {
            if let Ok(asset) = parse_with_limits(&effective[off..], SCAN_MIN_DIM, SCAN_MAX_DIM) {
                let size = asset.consumed_end;
                if size > 0 && off.checked_add(size).is_some_and(|e| e <= effective.len()) {
                    found.push(DetectedFormatExtent {
                        format_id: FORMAT_ID_MAP0,
                        subkind: "map0",
                        offset: off,
                        size,
                    });
                    off = align_up_4(off.saturating_add(size)).max(off + 4);
                    continue;
                }
            }
        }
        off += 4;
    }
    found
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn make_valid_map0(width: u16, height: u16) -> Vec<u8> {
        let pixels = (width as usize) * (height as usize);
        let body_len = pixels.div_ceil(2);
        let mut out = Vec::with_capacity(MAP0_HEADER_LEN + body_len);
        out.extend_from_slice(MAGIC_MAP0);
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        for i in 0..body_len {
            out.push(((i * 7) & 0xFF) as u8);
        }
        out
    }

    #[test]
    fn parses_and_roundtrips_map0_byte_identically() {
        let mut even = make_valid_map0(12, 12);
        let even_len = even.len();
        even.extend_from_slice(&[0xFF; 8]);
        assert!(detect(&even));
        let parsed = parse(&even).unwrap();
        assert_eq!(parsed.width, 12);
        assert_eq!(parsed.height, 12);
        assert_eq!(parsed.consumed_end, even_len);
        assert_eq!(parsed.to_bytes().unwrap(), even[..even_len]);

        // Odd pixel count (5x5 = 25 pixels -> 13 bytes)
        let odd = make_valid_map0(5, 5);
        let parsed_odd = parse(&odd).unwrap();
        assert_eq!(parsed_odd.consumed_end, 8 + 13);
        assert_eq!(parsed_odd.to_bytes().unwrap(), odd);
    }

    #[test]
    fn rejects_malformed_truncated_and_random_inputs() {
        assert!(!detect(&[]));
        assert!(parse(&[]).is_err());
        let good = make_valid_map0(16, 16);
        assert!(!detect(&good[..good.len() - 1]));
        assert!(parse(&good[..good.len() - 1]).is_err());

        let mut zero_dim = good.clone();
        zero_dim[4..8].copy_from_slice(&[0, 0, 0, 0]);
        assert!(!detect(&zero_dim));
        assert!(parse(&zero_dim).is_err());

        let mut huge = vec![0u8; 16];
        huge[..4].copy_from_slice(MAGIC_MAP0);
        huge[4..8].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(!detect(&huge));
        assert!(parse(&huge).is_err());

        let mut seed = 0x369C_F123u64;
        for _ in 0..200 {
            let mut buf = [0u8; 128];
            for b in &mut buf {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                *b = (seed >> 33) as u8;
            }
            assert!(!detect(&buf));
            assert!(parse(&buf).is_err());
        }
    }
}
