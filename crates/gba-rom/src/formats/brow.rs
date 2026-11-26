//! `BROWN` named record directory and message bank parser, symmetric writer, and ROM scanner.
//!
//! Built directly on `formatkit-layout` `0.1.17` [`AlignedStringBankSpec`] /
//! [`pack_aligned_string_bank`] and [`FixedRecordTableSpec`] / [`pack_fixed_record_table`]:
//! - **Bank**: 4-byte-aligned NUL-terminated message bank followed by a stride-84 section index.
//! - **Dir**: Consecutive 64-byte fixed-stride named records (`8-byte name` + 6 header words +
//!   8 ROM offsets).
//! - **Small**: Consecutive 30-byte fixed-stride named records (`8-byte name` + 11 `u16 LE < 256`
//!   attributes).

use formatkit_core::{bytes_at, u32_at, Error, Result};
use formatkit_layout::{
    pack_aligned_string_bank, pack_fixed_record_table, parse_aligned_string_bank,
    parse_fixed_record_table, AlignedStringBankSpec, FixedRecordInput, FixedRecordTableSpec,
    NameEncoding, NameFieldSpec, OffsetBase, PackPolicy, SizeMode, StringBankEncoding,
    StringBankTermination, TableTermination,
};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_BROW};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_BROWN: &[u8; 5] = b"BROWN";
const BANK_SECTION_STRIDE: u32 = 84;
const DIR_RECORD_LEN: usize = 64;
const SMALL_RECORD_LEN: usize = 30;
const NAME_LEN: usize = 8;
const DIR_OFFSET_COUNT: u32 = 8;
const MAX_MESSAGE_LEN: usize = 255;
const MAX_ROM_OFFSET: u32 = 0x0200_0000;
const MAX_DIR_RECORDS: usize = 4096;

/// Variant within `studio.brow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowKind {
    Bank,
    Dir,
    Small,
}

impl BrowKind {
    pub const fn subkind(self) -> &'static str {
        match self {
            Self::Bank => "brow_bank",
            Self::Dir => "brow_dir",
            Self::Small => "brow_small",
        }
    }
}

/// Parsed `studio.brow` asset with exact binary round-trip state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowAsset {
    Bank {
        strings: Vec<Vec<u8>>,
        section_tail: Vec<u8>,
        consumed_end: usize,
    },
    Dir {
        records: Vec<[u8; DIR_RECORD_LEN]>,
        consumed_end: usize,
    },
    Small {
        records: Vec<[u8; SMALL_RECORD_LEN]>,
        consumed_end: usize,
    },
}

impl BrowAsset {
    pub const fn kind(&self) -> BrowKind {
        match self {
            Self::Bank { .. } => BrowKind::Bank,
            Self::Dir { .. } => BrowKind::Dir,
            Self::Small { .. } => BrowKind::Small,
        }
    }

    pub const fn consumed_end(&self) -> usize {
        match self {
            Self::Bank { consumed_end, .. }
            | Self::Dir { consumed_end, .. }
            | Self::Small { consumed_end, .. } => *consumed_end,
        }
    }

    /// Serialize this asset back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Fast, panic-free structural detector for `studio.brow` assets.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse a `BROWN` message bank or named record directory from the start of `data`.
pub fn parse(data: &[u8]) -> Result<BrowAsset> {
    if data.len() < MAGIC_BROWN.len() {
        return Err(Error::Truncated {
            offset: 0,
            needed: MAGIC_BROWN.len(),
            available: data.len(),
        });
    }
    if &data[..MAGIC_BROWN.len()] != MAGIC_BROWN {
        let found = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        return Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*b"BROW"),
            found,
        });
    }
    if let Ok(bank) = parse_bank(data) {
        return Ok(bank);
    }
    if let Ok(dir) = parse_dir(data) {
        return Ok(dir);
    }
    parse_small(data)
}

/// Serialize a parsed `studio.brow` asset back into its exact binary representation.
pub fn pack(asset: &BrowAsset) -> Result<Vec<u8>> {
    match asset {
        BrowAsset::Bank {
            strings,
            section_tail,
            ..
        } => {
            if strings.is_empty() {
                return Err(Error::InvalidField {
                    what: "BROWN bank empty strings",
                    value: 0,
                });
            }
            let spec = AlignedStringBankSpec::aligned(0, 4, 8192)
                .with_encoding(StringBankEncoding::RawBytes)
                .with_termination(StringBankTermination::StopOnInvalid)
                .with_max_entry_len(MAX_MESSAGE_LEN);
            let slices: Vec<&[u8]> = strings.iter().map(Vec::as_slice).collect();
            let mut out = pack_aligned_string_bank(&spec, &PackPolicy::TIGHT, &[], &slices)
                .map_err(|_| Error::InvalidField {
                    what: "BROWN bank pack_aligned_string_bank",
                    value: strings.len() as u64,
                })?;
            out.extend_from_slice(section_tail);
            Ok(out)
        }
        BrowAsset::Dir { records, .. } => {
            if records.is_empty() || records.len() > MAX_DIR_RECORDS {
                return Err(Error::InvalidField {
                    what: "BROWN dir count",
                    value: records.len() as u64,
                });
            }
            let spec = FixedRecordTableSpec::new(
                DIR_RECORD_LEN,
                0,
                MAX_DIR_RECORDS,
                None,
                SizeMode::FixedSlot(0),
                TableTermination::Counted(records.len()),
            )
            .with_offset_base(OffsetBase::TableEnd)
            .with_name_field(NameFieldSpec::new(0, NAME_LEN, NameEncoding::Ascii));
            let inputs: Vec<FixedRecordInput<'_>> = records
                .iter()
                .map(|r| FixedRecordInput {
                    name: Some(trim_nul(&r[..NAME_LEN])),
                    key: None,
                    payload: &[],
                    record_template: Some(r.as_slice()),
                })
                .collect();
            pack_fixed_record_table(&spec, &PackPolicy::TIGHT, &[], &inputs).map_err(|_| {
                Error::InvalidField {
                    what: "BROWN dir pack_fixed_record_table",
                    value: records.len() as u64,
                }
            })
        }
        BrowAsset::Small { records, .. } => {
            if records.is_empty() || records.len() > MAX_DIR_RECORDS {
                return Err(Error::InvalidField {
                    what: "BROWN small count",
                    value: records.len() as u64,
                });
            }
            let spec = FixedRecordTableSpec::new(
                SMALL_RECORD_LEN,
                0,
                MAX_DIR_RECORDS,
                None,
                SizeMode::FixedSlot(0),
                TableTermination::Counted(records.len()),
            )
            .with_offset_base(OffsetBase::TableEnd)
            .with_name_field(NameFieldSpec::new(0, NAME_LEN, NameEncoding::Ascii));
            let inputs: Vec<FixedRecordInput<'_>> = records
                .iter()
                .map(|r| FixedRecordInput {
                    name: Some(trim_nul(&r[..NAME_LEN])),
                    key: None,
                    payload: &[],
                    record_template: Some(r.as_slice()),
                })
                .collect();
            pack_fixed_record_table(&spec, &PackPolicy::TIGHT, &[], &inputs).map_err(|_| {
                Error::InvalidField {
                    what: "BROWN small pack_fixed_record_table",
                    value: records.len() as u64,
                }
            })
        }
    }
}

fn trim_nul(field: &[u8]) -> &[u8] {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    &field[..end]
}

fn valid_name(field: &[u8]) -> bool {
    if field.len() != NAME_LEN || field[0] == 0 {
        return false;
    }
    let mut seen_nul = false;
    for &b in field {
        if b == 0 {
            seen_nul = true;
        } else if seen_nul || !(0x20..=0x7E).contains(&b) {
            return false;
        }
    }
    true
}

fn is_string_byte(b: u8) -> bool {
    (9..=13).contains(&b) || (0x20..=0x7E).contains(&b) || b >= 0x80
}

fn parse_bank(data: &[u8]) -> Result<BrowAsset> {
    let spec = AlignedStringBankSpec::aligned(0, 4, 8192)
        .with_encoding(StringBankEncoding::RawBytes)
        .with_termination(StringBankTermination::StopOnInvalid)
        .with_max_entry_len(MAX_MESSAGE_LEN);

    let bank = parse_aligned_string_bank(data, &spec).map_err(|_| Error::InvalidField {
        what: "BROWN bank string bank",
        value: 0,
    })?;

    // Collect leading valid message strings until a non-message byte or NUL-led index header.
    let mut strings = Vec::new();
    let mut off = 0usize;
    for entry in &bank.entries {
        if entry.offset != off {
            break;
        }
        let bytes = entry.bytes.as_slice();
        if bytes.is_empty() || !bytes.iter().all(|&b| is_string_byte(b)) {
            break;
        }
        let next = align_up_4(off.saturating_add(bytes.len()).saturating_add(1));
        if next > data.len() {
            return Err(Error::Truncated {
                offset: off,
                needed: next - off,
                available: data.len().saturating_sub(off),
            });
        }
        strings.push(entry.bytes.clone());
        off = next;
    }
    if strings.is_empty() {
        return Err(Error::InvalidField {
            what: "BROWN bank string count",
            value: 0,
        });
    }

    let consumed_end = validate_section_index(data, off)?;
    Ok(BrowAsset::Bank {
        strings,
        section_tail: data[off..consumed_end].to_vec(),
        consumed_end,
    })
}

fn validate_section_index(data: &[u8], base: usize) -> Result<usize> {
    let cnt = u32_at(data, base.checked_add(4).ok_or(Error::InvalidField {
        what: "BROWN index offset",
        value: base as u64,
    })?)?;
    let cnt_usize = cnt as usize;
    if cnt_usize == 0 || cnt_usize > 4096 {
        return Err(Error::InvalidField {
            what: "BROWN index count",
            value: u64::from(cnt),
        });
    }
    let table_bytes = cnt_usize
        .checked_mul(4)
        .and_then(|v| v.checked_add(20))
        .ok_or(Error::InvalidField {
            what: "BROWN table_bytes",
            value: cnt_usize as u64,
        })?;
    if u32_at(data, base + 8)? as usize != table_bytes
        || u32_at(data, base + 12)? != (table_bytes as u32).saturating_add(BANK_SECTION_STRIDE)
    {
        return Err(Error::InvalidField {
            what: "BROWN index header geometry",
            value: table_bytes as u64,
        });
    }
    let total = u32_at(data, base + 16)? as usize;
    if total > data.len() || base.checked_add(table_bytes).is_none_or(|e| e > data.len()) {
        return Err(Error::InvalidField {
            what: "BROWN index bounds",
            value: total as u64,
        });
    }
    let first = u32_at(data, base + 20)?;
    if (first as usize) < table_bytes {
        return Err(Error::InvalidField {
            what: "BROWN index first offset",
            value: u64::from(first),
        });
    }
    let expected_total = (first as usize)
        .checked_add(cnt_usize.checked_mul(BANK_SECTION_STRIDE as usize).ok_or(
            Error::InvalidField {
                what: "BROWN index span",
                value: cnt_usize as u64,
            },
        )?)
        .ok_or(Error::InvalidField {
            what: "BROWN index total",
            value: u64::from(first),
        })?;
    if expected_total != total {
        return Err(Error::InvalidField {
            what: "BROWN index total mismatch",
            value: total as u64,
        });
    }
    let mut prev = first;
    for i in 1..cnt_usize {
        let v = u32_at(data, base + 20 + i * 4)?;
        if v != prev.saturating_add(BANK_SECTION_STRIDE) {
            return Err(Error::InvalidField {
                what: "BROWN index stride",
                value: u64::from(v),
            });
        }
        prev = v;
    }
    Ok(total.max(base + table_bytes))
}

fn parse_dir(data: &[u8]) -> Result<BrowAsset> {
    if data.len() < DIR_RECORD_LEN {
        return Err(Error::Truncated {
            offset: 0,
            needed: DIR_RECORD_LEN,
            available: data.len(),
        });
    }
    let mut off = 0usize;
    let mut count = 0usize;
    let mut prev_first: Option<u32> = None;
    while data.len().saturating_sub(off) >= DIR_RECORD_LEN && count < MAX_DIR_RECORDS {
        let name = bytes_at(data, off, NAME_LEN)?;
        if !valid_name(name) {
            break;
        }
        let h0 = u32_at(data, off + 8)?;
        let h1 = u32_at(data, off + 12)?;
        let h2 = u32_at(data, off + 16)?;
        let h3 = u32_at(data, off + 20)?;
        let h4 = u32_at(data, off + 24)?;
        let h5 = u32_at(data, off + 28)?;
        if h0 >= 0x1_0000
            || (h0 & 0xFF) != h3
            || (h1 != 0x10 && h1 != 0x80)
            || !matches!(h2, 0x08 | 0x20 | 0x40 | 0x80 | 0x100)
            || h3 != DIR_OFFSET_COUNT
            || h4 != 0
            || h5 != 0
        {
            break;
        }
        let mut first = 0u32;
        let mut ok = true;
        for i in 0..DIR_OFFSET_COUNT as usize {
            let v = u32_at(data, off + 32 + i * 4)?;
            if v == 0 || !v.is_multiple_of(4) || v >= MAX_ROM_OFFSET {
                ok = false;
                break;
            }
            if i == 0 {
                first = v;
            }
        }
        if !ok {
            break;
        }
        if prev_first.is_some_and(|pf| first <= pf) {
            break;
        }
        prev_first = Some(first);
        count += 1;
        off += DIR_RECORD_LEN;
    }
    if count == 0 {
        return Err(Error::InvalidField {
            what: "BROWN dir count",
            value: 0,
        });
    }

    // Validate through FormatKit FixedRecordTableSpec.
    let spec = FixedRecordTableSpec::new(
        DIR_RECORD_LEN,
        0,
        MAX_DIR_RECORDS,
        None,
        SizeMode::FixedSlot(0),
        TableTermination::Counted(count),
    )
    .with_name_field(NameFieldSpec::new(0, NAME_LEN, NameEncoding::Ascii));
    let entries = parse_fixed_record_table(data, &spec).map_err(|_| Error::InvalidField {
        what: "BROWN dir FixedRecordTableSpec",
        value: count as u64,
    })?;
    let mut records = Vec::with_capacity(entries.len());
    for e in &entries {
        let mut arr = [0u8; DIR_RECORD_LEN];
        arr.copy_from_slice(&data[e.record_offset..e.record_offset + DIR_RECORD_LEN]);
        records.push(arr);
    }
    Ok(BrowAsset::Dir {
        records,
        consumed_end: off,
    })
}

fn parse_small(data: &[u8]) -> Result<BrowAsset> {
    if data.len() < SMALL_RECORD_LEN {
        return Err(Error::Truncated {
            offset: 0,
            needed: SMALL_RECORD_LEN,
            available: data.len(),
        });
    }
    let mut off = 0usize;
    let mut count = 0usize;
    while data.len().saturating_sub(off) >= SMALL_RECORD_LEN && count < MAX_DIR_RECORDS {
        let rec = bytes_at(data, off, SMALL_RECORD_LEN)?;
        if !valid_name(&rec[..NAME_LEN]) {
            break;
        }
        let attrs_ok = rec[NAME_LEN..]
            .as_chunks::<2>()
            .0
            .iter()
            .all(|pair| pair[1] == 0);
        if !attrs_ok {
            break;
        }
        count += 1;
        off += SMALL_RECORD_LEN;
    }
    if count == 0 {
        return Err(Error::InvalidField {
            what: "BROWN small count",
            value: 0,
        });
    }

    let spec = FixedRecordTableSpec::new(
        SMALL_RECORD_LEN,
        0,
        MAX_DIR_RECORDS,
        None,
        SizeMode::FixedSlot(0),
        TableTermination::Counted(count),
    )
    .with_name_field(NameFieldSpec::new(0, NAME_LEN, NameEncoding::Ascii));
    let entries = parse_fixed_record_table(data, &spec).map_err(|_| Error::InvalidField {
        what: "BROWN small FixedRecordTableSpec",
        value: count as u64,
    })?;
    let mut records = Vec::with_capacity(entries.len());
    for e in &entries {
        let mut arr = [0u8; SMALL_RECORD_LEN];
        arr.copy_from_slice(&data[e.record_offset..e.record_offset + SMALL_RECORD_LEN]);
        records.push(arr);
    }
    Ok(BrowAsset::Small {
        records,
        consumed_end: off,
    })
}

/// Scan `rom` on 4-byte aligned offsets for verified `studio.brow` assets.
pub fn scan_brow(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective = &rom[..rom.len().min(MAX_ROM_SIZE)];
    if effective.len() < HEADER_SIZE + SMALL_RECORD_LEN {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + SMALL_RECORD_LEN <= effective.len() {
        if &effective[off..off + MAGIC_BROWN.len()] == MAGIC_BROWN {
            if let Ok(asset) = parse(&effective[off..]) {
                let size = asset.consumed_end();
                if size > 0 && off.checked_add(size).is_some_and(|e| e <= effective.len()) {
                    found.push(DetectedFormatExtent {
                        format_id: FORMAT_ID_BROW,
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

    fn push_str4(out: &mut Vec<u8>, s: &[u8]) {
        out.extend_from_slice(s);
        out.push(0);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }

    fn make_valid_brow_bank() -> Vec<u8> {
        let mut data = Vec::new();
        push_str4(&mut data, b"BROWNTEST");
        push_str4(&mut data, b"AB");
        push_str4(&mut data, b"CD");
        push_str4(&mut data, b"EF");
        push_str4(&mut data, b"GH");
        let count: u32 = 2;
        let table_bytes: u32 = 20 + 4 * count;
        let blob_bytes: u32 = 60 + 84 * count;
        for w in [1u32, count, table_bytes, table_bytes + 84, blob_bytes, 60, 144] {
            data.extend_from_slice(&w.to_le_bytes());
        }
        data.resize(blob_bytes as usize, 0);
        data
    }

    pub(crate) fn make_valid_brow_dir() -> Vec<u8> {
        let mut data = Vec::new();
        for (r, name) in [b"BROWN001", b"BROWN002"].iter().enumerate() {
            data.extend_from_slice(&name[..]);
            for w in [0x1008u32, 0x80, 0x40, 8, 0, 0] {
                data.extend_from_slice(&w.to_le_bytes());
            }
            let base: u32 = if r == 0 { 0x1000 } else { 0x2000 };
            for i in 0..8 {
                data.extend_from_slice(&(base + i * 0x100).to_le_bytes());
            }
        }
        data
    }

    fn make_valid_brow_small() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(b"BROWN01\0");
        for i in 0..11u8 {
            data.push(i + 1);
            data.push(0);
        }
        data
    }

    #[test]
    fn parses_and_roundtrips_all_brow_variants_byte_identically() {
        for (raw, expected_kind) in [
            (make_valid_brow_bank(), BrowKind::Bank),
            (make_valid_brow_dir(), BrowKind::Dir),
            (make_valid_brow_small(), BrowKind::Small),
        ] {
            assert!(detect(&raw));
            let parsed = parse(&raw).unwrap();
            assert_eq!(parsed.kind(), expected_kind);
            assert_eq!(parsed.consumed_end(), raw.len());
            assert_eq!(parsed.to_bytes().unwrap(), raw);
        }
    }

    #[test]
    fn rejects_malformed_truncated_and_random_inputs() {
        assert!(!detect(&[]));
        assert!(parse(&[]).is_err());
        assert!(!detect(b"BROWN"));
        assert!(parse(b"BROWN").is_err());

        let dir = make_valid_brow_dir();
        assert!(!detect(&dir[..29]));
        let mut bad_dir = dir.clone();
        bad_dir[20] = 7;
        assert!(!detect(&bad_dir));
        assert!(parse(&bad_dir).is_err());

        let mut seed = 0x5AD2_7BF0u64;
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
