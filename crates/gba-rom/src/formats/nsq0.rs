//! `studio.nsq0` — `NSQ\0` 16-byte music event sequence table parser,
//! symmetric byte-identical writer, and ROM scanner.
//!
//! Built on `formatkit-layout` `FixedRecordTableSpec::with_emit_terminator(true)` and
//! `parse_fixed_record_table_with_extent`.

use formatkit_core::{u16_at, u32_at, Error, Result};
use formatkit_layout::{
    pack_fixed_record_table, parse_fixed_record_table_with_extent, FixedRecordInput,
    FixedRecordTableSpec, IntFieldSpec, OffsetBase, PackPolicy, SizeMode, TableTermination,
};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_NSQ0};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_NSQ0: &[u8; 4] = b"NSQ\0";
const NSQ_END_CMD: u8 = 0x2F;
const MAX_RECORDS: usize = 4096;

const NSQ_TABLE_SPEC: FixedRecordTableSpec = FixedRecordTableSpec::new(
    16,
    16,
    MAX_RECORDS,
    None,
    SizeMode::FixedSlot(0),
    TableTermination::FieldEquals {
        field: IntFieldSpec::u8(10),
        value: NSQ_END_CMD as u64,
    },
)
.with_offset_base(OffsetBase::TableEnd)
.with_emit_terminator(true);

/// One 16-byte `NSQ\0` event record (including the final `0x2F` END record).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NsqRecord {
    pub raw: [u8; 16],
}

/// Parsed `studio.nsq0` music event sequence table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nsq0Asset {
    pub start_tick: u16,
    pub header_param1: u8,
    pub header_param2: u8,
    /// Event records following the 16-byte header record, ending with the `0x2F` END record.
    pub records: Vec<NsqRecord>,
    pub consumed_end: usize,
}

impl Nsq0Asset {
    /// Serialize this `NSQ\0` table back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Fast, panic-free structural detector for `studio.nsq0` tables.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse an `NSQ\0` event sequence table from the start of `data`.
pub fn parse(data: &[u8]) -> Result<Nsq0Asset> {
    if data.len() < 40 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 40,
            available: data.len(),
        });
    }
    if &data[0..4] != MAGIC_NSQ0 {
        return Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_NSQ0),
            found: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        });
    }
    if data[4..8] != [0xCC; 4] || data[10] != 9 || data[13..16] != [0xCC; 3] {
        return Err(Error::InvalidField {
            what: "NSQ0 header fields",
            value: u64::from(data[10]),
        });
    }
    let start_tick = u16_at(data, 8)?;
    let header_param1 = data[11];
    let header_param2 = data[12];

    let res = parse_fixed_record_table_with_extent(data, &NSQ_TABLE_SPEC).map_err(|_| {
        Error::InvalidField {
            what: "NSQ0 record table",
            value: 0,
        }
    })?;
    if res.entries.is_empty() {
        return Err(Error::InvalidField {
            what: "NSQ0 empty table",
            value: 0,
        });
    }

    let trailer_end = res.consumed_end.checked_add(8).ok_or(Error::InvalidField {
        what: "NSQ0 trailer end",
        value: res.consumed_end as u64,
    })?;
    if trailer_end > data.len() || data[res.consumed_end..trailer_end] != [0u8; 8] {
        return Err(Error::InvalidField {
            what: "NSQ0 8-byte zero trailer",
            value: res.consumed_end as u64,
        });
    }

    let mut records = Vec::with_capacity(res.entries.len());
    let mut prev_tick = start_tick;
    let mut saw_end = false;

    for (idx, entry) in res.entries.iter().enumerate() {
        let slice = &data[entry.record_offset..entry.record_offset + 16];
        if slice[13..16] != [0xCC; 3] {
            return Err(Error::InvalidField {
                what: "NSQ0 record padding",
                value: idx as u64,
            });
        }
        let tick = u16_at(slice, 8)?;
        if tick < prev_tick {
            return Err(Error::InvalidField {
                what: "NSQ0 non-monotonic tick",
                value: u64::from(tick),
            });
        }
        prev_tick = tick;

        let cmd = slice[10];
        let p1 = slice[11];
        let p2 = slice[12];
        if idx + 1 == res.entries.len() {
            if cmd != NSQ_END_CMD || p1 != 0 || p2 != 0 {
                return Err(Error::InvalidField {
                    what: "NSQ0 END record",
                    value: u64::from(cmd),
                });
            }
            saw_end = true;
        } else {
            if !(cmd == 8 || cmd == 9) || p1 > 127 || p2 > 127 {
                return Err(Error::InvalidField {
                    what: "NSQ0 event command/params",
                    value: u64::from(cmd),
                });
            }
            let w0 = u32_at(slice, 0)?;
            let w1 = u32_at(slice, 4)?;
            let cond_a = w0 <= 0x7F;
            let cond_b = w1 == 0;
            if cond_a == cond_b {
                return Err(Error::InvalidField {
                    what: "NSQ0 voice/velocity invariant",
                    value: u64::from(w0),
                });
            }
        }
        let mut raw = [0u8; 16];
        raw.copy_from_slice(slice);
        records.push(NsqRecord { raw });
    }

    if !saw_end {
        return Err(Error::InvalidField {
            what: "NSQ0 missing END record",
            value: 0,
        });
    }

    Ok(Nsq0Asset {
        start_tick,
        header_param1,
        header_param2,
        records,
        consumed_end: trailer_end,
    })
}

/// Symmetrically pack an [`Nsq0Asset`] back into its exact binary representation.
pub fn pack(asset: &Nsq0Asset) -> Result<Vec<u8>> {
    let mut header = [0xCCu8; 16];
    header[0..4].copy_from_slice(MAGIC_NSQ0);
    header[8..10].copy_from_slice(&asset.start_tick.to_le_bytes());
    header[10] = 9;
    header[11] = asset.header_param1;
    header[12] = asset.header_param2;

    let inputs: Vec<FixedRecordInput<'_>> = asset
        .records
        .iter()
        .map(|r| FixedRecordInput {
            name: None,
            key: None,
            payload: &[],
            record_template: Some(&r.raw),
        })
        .collect();

    let mut out = pack_fixed_record_table(&NSQ_TABLE_SPEC, &PackPolicy::TIGHT, &header, &inputs)
        .map_err(|_| Error::InvalidField {
            what: "NSQ0 pack table",
            value: inputs.len() as u64,
        })?;
    out.extend_from_slice(&[0u8; 8]);
    Ok(out)
}

/// Scan a GBA ROM image for `studio.nsq0` music sequence tables.
pub fn scan_nsq0(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective_len = rom.len().min(MAX_ROM_SIZE);
    if effective_len < HEADER_SIZE + 40 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + 40 <= effective_len {
        if &rom[off..off + 4] == MAGIC_NSQ0 && rom[off + 4..off + 8] == [0xCC; 4] {
            if let Ok(asset) = parse(&rom[off..effective_len]) {
                let size = asset.consumed_end;
                found.push(DetectedFormatExtent {
                    format_id: FORMAT_ID_NSQ0,
                    subkind: "nsq0",
                    offset: off,
                    size,
                });
                off = align_up_4(off.saturating_add(size.max(4)));
                continue;
            }
        }
        off += 4;
    }
    found
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn make_valid_nsq0() -> Vec<u8> {
        let mut ev0 = [0xCCu8; 16];
        ev0[0..4].copy_from_slice(&100u32.to_le_bytes()); // velocity <= 0x7F
        ev0[4..8].copy_from_slice(&0x0000_0200u32.to_le_bytes()); // voice ref != 0
        ev0[8..10].copy_from_slice(&10u16.to_le_bytes()); // tick
        ev0[10] = 9; // note-on
        ev0[11] = 2; // voice idx
        ev0[12] = 60; // note

        let mut ev1 = [0xCCu8; 16];
        ev1[0..4].copy_from_slice(&0x0000_0200u32.to_le_bytes()); // voice ref > 0x7F
        ev1[4..8].copy_from_slice(&0u32.to_le_bytes()); // zero
        ev1[8..10].copy_from_slice(&20u16.to_le_bytes());
        ev1[10] = 8; // note-off
        ev1[11] = 2;
        ev1[12] = 60;

        let mut end = [0xCCu8; 16];
        end[0..4].copy_from_slice(&0u32.to_le_bytes());
        end[4..8].copy_from_slice(&0u32.to_le_bytes());
        end[8..10].copy_from_slice(&20u16.to_le_bytes());
        end[10] = NSQ_END_CMD;
        end[11] = 0;
        end[12] = 0;

        pack(&Nsq0Asset {
            start_tick: 0,
            header_param1: 2,
            header_param2: 60,
            records: vec![
                NsqRecord { raw: ev0 },
                NsqRecord { raw: ev1 },
                NsqRecord { raw: end },
            ],
            consumed_end: 0,
        })
        .unwrap()
    }

    #[test]
    fn round_trips_nsq0_with_emit_terminator_and_extent() {
        let mut raw = make_valid_nsq0();
        let exact = raw.len();
        raw.extend_from_slice(b"TRAILING_BYTES");
        let parsed = parse(&raw).unwrap();
        assert_eq!(parsed.records.len(), 3);
        assert_eq!(parsed.consumed_end, exact);
        assert_eq!(parsed.to_bytes().unwrap(), raw[..exact]);
    }

    #[test]
    fn rejects_malformed_nsq0() {
        assert!(!detect(b""));
        assert!(!detect(b"NSQ\0\xCC\xCC\xCC\xCC\0\0\x09\0\x3C\xCC\xCC\xCC"));
    }
}
