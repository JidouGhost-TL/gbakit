//! `studio.lv-dir` — `LV1`..`LV6` 4-byte-aligned level-name & GBA ROM-pointer directory parser,
//! symmetric byte-identical writer, and ROM scanner.
//!
//! Built on `formatkit-layout` `AlignedStringBankSpec` and `FixedRecordTableSpec`
//! with `parse_fixed_record_table_with_extent`.

use formatkit_core::{u32_at, Error, Result};
use formatkit_layout::{
    pack_aligned_string_bank, parse_aligned_string_bank, parse_fixed_record_table_with_extent,
    AlignedStringBankSpec, FixedRecordTableSpec, IntFieldSpec, OffsetBase, PackPolicy, SizeMode,
    StringBankEncoding, StringBankTermination, TableTermination,
};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_LV_DIR};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const PTR_TABLE_SPEC: FixedRecordTableSpec = FixedRecordTableSpec::new(
    4,
    0,
    4096,
    None,
    SizeMode::FixedSlot(0),
    TableTermination::FieldNotEquals {
        field: IntFieldSpec::u8(3),
        value: 0x08,
    },
)
.with_offset_base(OffsetBase::TableEnd);

/// One level group: 4-byte-aligned NUL-terminated ASCII names followed by optional `0x08xxxxxx` pointers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LvDirGroup {
    pub names: Vec<Vec<u8>>,
    pub pointers: Vec<u32>,
}

/// Parsed `studio.lv-dir` level-name & ROM-pointer directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LvDirAsset {
    pub groups: Vec<LvDirGroup>,
    pub total_names: usize,
    pub consumed_end: usize,
}

impl LvDirAsset {
    /// Serialize this directory back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Fast, panic-free structural detector for `studio.lv-dir` directories.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

fn is_lv_prefix(bytes: &[u8]) -> bool {
    bytes.len() >= 3 && bytes[0] == b'L' && bytes[1] == b'V' && (b'1'..=b'6').contains(&bytes[2])
}

/// Parse one or more chained `LV1`..`LV6` aligned name + ROM-pointer groups from `data[0..]`.
pub fn parse(data: &[u8]) -> Result<LvDirAsset> {
    if data.len() < 8 || !is_lv_prefix(data) {
        return Err(Error::InvalidField {
            what: "LV-dir prefix",
            value: data.len() as u64,
        });
    }

    let mut pos = 0usize;
    let mut groups = Vec::new();
    let mut total_names = 0usize;

    while pos < data.len() && groups.len() < 6 {
        if !is_lv_prefix(&data[pos..]) {
            break;
        }
        let bank_spec = AlignedStringBankSpec::aligned(pos, 4, 4096)
            .with_encoding(StringBankEncoding::PrintableAscii)
            .with_termination(StringBankTermination::StopOnInvalid)
            .with_max_entry_len(40);
        let bank = parse_aligned_string_bank(data, &bank_spec).map_err(|_| Error::InvalidField {
            what: "LV-dir aligned string bank",
            value: pos as u64,
        })?;
        // Keep only entries matching the LV[1..=6] prefix and non-empty.
        let mut names = Vec::new();
        let mut name_end = pos;
        for entry in &bank.entries {
            if entry.bytes.is_empty() || !is_lv_prefix(&entry.bytes) {
                break;
            }
            names.push(entry.bytes.clone());
            name_end = align_up_4(entry.offset + entry.len + 1);
        }
        if names.len() < 2 {
            if groups.is_empty() {
                return Err(Error::InvalidField {
                    what: "LV-dir minimum names",
                    value: names.len() as u64,
                });
            }
            break;
        }
        pos = name_end;

        // Parse optional contiguous run of 0x08xxxxxx GBA ROM pointers using FixedRecordTableSpec.
        let mut pointers = Vec::new();
        if pos + 4 <= data.len() && data[pos + 3] == 0x08 {
            let mut ptr_spec = PTR_TABLE_SPEC;
            ptr_spec.table_offset = pos;
            if let Ok(res) = parse_fixed_record_table_with_extent(data, &ptr_spec) {
                for e in &res.entries {
                    pointers.push(u32_at(data, e.record_offset)?);
                }
                pos += pointers.len() * 4;
            }
        }

        total_names += names.len();
        groups.push(LvDirGroup { names, pointers });
    }

    if groups.is_empty() || total_names < 2 {
        return Err(Error::InvalidField {
            what: "LV-dir groups",
            value: total_names as u64,
        });
    }

    Ok(LvDirAsset {
        groups,
        total_names,
        consumed_end: pos,
    })
}

/// Symmetrically pack an [`LvDirAsset`] back into its exact binary representation.
pub fn pack(asset: &LvDirAsset) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for group in &asset.groups {
        let refs: Vec<&[u8]> = group.names.iter().map(Vec::as_slice).collect();
        let bank_spec = AlignedStringBankSpec::aligned(0, 4, 4096)
            .with_encoding(StringBankEncoding::PrintableAscii)
            .with_termination(StringBankTermination::Counted(refs.len()));
        let bank_bytes = pack_aligned_string_bank(&bank_spec, &PackPolicy::TIGHT, &[], &refs)
            .map_err(|_| Error::InvalidField {
                what: "LV-dir pack names",
                value: refs.len() as u64,
            })?;
        out.extend_from_slice(&bank_bytes);
        for &ptr in &group.pointers {
            out.extend_from_slice(&ptr.to_le_bytes());
        }
    }
    Ok(out)
}

/// Scan a GBA ROM image for `studio.lv-dir` directories (requiring at least one pointer table
/// or >= 4 consecutive aligned `LVn_` names for zero-FP ROM scanning).
pub fn scan_lv_dir(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective_len = rom.len().min(MAX_ROM_SIZE);
    if effective_len < HEADER_SIZE + 16 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + 16 <= effective_len {
        if is_lv_prefix(&rom[off..]) && rom[off + 3] == b'_' {
            if let Ok(asset) = parse(&rom[off..effective_len]) {
                let has_ptrs = asset.groups.iter().any(|g| !g.pointers.is_empty());
                if has_ptrs || asset.total_names >= 4 {
                    let size = asset.consumed_end;
                    found.push(DetectedFormatExtent {
                        format_id: FORMAT_ID_LV_DIR,
                        subkind: "lv-dir",
                        offset: off,
                        size,
                    });
                    off = align_up_4(off.saturating_add(size.max(4)));
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

    pub(crate) fn make_valid_lv_dir() -> Vec<u8> {
        pack(&LvDirAsset {
            groups: vec![
                LvDirGroup {
                    names: vec![b"LV1_01_00".to_vec(), b"LV1_02_00".to_vec()],
                    pointers: vec![0x0812_BC54, 0x0812_BC60],
                },
                LvDirGroup {
                    names: vec![b"LV2_01_00".to_vec(), b"LV2_02_00".to_vec()],
                    pointers: vec![0x0812_BC54, 0x0812_BC6C],
                },
            ],
            total_names: 4,
            consumed_end: 0,
        })
        .unwrap()
    }

    #[test]
    fn round_trips_lv_dir_with_string_bank_and_pointers() {
        let mut raw = make_valid_lv_dir();
        let exact = raw.len();
        raw.extend_from_slice(&[0xFF; 16]);
        let parsed = parse(&raw).unwrap();
        assert_eq!(parsed.groups.len(), 2);
        assert_eq!(parsed.total_names, 4);
        assert_eq!(parsed.consumed_end, exact);
        assert_eq!(parsed.to_bytes().unwrap(), raw[..exact]);
    }

    #[test]
    fn rejects_malformed_lv_dir() {
        assert!(!detect(b""));
        assert!(!detect(b"LV1_ONLY_ONE\0\0\0\0\xFF\xFF\xFF\xFF"));
    }
}
