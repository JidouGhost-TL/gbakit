//! GBA studio archive / packfile scanner, extractor, repacker, and safe ROM write-back.
//!
//! Supports three empirically verified GBA studio container formats:
//! - **`PACK`**: 4-byte magic `b"PACK"` + `u32` entry count + relative `u32` offset TOC
//!   (`count` or `count + 1` entries with end-of-archive sentinel) + payload blobs.
//! - **`WAYFORWARD`**: `"WAYFORWARD"` header (`b"WAYFORWARD\0\0"` 4-byte aligned or unpadded
//!   `b"WAYFORWARD"`) + `u32` entry count + `u32` offset table (`count` or `count + 1`) + payloads.
//! - **`SFCD` (`.mmstr`)**: 4-byte magic `b"SFCD"` + `u32` entry count + 16-byte directory entries
//!   (`8`-byte ASCII name + `u32` relative offset + `u32` byte size) + payload blobs.

#![deny(unsafe_code)]

use formatkit_core::{Endian, Error, Result};
use formatkit_layout::{
    pack_counted_offset_array, pack_fixed_record_table, parse_counted_offset_array_metadata,
    parse_fixed_record_table_counted, CountedOffsetArraySpec, FixedRecordInput,
    FixedRecordTableSpec, IntFieldSpec, IntWidth, LayoutError, NameEncoding, NameFieldSpec,
    OffsetBase, PackPolicy, SizeMode, TableTermination,
};
use serde::{Deserialize, Serialize};

use crate::relocate::{find_free_space_prefer_ff, patch_rom_pointers};
use crate::rom_offset_to_bus;

/// Maximum entry count accepted when parsing or scanning an archive to prevent DoS allocations.
pub const MAX_ARCHIVE_ENTRIES: usize = 65_536;

/// Minimum ROM offset for archive free-space relocation (`0x200`).
pub const ARCHIVE_FREE_SPACE_START: usize = 0x200;

/// Default alignment (in bytes) for relocated archives and packed entry payloads.
pub const DEFAULT_ARCHIVE_ALIGN: usize = 4;

const MAGIC_PACK: &[u8; 4] = b"PACK";
const MAGIC_WAYFORWARD: &[u8; 10] = b"WAYFORWARD";
const MAGIC_SFCD: &[u8; 4] = b"SFCD";

/// Supported GBA studio archive / packfile container formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveFormat {
    /// Generic `PACK` offset-table archive (`b"PACK"`).
    Pack,
    /// WayForward Technologies packfile (`"WAYFORWARD"`).
    WayForward,
    /// `SFCD` / `.mmstr` named directory archive (`b"SFCD"`).
    Sfcd,
}

impl ArchiveFormat {
    /// Canonical lowercase identifier string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pack => "pack",
            Self::WayForward => "wayforward",
            Self::Sfcd => "sfcd",
        }
    }

    /// Parse a format name (`"pack"`, `"wayforward"`, `"sfcd"`, or `"mmstr"`).
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "pack" => Some(Self::Pack),
            "wayforward" | "wf" => Some(Self::WayForward),
            "sfcd" | "mmstr" => Some(Self::Sfcd),
            _ => None,
        }
    }
}

/// Single entry inside a parsed studio archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveEntry {
    /// Zero-based entry index in the archive TOC/directory.
    pub index: usize,
    /// Entry name (`SFCD` 8-byte name, or synthetic `entry_NNNN` for unnamed TOC formats).
    pub name: String,
    /// Byte offset of this entry's payload relative to the start of the archive.
    pub offset: usize,
    /// Byte length of this entry's payload.
    pub size: usize,
}

/// Parsed studio archive header and directory metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedArchive {
    /// Container format (`Pack`, `WayForward`, or `Sfcd`).
    pub format: ArchiveFormat,
    /// Byte offset of the archive within its parent ROM (or `0` for standalone files).
    pub rom_offset: usize,
    /// Total byte size of the archive (exact when sentinel/explicit sizes exist, bounded otherwise).
    pub total_size: usize,
    /// Whether an explicit end-of-archive sentinel offset (`count + 1` TOC) was present.
    pub has_end_sentinel: bool,
    /// Directory entries in TOC order.
    pub entries: Vec<ArchiveEntry>,
}

/// Serializable `manifest.json` written on extraction and consumed on repack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveManifest {
    pub format: ArchiveFormat,
    pub source_offset: usize,
    pub total_size: usize,
    pub entries: Vec<ArchiveManifestEntry>,
}

/// Single entry record inside [`ArchiveManifest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveManifestEntry {
    pub index: usize,
    pub name: String,
    pub file: String,
    pub original_offset: usize,
    pub size: usize,
}

/// Options controlling safe capacity-checked archive replacement in a ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveReplaceOptions {
    /// Explicit byte capacity of the original archive slot.
    /// If `None`, the existing archive at `rom_offset` is parsed to measure its `total_size`.
    pub original_capacity: Option<usize>,
    /// Fill byte written to slack bytes when the new archive is smaller than `original_capacity`.
    pub pad_byte: u8,
    /// When `true`, relocate overflowing archives to `0xFF` (then `0x00`) free space at `>= 0x200`
    /// and rewrite all 4-byte-aligned `0x08xxxxxx` ROM pointers.
    pub allow_relocate: bool,
    /// Alignment (in bytes) for relocated archives (default: `4`).
    pub relocate_alignment: usize,
    /// Minimum ROM offset to begin scanning for free space (default: `0x200`).
    pub free_space_start: usize,
}

impl Default for ArchiveReplaceOptions {
    fn default() -> Self {
        Self {
            original_capacity: None,
            pad_byte: 0x00,
            allow_relocate: false,
            relocate_alignment: DEFAULT_ARCHIVE_ALIGN,
            free_space_start: ARCHIVE_FREE_SPACE_START,
        }
    }
}

/// Structured report describing a completed archive replacement in a ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveReplaceReport {
    pub format: ArchiveFormat,
    pub entry_count: usize,
    pub original_offset: usize,
    pub written_offset: usize,
    pub written_bus_address: u32,
    pub original_capacity: usize,
    pub written_bytes: usize,
    pub slack_bytes: usize,
    pub relocated: bool,
    pub patched_pointers: usize,
    pub patched_pointer_offsets: Vec<usize>,
}

// -----------------------------------------------------------------------------
// Parsing & Scanning
// -----------------------------------------------------------------------------

/// Parse a standalone archive (`PACK`, `WAYFORWARD`, or `SFCD`) starting at byte 0 of `bytes`.
pub fn parse_archive(bytes: &[u8]) -> Result<ParsedArchive> {
    parse_archive_at(bytes, 0)
}

/// Parse an archive (`PACK`, `WAYFORWARD`, or `SFCD`) located at `rom_offset` within `rom`.
pub fn parse_archive_at(rom: &[u8], rom_offset: usize) -> Result<ParsedArchive> {
    if rom_offset >= rom.len() {
        return Err(Error::Truncated {
            offset: rom_offset,
            needed: 1,
            available: 0,
        });
    }
    let slice = &rom[rom_offset..];
    if slice.starts_with(MAGIC_PACK) {
        return parse_offset_toc_archive(slice, rom_offset, ArchiveFormat::Pack, 4, false);
    }
    if slice.starts_with(MAGIC_WAYFORWARD) {
        return parse_wayforward(slice, rom_offset, false);
    }
    if slice.starts_with(MAGIC_SFCD) {
        return parse_sfcd(slice, rom_offset);
    }
    Err(Error::Malformed(format!(
        "unrecognized studio archive magic at offset 0x{rom_offset:X} (expected PACK, WAYFORWARD, or SFCD)"
    )))
}

/// Scan `rom` on 4-byte-aligned boundaries for valid `PACK`, `WAYFORWARD`, and `SFCD` archives.
pub fn scan_archives(rom: &[u8]) -> Vec<ParsedArchive> {
    let mut found = Vec::new();
    let mut off = 0usize;
    while off + 16 <= rom.len() {
        let slice = &rom[off..];
        let parsed = if slice.starts_with(MAGIC_PACK) {
            parse_offset_toc_archive(slice, off, ArchiveFormat::Pack, 4, true).ok()
        } else if slice.starts_with(MAGIC_WAYFORWARD) {
            parse_wayforward(slice, off, true).ok()
        } else if slice.starts_with(MAGIC_SFCD) {
            parse_sfcd(slice, off).ok()
        } else {
            None
        };

        if let Some(archive) = parsed {
            let advance = align_up(archive.total_size.max(4), 4);
            found.push(archive);
            off = off.saturating_add(advance);
        } else {
            off += 4;
        }
    }
    found
}

/// Extract all entry payload slices for `archive` from `bytes` (either a ROM or standalone file).
pub fn extract_archive_entries<'a>(
    bytes: &'a [u8],
    archive: &ParsedArchive,
) -> Result<Vec<(ArchiveEntry, &'a [u8])>> {
    let mut out = Vec::with_capacity(archive.entries.len());
    for entry in &archive.entries {
        let abs_start = archive
            .rom_offset
            .checked_add(entry.offset)
            .ok_or(Error::InvalidField {
                what: "archive_entry_offset",
                value: entry.offset as u64,
            })?;
        let abs_end = abs_start
            .checked_add(entry.size)
            .ok_or(Error::InvalidField {
                what: "archive_entry_size",
                value: entry.size as u64,
            })?;
        if abs_end > bytes.len() {
            return Err(Error::Truncated {
                offset: abs_start,
                needed: entry.size,
                available: bytes.len().saturating_sub(abs_start),
            });
        }
        out.push((entry.clone(), &bytes[abs_start..abs_end]));
    }
    Ok(out)
}

fn parse_wayforward(slice: &[u8], rom_offset: usize, scanning: bool) -> Result<ParsedArchive> {
    if slice.len() < 16 || !slice.starts_with(MAGIC_WAYFORWARD) {
        return Err(Error::Malformed("invalid WAYFORWARD header".into()));
    }
    // Prefer 4-byte-aligned 12-byte magic (`WAYFORWARD\0\0`), falling back to 10-byte unpadded.
    if slice[10] == 0 && slice[11] == 0 {
        if let Ok(parsed) =
            parse_offset_toc_archive(slice, rom_offset, ArchiveFormat::WayForward, 12, scanning)
        {
            return Ok(parsed);
        }
    }
    parse_offset_toc_archive(slice, rom_offset, ArchiveFormat::WayForward, 10, scanning)
}

/// Shared parser for `PACK` and `WAYFORWARD` offset-table archives.
fn parse_offset_toc_archive(
    slice: &[u8],
    rom_offset: usize,
    format: ArchiveFormat,
    count_offset: usize,
    scanning: bool,
) -> Result<ParsedArchive> {
    let toc_start = count_offset + 4;
    if slice.len() < toc_start + 4 {
        return Err(Error::Truncated {
            offset: rom_offset,
            needed: toc_start + 4,
            available: slice.len(),
        });
    }

    let count = read_u32_le(slice, count_offset) as usize;
    if count == 0 || count > MAX_ARCHIVE_ENTRIES {
        return Err(Error::InvalidField {
            what: "archive_entry_count",
            value: count as u64,
        });
    }

    let toc_end_no_sentinel = count
        .checked_mul(4)
        .and_then(|n| toc_start.checked_add(n))
        .ok_or(Error::InvalidField {
            what: "archive_toc_size",
            value: count as u64,
        })?;
    if toc_end_no_sentinel > slice.len() {
        return Err(Error::Truncated {
            offset: rom_offset + toc_start,
            needed: toc_end_no_sentinel - toc_start,
            available: slice.len().saturating_sub(toc_start),
        });
    }

    let first_off = read_u32_le(slice, toc_start) as usize;
    let last_off = read_u32_le(slice, toc_start + (count - 1) * 4) as usize;
    let toc_end_with_sentinel = toc_end_no_sentinel + 4;
    let has_end_sentinel = first_off >= toc_end_with_sentinel
        && toc_end_with_sentinel <= slice.len()
        && {
            let sentinel = read_u32_le(slice, toc_end_no_sentinel) as usize;
            sentinel >= last_off && sentinel <= slice.len() && (sentinel > first_off || count == 1)
        };

    let carrier_len = if has_end_sentinel {
        slice.len()
    } else if scanning {
        // Without a sentinel during a ROM scan, avoid greedy consumption of the entire rest of the ROM.
        // Require at least 2 entries with strictly increasing offsets so random false positives are rejected.
        if count < 2 || last_off <= first_off {
            return Err(Error::Malformed(
                "unbounded archive without sentinel failed scan plausibility check".into(),
            ));
        }
        // Bound the trailing entry to a reasonable estimate (last offset + average entry size, clamped to slice).
        let span = last_off - first_off;
        let avg = (span / (count - 1)).max(4);
        (last_off.saturating_add(avg)).min(slice.len())
    } else {
        slice.len()
    };

    let spec = CountedOffsetArraySpec::new(
        toc_start,
        count,
        IntWidth::U32,
        Endian::Little,
        MAX_ARCHIVE_ENTRIES + 1,
    )
    .with_count_adjust(i32::from(has_end_sentinel))
    .with_offset_base(OffsetBase::Absolute)
    .with_has_eof_sentinel(has_end_sentinel);
    let spans = parse_counted_offset_array_metadata(slice, &spec, carrier_len as u64)
        .map_err(|err| map_toc_layout_error(err, rom_offset))?;
    if spans[0].offset < toc_end_no_sentinel as u64 {
        return Err(Error::InvalidField {
            what: "archive_toc_offset",
            value: spans[0].offset,
        });
    }

    let total_size = if has_end_sentinel {
        spans
            .last()
            .and_then(|s| s.end())
            .map_or(toc_end_with_sentinel, |end| end as usize)
    } else {
        carrier_len
    };

    let entries = spans
        .into_iter()
        .map(|span| ArchiveEntry {
            index: span.index,
            name: format!("entry_{:04}", span.index),
            offset: span.offset as usize,
            size: span.size as usize,
        })
        .collect();

    Ok(ParsedArchive {
        format,
        rom_offset,
        total_size,
        has_end_sentinel,
        entries,
    })
}

fn map_toc_layout_error(err: LayoutError, rom_offset: usize) -> Error {
    match err {
        LayoutError::Truncated {
            offset,
            needed,
            available,
        } => Error::Truncated {
            offset: rom_offset + offset,
            needed,
            available,
        },
        LayoutError::OutOfBounds { offset, .. } => Error::InvalidField {
            what: "archive_toc_offset",
            value: offset,
        },
        LayoutError::NonMonotonic {
            index,
            previous,
            current,
        } => Error::Malformed(format!(
            "non-monotonic archive TOC offset at entry {index}: {current} < {previous}"
        )),
        other => Error::Malformed(other.to_string()),
    }
}

/// Parse an `SFCD` (`.mmstr`) archive.
///
/// Supports both:
/// 1. Retail Majesco / 4Kids variable-length `SFCD` directories:
///    `b"SFCD"` + `u16 dir_size` + `u16 unk` + `u32 file_count`, followed by
///    `file_count` variable-length entries `(u32 size, u32 rel_off, NUL-terminated
///    ASCII filename padded to 4-byte alignment)` with payloads relative to
///    `data_base = 8 + dir_size`.
/// 2. Fixed 16-byte-entry `SFCD` archives:
///    `b"SFCD"` + `u32 count` + `count * (8-byte ASCII name + u32 offset + u32 size)`.
fn parse_sfcd(slice: &[u8], rom_offset: usize) -> Result<ParsedArchive> {
    if slice.len() < 8 || !slice.starts_with(MAGIC_SFCD) {
        return Err(Error::Malformed("invalid SFCD header".into()));
    }
    if let Ok(parsed) = parse_sfcd_variable(slice, rom_offset) {
        return Ok(parsed);
    }
    parse_sfcd_fixed(slice, rom_offset)
}

/// Parse a retail Majesco / 4Kids variable-length `SFCD` directory matching `mobiclip::SfcdArchive`.
fn parse_sfcd_variable(slice: &[u8], rom_offset: usize) -> Result<ParsedArchive> {
    if slice.len() < 12 || !slice.starts_with(MAGIC_SFCD) {
        return Err(Error::Truncated {
            offset: rom_offset,
            needed: 12,
            available: slice.len(),
        });
    }
    let dir_size = u16::from_le_bytes([slice[4], slice[5]]) as usize;
    let file_count = read_u32_le(slice, 8) as usize;
    if dir_size < 16 || file_count == 0 || file_count > MAX_ARCHIVE_ENTRIES {
        return Err(Error::InvalidField {
            what: "sfcd_variable_header",
            value: file_count as u64,
        });
    }
    let data_base = 8usize.checked_add(dir_size).ok_or(Error::InvalidField {
        what: "sfcd_data_base",
        value: dir_size as u64,
    })?;
    if data_base > slice.len() {
        return Err(Error::Truncated {
            offset: rom_offset + 8,
            needed: dir_size,
            available: slice.len().saturating_sub(8),
        });
    }

    let mut pos = 12usize;
    let mut entries = Vec::with_capacity(file_count);
    let mut max_end = data_base;

    for i in 0..file_count {
        let name_start = pos.checked_add(8).ok_or(Error::InvalidField {
            what: "sfcd_dir_overflow",
            value: pos as u64,
        })?;
        if name_start > data_base {
            return Err(Error::Malformed("truncated variable SFCD directory".into()));
        }
        let size = read_u32_le(slice, pos) as usize;
        let rel_off = read_u32_le(slice, pos + 4) as usize;

        let nul_rel = slice[name_start..data_base]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| Error::Malformed("unterminated variable SFCD filename".into()))?;
        let name_bytes = &slice[name_start..name_start + nul_rel];
        if name_bytes.is_empty() || !name_bytes.iter().all(|&b| (0x20..=0x7E).contains(&b)) {
            return Err(Error::Malformed(format!(
                "invalid variable SFCD filename at entry {i}"
            )));
        }
        let name = std::str::from_utf8(name_bytes)
            .map_err(|_| Error::Malformed(format!("invalid UTF-8 in SFCD entry {i} name")))?
            .to_string();
        pos = align_up(name_start + nul_rel + 1, 4);

        let offset = data_base.checked_add(rel_off).ok_or(Error::InvalidField {
            what: "sfcd_entry_offset",
            value: rel_off as u64,
        })?;
        let end = offset.checked_add(size).ok_or(Error::InvalidField {
            what: "sfcd_entry_end",
            value: offset as u64,
        })?;
        if end > slice.len() {
            return Err(Error::Truncated {
                offset: rom_offset + offset,
                needed: size,
                available: slice.len().saturating_sub(offset),
            });
        }
        if end > max_end {
            max_end = end;
        }
        entries.push(ArchiveEntry {
            index: i,
            name,
            offset,
            size,
        });
    }

    Ok(ParsedArchive {
        format: ArchiveFormat::Sfcd,
        rom_offset,
        total_size: max_end,
        has_end_sentinel: false,
        entries,
    })
}

const SFCD_FIXED_SPEC: FixedRecordTableSpec = FixedRecordTableSpec::new(
    16,
    8,
    MAX_ARCHIVE_ENTRIES,
    Some(IntFieldSpec::u32_le(8)),
    SizeMode::Stored(IntFieldSpec::u32_le(12)),
    TableTermination::Counted(0),
)
.with_name_field(NameFieldSpec::new(0, 8, NameEncoding::Ascii));

/// Parse a fixed 16-byte-entry `SFCD` archive:
/// `b"SFCD"` + `u32 count` + `count * (8-byte name + u32 offset + u32 size)`.
fn parse_sfcd_fixed(slice: &[u8], rom_offset: usize) -> Result<ParsedArchive> {
    if slice.len() < 8 || !slice.starts_with(MAGIC_SFCD) {
        return Err(Error::Malformed("invalid SFCD header".into()));
    }
    let count = read_u32_le(slice, 4) as usize;
    if count == 0 || count > MAX_ARCHIVE_ENTRIES {
        return Err(Error::InvalidField {
            what: "sfcd_entry_count",
            value: count as u64,
        });
    }

    let dir_end = count
        .checked_mul(16)
        .and_then(|n| 8usize.checked_add(n))
        .ok_or(Error::InvalidField {
            what: "sfcd_dir_size",
            value: count as u64,
        })?;
    let rows = parse_fixed_record_table_counted(slice, &SFCD_FIXED_SPEC, count).map_err(|err| {
        match err {
            LayoutError::Truncated {
                offset,
                needed,
                available,
            } => Error::Truncated {
                offset: rom_offset + offset,
                needed,
                available,
            },
            LayoutError::OutOfBounds {
                offset,
                size,
                data_len,
                ..
            } => Error::Truncated {
                offset: rom_offset + offset as usize,
                needed: size as usize,
                available: (data_len as usize).saturating_sub(offset as usize),
            },
            LayoutError::InvalidName { index } => Error::Malformed(format!(
                "non-ASCII bytes in SFCD directory entry {index} name"
            )),
            other => Error::Malformed(other.to_string()),
        }
    })?;

    let mut entries = Vec::with_capacity(rows.len());
    let mut max_end = dir_end;
    for row in rows {
        let name_bytes = row.name.as_deref().unwrap_or(&[]);
        if !name_bytes.iter().all(|&b| (0x20..=0x7E).contains(&b)) {
            return Err(Error::Malformed(format!(
                "non-ASCII bytes in SFCD directory entry {} name",
                row.index
            )));
        }
        let name = match row.name_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => format!("entry_{:04}", row.index),
        };
        let offset = row.offset as usize;
        let size = row.size as usize;
        if size > 0 && offset < dir_end {
            return Err(Error::InvalidField {
                what: "sfcd_entry_offset",
                value: row.offset,
            });
        }
        max_end = max_end.max(offset + size);
        entries.push(ArchiveEntry {
            index: row.index,
            name,
            offset,
            size,
        });
    }

    Ok(ParsedArchive {
        format: ArchiveFormat::Sfcd,
        rom_offset,
        total_size: max_end,
        has_end_sentinel: false,
        entries,
    })
}

// -----------------------------------------------------------------------------
// Repacking
// -----------------------------------------------------------------------------

/// Pack `(name, payload)` entries into a byte-valid `PACK`, `WAYFORWARD`, or `SFCD` archive.
///
/// - `PACK` and `WAYFORWARD` emit a self-delimiting `count + 1` offset TOC (with end-of-archive
///   sentinel) and 4-byte-aligned payload offsets.
/// - `SFCD` emits 16-byte directory entries (8-byte null-padded ASCII name, `u32` offset,
///   `u32` size) and 4-byte-aligned payload blobs.
pub fn pack_archive(format: ArchiveFormat, entries: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    if entries.is_empty() || entries.len() > MAX_ARCHIVE_ENTRIES {
        return Err(Error::InvalidField {
            what: "archive_entry_count",
            value: entries.len() as u64,
        });
    }

    match format {
        ArchiveFormat::Pack => pack_offset_toc(MAGIC_PACK, entries),
        ArchiveFormat::WayForward => pack_offset_toc(b"WAYFORWARD\0\0", entries),
        ArchiveFormat::Sfcd => pack_sfcd(entries),
    }
}

fn pack_offset_toc(magic: &[u8], entries: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    let count = entries.len();
    let mut header = Vec::with_capacity(magic.len() + 4);
    header.extend_from_slice(magic);
    header.extend_from_slice(&(count as u32).to_le_bytes());

    let spec = CountedOffsetArraySpec::new(
        header.len(),
        count,
        IntWidth::U32,
        Endian::Little,
        MAX_ARCHIVE_ENTRIES + 1,
    )
    .with_count_adjust(1)
    .with_offset_base(OffsetBase::Absolute)
    .with_has_eof_sentinel(true);
    let policy = PackPolicy {
        data_alignment: DEFAULT_ARCHIVE_ALIGN,
        pad_final_entry: true,
        ..PackPolicy::TIGHT
    };
    let payloads: Vec<&[u8]> = entries.iter().map(|&(_, p)| p).collect();
    pack_counted_offset_array(&spec, &policy, &header, &payloads).map_err(map_pack_layout_error)
}

fn pack_sfcd(entries: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    let count = entries.len();
    let mut header = [0u8; 8];
    header[..4].copy_from_slice(MAGIC_SFCD);
    header[4..8].copy_from_slice(&(count as u32).to_le_bytes());

    let names: Vec<Vec<u8>> = entries
        .iter()
        .enumerate()
        .map(|(i, &(name, _))| {
            let clean: Vec<u8> = name
                .bytes()
                .filter(|&b| (0x20..=0x7E).contains(&b))
                .take(8)
                .collect();
            if clean.is_empty() {
                format!("E{i:07}").into_bytes()
            } else {
                clean
            }
        })
        .collect();
    let inputs: Vec<FixedRecordInput<'_>> = entries
        .iter()
        .enumerate()
        .map(|(i, &(_, payload))| FixedRecordInput {
            name: Some(names[i].as_slice()),
            key: None,
            payload,
            record_template: None,
        })
        .collect();
    let policy = PackPolicy {
        data_alignment: DEFAULT_ARCHIVE_ALIGN,
        pad_final_entry: false,
        ..PackPolicy::TIGHT
    };
    pack_fixed_record_table(&SFCD_FIXED_SPEC, &policy, &header, &inputs)
        .map_err(map_pack_layout_error)
}

fn map_pack_layout_error(err: LayoutError) -> Error {
    match err {
        LayoutError::ValueOutOfRange { value, max } => Error::ResourceLimit {
            resource: "archive_size",
            requested: value,
            limit: max,
        },
        LayoutError::Overflow { .. } => Error::ResourceLimit {
            resource: "archive_size",
            requested: u64::MAX,
            limit: u32::MAX as u64,
        },
        other => Error::Malformed(other.to_string()),
    }
}

// -----------------------------------------------------------------------------
// Safe ROM Write-Back (In-Place + Free-Space Relocation & Pointer Patching)
// -----------------------------------------------------------------------------

/// Replace an archive in `rom` at `rom_offset` with `new_archive_bytes`.
///
/// Validates `new_archive_bytes` as a well-formed `PACK`, `WAYFORWARD`, or `SFCD` archive.
/// Writes in-place if it fits within `original_capacity` (auto-measured from the existing
/// archive at `rom_offset` when not explicitly specified), padding any slack with `options.pad_byte`.
/// When `options.allow_relocate` is `true` and the new archive overflows, relocates into `0xFF`
/// free space (falling back to `0x00`) at `>= 0x200` and patches all 4-byte-aligned `0x08xxxxxx`
/// ROM pointers referencing `rom_offset`.
pub fn replace_archive_in_rom(
    rom: &mut [u8],
    rom_offset: usize,
    new_archive_bytes: &[u8],
    options: &ArchiveReplaceOptions,
) -> Result<ArchiveReplaceReport> {
    if rom_offset >= rom.len() {
        return Err(Error::Truncated {
            offset: rom_offset,
            needed: 1,
            available: 0,
        });
    }

    let parsed_new = parse_archive(new_archive_bytes)?;

    let original_capacity = match options.original_capacity {
        Some(cap) => cap,
        None => {
            let existing = parse_archive_at(rom, rom_offset)?;
            existing.total_size
        }
    };

    if rom_offset
        .checked_add(original_capacity)
        .is_none_or(|end| end > rom.len())
    {
        return Err(Error::Truncated {
            offset: rom_offset,
            needed: original_capacity,
            available: rom.len().saturating_sub(rom_offset),
        });
    }

    let written_bytes = new_archive_bytes.len();
    if written_bytes <= original_capacity {
        let written_bus_address = rom_offset_to_bus(rom_offset).ok_or(Error::InvalidField {
            what: "written_rom_offset",
            value: rom_offset as u64,
        })?;
        rom[rom_offset..rom_offset + written_bytes].copy_from_slice(new_archive_bytes);
        let slack_bytes = original_capacity - written_bytes;
        if slack_bytes > 0 {
            rom[rom_offset + written_bytes..rom_offset + original_capacity].fill(options.pad_byte);
        }
        return Ok(ArchiveReplaceReport {
            format: parsed_new.format,
            entry_count: parsed_new.entries.len(),
            original_offset: rom_offset,
            written_offset: rom_offset,
            written_bus_address,
            original_capacity,
            written_bytes,
            slack_bytes,
            relocated: false,
            patched_pointers: 0,
            patched_pointer_offsets: Vec::new(),
        });
    }

    if !options.allow_relocate {
        return Err(Error::ResourceLimit {
            resource: "rom_archive_capacity",
            requested: written_bytes as u64,
            limit: original_capacity as u64,
        });
    }

    let search_start = options.free_space_start.max(ARCHIVE_FREE_SPACE_START);
    let new_offset = find_free_space_prefer_ff(
        rom,
        written_bytes,
        options.relocate_alignment,
        search_start,
        Some((rom_offset, rom_offset + original_capacity)),
    )
    .ok_or(Error::ResourceLimit {
        resource: "rom_free_space",
        requested: written_bytes as u64,
        limit: 0,
    })?;

    let _old_bus_address = rom_offset_to_bus(rom_offset).ok_or(Error::InvalidField {
        what: "old_rom_offset",
        value: rom_offset as u64,
    })?;
    let written_bus_address = rom_offset_to_bus(new_offset).ok_or(Error::InvalidField {
        what: "relocated_rom_offset",
        value: new_offset as u64,
    })?;

    rom[new_offset..new_offset + written_bytes].copy_from_slice(new_archive_bytes);
    let patched_pointer_offsets = patch_rom_pointers(
        rom,
        rom_offset,
        new_offset,
        Some((new_offset, new_offset + written_bytes)),
    )?;

    Ok(ArchiveReplaceReport {
        format: parsed_new.format,
        entry_count: parsed_new.entries.len(),
        original_offset: rom_offset,
        written_offset: new_offset,
        written_bus_address,
        original_capacity,
        written_bytes,
        slack_bytes: 0,
        relocated: true,
        patched_pointers: patched_pointer_offsets.len(),
        patched_pointer_offsets,
    })
}

fn read_u32_le(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]])
}

fn align_up(value: usize, align: usize) -> usize {
    if align <= 1 {
        return value;
    }
    let rem = value % align;
    if rem == 0 {
        value
    } else {
        value.saturating_add(align - rem)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_format(format: ArchiveFormat) {
        let p0 = b"HELLO_GBA_ARCHIVE_ENTRY_0";
        let p1 = b"\x10\x20\x00\x00PAYLOAD_TWO!";
        let p2 = b"THIRD";
        let input: Vec<(&str, &[u8])> = vec![("ALPHA", p0), ("BETA02", p1), ("GAMMA", p2)];

        let packed = pack_archive(format, &input).unwrap();
        let parsed = parse_archive(&packed).unwrap();
        assert_eq!(parsed.format, format);
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(parsed.total_size, packed.len());

        let extracted = extract_archive_entries(&packed, &parsed).unwrap();
        assert_eq!(extracted.len(), 3);
        assert_eq!(&extracted[0].1[..p0.len()], p0);
        assert_eq!(&extracted[1].1[..p1.len()], p1);
        assert_eq!(&extracted[2].1[..p2.len()], p2);

        if format == ArchiveFormat::Sfcd {
            assert_eq!(extracted[0].0.name, "ALPHA");
            assert_eq!(extracted[1].0.name, "BETA02");
            assert_eq!(extracted[2].0.name, "GAMMA");
            // SFCD stores exact unpadded payload sizes!
            assert_eq!(extracted[0].1, p0);
            assert_eq!(extracted[1].1, p1);
            assert_eq!(extracted[2].1, p2);
        }

        // Repack extracted entries and verify byte-identical archive!
        let repack_in: Vec<(&str, &[u8])> = extracted
            .iter()
            .map(|(e, data)| (e.name.as_str(), *data))
            .collect();
        let repacked = pack_archive(format, &repack_in).unwrap();
        assert_eq!(repacked, packed);
    }

    #[test]
    fn pack_wayforward_sfcd_extract_repack_roundtrips() {
        roundtrip_format(ArchiveFormat::Pack);
        roundtrip_format(ArchiveFormat::WayForward);
        roundtrip_format(ArchiveFormat::Sfcd);
    }

    #[test]
    fn scans_all_three_archive_formats_in_rom() {
        let mut rom = vec![0xAAu8; 4096];
        let pack_bin = pack_archive(ArchiveFormat::Pack, &[("a", b"12345678"), ("b", b"ABCD")]).unwrap();
        let wf_bin = pack_archive(ArchiveFormat::WayForward, &[("x", b"WFDATA01"), ("y", b"WFDATA02")]).unwrap();
        let sfcd_bin = pack_archive(ArchiveFormat::Sfcd, &[("BGM01", b"SOUND1"), ("SFX02", b"SOUND2")]).unwrap();

        rom[0x200..0x200 + pack_bin.len()].copy_from_slice(&pack_bin);
        rom[0x600..0x600 + wf_bin.len()].copy_from_slice(&wf_bin);
        rom[0xA00..0xA00 + sfcd_bin.len()].copy_from_slice(&sfcd_bin);

        let found = scan_archives(&rom);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].format, ArchiveFormat::Pack);
        assert_eq!(found[0].rom_offset, 0x200);
        assert_eq!(found[1].format, ArchiveFormat::WayForward);
        assert_eq!(found[1].rom_offset, 0x600);
        assert_eq!(found[2].format, ArchiveFormat::Sfcd);
        assert_eq!(found[2].rom_offset, 0xA00);
        assert_eq!(found[2].entries[0].name, "BGM01");
        assert_eq!(found[2].entries[1].name, "SFX02");
    }

    #[test]
    fn parses_pack_without_sentinel_and_unpadded_wayforward() {
        // PACK with count=2 and no sentinel (offsets right at 8 + 8 = 16).
        let mut pack_no_sent = Vec::new();
        pack_no_sent.extend_from_slice(b"PACK");
        pack_no_sent.extend_from_slice(&2u32.to_le_bytes());
        pack_no_sent.extend_from_slice(&16u32.to_le_bytes());
        pack_no_sent.extend_from_slice(&20u32.to_le_bytes());
        pack_no_sent.extend_from_slice(b"AAAA");
        pack_no_sent.extend_from_slice(b"BBBBBB");

        let parsed = parse_archive(&pack_no_sent).unwrap();
        assert_eq!(parsed.format, ArchiveFormat::Pack);
        assert!(!parsed.has_end_sentinel);
        assert_eq!(parsed.entries.len(), 2);
        let ext = extract_archive_entries(&pack_no_sent, &parsed).unwrap();
        assert_eq!(ext[0].1, b"AAAA");
        assert_eq!(ext[1].1, b"BBBBBB");

        // WAYFORWARD unpadded 10-byte magic + count=1 + 2 offsets (with sentinel).
        let mut wf_unpadded = Vec::new();
        wf_unpadded.extend_from_slice(b"WAYFORWARD");
        wf_unpadded.extend_from_slice(&1u32.to_le_bytes());
        wf_unpadded.extend_from_slice(&22u32.to_le_bytes()); // 10 + 4 + 8 = 22
        wf_unpadded.extend_from_slice(&28u32.to_le_bytes());
        wf_unpadded.extend_from_slice(b"123456");

        let parsed_wf = parse_archive(&wf_unpadded).unwrap();
        assert_eq!(parsed_wf.format, ArchiveFormat::WayForward);
        assert!(parsed_wf.has_end_sentinel);
        let ext_wf = extract_archive_entries(&wf_unpadded, &parsed_wf).unwrap();
        assert_eq!(ext_wf[0].1, b"123456");
    }

    #[test]
    fn replace_archive_in_rom_inplace_and_relocated_pointer_patching() {
        let mut rom = vec![0xAAu8; 4096];
        rom[0xB2] = crate::FIXED_VALUE;

        let orig_off = 0x300usize;
        let orig_archive = pack_archive(
            ArchiveFormat::Sfcd,
            &[("MAP01", &[1u8; 64]), ("MAP02", &[2u8; 64])],
        )
        .unwrap();
        rom[orig_off..orig_off + orig_archive.len()].copy_from_slice(&orig_archive);

        // Write two ROM pointers to 0x0800_0300.
        let orig_bus = rom_offset_to_bus(orig_off).unwrap();
        rom[0x100..0x104].copy_from_slice(&orig_bus.to_le_bytes());
        rom[0x110..0x114].copy_from_slice(&orig_bus.to_le_bytes());

        // 1) Smaller archive fits in-place.
        let smaller = pack_archive(ArchiveFormat::Sfcd, &[("MAP01", &[9u8; 16])]).unwrap();
        let rep_in = replace_archive_in_rom(
            &mut rom,
            orig_off,
            &smaller,
            &ArchiveReplaceOptions::default(),
        )
        .unwrap();
        assert!(!rep_in.relocated);
        assert_eq!(rep_in.written_offset, orig_off);
        assert_eq!(rep_in.original_capacity, orig_archive.len());
        assert_eq!(rep_in.slack_bytes, orig_archive.len() - smaller.len());
        let check = parse_archive_at(&rom, orig_off).unwrap();
        assert_eq!(check.entries.len(), 1);
        assert_eq!(check.entries[0].name, "MAP01");

        // 2) Oversized archive rejected when allow_relocate = false.
        let larger = pack_archive(
            ArchiveFormat::Sfcd,
            &[("BIG01", &[0x55u8; 256]), ("BIG02", &[0x66u8; 256])],
        )
        .unwrap();
        let err = replace_archive_in_rom(
            &mut rom,
            orig_off,
            &larger,
            &ArchiveReplaceOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            Error::ResourceLimit {
                resource: "rom_archive_capacity",
                ..
            }
        ));

        // 3) Carve 0x00 free space at 0x600..0xA00 and 0xFF free space at 0xB00..0xF00.
        // Relocate must prefer 0xFF at 0xB00 over 0x00 at 0x600!
        rom[0x600..0xA00].fill(0x00);
        rom[0xB00..0xF00].fill(0xFF);
        let reloc_opts = ArchiveReplaceOptions {
            allow_relocate: true,
            ..ArchiveReplaceOptions::default()
        };
        let rep_reloc =
            replace_archive_in_rom(&mut rom, orig_off, &larger, &reloc_opts).unwrap();
        assert!(rep_reloc.relocated);
        assert_eq!(rep_reloc.written_offset, 0xB00);
        assert_eq!(rep_reloc.written_bus_address, 0x0800_0B00);
        assert_eq!(rep_reloc.patched_pointers, 2);
        assert_eq!(rep_reloc.patched_pointer_offsets, vec![0x100, 0x110]);
        assert_eq!(read_u32_le(&rom, 0x100), 0x0800_0B00);
        assert_eq!(read_u32_le(&rom, 0x110), 0x0800_0B00);

        let parsed_reloc = parse_archive_at(&rom, 0xB00).unwrap();
        assert_eq!(parsed_reloc.entries.len(), 2);
        assert_eq!(parsed_reloc.entries[0].name, "BIG01");
        assert_eq!(parsed_reloc.entries[1].name, "BIG02");
    }

    #[test]
    fn parses_retail_variable_length_sfcd_archive() {
        let files: [(&str, &[u8]); 3] = [
            ("main.bin", b"ARM_MAIN_PAYLOAD"),
            ("movie_1.mmstr", b"MMSTR_STREAM_ONE_DATA"),
            ("menuresources.mmstr", b"MENU_RES"),
        ];

        // Build directory body starting at +12: (u32 size, u32 rel_off, NUL-terminated name, 4B aligned)
        let mut dir_entries = Vec::new();
        let mut rel_off = 0usize;
        for &(name, data) in &files {
            dir_entries.extend_from_slice(&(data.len() as u32).to_le_bytes());
            dir_entries.extend_from_slice(&(rel_off as u32).to_le_bytes());
            dir_entries.extend_from_slice(name.as_bytes());
            dir_entries.push(0);
            while !dir_entries.len().is_multiple_of(4) {
                dir_entries.push(0);
            }
            rel_off += align_up(data.len(), 4);
        }

        // dir_size covers [file_count(4) + dir_entries], so data_base = 8 + dir_size
        let dir_size = (4 + dir_entries.len()) as u16;
        let mut archive = Vec::new();
        archive.extend_from_slice(b"SFCD");
        archive.extend_from_slice(&dir_size.to_le_bytes());
        archive.extend_from_slice(&0u16.to_le_bytes());
        archive.extend_from_slice(&(files.len() as u32).to_le_bytes());
        archive.extend_from_slice(&dir_entries);
        assert_eq!(archive.len(), 8 + dir_size as usize);

        for &(_, data) in &files {
            archive.extend_from_slice(data);
            while !archive.len().is_multiple_of(4) {
                archive.push(0);
            }
        }

        // Embed inside a ROM buffer and verify both parse_archive_at and scan_archives.
        let mut rom = vec![0xFFu8; 0x1000];
        let off = 0x0400usize;
        rom[off..off + archive.len()].copy_from_slice(&archive);

        let parsed = parse_archive_at(&rom, off).expect("variable SFCD should parse");
        assert_eq!(parsed.format, ArchiveFormat::Sfcd);
        assert_eq!(parsed.entries.len(), 3);
        assert_eq!(parsed.entries[0].name, "main.bin");
        assert_eq!(parsed.entries[1].name, "movie_1.mmstr");
        assert_eq!(parsed.entries[2].name, "menuresources.mmstr");

        let extracted = extract_archive_entries(&rom, &parsed).expect("extract should succeed");
        assert_eq!(extracted[0].1, files[0].1);
        assert_eq!(extracted[1].1, files[1].1);
        assert_eq!(extracted[2].1, files[2].1);

        let scanned = scan_archives(&rom);
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].rom_offset, off);
        assert_eq!(scanned[0].entries.len(), 3);
    }
}
