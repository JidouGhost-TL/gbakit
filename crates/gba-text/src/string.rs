//! String extraction, `0x08xxxxxx` ROM pointer-table scanning, and capacity-checked
//! in-place or free-space-relocated string write-back.

#![deny(unsafe_code)]

use std::collections::HashSet;

use formatkit_core::{Error, Result};
use gba_compress::{decode_lz77, encode_lz77, DEFAULT_MAX_OUTPUT};
use gba_rom::{
    bus_to_rom_offset, find_free_space_prefer_ff, patch_rom_pointers, rom_offset_to_bus,
    scan_pointer_tables, HEADER_SIZE,
};
use serde::{Deserialize, Serialize};

use crate::charmap::Charmap;

/// Default maximum bytes to inspect when extracting a string.
pub const DEFAULT_MAX_STRING_LEN: usize = 512;

/// Default byte alignment for relocated strings in ROM free space.
pub const DEFAULT_RELOCATE_ALIGNMENT: usize = 4;

/// A single extracted string decoded from a GBA ROM using a [`Charmap`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedString {
    /// ROM file offset where the string starts.
    pub offset: usize,
    /// GBA Wait State 0 bus address (`0x0800_0000 + offset`), if within 32 MiB.
    pub bus_address: Option<u32>,
    /// Decoded UTF-8 text (excluding the trailing terminator).
    pub text: String,
    /// Byte length of the string payload before the terminator.
    pub payload_bytes_len: usize,
    /// Total raw byte length including the terminator (if terminated).
    pub raw_bytes_len: usize,
    /// Whether a charmap terminator was encountered before `max_len` or EOF.
    pub terminated: bool,
    /// Terminator byte sequence encountered, if any.
    pub terminator: Option<Vec<u8>>,
    /// Number of mapped charmap tokens decoded.
    pub mapped_tokens: usize,
    /// Number of unmapped fallback tokens (`<$XX>`) decoded.
    pub unmapped_tokens: usize,
}

/// Extract and decode a string from `rom` starting at `offset` using `charmap`.
///
/// Stops at the charmap's terminator (if defined), `max_len` payload bytes, or end of `rom`.
pub fn extract_string(
    rom: &[u8],
    offset: usize,
    charmap: &Charmap,
    max_len: usize,
) -> Result<ExtractedString> {
    if offset >= rom.len() {
        return Err(Error::Truncated {
            offset,
            needed: 1,
            available: 0,
        });
    }

    let limit = rom.len().min(offset.saturating_add(max_len));
    let mut pos = offset;
    let mut text = String::new();
    let mut mapped_tokens = 0usize;
    let mut unmapped_tokens = 0usize;
    let mut terminated = false;
    let mut terminator = None;

    while pos < limit {
        if let Some(tlen) = charmap.terminator_len_at(&rom[pos..]) {
            if pos + tlen <= rom.len() {
                terminated = true;
                terminator = Some(rom[pos..pos + tlen].to_vec());
                let payload_bytes_len = pos - offset;
                let raw_bytes_len = payload_bytes_len + tlen;
                return Ok(ExtractedString {
                    offset,
                    bus_address: rom_offset_to_bus(offset),
                    text,
                    payload_bytes_len,
                    raw_bytes_len,
                    terminated,
                    terminator,
                    mapped_tokens,
                    unmapped_tokens,
                });
            }
        }

        if let Some((tok, consumed, is_mapped)) = charmap.decode_next(&rom[pos..limit]) {
            text.push_str(&tok);
            if is_mapped {
                mapped_tokens += 1;
            } else {
                unmapped_tokens += 1;
            }
            pos += consumed;
        } else {
            break;
        }
    }

    // Check if a terminator sits right at `limit` (when payload filled `max_len` exactly).
    if pos == limit && pos < rom.len() {
        if let Some(tlen) = charmap.terminator_len_at(&rom[pos..]) {
            if pos + tlen <= rom.len() {
                terminated = true;
                terminator = Some(rom[pos..pos + tlen].to_vec());
                let payload_bytes_len = pos - offset;
                return Ok(ExtractedString {
                    offset,
                    bus_address: rom_offset_to_bus(offset),
                    text,
                    payload_bytes_len,
                    raw_bytes_len: payload_bytes_len + tlen,
                    terminated,
                    terminator,
                    mapped_tokens,
                    unmapped_tokens,
                });
            }
        }
    }

    let payload_bytes_len = pos - offset;
    Ok(ExtractedString {
        offset,
        bus_address: rom_offset_to_bus(offset),
        text,
        payload_bytes_len,
        raw_bytes_len: payload_bytes_len,
        terminated,
        terminator,
        mapped_tokens,
        unmapped_tokens,
    })
}

/// One entry inside a detected ROM string pointer table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StringTableEntry {
    /// ROM offset of the 32-bit pointer word in the table.
    pub pointer_offset: usize,
    /// ROM offset of the target string.
    pub target_offset: usize,
    /// GBA Wait State 0 bus address stored in the pointer word.
    pub bus_address: u32,
    /// Decoded preview of the target string.
    pub preview: String,
    /// Total raw byte length of the target string (including terminator).
    pub raw_bytes_len: usize,
}

/// A detected `0x08xxxxxx` ROM pointer table referencing valid strings under a [`Charmap`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScannedStringTable {
    /// ROM offset where the pointer table starts.
    pub table_offset: usize,
    /// GBA Wait State 0 bus address of the pointer table.
    pub bus_address: u32,
    /// Total number of 32-bit pointers in the contiguous pointer table.
    pub entry_count: usize,
    /// Number of pointers in this table that decoded as valid strings.
    pub valid_string_count: usize,
    /// Decoded valid string entries.
    pub entries: Vec<StringTableEntry>,
}

/// Scan `rom` for 4-byte-aligned `0x08xxxxxx` pointer tables whose entries point to valid
/// strings under `charmap`.
///
/// Filters out code/graphics/audio pointer tables by requiring terminated, high-mapped-ratio
/// strings with printable content across at least `min_entries` (and >= 70% of the table).
#[must_use]
pub fn scan_string_tables(
    rom: &[u8],
    charmap: &Charmap,
    min_entries: usize,
) -> Vec<ScannedStringTable> {
    if min_entries == 0 || rom.len() < HEADER_SIZE {
        return Vec::new();
    }

    let raw_tables = scan_pointer_tables(rom, min_entries);
    let mut results = Vec::new();

    for table in raw_tables {
        if table.offset < HEADER_SIZE {
            continue;
        }
        let mut valid_entries = Vec::new();
        let mut distinct_targets = HashSet::new();

        for (idx, &bus_addr) in table.entries.iter().enumerate() {
            let Some(target_off) = bus_to_rom_offset(bus_addr, rom.len()) else {
                continue;
            };
            if target_off < HEADER_SIZE {
                continue;
            }
            let Ok(extracted) = extract_string(rom, target_off, charmap, 256) else {
                continue;
            };
            if !is_plausible_string(&extracted) {
                continue;
            }
            distinct_targets.insert(target_off);
            let preview = truncate_preview(&extracted.text, 80);
            valid_entries.push(StringTableEntry {
                pointer_offset: table.offset + idx * 4,
                target_offset: target_off,
                bus_address: bus_addr,
                preview,
                raw_bytes_len: extracted.raw_bytes_len,
            });
        }

        let valid_count = valid_entries.len();
        let total_count = table.entries.len();
        let min_distinct = min_entries.min(2);
        if valid_count >= min_entries
            && distinct_targets.len() >= min_distinct
            && valid_count.saturating_mul(10) >= total_count.saturating_mul(7)
        {
            if let Some(bus_address) = rom_offset_to_bus(table.offset) {
                results.push(ScannedStringTable {
                    table_offset: table.offset,
                    bus_address,
                    entry_count: total_count,
                    valid_string_count: valid_count,
                    entries: valid_entries,
                });
            }
        }
    }

    results
}

fn is_plausible_string(s: &ExtractedString) -> bool {
    if !s.terminated || s.payload_bytes_len == 0 {
        return false;
    }
    let total_tokens = s.mapped_tokens + s.unmapped_tokens;
    if total_tokens == 0 {
        return false;
    }
    // Require >= 85% mapped tokens.
    if s.mapped_tokens.saturating_mul(100) < total_tokens.saturating_mul(85) {
        return false;
    }
    // Require at least one alphanumeric / kana / letter character (reject pure spaces/punctuation/control).
    s.text.chars().any(|c| c.is_alphanumeric())
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    let single_line = text.replace('\n', "\\n");
    let mut chars = single_line.chars();
    let taken: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{taken}…")
    } else {
        taken
    }
}

/// Strategy used when writing a replacement string into a ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextReplaceMode {
    /// Replacement fit within the original slot capacity and was written in-place.
    InPlace,
    /// Replacement exceeded original capacity and was relocated to ROM free space with
    /// 4-byte-aligned `0x08xxxxxx` ROM pointers updated.
    Relocated,
}

/// Options controlling safe capacity-checked string replacement in a GBA ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextReplaceOptions {
    /// Explicit byte capacity of the original slot (including terminator).
    /// When `None`, the capacity is measured by extracting the original string at `offset`.
    pub capacity: Option<usize>,
    /// Append the charmap's default terminator if the encoded string does not already end with it.
    pub append_terminator: bool,
    /// Fill byte for slack bytes after an in-place write. When `None`, uses the charmap's
    /// single-byte terminator if available, otherwise `0x00`.
    pub pad_byte: Option<u8>,
    /// Allow relocating overflowing strings to ROM free space and patching `0x08xxxxxx` pointers.
    pub allow_relocate: bool,
    /// Alignment (in bytes) for relocated strings in free space (default: `4`).
    pub relocate_alignment: usize,
    /// Minimum ROM offset to start searching for free space (default: `0xC0`).
    pub free_space_start: usize,
}

impl Default for TextReplaceOptions {
    fn default() -> Self {
        Self {
            capacity: None,
            append_terminator: true,
            pad_byte: None,
            allow_relocate: false,
            relocate_alignment: DEFAULT_RELOCATE_ALIGNMENT,
            free_space_start: HEADER_SIZE,
        }
    }
}

/// Structured report returned by [`replace_string_in_rom`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextReplaceReport {
    /// Whether the string was written in-place or relocated.
    pub mode: TextReplaceMode,
    /// Original ROM offset of the replaced string.
    pub original_offset: usize,
    /// Final ROM offset where the new string was written.
    pub written_offset: usize,
    /// GBA Wait State 0 bus address (`0x0800_0000 + written_offset`).
    pub written_bus_address: u32,
    /// Measured raw byte length of the original string (including terminator if present).
    pub original_bytes_len: usize,
    /// Capacity of the original slot in bytes.
    pub original_capacity: usize,
    /// Encoded byte length of the new string (including terminator if appended).
    pub new_bytes_len: usize,
    /// Slack bytes padded after an in-place write (`0` if relocated or exact fit).
    pub slack_bytes: usize,
    /// Number of 4-byte-aligned `0x08xxxxxx` ROM pointers rewritten.
    pub patched_pointer_count: usize,
    /// ROM file offsets of every rewritten 32-bit pointer word.
    pub patched_pointer_offsets: Vec<usize>,
}

/// Safely replace a string in `rom` at `offset` using `charmap`.
///
/// - Encodes `new_text` with `charmap` (appending the charmap's default terminator if
///   `options.append_terminator` is `true` and not already present).
/// - If `encoded.len() <= original_capacity`, writes in-place and pads any leftover bytes
///   with `options.pad_byte` (or the charmap's terminator byte).
/// - If `encoded.len() > original_capacity`:
///   - When `options.allow_relocate` is `true`, allocates aligned `0xFF`/`0x00` free space,
///     writes the encoded string, and patches all 4-byte-aligned `0x08xxxxxx` ROM pointers
///     referencing `offset`.
///   - Otherwise returns [`Error::ResourceLimit`].
pub fn replace_string_in_rom(
    rom: &mut [u8],
    offset: usize,
    new_text: &str,
    charmap: &Charmap,
    options: &TextReplaceOptions,
) -> Result<TextReplaceReport> {
    if offset >= rom.len() {
        return Err(Error::Truncated {
            offset,
            needed: 1,
            available: 0,
        });
    }

    let orig = extract_string(rom, offset, charmap, 4096)?;
    if options.capacity.is_none() && !orig.terminated {
        return Err(Error::Malformed(format!(
            "string at offset 0x{offset:X} is not terminated within 4096 bytes; specify explicit capacity"
        )));
    }
    let original_capacity = options.capacity.unwrap_or(orig.raw_bytes_len);
    if original_capacity == 0 {
        return Err(Error::InvalidField {
            what: "string_capacity",
            value: 0,
        });
    }
    if offset
        .checked_add(original_capacity)
        .is_none_or(|end| end > rom.len())
    {
        return Err(Error::Truncated {
            offset,
            needed: original_capacity,
            available: rom.len().saturating_sub(offset),
        });
    }

    let mut encoded = charmap.encode(new_text)?;
    if options.append_terminator {
        if let Some(term) = charmap.default_terminator() {
            if !encoded.ends_with(term) {
                encoded.extend_from_slice(term);
            }
        }
    }

    let pad_byte = options.pad_byte.unwrap_or_else(|| {
        charmap
            .default_terminator()
            .and_then(|t| if t.len() == 1 { Some(t[0]) } else { None })
            .unwrap_or(0x00)
    });

    write_or_relocate_bytes(rom, offset, orig.raw_bytes_len, &encoded, pad_byte, options)
}

/// Shared capacity-checked in-place or free-space-relocated byte writer with automatic
/// `0x08xxxxxx` ROM pointer patching.
///
/// Prefers `0xFF` free-space runs before falling back to `0x00` runs at `>= 0x200`.
pub(crate) fn write_or_relocate_bytes(
    rom: &mut [u8],
    offset: usize,
    original_bytes_len: usize,
    encoded: &[u8],
    pad_byte: u8,
    options: &TextReplaceOptions,
) -> Result<TextReplaceReport> {
    if offset >= rom.len() {
        return Err(Error::Truncated {
            offset,
            needed: 1,
            available: 0,
        });
    }

    let original_capacity = options.capacity.unwrap_or(original_bytes_len);
    if original_capacity == 0 {
        return Err(Error::InvalidField {
            what: "string_capacity",
            value: 0,
        });
    }
    if offset
        .checked_add(original_capacity)
        .is_none_or(|end| end > rom.len())
    {
        return Err(Error::Truncated {
            offset,
            needed: original_capacity,
            available: rom.len().saturating_sub(offset),
        });
    }

    let orig_bus_address = rom_offset_to_bus(offset).ok_or(Error::InvalidField {
        what: "written_rom_offset",
        value: offset as u64,
    })?;

    let new_bytes_len = encoded.len();
    if new_bytes_len <= original_capacity {
        rom[offset..offset + new_bytes_len].copy_from_slice(encoded);
        let slack_bytes = original_capacity - new_bytes_len;
        if slack_bytes > 0 {
            rom[offset + new_bytes_len..offset + original_capacity].fill(pad_byte);
        }
        return Ok(TextReplaceReport {
            mode: TextReplaceMode::InPlace,
            original_offset: offset,
            written_offset: offset,
            written_bus_address: orig_bus_address,
            original_bytes_len,
            original_capacity,
            new_bytes_len,
            slack_bytes,
            patched_pointer_count: 0,
            patched_pointer_offsets: Vec::new(),
        });
    }

    if !options.allow_relocate {
        return Err(Error::ResourceLimit {
            resource: "rom_string_capacity",
            requested: new_bytes_len as u64,
            limit: original_capacity as u64,
        });
    }

    let new_offset = find_free_space_prefer_ff(
        rom,
        new_bytes_len,
        options.relocate_alignment.max(1),
        options.free_space_start,
        Some((offset, offset + original_capacity)),
    )
    .ok_or(Error::ResourceLimit {
        resource: "rom_free_space",
        requested: new_bytes_len as u64,
        limit: 0,
    })?;

    let written_bus_address = rom_offset_to_bus(new_offset).ok_or(Error::InvalidField {
        what: "relocated_rom_offset",
        value: new_offset as u64,
    })?;

    rom[new_offset..new_offset + new_bytes_len].copy_from_slice(encoded);
    let patched_pointer_offsets = patch_rom_pointers(
        rom,
        offset,
        new_offset,
        Some((new_offset, new_offset + new_bytes_len)),
    )?;

    Ok(TextReplaceReport {
        mode: TextReplaceMode::Relocated,
        original_offset: offset,
        written_offset: new_offset,
        written_bus_address,
        original_bytes_len,
        original_capacity,
        new_bytes_len,
        slack_bytes: 0,
        patched_pointer_count: patched_pointer_offsets.len(),
        patched_pointer_offsets,
    })
}

/// A single extracted BIOS LZ77 (`0x10`) compressed string decoded from a GBA ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractedLz77String {
    /// ROM file offset where the BIOS LZ77 (`0x10`) stream starts.
    pub offset: usize,
    /// GBA Wait State 0 bus address (`0x0800_0000 + offset`), if within 32 MiB.
    pub bus_address: Option<u32>,
    /// Decoded UTF-8 text (excluding the trailing terminator).
    pub text: String,
    /// Exact compressed byte length of the BIOS LZ77 (`0x10`) stream in ROM.
    pub compressed_bytes_len: usize,
    /// Decompressed byte length produced by BIOS LZ77 (`0x10`).
    pub decompressed_bytes_len: usize,
    /// Byte length of the decoded string payload before the terminator.
    pub payload_bytes_len: usize,
    /// Total decoded string byte length including terminator (if terminated).
    pub raw_bytes_len: usize,
    /// Whether a charmap terminator was encountered.
    pub terminated: bool,
    /// Terminator byte sequence encountered, if any.
    pub terminator: Option<Vec<u8>>,
    /// Number of mapped charmap tokens decoded.
    pub mapped_tokens: usize,
    /// Number of unmapped fallback tokens (`<$XX>`) decoded.
    pub unmapped_tokens: usize,
}

/// Decompress a GBA BIOS LZ77 (`0x10`) stream at `offset` in `rom` and decode it with `charmap`.
pub fn extract_lz77_string(
    rom: &[u8],
    offset: usize,
    charmap: &Charmap,
    max_output: usize,
) -> Result<ExtractedLz77String> {
    if offset >= rom.len() {
        return Err(Error::Truncated {
            offset,
            needed: 1,
            available: 0,
        });
    }

    let decoded_stream = decode_lz77(&rom[offset..], max_output)?;
    let decompressed_bytes_len = decoded_stream.bytes.len();
    let extracted = extract_string(&decoded_stream.bytes, 0, charmap, decompressed_bytes_len)?;

    Ok(ExtractedLz77String {
        offset,
        bus_address: rom_offset_to_bus(offset),
        text: extracted.text,
        compressed_bytes_len: decoded_stream.consumed,
        decompressed_bytes_len,
        payload_bytes_len: extracted.payload_bytes_len,
        raw_bytes_len: extracted.raw_bytes_len,
        terminated: extracted.terminated,
        terminator: extracted.terminator,
        mapped_tokens: extracted.mapped_tokens,
        unmapped_tokens: extracted.unmapped_tokens,
    })
}

/// Replace a BIOS LZ77 (`0x10`) compressed string in `rom` at `offset`.
///
/// - Decompresses the existing LZ77 stream at `offset` to measure its original compressed slot.
/// - Encodes `new_text` with `charmap` (appending terminator if `options.append_terminator`),
///   recompresses with BIOS LZ77 (`0x10`), and writes in-place if it fits or relocates to
///   `0xFF` free space (`0x00` fallback at `>= 0x200`) + patches `0x08xxxxxx` pointers.
pub fn replace_lz77_string_in_rom(
    rom: &mut [u8],
    offset: usize,
    new_text: &str,
    charmap: &Charmap,
    options: &TextReplaceOptions,
) -> Result<TextReplaceReport> {
    if offset >= rom.len() {
        return Err(Error::Truncated {
            offset,
            needed: 1,
            available: 0,
        });
    }

    let orig_stream = decode_lz77(&rom[offset..], DEFAULT_MAX_OUTPUT)?;
    let mut encoded = charmap.encode(new_text)?;
    if options.append_terminator {
        if let Some(term) = charmap.default_terminator() {
            if !encoded.ends_with(term) {
                encoded.extend_from_slice(term);
            }
        }
    }
    let compressed = encode_lz77(&encoded)?;
    let pad_byte = options.pad_byte.unwrap_or(0x00);

    write_or_relocate_bytes(
        rom,
        offset,
        orig_stream.consumed,
        &compressed,
        pad_byte,
        options,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_rom_with_strings(cm: &Charmap, strings: &[&str]) -> (Vec<u8>, usize, Vec<usize>) {
        let mut rom = vec![0u8; 0x8000];
        rom[0x4000..].fill(0xFF); // free space in back half

        let table_off = 0x200usize;
        let mut str_cursor = 0x1000usize;
        let mut str_offsets = Vec::new();

        for (i, &s) in strings.iter().enumerate() {
            let mut enc = cm.encode(s).unwrap();
            if let Some(term) = cm.default_terminator() {
                if !enc.ends_with(term) {
                    enc.extend_from_slice(term);
                }
            }
            rom[str_cursor..str_cursor + enc.len()].copy_from_slice(&enc);
            let ptr = rom_offset_to_bus(str_cursor).unwrap();
            let ptr_off = table_off + i * 4;
            rom[ptr_off..ptr_off + 4].copy_from_slice(&ptr.to_le_bytes());
            str_offsets.push(str_cursor);
            str_cursor = (str_cursor + enc.len() + 3) & !3;
        }

        (rom, table_off, str_offsets)
    }

    #[test]
    fn scan_string_tables_finds_pokemon_gen3_table() {
        let cm = Charmap::pokemon_gen3_en();
        let strings = [
            "BULBASAUR",
            "CHARMANDER",
            "SQUIRTLE",
            "PIKACHU",
            "EEVEE",
        ];
        let (rom, table_off, offsets) = synthetic_rom_with_strings(&cm, &strings);

        let tables = scan_string_tables(&rom, &cm, 4);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].table_offset, table_off);
        assert_eq!(tables[0].valid_string_count, 5);
        assert_eq!(tables[0].entries[0].target_offset, offsets[0]);
        assert_eq!(tables[0].entries[0].preview, "BULBASAUR");
        assert_eq!(tables[0].entries[3].preview, "PIKACHU");
    }

    #[test]
    fn replace_string_in_place_and_overflow_rejection() {
        let cm = Charmap::pokemon_gen3_en();
        let (mut rom, _table_off, offsets) =
            synthetic_rom_with_strings(&cm, &["CHARMANDER", "SQUIRTLE"]);

        // Shorter replacement fits in-place and pads slack with 0xFF terminator.
        let report = replace_string_in_rom(
            &mut rom,
            offsets[0],
            "CHAR",
            &cm,
            &TextReplaceOptions::default(),
        )
        .unwrap();
        assert_eq!(report.mode, TextReplaceMode::InPlace);
        assert_eq!(report.written_offset, offsets[0]);
        assert_eq!(report.original_capacity, 11); // "CHARMANDER" + 0xFF
        assert_eq!(report.new_bytes_len, 5); // "CHAR" + 0xFF
        assert_eq!(report.slack_bytes, 6);
        assert!(rom[offsets[0] + 5..offsets[0] + 11]
            .iter()
            .all(|&b| b == 0xFF));

        let extracted = extract_string(&rom, offsets[0], &cm, 64).unwrap();
        assert_eq!(extracted.text, "CHAR");
        assert!(extracted.terminated);

        // Longer replacement without --allow-relocate is rejected.
        let err = replace_string_in_rom(
            &mut rom,
            offsets[1],
            "A VERY LONG REPLACEMENT STRING",
            &cm,
            &TextReplaceOptions::default(),
        );
        assert!(matches!(err, Err(Error::ResourceLimit { .. })));
    }

    #[test]
    fn replace_string_relocate_patches_pointers() {
        let cm = Charmap::ascii();
        let (mut rom, table_off, offsets) =
            synthetic_rom_with_strings(&cm, &["ITEM", "POTION", "ETHER", "ELIXIR"]);

        // Add a second pointer referencing offsets[0]
        let extra_ptr_off = 0x300usize;
        let orig_bus = rom_offset_to_bus(offsets[0]).unwrap();
        rom[extra_ptr_off..extra_ptr_off + 4].copy_from_slice(&orig_bus.to_le_bytes());

        let opts = TextReplaceOptions {
            allow_relocate: true,
            ..TextReplaceOptions::default()
        };
        let report = replace_string_in_rom(
            &mut rom,
            offsets[0],
            "SUPER HYPER LEGENDARY ITEM",
            &cm,
            &opts,
        )
        .unwrap();

        assert_eq!(report.mode, TextReplaceMode::Relocated);
        assert!(report.written_offset >= 0x4000);
        assert_eq!(report.written_offset % 4, 0);
        assert_eq!(report.patched_pointer_count, 2);
        assert_eq!(report.patched_pointer_offsets, vec![table_off, extra_ptr_off]);

        let expected_bus = rom_offset_to_bus(report.written_offset).unwrap();
        let p1 = u32::from_le_bytes(rom[table_off..table_off + 4].try_into().unwrap());
        let p2 = u32::from_le_bytes(rom[extra_ptr_off..extra_ptr_off + 4].try_into().unwrap());
        assert_eq!(p1, expected_bus);
        assert_eq!(p2, expected_bus);

        let extracted = extract_string(&rom, report.written_offset, &cm, 128).unwrap();
        assert_eq!(extracted.text, "SUPER HYPER LEGENDARY ITEM");
        assert!(extracted.terminated);
    }

    #[test]
    fn lz77_string_extract_replace_and_relocate() {
        let cm = Charmap::ascii();
        let mut rom = vec![0u8; 0x8000];
        rom[0x4000..].fill(0xFF); // free space

        let orig_text = "HELLO HELLO HELLO WORLD\0";
        let compressed = encode_lz77(orig_text.as_bytes()).unwrap();
        let entry_off = 0x1000usize;
        rom[entry_off..entry_off + compressed.len()].copy_from_slice(&compressed);

        let ptr_off = 0x200usize;
        let bus = rom_offset_to_bus(entry_off).unwrap();
        rom[ptr_off..ptr_off + 4].copy_from_slice(&bus.to_le_bytes());

        let ext = extract_lz77_string(&rom, entry_off, &cm, 1024).unwrap();
        assert_eq!(ext.text, "HELLO HELLO HELLO WORLD");
        assert_eq!(ext.compressed_bytes_len, compressed.len());
        assert_eq!(ext.decompressed_bytes_len, orig_text.len());
        assert!(ext.terminated);

        // Shorter replacement fits in-place
        let rep = replace_lz77_string_in_rom(
            &mut rom,
            entry_off,
            "SHORT",
            &cm,
            &TextReplaceOptions::default(),
        )
        .unwrap();
        assert_eq!(rep.mode, TextReplaceMode::InPlace);
        assert_eq!(rep.written_offset, entry_off);

        let ext2 = extract_lz77_string(&rom, entry_off, &cm, 1024).unwrap();
        assert_eq!(ext2.text, "SHORT");

        // Larger incompressible replacement requires relocation
        let long_text = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!@#$%^&*()";
        let err = replace_lz77_string_in_rom(
            &mut rom,
            entry_off,
            long_text,
            &cm,
            &TextReplaceOptions::default(),
        );
        assert!(matches!(err, Err(Error::ResourceLimit { .. })));

        let opts = TextReplaceOptions {
            allow_relocate: true,
            ..TextReplaceOptions::default()
        };
        let reloc = replace_lz77_string_in_rom(&mut rom, entry_off, long_text, &cm, &opts).unwrap();
        assert_eq!(reloc.mode, TextReplaceMode::Relocated);
        assert!(reloc.written_offset >= 0x4000);
        assert_eq!(reloc.patched_pointer_count, 1);
        assert_eq!(reloc.patched_pointer_offsets, vec![ptr_off]);

        let ext3 = extract_lz77_string(&rom, reloc.written_offset, &cm, 1024).unwrap();
        assert_eq!(ext3.text, long_text);
        assert!(ext3.terminated);
    }

    #[test]
    fn replace_string_rejects_unterminated_without_explicit_capacity() {
        let cm = Charmap::ascii();
        // Fill ROM with 'A' (no 0x00 terminator anywhere)
        let mut rom = vec![b'A'; 0x2000];
        let snapshot = rom.clone();
        let err = replace_string_in_rom(
            &mut rom,
            0x400,
            "HELLO",
            &cm,
            &TextReplaceOptions::default(),
        );
        assert!(matches!(err, Err(Error::Malformed(_))));
        assert_eq!(rom, snapshot);

        // With explicit capacity, replacement succeeds.
        let opts = TextReplaceOptions {
            capacity: Some(8),
            ..TextReplaceOptions::default()
        };
        let rep = replace_string_in_rom(&mut rom, 0x400, "HELLO", &cm, &opts).unwrap();
        assert_eq!(rep.mode, TextReplaceMode::InPlace);
        assert_eq!(&rom[0x400..0x406], b"HELLO\0");
    }
}
