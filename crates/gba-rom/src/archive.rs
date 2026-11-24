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

use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::{rom_offset_to_bus, HEADER_SIZE};

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

    let mut offsets = Vec::with_capacity(count + 1);
    for i in 0..count {
        let off = read_u32_le(slice, toc_start + i * 4) as usize;
        if off < toc_end_no_sentinel || off > slice.len() {
            return Err(Error::InvalidField {
                what: "archive_toc_offset",
                value: off as u64,
            });
        }
        if let Some(&prev) = offsets.last() {
            if off < prev {
                return Err(Error::Malformed(format!(
                    "non-monotonic archive TOC offset at entry {i}: {off} < {prev}"
                )));
            }
        }
        offsets.push(off);
    }

    // Check for an explicit (count + 1)-th end-of-archive sentinel offset.
    let toc_end_with_sentinel = toc_end_no_sentinel + 4;
    let mut has_end_sentinel = false;
    if offsets[0] >= toc_end_with_sentinel && toc_end_with_sentinel <= slice.len() {
        let sentinel = read_u32_le(slice, toc_end_no_sentinel) as usize;
        let last = offsets[count - 1];
        if sentinel >= last && sentinel <= slice.len() && (sentinel > offsets[0] || count == 1) {
            offsets.push(sentinel);
            has_end_sentinel = true;
        }
    }

    let total_size = if has_end_sentinel {
        *offsets.last().unwrap_or(&toc_end_with_sentinel)
    } else if scanning {
        // Without a sentinel during a ROM scan, avoid greedy consumption of the entire rest of the ROM.
        // Require at least 2 entries with strictly increasing offsets so random false positives are rejected.
        if count < 2 || offsets[count - 1] <= offsets[0] {
            return Err(Error::Malformed(
                "unbounded archive without sentinel failed scan plausibility check".into(),
            ));
        }
        // Bound the trailing entry to a reasonable estimate (last offset + average entry size, clamped to slice).
        let span = offsets[count - 1] - offsets[0];
        let avg = (span / (count - 1)).max(4);
        (offsets[count - 1].saturating_add(avg)).min(slice.len())
    } else {
        slice.len()
    };

    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let start = offsets[i];
        let end = if i + 1 < offsets.len() {
            offsets[i + 1]
        } else {
            total_size
        };
        let size = end.saturating_sub(start);
        entries.push(ArchiveEntry {
            index: i,
            name: format!("entry_{i:04}"),
            offset: start,
            size,
        });
    }

    Ok(ParsedArchive {
        format,
        rom_offset,
        total_size,
        has_end_sentinel,
        entries,
    })
}

/// Parse an `SFCD` (`.mmstr`) archive:
/// `b"SFCD"` + `u32 count` + `count * (8-byte name + u32 offset + u32 size)`.
fn parse_sfcd(slice: &[u8], rom_offset: usize) -> Result<ParsedArchive> {
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
    if dir_end > slice.len() {
        return Err(Error::Truncated {
            offset: rom_offset + 8,
            needed: dir_end - 8,
            available: slice.len().saturating_sub(8),
        });
    }

    let mut entries = Vec::with_capacity(count);
    let mut max_end = dir_end;

    for i in 0..count {
        let rec_off = 8 + i * 16;
        let name_raw = &slice[rec_off..rec_off + 8];
        let name = parse_sfcd_name(name_raw, i)?;
        let offset = read_u32_le(slice, rec_off + 8) as usize;
        let size = read_u32_le(slice, rec_off + 12) as usize;

        if size > 0 && offset < dir_end {
            return Err(Error::InvalidField {
                what: "sfcd_entry_offset",
                value: offset as u64,
            });
        }
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

fn parse_sfcd_name(raw: &[u8], index: usize) -> Result<String> {
    let nul_pos = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let name_bytes = &raw[..nul_pos];
    if !name_bytes.iter().all(|&b| (0x20..=0x7E).contains(&b)) {
        return Err(Error::Malformed(format!(
            "non-ASCII bytes in SFCD directory entry {index} name"
        )));
    }
    let trimmed = std::str::from_utf8(name_bytes)
        .map_err(|_| Error::Malformed(format!("invalid UTF-8 in SFCD entry {index} name")))?
        .trim_end();
    if trimmed.is_empty() {
        Ok(format!("entry_{index:04}"))
    } else {
        Ok(trimmed.to_string())
    }
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
    let header_len = magic.len() + 4 + (count + 1) * 4;
    let mut cursor = align_up(header_len, DEFAULT_ARCHIVE_ALIGN);

    let mut offsets = Vec::with_capacity(count + 1);
    for &(_, payload) in entries {
        offsets.push(cursor);
        cursor = align_up(
            cursor.checked_add(payload.len()).ok_or(Error::ResourceLimit {
                resource: "archive_size",
                requested: u64::MAX,
                limit: u32::MAX as u64,
            })?,
            DEFAULT_ARCHIVE_ALIGN,
        );
    }
    let total_size = cursor;
    if total_size > u32::MAX as usize {
        return Err(Error::ResourceLimit {
            resource: "archive_size",
            requested: total_size as u64,
            limit: u32::MAX as u64,
        });
    }
    offsets.push(total_size);

    let mut out = vec![0u8; total_size];
    out[..magic.len()].copy_from_slice(magic);
    out[magic.len()..magic.len() + 4].copy_from_slice(&(count as u32).to_le_bytes());

    let toc_start = magic.len() + 4;
    for (i, &off) in offsets.iter().enumerate() {
        let pos = toc_start + i * 4;
        out[pos..pos + 4].copy_from_slice(&(off as u32).to_le_bytes());
    }

    for (i, &(_, payload)) in entries.iter().enumerate() {
        let start = offsets[i];
        out[start..start + payload.len()].copy_from_slice(payload);
    }

    Ok(out)
}

fn pack_sfcd(entries: &[(&str, &[u8])]) -> Result<Vec<u8>> {
    let count = entries.len();
    let dir_end = 8 + count * 16;
    let mut cursor = align_up(dir_end, DEFAULT_ARCHIVE_ALIGN);

    let mut offsets = Vec::with_capacity(count);
    for (i, &(_, payload)) in entries.iter().enumerate() {
        offsets.push(cursor);
        let end = cursor.checked_add(payload.len()).ok_or(Error::ResourceLimit {
            resource: "archive_size",
            requested: u64::MAX,
            limit: u32::MAX as u64,
        })?;
        cursor = if i + 1 < count {
            align_up(end, DEFAULT_ARCHIVE_ALIGN)
        } else {
            end
        };
    }
    let total_size = cursor;
    if total_size > u32::MAX as usize {
        return Err(Error::ResourceLimit {
            resource: "archive_size",
            requested: total_size as u64,
            limit: u32::MAX as u64,
        });
    }

    let mut out = vec![0u8; total_size];
    out[0..4].copy_from_slice(MAGIC_SFCD);
    out[4..8].copy_from_slice(&(count as u32).to_le_bytes());

    for (i, &(name, payload)) in entries.iter().enumerate() {
        let rec_off = 8 + i * 16;
        let mut name_buf = [0u8; 8];
        let clean: Vec<u8> = name
            .bytes()
            .filter(|&b| (0x20..=0x7E).contains(&b))
            .take(8)
            .collect();
        if clean.is_empty() {
            let fallback = format!("E{i:07}");
            name_buf.copy_from_slice(&fallback.as_bytes()[..8]);
        } else {
            name_buf[..clean.len()].copy_from_slice(&clean);
        }
        out[rec_off..rec_off + 8].copy_from_slice(&name_buf);
        out[rec_off + 8..rec_off + 12].copy_from_slice(&(offsets[i] as u32).to_le_bytes());
        out[rec_off + 12..rec_off + 16].copy_from_slice(&(payload.len() as u32).to_le_bytes());

        let start = offsets[i];
        out[start..start + payload.len()].copy_from_slice(payload);
    }

    Ok(out)
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
        rom[rom_offset..rom_offset + written_bytes].copy_from_slice(new_archive_bytes);
        let slack_bytes = original_capacity - written_bytes;
        if slack_bytes > 0 {
            rom[rom_offset + written_bytes..rom_offset + original_capacity].fill(options.pad_byte);
        }
        let written_bus_address = rom_offset_to_bus(rom_offset).ok_or(Error::InvalidField {
            what: "written_rom_offset",
            value: rom_offset as u64,
        })?;
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
    let new_offset = find_archive_free_space(
        rom,
        written_bytes,
        options.relocate_alignment,
        search_start,
        &[0xFF, 0x00],
        Some((rom_offset, rom_offset + original_capacity)),
    )
    .ok_or(Error::ResourceLimit {
        resource: "rom_free_space",
        requested: written_bytes as u64,
        limit: 0,
    })?;

    rom[new_offset..new_offset + written_bytes].copy_from_slice(new_archive_bytes);
    let patched_pointer_offsets = patch_archive_rom_pointers(
        rom,
        rom_offset,
        new_offset,
        Some((new_offset, new_offset + written_bytes)),
    )?;
    let written_bus_address = rom_offset_to_bus(new_offset).ok_or(Error::InvalidField {
        what: "relocated_rom_offset",
        value: new_offset as u64,
    })?;

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

fn find_archive_free_space(
    rom: &[u8],
    needed_len: usize,
    alignment: usize,
    search_start: usize,
    free_bytes: &[u8],
    exclude_range: Option<(usize, usize)>,
) -> Option<usize> {
    if needed_len == 0 || rom.len() < needed_len {
        return None;
    }
    let align = alignment.max(1);
    let base_start = align_up(search_start.max(ARCHIVE_FREE_SPACE_START), align);
    if base_start + needed_len > rom.len() {
        return None;
    }

    for &fill in free_bytes {
        let mut pos = base_start;
        while pos + needed_len <= rom.len() {
            if let Some((ex_start, ex_end)) = exclude_range {
                if pos < ex_end && pos + needed_len > ex_start {
                    pos = align_up(ex_end, align);
                    continue;
                }
            }
            if let Some(mismatch) = rom[pos..pos + needed_len].iter().position(|&b| b != fill) {
                pos = align_up(pos + mismatch + 1, align);
            } else {
                return Some(pos);
            }
        }
    }
    None
}

fn patch_archive_rom_pointers(
    rom: &mut [u8],
    old_offset: usize,
    new_offset: usize,
    skip_range: Option<(usize, usize)>,
) -> Result<Vec<usize>> {
    let old_bus = rom_offset_to_bus(old_offset).ok_or(Error::InvalidField {
        what: "old_rom_offset",
        value: old_offset as u64,
    })?;
    let new_bus = rom_offset_to_bus(new_offset).ok_or(Error::InvalidField {
        what: "new_rom_offset",
        value: new_offset as u64,
    })?;

    let old_bytes = old_bus.to_le_bytes();
    let new_bytes = new_bus.to_le_bytes();
    let mut patched = Vec::new();

    let scan_end = rom.len() & !3;
    let mut pos = HEADER_SIZE;
    while pos < scan_end {
        if let Some((skip_start, skip_end)) = skip_range {
            if pos >= skip_start && pos < skip_end {
                pos = align_up(skip_end, 4);
                continue;
            }
        }
        if rom[pos..pos + 4] == old_bytes {
            rom[pos..pos + 4].copy_from_slice(&new_bytes);
            patched.push(pos);
        }
        pos += 4;
    }
    Ok(patched)
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
}
