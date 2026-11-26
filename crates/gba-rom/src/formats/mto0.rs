//! `MTO\0` tagged key/value record table parser, symmetric writer, and ROM scanner.
//!
//! Built directly on `formatkit-layout` `0.1.17` [`FixedRecordTableSpec`] +
//! [`TableTermination::FieldIn`] + [`pack_fixed_record_table`]:
//! - **Big variant**: 12-byte `MTO\0`-tagged `(key: u32, value: u32)` records terminated by a
//!   sentinel record whose key is in `{0xFFFFFFFF, 0xC0300C03}`.
//! - **Compact variant**: 12-byte header (`MTO\0`, `count`, `param`, `order`) + `count` * 16-byte
//!   fixed records + optional zero padding + counted NUL-terminated printable ASCII string pool +
//!   optional trailing zero `MTO\0` slots.

use formatkit_core::{bytes_at, u32_at, Error, Result};
use formatkit_layout::{
    pack_fixed_record_table, parse_fixed_record_table, FixedRecordInput, FixedRecordTableSpec,
    IntFieldSpec, OffsetBase, PackPolicy, SizeMode, TableTermination,
};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_MTO0};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_MTO0: &[u8; 4] = b"MTO\0";
const BIG_STRIDE: usize = 12;
const SMALL_STRIDE: usize = 16;
const SMALL_HEADER: usize = 12;
const SENTINEL_A: u32 = 0xFFFF_FFFF;
const SENTINEL_B: u32 = 0xC030_0C03;
const BIG_SENTINELS: &[u64] = &[SENTINEL_A as u64, SENTINEL_B as u64];
const MAX_RECORDS: usize = 16384;
const SMALL_MAX_COUNT: u32 = 64;
const MAX_STRINGS: usize = 256;
const MAX_STRING_LEN: usize = 512;
const MAX_SKIP: usize = 4096;

const BIG_TABLE_SPEC: FixedRecordTableSpec = FixedRecordTableSpec::new(
    BIG_STRIDE,
    0,
    MAX_RECORDS,
    None,
    SizeMode::FixedSlot(0),
    TableTermination::FieldIn {
        field: IntFieldSpec::u32_le(4),
        values: BIG_SENTINELS,
    },
)
.with_offset_base(OffsetBase::TableEnd);

/// Variant within `studio.mto0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mto0Kind {
    Big,
    Compact,
}

impl Mto0Kind {
    pub const fn subkind(self) -> &'static str {
        match self {
            Self::Big => "mto0_big",
            Self::Compact => "mto0_compact",
        }
    }
}

/// Parsed `studio.mto0` asset with exact binary round-trip state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mto0Asset {
    Big {
        records: Vec<(u32, u32)>,
        sentinel_key: u32,
        sentinel_value: u32,
        consumed_end: usize,
    },
    Compact {
        param: u32,
        order: u32,
        records: Vec<[u32; 4]>,
        pad_before_pool: usize,
        strings: Vec<Vec<u8>>,
        pad_after_pool: usize,
        trailing_zero_slots: usize,
        consumed_end: usize,
    },
}

impl Mto0Asset {
    pub const fn kind(&self) -> Mto0Kind {
        match self {
            Self::Big { .. } => Mto0Kind::Big,
            Self::Compact { .. } => Mto0Kind::Compact,
        }
    }

    pub const fn consumed_end(&self) -> usize {
        match self {
            Self::Big { consumed_end, .. } | Self::Compact { consumed_end, .. } => *consumed_end,
        }
    }

    /// Serialize this table back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Fast, panic-free structural detector for `studio.mto0` record tables.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse an `MTO\0` tagged key/value or compact record table from the start of `data`.
pub fn parse(data: &[u8]) -> Result<Mto0Asset> {
    if data.len() < 4 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 4,
            available: data.len(),
        });
    }
    if &data[..4] != MAGIC_MTO0 {
        let found = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        return Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_MTO0),
            found,
        });
    }
    if let Ok(big) = parse_big(data) {
        return Ok(big);
    }
    parse_compact(data)
}

/// Serialize a parsed `studio.mto0` asset back into its exact binary representation.
pub fn pack(asset: &Mto0Asset) -> Result<Vec<u8>> {
    match asset {
        Mto0Asset::Big {
            records,
            sentinel_key,
            sentinel_value,
            ..
        } => {
            if records.is_empty() || records.len() >= MAX_RECORDS {
                return Err(Error::InvalidField {
                    what: "MTO0 big record count",
                    value: records.len() as u64,
                });
            }
            if *sentinel_key != SENTINEL_A && *sentinel_key != SENTINEL_B {
                return Err(Error::InvalidField {
                    what: "MTO0 sentinel key",
                    value: u64::from(*sentinel_key),
                });
            }
            let mut templates: Vec<[u8; BIG_STRIDE]> = Vec::with_capacity(records.len());
            for &(k, v) in records {
                let mut row = [0u8; BIG_STRIDE];
                row[0..4].copy_from_slice(MAGIC_MTO0);
                row[4..8].copy_from_slice(&k.to_le_bytes());
                row[8..12].copy_from_slice(&v.to_le_bytes());
                templates.push(row);
            }
            let inputs: Vec<FixedRecordInput<'_>> = templates
                .iter()
                .map(|row| FixedRecordInput {
                    name: None,
                    key: None,
                    payload: &[],
                    record_template: Some(row.as_slice()),
                })
                .collect();
            let mut out = pack_fixed_record_table(&BIG_TABLE_SPEC, &PackPolicy::TIGHT, &[], &inputs)
                .map_err(|_| Error::InvalidField {
                    what: "MTO0 pack_fixed_record_table",
                    value: records.len() as u64,
                })?;
            // Stamp the exact MTO\0 tag, sentinel_key, and sentinel_value into the sentinel row.
            let sentinel_start = records.len().checked_mul(BIG_STRIDE).ok_or(Error::InvalidField {
                what: "MTO0 sentinel offset",
                value: records.len() as u64,
            })?;
            if sentinel_start + BIG_STRIDE != out.len() {
                return Err(Error::InvalidField {
                    what: "MTO0 packed length",
                    value: out.len() as u64,
                });
            }
            out[sentinel_start..sentinel_start + 4].copy_from_slice(MAGIC_MTO0);
            out[sentinel_start + 4..sentinel_start + 8]
                .copy_from_slice(&sentinel_key.to_le_bytes());
            out[sentinel_start + 8..sentinel_start + 12]
                .copy_from_slice(&sentinel_value.to_le_bytes());
            Ok(out)
        }
        Mto0Asset::Compact {
            param,
            order,
            records,
            pad_before_pool,
            strings,
            pad_after_pool,
            trailing_zero_slots,
            ..
        } => {
            let count = records.len();
            if count == 0 || count > SMALL_MAX_COUNT as usize {
                return Err(Error::InvalidField {
                    what: "MTO0 compact record count",
                    value: count as u64,
                });
            }
            let mut header = [0u8; SMALL_HEADER];
            header[0..4].copy_from_slice(MAGIC_MTO0);
            header[4..8].copy_from_slice(&(count as u32).to_le_bytes());
            header[8..12].copy_from_slice(&param.to_le_bytes());
            // Note: header layout is 12 bytes (MTO\0, count, param) or 16?
            // Wait: in compact format, SMALL_HEADER = 12 (magic + count + param), and `order` is
            // the 3rd u32 word in `small_sample`? Let's preserve exact 12-byte header (`magic`, `count`, `param`)!
            let _ = order;
            let spec = FixedRecordTableSpec::new(
                SMALL_STRIDE,
                SMALL_HEADER,
                SMALL_MAX_COUNT as usize,
                None,
                SizeMode::FixedSlot(0),
                TableTermination::Counted(count),
            )
            .with_offset_base(OffsetBase::TableEnd);
            let mut templates: Vec<[u8; SMALL_STRIDE]> = Vec::with_capacity(count);
            for rec in records {
                let mut row = [0u8; SMALL_STRIDE];
                row[0..4].copy_from_slice(&rec[0].to_le_bytes());
                row[4..8].copy_from_slice(&rec[1].to_le_bytes());
                row[8..12].copy_from_slice(&rec[2].to_le_bytes());
                row[12..16].copy_from_slice(&rec[3].to_le_bytes());
                templates.push(row);
            }
            let inputs: Vec<FixedRecordInput<'_>> = templates
                .iter()
                .map(|row| FixedRecordInput {
                    name: None,
                    key: None,
                    payload: &[],
                    record_template: Some(row.as_slice()),
                })
                .collect();
            let mut out = pack_fixed_record_table(&spec, &PackPolicy::TIGHT, &header, &inputs)
                .map_err(|_| Error::InvalidField {
                    what: "MTO0 compact pack_fixed_record_table",
                    value: count as u64,
                })?;
            out.resize(out.len().saturating_add(*pad_before_pool), 0);
            out.extend_from_slice(&(strings.len() as u32).to_le_bytes());
            for s in strings {
                out.extend_from_slice(s);
                out.push(0);
            }
            out.resize(out.len().saturating_add(*pad_after_pool), 0);
            for _ in 0..*trailing_zero_slots {
                out.extend_from_slice(MAGIC_MTO0);
                out.extend_from_slice(&0u32.to_le_bytes());
                out.extend_from_slice(&0u32.to_le_bytes());
            }
            Ok(out)
        }
    }
}

fn is_sentinel(key: u32) -> bool {
    key == SENTINEL_A || key == SENTINEL_B
}

fn parse_big(data: &[u8]) -> Result<Mto0Asset> {
    if data.len() < BIG_STRIDE * 2 || bytes_at(data, BIG_STRIDE, 4)? != MAGIC_MTO0 {
        return Err(Error::InvalidField {
            what: "MTO0 big second record tag",
            value: 0,
        });
    }
    let entries = parse_fixed_record_table(data, &BIG_TABLE_SPEC).map_err(|_| {
        Error::InvalidField {
            what: "MTO0 big FixedRecordTableSpec",
            value: 0,
        }
    })?;
    if entries.is_empty() || entries.len() >= MAX_RECORDS {
        return Err(Error::InvalidField {
            what: "MTO0 big record count",
            value: entries.len() as u64,
        });
    }
    let mut records = Vec::with_capacity(entries.len());
    for entry in &entries {
        let off = entry.record_offset;
        if bytes_at(data, off, 4)? != MAGIC_MTO0 {
            return Err(Error::InvalidField {
                what: "MTO0 big record tag",
                value: off as u64,
            });
        }
        let key = u32_at(data, off + 4)?;
        let val = u32_at(data, off + 8)?;
        if is_sentinel(key) {
            return Err(Error::InvalidField {
                what: "MTO0 big early sentinel",
                value: u64::from(key),
            });
        }
        records.push((key, val));
    }
    let term_off = entries
        .len()
        .checked_mul(BIG_STRIDE)
        .ok_or(Error::InvalidField {
            what: "MTO0 sentinel offset",
            value: entries.len() as u64,
        })?;
    let consumed_end = term_off
        .checked_add(BIG_STRIDE)
        .ok_or(Error::InvalidField {
            what: "MTO0 big end",
            value: term_off as u64,
        })?;
    if consumed_end > data.len() || bytes_at(data, term_off, 4)? != MAGIC_MTO0 {
        return Err(Error::InvalidField {
            what: "MTO0 big sentinel tag",
            value: term_off as u64,
        });
    }
    let sentinel_key = u32_at(data, term_off + 4)?;
    let sentinel_value = u32_at(data, term_off + 8)?;
    if !is_sentinel(sentinel_key) {
        return Err(Error::InvalidField {
            what: "MTO0 big sentinel key",
            value: u64::from(sentinel_key),
        });
    }
    Ok(Mto0Asset::Big {
        records,
        sentinel_key,
        sentinel_value,
        consumed_end,
    })
}

fn is_printable_pool_byte(b: u8) -> bool {
    (0x20..=0x7E).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t'
}

fn cstring_at(data: &[u8], pos: usize) -> bool {
    let mut len = 0usize;
    while len <= MAX_STRING_LEN {
        let Some(&b) = data.get(pos.saturating_add(len)) else {
            return false;
        };
        if b == 0 {
            return len >= 3;
        }
        if !is_printable_pool_byte(b) {
            return false;
        }
        len += 1;
    }
    false
}

fn parse_compact(data: &[u8]) -> Result<Mto0Asset> {
    if data.len() < SMALL_HEADER + SMALL_STRIDE + 4 {
        return Err(Error::Truncated {
            offset: 0,
            needed: SMALL_HEADER + SMALL_STRIDE + 4,
            available: data.len(),
        });
    }
    if bytes_at(data, BIG_STRIDE, 4).ok() == Some(MAGIC_MTO0.as_slice()) {
        return Err(Error::InvalidField {
            what: "MTO0 compact collides with big tag",
            value: 0,
        });
    }
    let n = u32_at(data, 4)?;
    let param = u32_at(data, 8)?;
    if !(1..=SMALL_MAX_COUNT).contains(&n) {
        return Err(Error::InvalidField {
            what: "MTO0 compact count",
            value: u64::from(n),
        });
    }
    let count = n as usize;
    let rec_end = SMALL_HEADER
        .checked_add(count.checked_mul(SMALL_STRIDE).ok_or(Error::InvalidField {
            what: "MTO0 compact records size",
            value: count as u64,
        })?)
        .ok_or(Error::InvalidField {
            what: "MTO0 compact rec_end",
            value: count as u64,
        })?;
    if rec_end.checked_add(4).is_none_or(|e| e > data.len()) {
        return Err(Error::Truncated {
            offset: 0,
            needed: rec_end.saturating_add(4),
            available: data.len(),
        });
    }

    // Require a printable C-string within 16 bytes after rec_end (anti-FP invariant).
    let mut found_cstring = false;
    for delta in 0..16usize {
        if rec_end + delta < data.len() && cstring_at(data, rec_end + delta) {
            found_cstring = true;
            break;
        }
    }
    if !found_cstring {
        return Err(Error::InvalidField {
            what: "MTO0 compact pool cstring",
            value: rec_end as u64,
        });
    }

    let spec = FixedRecordTableSpec::new(
        SMALL_STRIDE,
        SMALL_HEADER,
        SMALL_MAX_COUNT as usize,
        None,
        SizeMode::FixedSlot(0),
        TableTermination::Counted(count),
    );
    let entries = parse_fixed_record_table(data, &spec).map_err(|_| Error::InvalidField {
        what: "MTO0 compact FixedRecordTableSpec",
        value: count as u64,
    })?;
    let mut records = Vec::with_capacity(entries.len());
    for e in &entries {
        let o = e.record_offset;
        records.push([
            u32_at(data, o)?,
            u32_at(data, o + 4)?,
            u32_at(data, o + 8)?,
            u32_at(data, o + 12)?,
        ]);
    }

    let mut pos = rec_end;
    let mut pad_before_pool = 0usize;
    while pad_before_pool < MAX_SKIP && data.get(pos) == Some(&0) {
        pos += 1;
        pad_before_pool += 1;
    }
    if pad_before_pool >= MAX_SKIP {
        return Err(Error::InvalidField {
            what: "MTO0 compact pre-pool padding",
            value: pad_before_pool as u64,
        });
    }

    let scount = u32_at(data, pos)? as usize;
    if scount > MAX_STRINGS {
        return Err(Error::InvalidField {
            what: "MTO0 compact string count",
            value: scount as u64,
        });
    }
    pos = pos.checked_add(4).ok_or(Error::InvalidField {
        what: "MTO0 compact pool start",
        value: pos as u64,
    })?;

    let mut strings = Vec::with_capacity(scount);
    for _ in 0..scount {
        let start = pos;
        let mut len = 0usize;
        loop {
            if len > MAX_STRING_LEN {
                return Err(Error::InvalidField {
                    what: "MTO0 compact string length",
                    value: len as u64,
                });
            }
            let Some(&b) = data.get(pos) else {
                return Err(Error::Truncated {
                    offset: pos,
                    needed: 1,
                    available: 0,
                });
            };
            if b == 0 {
                pos += 1;
                break;
            }
            if !is_printable_pool_byte(b) {
                return Err(Error::InvalidField {
                    what: "MTO0 compact non-text byte",
                    value: u64::from(b),
                });
            }
            pos += 1;
            len += 1;
        }
        strings.push(data[start..start + len].to_vec());
    }

    let mut pad_after_pool = 0usize;
    while pad_after_pool < MAX_SKIP && data.get(pos) == Some(&0) {
        pos += 1;
        pad_after_pool += 1;
    }
    let mut trailing_zero_slots = 0usize;
    while trailing_zero_slots < MAX_SKIP && pos.checked_add(BIG_STRIDE).is_some_and(|e| e <= data.len())
    {
        if &data[pos..pos + 4] != MAGIC_MTO0
            || u32_at(data, pos + 4)? != 0
            || u32_at(data, pos + 8)? != 0
        {
            break;
        }
        pos += BIG_STRIDE;
        trailing_zero_slots += 1;
    }

    Ok(Mto0Asset::Compact {
        param,
        order: 0,
        records,
        pad_before_pool,
        strings,
        pad_after_pool,
        trailing_zero_slots,
        consumed_end: pos,
    })
}

/// Scan `rom` on 4-byte aligned offsets for verified `studio.mto0` record tables.
pub fn scan_mto0(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective = &rom[..rom.len().min(MAX_ROM_SIZE)];
    if effective.len() < HEADER_SIZE + BIG_STRIDE * 2 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + BIG_STRIDE * 2 <= effective.len() {
        if &effective[off..off + 4] == MAGIC_MTO0 {
            if let Ok(asset) = parse(&effective[off..]) {
                let size = asset.consumed_end();
                if size > 0 && off.checked_add(size).is_some_and(|e| e <= effective.len()) {
                    found.push(DetectedFormatExtent {
                        format_id: FORMAT_ID_MTO0,
                        subkind: asset.kind().subkind(),
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

    pub(crate) fn make_valid_mto0_big(sentinel: u32, sval: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for (k, v) in [(3000u32, 0u32), (4500, 13), (17157, 1)] {
            out.extend_from_slice(MAGIC_MTO0);
            out.extend_from_slice(&k.to_le_bytes());
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(MAGIC_MTO0);
        out.extend_from_slice(&sentinel.to_le_bytes());
        out.extend_from_slice(&sval.to_le_bytes());
        out
    }

    fn make_valid_mto0_compact() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC_MTO0);
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&42u32.to_le_bytes());
        for rec in [[4u32, 8, 74, 1], [6, 88, 106, 0]] {
            for w in rec {
                out.extend_from_slice(&w.to_le_bytes());
            }
        }
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(b"ABCD\0EFGH\0");
        out
    }

    #[test]
    fn parses_and_roundtrips_mto0_variants_byte_identically() {
        let big_a = make_valid_mto0_big(SENTINEL_A, 9);
        assert!(detect(&big_a));
        let pa = parse(&big_a).unwrap();
        assert_eq!(pa.kind(), Mto0Kind::Big);
        assert_eq!(pa.consumed_end(), big_a.len());
        assert_eq!(pa.to_bytes().unwrap(), big_a);

        let big_b = make_valid_mto0_big(SENTINEL_B, 0x5F005E);
        assert!(detect(&big_b));
        let pb = parse(&big_b).unwrap();
        assert_eq!(pb.to_bytes().unwrap(), big_b);

        let compact = make_valid_mto0_compact();
        assert!(detect(&compact));
        let pc = parse(&compact).unwrap();
        assert_eq!(pc.kind(), Mto0Kind::Compact);
        assert_eq!(pc.consumed_end(), compact.len());
        assert_eq!(pc.to_bytes().unwrap(), compact);
    }

    #[test]
    fn rejects_malformed_truncated_and_random_inputs() {
        assert!(!detect(&[]));
        assert!(parse(&[]).is_err());
        let big = make_valid_mto0_big(SENTINEL_A, 9);
        assert!(!detect(&big[..20]));
        assert!(parse(&big[..20]).is_err());

        let bad_sentinel = make_valid_mto0_big(0x1234, 0);
        assert!(!detect(&bad_sentinel));
        assert!(parse(&bad_sentinel).is_err());

        let mut seed = 0x48C1_59D2u64;
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
