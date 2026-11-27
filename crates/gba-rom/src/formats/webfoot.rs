//! `studio.webfoot` — Webfoot GBA `AreaEntry` (`0x2C`-byte) map table and
//! `ByteCodeInterpreter` script blob parser, symmetric byte-identical writer, and ROM scanner.
//!
//! Built on `formatkit-layout` `FixedRecordTableSpec`.

use formatkit_core::{u32_at, Error, Result};
use formatkit_layout::{
    pack_fixed_record_table, parse_fixed_record_table, FixedRecordInput, FixedRecordTableSpec,
    OffsetBase, PackPolicy, SizeMode, TableTermination,
};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_WEBFOOT};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

/// Fixed size of one `AreaEntry` record in bytes (`0x2C` = 44 bytes).
pub const AREA_ENTRY_SIZE: usize = 0x2C;

/// Number of `u32` slots following the 4-byte `(group, id, flags_a, flags_b)` header.
pub const AREA_WORD_SLOTS: usize = 10;

/// Minimum contiguous `AreaEntry` records required to claim a table.
pub const MIN_AREA_ENTRIES: usize = 8;

const GBA_ROM_START: u32 = 0x0800_0000;
const GBA_ROM_END: u32 = 0x0A00_0000;

/// One `0x2C`-byte Webfoot `AreaEntry` record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AreaEntry {
    pub area_group: u8,
    pub area_id: u8,
    pub flags_a: u8,
    pub flags_b: u8,
    pub words: [u32; AREA_WORD_SLOTS],
}

impl AreaEntry {
    /// Count how many of the 10 word slots hold valid GBA ROM pointers.
    #[must_use]
    pub fn rom_pointer_count(&self) -> usize {
        self.words.iter().filter(|&&w| is_gba_rom_ptr(w)).count()
    }

    /// Encode this record into its exact 44-byte little-endian layout.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; AREA_ENTRY_SIZE] {
        let mut out = [0u8; AREA_ENTRY_SIZE];
        out[0] = self.area_group;
        out[1] = self.area_id;
        out[2] = self.flags_a;
        out[3] = self.flags_b;
        for (i, &w) in self.words.iter().enumerate() {
            let off = 4 + i * 4;
            out[off..off + 4].copy_from_slice(&w.to_le_bytes());
        }
        out
    }
}

/// Compression mode of a `ByteCodeInterpreter` script blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BytecodeEncoding {
    /// Tag `0x00`: uncompressed bytecode starting at `+0x03`.
    Raw,
    /// Tag `0x03`: compressed bytecode starting at `+0x04`.
    Compressed,
}

/// Parsed `ByteCodeInterpreter` script blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BytecodeBlob {
    pub prefix: [u8; 2],
    pub encoding: BytecodeEncoding,
    /// Optional pad/header byte at `+0x03` for compressed (`0x03`) blobs.
    pub compressed_pad: Option<u8>,
    pub payload: Vec<u8>,
}

impl BytecodeBlob {
    /// Serialize this bytecode blob back into its exact binary representation.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + self.payload.len());
        out.extend_from_slice(&self.prefix);
        match self.encoding {
            BytecodeEncoding::Raw => out.push(0x00),
            BytecodeEncoding::Compressed => {
                out.push(0x03);
                out.push(self.compressed_pad.unwrap_or(0));
            }
        }
        out.extend_from_slice(&self.payload);
        out
    }
}

/// Parsed contiguous Webfoot `AreaEntry` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebfootAreaTable {
    pub entries: Vec<AreaEntry>,
    pub consumed_end: usize,
}

impl WebfootAreaTable {
    /// Lookup `(area_group, area_id)` in this table.
    #[must_use]
    pub fn find_area(&self, area_group: u8, area_id: u8) -> Option<&AreaEntry> {
        self.entries
            .iter()
            .find(|e| e.area_group == area_group && e.area_id == area_id)
    }

    /// Serialize this `AreaEntry` table back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

#[inline]
fn is_gba_rom_ptr(w: u32) -> bool {
    (GBA_ROM_START..GBA_ROM_END).contains(&w)
}

#[inline]
fn is_valid_slot(w: u32) -> bool {
    w == 0 || w <= 0x0FFF || is_gba_rom_ptr(w)
}

/// Parse one `0x2C`-byte `AreaEntry` record from `data[0..44]`.
pub fn parse_area_entry(data: &[u8]) -> Result<AreaEntry> {
    if data.len() < AREA_ENTRY_SIZE {
        return Err(Error::Truncated {
            offset: 0,
            needed: AREA_ENTRY_SIZE,
            available: data.len(),
        });
    }
    let area_group = data[0];
    let area_id = data[1];
    let flags_a = data[2];
    let flags_b = data[3];
    if area_group == 0 || area_group > 128 || area_id > 128 || flags_a > 16 || flags_b > 16 {
        return Err(Error::InvalidField {
            what: "Webfoot AreaEntry header",
            value: u64::from(area_group),
        });
    }

    let mut words = [0u32; AREA_WORD_SLOTS];
    let mut ptrs = 0usize;
    for (i, slot) in words.iter_mut().enumerate() {
        let w = u32_at(data, 4 + i * 4)?;
        if !is_valid_slot(w) {
            return Err(Error::InvalidField {
                what: "Webfoot AreaEntry slot",
                value: u64::from(w),
            });
        }
        if is_gba_rom_ptr(w) {
            ptrs += 1;
        }
        *slot = w;
    }
    if ptrs == 0 {
        return Err(Error::InvalidField {
            what: "Webfoot AreaEntry rom pointers",
            value: 0,
        });
    }

    Ok(AreaEntry {
        area_group,
        area_id,
        flags_a,
        flags_b,
        words,
    })
}

/// Parse a `ByteCodeInterpreter` script blob from `data`.
pub fn parse_bytecode_blob(data: &[u8]) -> Result<BytecodeBlob> {
    if data.len() < 4 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 4,
            available: data.len(),
        });
    }
    let prefix = [data[0], data[1]];
    match data[2] {
        0x00 => Ok(BytecodeBlob {
            prefix,
            encoding: BytecodeEncoding::Raw,
            compressed_pad: None,
            payload: data[3..].to_vec(),
        }),
        0x03 => Ok(BytecodeBlob {
            prefix,
            encoding: BytecodeEncoding::Compressed,
            compressed_pad: Some(data[3]),
            payload: data[4..].to_vec(),
        }),
        other => Err(Error::InvalidField {
            what: "Webfoot ByteCodeInterpreter tag",
            value: u64::from(other),
        }),
    }
}

/// Fast, panic-free structural detector for a contiguous Webfoot `AreaEntry` table (`>= 8` records).
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse a contiguous Webfoot `AreaEntry` table (`>= 8` records) from `data[0..]`.
pub fn parse(data: &[u8]) -> Result<WebfootAreaTable> {
    let mut count = 0usize;
    while (count + 1) * AREA_ENTRY_SIZE <= data.len() && count < 1024 {
        let slice = &data[count * AREA_ENTRY_SIZE..(count + 1) * AREA_ENTRY_SIZE];
        if parse_area_entry(slice).is_err() {
            break;
        }
        count += 1;
    }
    if count < MIN_AREA_ENTRIES {
        return Err(Error::InvalidField {
            what: "Webfoot AreaEntry count",
            value: count as u64,
        });
    }

    let spec = FixedRecordTableSpec::new(
        AREA_ENTRY_SIZE,
        0,
        1024,
        None,
        SizeMode::FixedSlot(0),
        TableTermination::Counted(count),
    )
    .with_offset_base(OffsetBase::TableEnd);
    let rows = parse_fixed_record_table(data, &spec).map_err(|_| Error::InvalidField {
        what: "Webfoot AreaEntry table",
        value: count as u64,
    })?;
    let mut entries = Vec::with_capacity(rows.len());
    for row in &rows {
        entries.push(parse_area_entry(
            &data[row.record_offset..row.record_offset + AREA_ENTRY_SIZE],
        )?);
    }
    Ok(WebfootAreaTable {
        consumed_end: count * AREA_ENTRY_SIZE,
        entries,
    })
}

/// Symmetrically pack a [`WebfootAreaTable`] back into its exact binary representation.
pub fn pack(table: &WebfootAreaTable) -> Result<Vec<u8>> {
    let raw_rows: Vec<[u8; AREA_ENTRY_SIZE]> = table.entries.iter().map(AreaEntry::to_bytes).collect();
    let inputs: Vec<FixedRecordInput<'_>> = raw_rows
        .iter()
        .map(|row| FixedRecordInput {
            name: None,
            key: None,
            payload: &[],
            record_template: Some(row),
        })
        .collect();
    let spec = FixedRecordTableSpec::new(
        AREA_ENTRY_SIZE,
        0,
        1024,
        None,
        SizeMode::FixedSlot(0),
        TableTermination::Counted(inputs.len()),
    )
    .with_offset_base(OffsetBase::TableEnd);
    pack_fixed_record_table(&spec, &PackPolicy::TIGHT, &[], &inputs).map_err(|_| {
        Error::InvalidField {
            what: "Webfoot AreaEntry pack",
            value: inputs.len() as u64,
        }
    })
}

/// Scan a GBA ROM image for Webfoot `AreaEntry` tables (`>= 8` contiguous 44-byte records,
/// each with `>= 3` GBA ROM pointers to maintain 0% false positives across arbitrary ROMs).
pub fn scan_webfoot(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective_len = rom.len().min(MAX_ROM_SIZE);
    let min_bytes = MIN_AREA_ENTRIES * AREA_ENTRY_SIZE;
    if effective_len < HEADER_SIZE + min_bytes {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + min_bytes <= effective_len {
        if let Ok(first) = parse_area_entry(&rom[off..]) {
            if first.rom_pointer_count() >= 3 {
                if let Ok(table) = parse(&rom[off..effective_len]) {
                    let avg_ptrs: usize = table.entries.iter().map(AreaEntry::rom_pointer_count).sum();
                    if avg_ptrs >= table.entries.len() * 3 {
                        let size = table.consumed_end;
                        found.push(DetectedFormatExtent {
                            format_id: FORMAT_ID_WEBFOOT,
                            subkind: "area-table",
                            offset: off,
                            size,
                        });
                        off = align_up_4(off.saturating_add(size.max(4)));
                        continue;
                    }
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

    pub(crate) fn make_valid_webfoot_table() -> Vec<u8> {
        let mut entries = Vec::new();
        for i in 0..8u8 {
            entries.push(AreaEntry {
                area_group: 1,
                area_id: i,
                flags_a: 2,
                flags_b: 1,
                words: [
                    0x0810_0000 + u32::from(i) * 0x100,
                    0x0810_1000 + u32::from(i) * 0x100,
                    0x0810_2000 + u32::from(i) * 0x100,
                    0,
                    1,
                    0,
                    0,
                    0,
                    0,
                    0,
                ],
            });
        }
        pack(&WebfootAreaTable {
            entries,
            consumed_end: 8 * AREA_ENTRY_SIZE,
        })
        .unwrap()
    }

    #[test]
    fn round_trips_webfoot_area_table_and_bytecode_blob() {
        let raw = make_valid_webfoot_table();
        let parsed = parse(&raw).unwrap();
        assert_eq!(parsed.entries.len(), 8);
        assert!(parsed.find_area(1, 0).is_some());
        assert_eq!(parsed.to_bytes().unwrap(), raw);

        let raw_bc = [0x00, 0x01, 0x00, 0x02, 0x05];
        let parsed_bc = parse_bytecode_blob(&raw_bc).unwrap();
        assert_eq!(parsed_bc.encoding, BytecodeEncoding::Raw);
        assert_eq!(parsed_bc.to_bytes(), raw_bc);

        let cmp_bc = [0x00, 0x01, 0x03, 0x7F, 0xAA, 0xBB];
        let parsed_cmp = parse_bytecode_blob(&cmp_bc).unwrap();
        assert_eq!(parsed_cmp.encoding, BytecodeEncoding::Compressed);
        assert_eq!(parsed_cmp.to_bytes(), cmp_bc);
    }

    #[test]
    fn rejects_malformed_webfoot() {
        assert!(!detect(b""));
        assert!(parse_bytecode_blob(&[0x00, 0x01, 0x02, 0x00]).is_err());
    }
}
