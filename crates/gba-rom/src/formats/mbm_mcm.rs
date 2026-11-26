//! Unified `MBM\0` / `MCM\0` paired ROM asset pack parser, writer, and ROM scanner.
//!
//! Consolidates both entry-point orderings of the paired 24-byte `MBM\0` directory header
//! and 24-byte `MCM\0` member header:
//! - **`MBM\0` + `MCM\0` + payload**: `stored_len = rom_ptr_2 - mcm_ptr` (`0x10..=0x10000`),
//!   `flags == 0` (raw: `stored_len == decoded_size`) or `flags == 0x203` (encoded:
//!   `stored_len < decoded_size`).
//! - **`MCM\0` + payload + `MBM\0` footer**: `page == 0x1000`, `count in 1..=64`,
//!   `flags in {0, 0x203}`, followed by the 24-byte `MBM\0` descriptor footer.

use formatkit_core::{bytes_at, u16_at, u32_at, Error, Result};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_MBM_MCM};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_MBM: &[u8; 4] = b"MBM\0";
const MAGIC_MCM: &[u8; 4] = b"MCM\0";
const MBM_HEADER_LEN: usize = 24;
const MCM_HEADER_LEN: usize = 24;
const HEADERS_LEN: usize = MBM_HEADER_LEN + MCM_HEADER_LEN;
const MCM_CONSTANT_WORD: u32 = 0x1000;
const FLAGS_RAW: u32 = 0;
const FLAGS_ENCODED: u32 = 0x203;
const ROM_PTR_MIN: u32 = 0x0800_0000;
const ROM_PTR_MAX: u32 = 0x0C00_0000;
const MIN_STORED_SIZE: u32 = 16;
const MAX_ASSET_SIZE: u32 = 0x1_0000;
const MAX_VARIANT_WORD: u32 = 0x100_0000;
const MAX_BLOCK_COUNT: u32 = 64;
const MAX_FOOTER_DIM: u16 = 4096;

/// Ordering variant within `studio.mbm-mcm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MbmMcmKind {
    /// 24-byte `MBM\0` header + 24-byte `MCM\0` header + payload.
    MbmMcm,
    /// 24-byte `MCM\0` header + payload + 24-byte `MBM\0` footer.
    McmMbm,
}

impl MbmMcmKind {
    pub const fn subkind(self) -> &'static str {
        match self {
            Self::MbmMcm => "mbm0",
            Self::McmMbm => "mcm0",
        }
    }
}

/// Parsed 24-byte `MBM\0` descriptor header/footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MbmHeader {
    pub raw_words: [u8; 20],
}

/// Parsed 24-byte `MCM\0` member header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McmHeader {
    pub size_field: u32,
    pub page_class: u32,
    pub block_count: u32,
    pub flags: u32,
    pub rom_ptr: u32,
}

/// Parsed `studio.mbm-mcm` asset with exact binary round-trip state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MbmMcmAsset {
    pub kind: MbmMcmKind,
    pub mbm: MbmHeader,
    pub mcm: McmHeader,
    pub payload: Vec<u8>,
    pub consumed_end: usize,
}

impl MbmMcmAsset {
    /// Serialize this asset back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Fast, panic-free structural detector for `studio.mbm-mcm` assets.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse an `MBM\0`+`MCM\0` or `MCM\0`+`MBM\0` paired ROM asset pack from the start of `data`.
pub fn parse(data: &[u8]) -> Result<MbmMcmAsset> {
    parse_inner(data, false)
}

fn parse_inner(data: &[u8], rom_scan_mode: bool) -> Result<MbmMcmAsset> {
    let Some(magic) = data.get(..4) else {
        return Err(Error::Truncated {
            offset: 0,
            needed: 4,
            available: data.len(),
        });
    };
    if magic == MAGIC_MBM {
        parse_mbm_mcm(data)
    } else if magic == MAGIC_MCM {
        parse_mcm_mbm(data, rom_scan_mode)
    } else {
        let found = u32::from_be_bytes([magic[0], magic[1], magic[2], magic[3]]);
        Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_MBM),
            found,
        })
    }
}

/// Serialize a parsed `studio.mbm-mcm` asset back into its exact binary representation.
pub fn pack(asset: &MbmMcmAsset) -> Result<Vec<u8>> {
    if asset.mcm.page_class != MCM_CONSTANT_WORD
        || (asset.mcm.flags != FLAGS_RAW && asset.mcm.flags != FLAGS_ENCODED)
        || asset.mcm.block_count == 0
        || asset.mcm.block_count > MAX_BLOCK_COUNT
    {
        return Err(Error::InvalidField {
            what: "MBM/MCM header fields",
            value: u64::from(asset.mcm.flags),
        });
    }
    let total = HEADERS_LEN
        .checked_add(asset.payload.len())
        .ok_or(Error::InvalidField {
            what: "MBM/MCM total size",
            value: asset.payload.len() as u64,
        })?;
    let mut out = Vec::with_capacity(total);
    let write_mbm = |buf: &mut Vec<u8>, mbm: &MbmHeader| {
        buf.extend_from_slice(MAGIC_MBM);
        buf.extend_from_slice(&mbm.raw_words);
    };
    let write_mcm = |buf: &mut Vec<u8>, mcm: &McmHeader| {
        buf.extend_from_slice(MAGIC_MCM);
        buf.extend_from_slice(&mcm.size_field.to_le_bytes());
        buf.extend_from_slice(&mcm.page_class.to_le_bytes());
        buf.extend_from_slice(&mcm.block_count.to_le_bytes());
        buf.extend_from_slice(&mcm.flags.to_le_bytes());
        buf.extend_from_slice(&mcm.rom_ptr.to_le_bytes());
    };
    match asset.kind {
        MbmMcmKind::MbmMcm => {
            write_mbm(&mut out, &asset.mbm);
            write_mcm(&mut out, &asset.mcm);
            out.extend_from_slice(&asset.payload);
        }
        MbmMcmKind::McmMbm => {
            write_mcm(&mut out, &asset.mcm);
            out.extend_from_slice(&asset.payload);
            write_mbm(&mut out, &asset.mbm);
        }
    }
    Ok(out)
}

fn parse_mbm_mcm(data: &[u8]) -> Result<MbmMcmAsset> {
    if data.len() < HEADERS_LEN {
        return Err(Error::Truncated {
            offset: 0,
            needed: HEADERS_LEN,
            available: data.len(),
        });
    }
    if bytes_at(data, MBM_HEADER_LEN, 4)? != MAGIC_MCM {
        return Err(Error::InvalidField {
            what: "MBM secondary MCM magic",
            value: 0,
        });
    }
    let variant = u32_at(data, 4)?;
    if variant >= MAX_VARIANT_WORD || u16_at(data, 8)? == 0 || u16_at(data, 10)? == 0 {
        return Err(Error::InvalidField {
            what: "MBM header fields",
            value: u64::from(variant),
        });
    }
    for off in [16usize, 20, 44] {
        let ptr = u32_at(data, off)?;
        if !(ROM_PTR_MIN..ROM_PTR_MAX).contains(&ptr) {
            return Err(Error::InvalidField {
                what: "MBM ROM pointer",
                value: u64::from(ptr),
            });
        }
    }
    let rom_end = u32_at(data, 20)?;
    let mcm_ptr = u32_at(data, 44)?;
    if rom_end <= mcm_ptr {
        return Err(Error::InvalidField {
            what: "MBM pointer order",
            value: u64::from(rom_end),
        });
    }
    let stored_len_u32 = rom_end - mcm_ptr;
    if !(MIN_STORED_SIZE..=MAX_ASSET_SIZE).contains(&stored_len_u32) {
        return Err(Error::InvalidField {
            what: "MBM stored size",
            value: u64::from(stored_len_u32),
        });
    }
    let decoded_u32 = u32_at(data, 28)?;
    let page_class = u32_at(data, 32)?;
    let block_count = u32_at(data, 36)?;
    let flags = u32_at(data, 40)?;
    if page_class != MCM_CONSTANT_WORD
        || block_count == 0
        || block_count > MAX_BLOCK_COUNT
        || (flags != FLAGS_RAW && flags != FLAGS_ENCODED)
        || !(MIN_STORED_SIZE..=MAX_ASSET_SIZE).contains(&decoded_u32)
        || (flags == FLAGS_RAW && stored_len_u32 != decoded_u32)
        || (flags == FLAGS_ENCODED && stored_len_u32 >= decoded_u32)
    {
        return Err(Error::InvalidField {
            what: "MCM member header",
            value: u64::from(flags),
        });
    }

    let stored_len = stored_len_u32 as usize;
    let consumed_end = HEADERS_LEN
        .checked_add(stored_len)
        .ok_or(Error::InvalidField {
            what: "MBM payload end",
            value: stored_len as u64,
        })?;
    if consumed_end > data.len() {
        return Err(Error::Truncated {
            offset: HEADERS_LEN,
            needed: stored_len,
            available: data.len().saturating_sub(HEADERS_LEN),
        });
    }

    let mut raw_words = [0u8; 20];
    raw_words.copy_from_slice(&data[4..24]);
    Ok(MbmMcmAsset {
        kind: MbmMcmKind::MbmMcm,
        mbm: MbmHeader { raw_words },
        mcm: McmHeader {
            size_field: decoded_u32,
            page_class,
            block_count,
            flags,
            rom_ptr: mcm_ptr,
        },
        payload: data[HEADERS_LEN..consumed_end].to_vec(),
        consumed_end,
    })
}

fn rom_ptr_ok(ptr: u32) -> bool {
    ptr == 0 || (ptr & 0xFF00_0000) == 0x0800_0000 || (ptr & 0xFF00_0000) == 0x0900_0000
}

fn validate_mbm_footer(data: &[u8], footer_offset: usize) -> Result<MbmHeader> {
    if footer_offset
        .checked_add(MBM_HEADER_LEN)
        .is_none_or(|e| e > data.len())
    {
        return Err(Error::Truncated {
            offset: footer_offset,
            needed: MBM_HEADER_LEN,
            available: data.len().saturating_sub(footer_offset),
        });
    }
    if &data[footer_offset..footer_offset + 4] != MAGIC_MBM {
        return Err(Error::InvalidField {
            what: "MCM missing MBM footer",
            value: footer_offset as u64,
        });
    }
    let dim_a = u16_at(data, footer_offset + 6)?;
    let dim_b = u16_at(data, footer_offset + 8)?;
    let dim_c = u16_at(data, footer_offset + 10)?;
    if dim_a == 0
        || dim_a > MAX_FOOTER_DIM
        || dim_b == 0
        || dim_b > MAX_FOOTER_DIM
        || dim_c == 0
        || dim_c > MAX_FOOTER_DIM
    {
        return Err(Error::InvalidField {
            what: "MBM footer dims",
            value: u64::from(dim_a),
        });
    }
    let ptr_a = u32_at(data, footer_offset + 16)?;
    let ptr_b = u32_at(data, footer_offset + 20)?;
    if !rom_ptr_ok(ptr_a) || !rom_ptr_ok(ptr_b) {
        return Err(Error::InvalidField {
            what: "MBM footer ROM pointers",
            value: u64::from(ptr_a),
        });
    }
    let mut raw_words = [0u8; 20];
    raw_words.copy_from_slice(&data[footer_offset + 4..footer_offset + MBM_HEADER_LEN]);
    Ok(MbmHeader { raw_words })
}

fn parse_mcm_mbm(data: &[u8], rom_scan_mode: bool) -> Result<MbmMcmAsset> {
    if data.len() < HEADERS_LEN {
        return Err(Error::Truncated {
            offset: 0,
            needed: HEADERS_LEN,
            available: data.len(),
        });
    }
    let size_field = u32_at(data, 4)?;
    let page_class = u32_at(data, 8)?;
    let block_count = u32_at(data, 12)?;
    let flags = u32_at(data, 16)?;
    let rom_ptr = u32_at(data, 20)?;

    if page_class != MCM_CONSTANT_WORD
        || block_count == 0
        || block_count > MAX_BLOCK_COUNT
        || (flags != FLAGS_RAW && flags != FLAGS_ENCODED)
        || !rom_ptr_ok(rom_ptr)
        || size_field == 0
        || size_field > MAX_ASSET_SIZE
    {
        return Err(Error::InvalidField {
            what: "MCM header",
            value: u64::from(flags),
        });
    }

    let (payload_len, footer_offset, mbm) = if flags == FLAGS_RAW {
        let size = size_field as usize;
        let foff = MCM_HEADER_LEN
            .checked_add(size)
            .ok_or(Error::InvalidField {
                what: "MCM footer offset",
                value: size as u64,
            })?;
        let mbm = validate_mbm_footer(data, foff)?;
        (size, foff, mbm)
    } else if !rom_scan_mode {
        let foff = data.len().saturating_sub(MBM_HEADER_LEN);
        if foff <= MCM_HEADER_LEN {
            return Err(Error::InvalidField {
                what: "MCM encoded payload empty",
                value: 0,
            });
        }
        let mbm = validate_mbm_footer(data, foff)?;
        (foff - MCM_HEADER_LEN, foff, mbm)
    } else {
        // In ROM scan mode, search a bounded window (<= size_field) for a valid MBM\0 footer.
        let search_limit = data
            .len()
            .min(MCM_HEADER_LEN.saturating_add(size_field as usize));
        let mut found_footer = None;
        let mut pos = MCM_HEADER_LEN + 4;
        while pos + MBM_HEADER_LEN <= search_limit {
            if &data[pos..pos + 4] == MAGIC_MBM {
                if let Ok(mbm) = validate_mbm_footer(data, pos) {
                    found_footer = Some((pos - MCM_HEADER_LEN, pos, mbm));
                    break;
                }
            }
            pos += 4;
        }
        let Some(hit) = found_footer else {
            return Err(Error::InvalidField {
                what: "MCM encoded MBM footer",
                value: 0,
            });
        };
        hit
    };

    let consumed_end = footer_offset
        .checked_add(MBM_HEADER_LEN)
        .ok_or(Error::InvalidField {
            what: "MCM consumed_end",
            value: footer_offset as u64,
        })?;
    Ok(MbmMcmAsset {
        kind: MbmMcmKind::McmMbm,
        mbm,
        mcm: McmHeader {
            size_field,
            page_class,
            block_count,
            flags,
            rom_ptr,
        },
        payload: data[MCM_HEADER_LEN..MCM_HEADER_LEN + payload_len].to_vec(),
        consumed_end,
    })
}

/// Scan `rom` on 4-byte aligned offsets for verified `studio.mbm-mcm` asset packs.
pub fn scan_mbm_mcm(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective = &rom[..rom.len().min(MAX_ROM_SIZE)];
    if effective.len() < HEADER_SIZE + HEADERS_LEN {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + HEADERS_LEN <= effective.len() {
        let tag = &effective[off..off + 4];
        if tag == MAGIC_MBM || tag == MAGIC_MCM {
            if let Ok(asset) = parse_inner(&effective[off..], true) {
                let size = asset.consumed_end;
                if size > 0 && off.checked_add(size).is_some_and(|e| e <= effective.len()) {
                    found.push(DetectedFormatExtent {
                        format_id: FORMAT_ID_MBM_MCM,
                        subkind: asset.kind.subkind(),
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

    pub(crate) fn make_valid_mbm_mcm(flags: u32, stored: usize, decoded: u32) -> Vec<u8> {
        let mut d = vec![0u8; HEADERS_LEN + stored];
        d[0..4].copy_from_slice(MAGIC_MBM);
        d[4..8].copy_from_slice(&0x0020_0400u32.to_le_bytes());
        d[8..10].copy_from_slice(&0x20u16.to_le_bytes());
        d[10..12].copy_from_slice(&0x10u16.to_le_bytes());
        d[14..16].copy_from_slice(&0x10u16.to_le_bytes());
        d[16..20].copy_from_slice(&0x0800_1000u32.to_le_bytes());
        d[20..24].copy_from_slice(&(0x0800_0000u32 + stored as u32).to_le_bytes());
        d[24..28].copy_from_slice(MAGIC_MCM);
        d[28..32].copy_from_slice(&decoded.to_le_bytes());
        d[32..36].copy_from_slice(&MCM_CONSTANT_WORD.to_le_bytes());
        d[36..40].copy_from_slice(&1u32.to_le_bytes());
        d[40..44].copy_from_slice(&flags.to_le_bytes());
        d[44..48].copy_from_slice(&0x0800_0000u32.to_le_bytes());
        for (i, b) in d[HEADERS_LEN..].iter_mut().enumerate() {
            *b = (i & 0xFF) as u8;
        }
        d
    }

    fn make_valid_mcm_mbm_raw() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(MAGIC_MCM);
        v.extend_from_slice(&64u32.to_le_bytes());
        v.extend_from_slice(&MCM_CONSTANT_WORD.to_le_bytes());
        v.extend_from_slice(&1u32.to_le_bytes());
        v.extend_from_slice(&FLAGS_RAW.to_le_bytes());
        v.extend_from_slice(&0x0800_0000u32.to_le_bytes());
        v.extend_from_slice(&[0x7Fu8; 64]);
        v.extend_from_slice(MAGIC_MBM);
        v.extend_from_slice(&0x0400u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(&0u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(&0x0800_0000u32.to_le_bytes());
        v.extend_from_slice(&0x0800_0000u32.to_le_bytes());
        v
    }

    #[test]
    fn parses_and_roundtrips_mbm_and_mcm_byte_identically() {
        let mbm_raw = make_valid_mbm_mcm(FLAGS_RAW, 64, 64);
        assert!(detect(&mbm_raw));
        let a = parse(&mbm_raw).unwrap();
        assert_eq!(a.kind, MbmMcmKind::MbmMcm);
        assert_eq!(a.consumed_end, mbm_raw.len());
        assert_eq!(a.to_bytes().unwrap(), mbm_raw);

        let mbm_enc = make_valid_mbm_mcm(FLAGS_ENCODED, 64, 256);
        assert!(detect(&mbm_enc));
        let b = parse(&mbm_enc).unwrap();
        assert_eq!(b.to_bytes().unwrap(), mbm_enc);

        let mcm = make_valid_mcm_mbm_raw();
        assert!(detect(&mcm));
        let c = parse(&mcm).unwrap();
        assert_eq!(c.kind, MbmMcmKind::McmMbm);
        assert_eq!(c.consumed_end, mcm.len());
        assert_eq!(c.to_bytes().unwrap(), mcm);
    }

    #[test]
    fn rejects_malformed_truncated_and_random_inputs() {
        assert!(!detect(&[]));
        assert!(parse(&[]).is_err());
        let good = make_valid_mbm_mcm(FLAGS_RAW, 64, 64);
        assert!(!detect(&good[..HEADERS_LEN - 1]));
        assert!(parse(&good[..HEADERS_LEN + 63]).is_err());

        let bad_enc = make_valid_mbm_mcm(FLAGS_ENCODED, 64, 64);
        assert!(!detect(&bad_enc));
        assert!(parse(&bad_enc).is_err());

        let mut bad_const = good.clone();
        bad_const[32] = 1;
        assert!(!detect(&bad_const));
        assert!(parse(&bad_const).is_err());

        let mut seed = 0x2468_ACE0u64;
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
