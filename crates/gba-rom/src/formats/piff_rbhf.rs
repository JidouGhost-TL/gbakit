//! Unified `PIFF` / `RBHF` relocatable module container & `GLOBAL_SET` manifest parser.
//!
//! Consolidates the `PIFF` / `RBHF` / `RBHH` chunk family (`BODY`, `PACK`, `PKDW`, `PKDL`,
//! `RELC`, `GLOB`) and `GLOBAL_SET\0\0` aligned resource manifests.
//!
//! **Explicit `PACK` disambiguation:** `PACK` sub-chunks inside `PIFF` containers carry
//! `(b"PACK", u32 LE payload_len, opaque compressed blob)` and are **not**
//! [`crate::archive::ArchiveFormat::Pack`] offset-table TOC archives. This module claims the
//! entire enclosing `PIFF` container (and exposes [`is_piff_pack_chunk`]) so ROM asset maps
//! never misclassify `PIFF` sub-chunks as standalone `archive.pack` containers.
//!
//! Built directly on `formatkit-layout` `0.1.17` [`ChunkStreamSpec`] (`with_allowed_tags`,
//! `with_stop_on_unknown_tag`, [`parse_chunk_stream_with_extent`]),
//! [`read_header_inclusive_chunk`], and [`AlignedStringBankSpec`].

use formatkit_core::{u32_at, Endian, Error, Result};
use formatkit_layout::{
    pack_aligned_string_bank, pack_chunk_stream, parse_aligned_string_bank,
    parse_chunk_stream_with_extent, read_header_inclusive_chunk, AlignedStringBankSpec, ChunkInput,
    ChunkSizeMode, ChunkStreamSpec, IntFieldSpec, PackPolicy, StringBankEncoding,
    StringBankTermination,
};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_PIFF_RBHF};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_PIFF: &[u8; 4] = b"PIFF";
const MAGIC_RBHF: &[u8; 4] = b"RBHF";
const MAGIC_RBHH: &[u8; 4] = b"RBHH";
const MAGIC_GLOB: &[u8; 4] = b"GLOB";
const MAGIC_PACK: &[u8; 4] = b"PACK";
const MAGIC_GLOBAL_SET: &[u8; 12] = b"GLOBAL_SET\0\0";

const PIFF_INNER_TAGS: &[&[u8]] = &[b"BODY", b"PACK", b"PKDW", b"PKDL", b"RELC"];
const MAX_PIFF_CHUNKS: usize = 4096;
const MAX_TABLE_ENTRIES: usize = 1024;

/// Entry-point variant of a `studio.piff-rbhf` asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiffRbhfKind {
    /// `PIFF` + `RBHF`/`RBHH` relocatable module container.
    Piff,
    /// `GLOBAL_SET\0\0` 4-byte-aligned text manifest.
    GlobalSet,
}

impl PiffRbhfKind {
    pub const fn subkind(self) -> &'static str {
        match self {
            Self::Piff => "piff",
            Self::GlobalSet => "global_set",
        }
    }
}

/// One 12-byte section-table entry in a `PIFF`/`RBHH` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PiffSectionEntry {
    pub field_a: u32,
    pub decoded_size: u32,
    pub flags: u32,
}

/// One validated chunk inside a `PIFF` container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiffChunk {
    pub tag: [u8; 4],
    pub header_offset: usize,
    pub payload_offset: usize,
    pub payload_size: usize,
    pub payload: Vec<u8>,
}

/// Parsed `studio.piff-rbhf` asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiffRbhfAsset {
    pub kind: PiffRbhfKind,
    pub sections: Vec<PiffSectionEntry>,
    pub chunks: Vec<PiffChunk>,
    pub manifest_entry_count: usize,
    pub manifest_entries: Vec<Vec<u8>>,
    pub consumed_end: usize,
}

impl PiffRbhfAsset {
    /// Serialize this container or manifest back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Serialize a parsed `studio.piff-rbhf` asset back into its exact binary representation.
pub fn pack(asset: &PiffRbhfAsset) -> Result<Vec<u8>> {
    match asset.kind {
        PiffRbhfKind::Piff => pack_piff(asset),
        PiffRbhfKind::GlobalSet => pack_global_set(asset),
    }
}

fn pack_piff(asset: &PiffRbhfAsset) -> Result<Vec<u8>> {
    let Some((glob, inner)) = asset.chunks.split_last() else {
        return Err(Error::InvalidField {
            what: "PIFF chunks empty",
            value: 0,
        });
    };
    if &glob.tag != MAGIC_GLOB {
        return Err(Error::InvalidField {
            what: "PIFF missing terminal GLOB",
            value: 0,
        });
    }
    if asset.sections.is_empty() || asset.sections.len() > MAX_TABLE_ENTRIES {
        return Err(Error::InvalidField {
            what: "PIFF section_count",
            value: asset.sections.len() as u64,
        });
    }
    let table_len = asset
        .sections
        .len()
        .checked_mul(12)
        .ok_or(Error::InvalidField {
            what: "PIFF table_len",
            value: asset.sections.len() as u64,
        })?;
    let chunks_start = 20usize.checked_add(table_len).ok_or(Error::InvalidField {
        what: "PIFF chunks_start",
        value: table_len as u64,
    })?;
    let mut inner_bytes = 0usize;
    for ch in inner {
        inner_bytes = inner_bytes
            .checked_add(8)
            .and_then(|v| v.checked_add(ch.payload.len()))
            .ok_or(Error::InvalidField {
                what: "PIFF inner chunk size",
                value: ch.payload.len() as u64,
            })?;
    }
    let glob_inclusive = 8usize
        .checked_add(glob.payload.len())
        .ok_or(Error::InvalidField {
            what: "PIFF GLOB inclusive size",
            value: glob.payload.len() as u64,
        })?;
    let total_size = chunks_start
        .checked_add(inner_bytes)
        .and_then(|v| v.checked_add(glob_inclusive))
        .ok_or(Error::InvalidField {
            what: "PIFF total_size",
            value: inner_bytes as u64,
        })?;

    let mut header_prefix = Vec::with_capacity(chunks_start);
    header_prefix.extend_from_slice(MAGIC_PIFF);
    header_prefix.extend_from_slice(&(total_size as u32).to_le_bytes());
    header_prefix.extend_from_slice(MAGIC_RBHF);
    header_prefix.extend_from_slice(MAGIC_RBHH);
    header_prefix.extend_from_slice(&(table_len as u32).to_le_bytes());
    for sec in &asset.sections {
        header_prefix.extend_from_slice(&sec.field_a.to_le_bytes());
        header_prefix.extend_from_slice(&sec.decoded_size.to_le_bytes());
        header_prefix.extend_from_slice(&sec.flags.to_le_bytes());
    }

    let spec = ChunkStreamSpec::new(
        chunks_start,
        8,
        IntFieldSpec::u32_le(4),
        ChunkSizeMode::PayloadOnly,
        MAX_PIFF_CHUNKS,
    )
    .with_tag(0, 4)
    .with_allowed_tags(PIFF_INNER_TAGS)
    .with_stop_on_unknown_tag(true)
    .with_require_eof_closure(false);

    let inputs: Vec<ChunkInput<'_>> = inner
        .iter()
        .map(|c| ChunkInput {
            tag: &c.tag,
            payload: &c.payload,
            header_template: None,
            trailer: None,
        })
        .collect();

    let mut out = pack_chunk_stream(&spec, &PackPolicy::TIGHT, &header_prefix, &inputs).map_err(
        |_| Error::InvalidField {
            what: "PIFF pack_chunk_stream",
            value: inner.len() as u64,
        },
    )?;

    out.extend_from_slice(MAGIC_GLOB);
    out.extend_from_slice(&(glob_inclusive as u32).to_le_bytes());
    out.extend_from_slice(&glob.payload);
    Ok(out)
}

fn pack_global_set(asset: &PiffRbhfAsset) -> Result<Vec<u8>> {
    let spec = AlignedStringBankSpec::aligned(0, 4, 2048)
        .with_encoding(StringBankEncoding::PrintableAscii)
        .with_termination(StringBankTermination::StopOnInvalid)
        .with_max_entry_len(260);
    let slices: Vec<&[u8]> = asset.manifest_entries.iter().map(Vec::as_slice).collect();
    pack_aligned_string_bank(&spec, &PackPolicy::TIGHT, &[], &slices).map_err(|_| {
        Error::InvalidField {
            what: "GLOBAL_SET pack_aligned_string_bank",
            value: asset.manifest_entries.len() as u64,
        }
    })
}

/// Returns `true` if `data` starts with a valid `PIFF`/`RBHF` container or `GLOBAL_SET` manifest.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Returns `true` if `data` looks like a `PIFF` `PACK` sub-chunk (`b"PACK"` + `u32 LE payload_len`
/// aligned to 4 bytes) rather than a `gbakit` `archive.pack` TOC archive.
pub fn is_piff_pack_chunk(data: &[u8]) -> bool {
    if data.len() < 16 || &data[..4] != MAGIC_PACK {
        return false;
    }
    let Ok(payload_len) = u32_at(data, 4) else {
        return false;
    };
    let len = payload_len as usize;
    if !(8..=16 * 1024 * 1024).contains(&len) || (len & 3) != 0 {
        return false;
    }
    let Some(end) = 8usize.checked_add(len) else {
        return false;
    };
    if end > data.len() {
        return false;
    }
    // Disambiguate from archive.pack: if the next chunk right at `end` is a known PIFF sibling tag,
    // this is definitely a PIFF PACK sub-chunk.
    if let Some(next_tag) = data.get(end..end + 4) {
        if PIFF_INNER_TAGS.contains(&next_tag) || next_tag == MAGIC_GLOB {
            return true;
        }
    }
    false
}

/// Parse a `PIFF`/`RBHF` container or `GLOBAL_SET` text manifest from the start of `data`.
pub fn parse(data: &[u8]) -> Result<PiffRbhfAsset> {
    if data.starts_with(MAGIC_PIFF) {
        parse_piff(data)
    } else if data.starts_with(MAGIC_GLOBAL_SET) {
        parse_global_set(data)
    } else if data.len() < 4 {
        Err(Error::Truncated {
            offset: 0,
            needed: 4,
            available: data.len(),
        })
    } else {
        let found = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_PIFF),
            found,
        })
    }
}

fn parse_piff(data: &[u8]) -> Result<PiffRbhfAsset> {
    if data.len() < 20 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 20,
            available: data.len(),
        });
    }
    let total_size_u32 = u32_at(data, 4)?;
    let total_size = total_size_u32 as usize;
    if total_size < 20 || total_size > data.len() {
        return Err(Error::InvalidField {
            what: "PIFF total_size",
            value: u64::from(total_size_u32),
        });
    }
    if &data[8..12] != MAGIC_RBHF || &data[12..16] != MAGIC_RBHH {
        return Err(Error::InvalidField {
            what: "PIFF RBHF/RBHH magic",
            value: 0,
        });
    }

    let table_len_u32 = u32_at(data, 16)?;
    let table_len = table_len_u32 as usize;
    if table_len < 12 || !table_len.is_multiple_of(12) {
        return Err(Error::InvalidField {
            what: "PIFF RBHH table_len",
            value: u64::from(table_len_u32),
        });
    }
    let section_count = table_len / 12;
    if section_count > MAX_TABLE_ENTRIES {
        return Err(Error::InvalidField {
            what: "PIFF section_count",
            value: section_count as u64,
        });
    }
    let chunks_start = 20usize.checked_add(table_len).ok_or(Error::InvalidField {
        what: "PIFF chunks_start",
        value: table_len as u64,
    })?;
    if chunks_start > total_size {
        return Err(Error::Truncated {
            offset: 20,
            needed: table_len,
            available: total_size.saturating_sub(20),
        });
    }

    let mut sections = Vec::with_capacity(section_count);
    for i in 0..section_count {
        let off = 20usize + i * 12;
        sections.push(PiffSectionEntry {
            field_a: u32_at(data, off)?,
            decoded_size: u32_at(data, off + 4)?,
            flags: u32_at(data, off + 8)?,
        });
    }

    // Walk inner payload-sized chunks (BODY, PACK, PKDW, PKDL, RELC) via FormatKit ChunkStreamSpec,
    // stopping cleanly at the terminal header-inclusive GLOB chunk.
    let container = &data[..total_size];
    let spec = ChunkStreamSpec::new(
        chunks_start,
        8,
        IntFieldSpec::u32_le(4),
        ChunkSizeMode::PayloadOnly,
        MAX_PIFF_CHUNKS,
    )
    .with_tag(0, 4)
    .with_allowed_tags(PIFF_INNER_TAGS)
    .with_stop_on_unknown_tag(true)
    .with_require_eof_closure(false);

    let stream = parse_chunk_stream_with_extent(container, &spec).map_err(|_| {
        Error::InvalidField {
            what: "PIFF inner chunk stream",
            value: chunks_start as u64,
        }
    })?;

    let mut chunks = Vec::with_capacity(stream.chunks.len() + 1);
    let mut data_chunk_count = 0usize;
    for ch in &stream.chunks {
        if ch.tag.len() != 4 {
            return Err(Error::InvalidField {
                what: "PIFF chunk tag length",
                value: ch.tag.len() as u64,
            });
        }
        let tag = [ch.tag[0], ch.tag[1], ch.tag[2], ch.tag[3]];
        if &tag != b"RELC" {
            data_chunk_count += 1;
        }
        let payload = ch
            .payload(container)
            .ok_or(Error::InvalidField {
                what: "PIFF chunk payload bounds",
                value: ch.payload_offset as u64,
            })?
            .to_vec();
        chunks.push(PiffChunk {
            tag,
            header_offset: ch.header_offset,
            payload_offset: ch.payload_offset,
            payload_size: ch.payload_size,
            payload,
        });
    }

    // Terminal GLOB chunk uses header-inclusive sizing: glob_offset + glob_size == total_size.
    let glob_offset = stream.consumed_end;
    let glob = read_header_inclusive_chunk(container, glob_offset, Endian::Little).map_err(
        |_| Error::InvalidField {
            what: "PIFF terminal GLOB chunk",
            value: glob_offset as u64,
        },
    )?;
    if &glob.tag != MAGIC_GLOB || glob.whole.end != total_size {
        return Err(Error::InvalidField {
            what: "PIFF terminal GLOB end",
            value: glob.whole.end as u64,
        });
    }
    if data_chunk_count == 0 {
        return Err(Error::InvalidField {
            what: "PIFF data chunk count",
            value: 0,
        });
    }

    chunks.push(PiffChunk {
        tag: glob.tag,
        header_offset: glob.whole.start,
        payload_offset: glob.payload.start,
        payload_size: glob.payload.len(),
        payload: container[glob.payload.clone()].to_vec(),
    });

    Ok(PiffRbhfAsset {
        kind: PiffRbhfKind::Piff,
        sections,
        chunks,
        manifest_entry_count: 0,
        manifest_entries: Vec::new(),
        consumed_end: total_size,
    })
}

fn parse_global_set(data: &[u8]) -> Result<PiffRbhfAsset> {
    if data.len() < 32 || !data.starts_with(MAGIC_GLOBAL_SET) {
        return Err(Error::Truncated {
            offset: 0,
            needed: 32,
            available: data.len(),
        });
    }

    let spec = AlignedStringBankSpec::aligned(0, 4, 2048)
        .with_encoding(StringBankEncoding::PrintableAscii)
        .with_termination(StringBankTermination::StopOnInvalid)
        .with_max_entry_len(260);

    let bank = parse_aligned_string_bank(data, &spec).map_err(|_| Error::InvalidField {
        what: "GLOBAL_SET string bank",
        value: 0,
    })?;

    // Stop cleanly before any empty string or non-zero alignment padding slot (trailing binary data).
    let mut manifest_entries = Vec::with_capacity(bank.entries.len());
    let mut consumed_end = 0usize;
    for entry in &bank.entries {
        if entry.bytes.is_empty() {
            break;
        }
        let Some(nul_end) = entry.offset.checked_add(entry.len).and_then(|v| v.checked_add(1)) else {
            break;
        };
        let slot_end = align_up_4(nul_end);
        if slot_end > data.len() || data[entry.offset + entry.len..slot_end].iter().any(|&b| b != 0) {
            break;
        }
        manifest_entries.push(entry.bytes.clone());
        consumed_end = slot_end;
    }

    if manifest_entries.len() < 8 {
        return Err(Error::InvalidField {
            what: "GLOBAL_SET leading record count",
            value: manifest_entries.len() as u64,
        });
    }

    // Anti-FP check: require `_SET_END` or `.rbh` marker within the parsed bank (or first 2048 bytes).
    let probe = &data[..consumed_end.min(2048)];
    let has_marker = contains_subslice(probe, b"_SET_END") || contains_subslice(probe, b".rbh");
    if !has_marker {
        return Err(Error::InvalidField {
            what: "GLOBAL_SET manifest markers",
            value: 0,
        });
    }

    Ok(PiffRbhfAsset {
        kind: PiffRbhfKind::GlobalSet,
        sections: Vec::new(),
        chunks: Vec::new(),
        manifest_entry_count: manifest_entries.len(),
        manifest_entries,
        consumed_end,
    })
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Scan `rom` on 4-byte aligned offsets for verified `studio.piff-rbhf` containers and manifests.
pub fn scan_piff_rbhf(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective = &rom[..rom.len().min(MAX_ROM_SIZE)];
    if effective.len() < HEADER_SIZE + 20 {
        return Vec::new();
    }

    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + 12 <= effective.len() {
        let tag = &effective[off..off + 4];
        let parsed = if tag == MAGIC_PIFF {
            parse_piff(&effective[off..])
        } else if effective[off..].starts_with(MAGIC_GLOBAL_SET) {
            parse_global_set(&effective[off..])
        } else {
            off += 4;
            continue;
        };

        if let Ok(asset) = parsed {
            let size = asset.consumed_end;
            if size > 0 && off.checked_add(size).is_some_and(|e| e <= effective.len()) {
                found.push(DetectedFormatExtent {
                    format_id: FORMAT_ID_PIFF_RBHF,
                    subkind: asset.kind.subkind(),
                    offset: off,
                    size,
                });
                off = align_up_4(off.saturating_add(size)).max(off + 4);
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
    use crate::archive::scan_archives;

    pub(crate) fn make_valid_piff() -> Vec<u8> {
        let mut body = Vec::new();
        // 2 data chunks: PACK (16 bytes) + BODY (12 bytes)
        body.extend_from_slice(b"PACK");
        body.extend_from_slice(&16u32.to_le_bytes());
        body.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        body.extend_from_slice(b"BODY");
        body.extend_from_slice(&12u32.to_le_bytes());
        body.extend_from_slice(&[0x42u8; 12]);
        // RELC chunk (12 bytes)
        body.extend_from_slice(b"RELC");
        body.extend_from_slice(&12u32.to_le_bytes());
        body.extend_from_slice(&[0u8; 12]);
        // Terminal GLOB chunk (header-inclusive size = 8 + 16 = 24)
        body.extend_from_slice(MAGIC_GLOB);
        body.extend_from_slice(&24u32.to_le_bytes());
        body.extend_from_slice(&[0x11u8; 16]);

        let table_len = 24u32; // 2 entries * 12 bytes
        let total_size = 20u32 + table_len + body.len() as u32;

        let mut out = Vec::with_capacity(total_size as usize);
        out.extend_from_slice(MAGIC_PIFF);
        out.extend_from_slice(&total_size.to_le_bytes());
        out.extend_from_slice(MAGIC_RBHF);
        out.extend_from_slice(MAGIC_RBHH);
        out.extend_from_slice(&table_len.to_le_bytes());
        // Entry 0 (PACK)
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&64u32.to_le_bytes());
        out.extend_from_slice(&0x0004_0002u32.to_le_bytes());
        // Entry 1 (BODY)
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&12u32.to_le_bytes());
        out.extend_from_slice(&0x0004_0000u32.to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn make_valid_global_set() -> Vec<u8> {
        let entries = [
            "GLOBAL_SET",
            "art/ui/hud.rbh",
            "art/ui/font.rbh",
            "art/bg/sky.rbh",
            "TEXTURES",
            "PALETTE_0",
            "SequenceList",
            "MapList",
            "GLOBAL_SET_END",
        ];
        let mut out = Vec::new();
        for s in entries {
            out.extend_from_slice(s.as_bytes());
            out.push(0);
            while !out.len().is_multiple_of(4) {
                out.push(0);
            }
        }
        out
    }

    #[test]
    fn parses_piff_and_global_set_and_disambiguates_pack() {
        let mut piff = make_valid_piff();
        let piff_len = piff.len();
        piff.extend_from_slice(&[0xFF; 12]); // trailing ROM bytes

        assert!(detect(&piff));
        let parsed = parse(&piff).unwrap();
        assert_eq!(parsed.kind, PiffRbhfKind::Piff);
        assert_eq!(parsed.sections.len(), 2);
        assert_eq!(parsed.chunks.len(), 4);
        assert_eq!(parsed.consumed_end, piff_len);

        // PACK sub-chunk inside PIFF is recognized as a PIFF PACK sub-chunk, NOT archive.pack
        let pack_off = parsed.chunks[0].header_offset;
        assert!(is_piff_pack_chunk(&piff[pack_off..]));
        let mut rom = vec![0u8; 0x400];
        rom[0x200..0x200 + piff_len].copy_from_slice(&piff[..piff_len]);
        assert!(scan_archives(&rom).is_empty());
        let piff_found = scan_piff_rbhf(&rom);
        assert_eq!(piff_found.len(), 1);
        assert_eq!(piff_found[0].offset, 0x200);
        assert_eq!(piff_found[0].size, piff_len);

        let gset = make_valid_global_set();
        assert!(detect(&gset));
        let g = parse(&gset).unwrap();
        assert_eq!(g.kind, PiffRbhfKind::GlobalSet);
        assert_eq!(g.manifest_entry_count, 9);
        assert_eq!(g.consumed_end, gset.len());
    }

    #[test]
    fn roundtrips_piff_and_global_set_byte_identically() {
        let piff = make_valid_piff();
        let p = parse(&piff).unwrap();
        assert_eq!(p.to_bytes().unwrap(), piff[..p.consumed_end]);

        let gset = make_valid_global_set();
        let g = parse(&gset).unwrap();
        assert_eq!(g.to_bytes().unwrap(), gset[..g.consumed_end]);

        // Trailing non-zero alignment slots (binary data after the string bank) must not be
        // swallowed as string entries, preserving byte-identical round-trip.
        let mut with_tail = gset.clone();
        with_tail.extend_from_slice(&[0x00, 0x00, 0x40, 0x50]);
        let g2 = parse(&with_tail).unwrap();
        assert_eq!(g2.consumed_end, gset.len());
        assert_eq!(g2.to_bytes().unwrap(), gset);

        let mut with_nonzero_pad = gset.clone();
        with_nonzero_pad.extend_from_slice(&[b'a', 0x00, 0x62, 0x96]);
        let g3 = parse(&with_nonzero_pad).unwrap();
        assert_eq!(g3.consumed_end, gset.len());
        assert_eq!(g3.to_bytes().unwrap(), gset);
    }

    #[test]
    fn rejects_malformed_truncated_and_random_inputs() {
        assert!(!detect(&[]));
        let piff = make_valid_piff();
        assert!(!detect(&piff[..piff.len() - 4]));

        let mut bad_glob = piff.clone();
        // Break GLOB inclusive size so it no longer reaches total_size
        let glob_size_off = bad_glob.len() - 20;
        bad_glob[glob_size_off] = 16;
        assert!(!detect(&bad_glob));

        let mut bad_table = piff.clone();
        bad_table[16] = 10; // not multiple of 12
        assert!(!detect(&bad_table));

        let mut seed = 0xDEAD_BEEFu64;
        for _ in 0..200 {
            let mut buf = [0u8; 128];
            for b in &mut buf {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                *b = (seed >> 33) as u8;
            }
            assert!(!detect(&buf));
        }
    }
}
