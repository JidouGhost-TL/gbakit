//! Unified `RIFF` / `SCR CODE` script & container family parser.
//!
//! Consolidates:
//! - **`RIFF` containers** (`SCR `, `WAVE`, `XGF `), including the retail header-inclusive
//!   `RIFF` `SCR ` size quirk, parsed directly via `formatkit-layout` `0.1.17`
//!   [`parse_riff_form_with_options`] with [`RiffSizeMode::Auto`].
//! - **Standalone `SCR CODE` scripts** (`CODE` bytecode, optional `JUMP` chunks, and required
//!   `STR ` Shift-JIS string table parsed via [`CountedOffsetArraySpec`]).
//! - **`SCR ` aligned name tables** (4-byte-aligned NUL-terminated printable ASCII names parsed
//!   via [`AlignedStringBankSpec`]).

use formatkit_core::{u32_at, Endian, Error, Result};
use formatkit_layout::{
    pack_aligned_string_bank, pack_chunk_stream, parse_aligned_string_bank,
    parse_counted_offset_array, parse_riff_form_with_options, read_payload_chunk,
    AlignedStringBankSpec, ChunkInput, ChunkSizeMode, ChunkStreamSpec, CountedOffsetArraySpec,
    IffChunk, IntFieldSpec, IntWidth, OffsetBase, OffsetOrderPolicy, PackPolicy, RiffParseOptions,
    RiffSizeMode, StringBankEncoding, StringBankTermination, RIFF_MAGIC,
};

use super::{align_up_4, is_printable_fourcc, DetectedFormatExtent, FORMAT_ID_RIFF_SCR};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_SCR: &[u8; 4] = b"SCR ";
const MAGIC_SCR_CODE: &[u8; 8] = b"SCR CODE";
const TAG_CODE: &[u8; 4] = b"CODE";
const TAG_JUMP: &[u8; 4] = b"JUMP";
const TAG_STR: &[u8; 4] = b"STR ";
const MAX_STR_COUNT: usize = 4096;
const MAX_JUMP_CHUNKS: usize = 64;

/// Variant within the `studio.riff-scr` family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiffScrKind {
    /// `RIFF` container with `SCR ` form (header-inclusive or standard framing).
    RiffScr,
    /// Standard `RIFF` container (`WAVE`, `XGF `, etc.).
    RiffOther,
    /// Standalone `SCR CODE` bytecode + `STR ` script container.
    ScrCode,
    /// `SCR ` 4-byte-aligned printable ASCII name table.
    ScrTable,
}

impl RiffScrKind {
    pub const fn subkind(self) -> &'static str {
        match self {
            Self::RiffScr => "riff_scr",
            Self::RiffOther => "riff",
            Self::ScrCode => "scr_code",
            Self::ScrTable => "scr_table",
        }
    }
}

/// Parsed `STR ` string-table metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrTableInfo {
    pub string_count: usize,
    pub payload_size: usize,
}

/// Parsed `studio.riff-scr` container or script asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiffScrAsset {
    pub kind: RiffScrKind,
    pub form_type: Option<[u8; 4]>,
    /// Outer RIFF size-word mode (`Exclusive` or `Inclusive`) preserved for exact re-serialization.
    pub size_mode: RiffSizeMode,
    pub chunks: Vec<IffChunk>,
    pub chunk_payloads: Vec<Vec<u8>>,
    pub chunk_pad_bytes: Vec<Option<u8>>,
    pub code_len: usize,
    pub jump_chunk_count: usize,
    pub str_table: Option<StrTableInfo>,
    pub table_entry_count: usize,
    pub table_entries: Vec<Vec<u8>>,
    pub consumed_end: usize,
}

impl RiffScrAsset {
    /// Serialize this container, script, or name table back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Serialize a parsed `studio.riff-scr` asset back into its exact binary representation.
pub fn pack(asset: &RiffScrAsset) -> Result<Vec<u8>> {
    match asset.kind {
        RiffScrKind::RiffScr | RiffScrKind::RiffOther => pack_riff(asset),
        RiffScrKind::ScrCode => pack_scr_code(asset),
        RiffScrKind::ScrTable => pack_scr_table(asset),
    }
}

fn pack_riff(asset: &RiffScrAsset) -> Result<Vec<u8>> {
    let Some(form_type) = asset.form_type else {
        return Err(Error::InvalidField {
            what: "RIFF missing form_type",
            value: 0,
        });
    };
    if asset.chunks.len() != asset.chunk_payloads.len() {
        return Err(Error::InvalidField {
            what: "RIFF chunk payload count mismatch",
            value: asset.chunk_payloads.len() as u64,
        });
    }
    let mut body = Vec::new();
    body.extend_from_slice(&form_type);
    for (idx, (ch, payload)) in asset.chunks.iter().zip(asset.chunk_payloads.iter()).enumerate() {
        body.extend_from_slice(&ch.id);
        body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        body.extend_from_slice(payload);
        if (payload.len() & 1) == 1 {
            if let Some(Some(pad)) = asset.chunk_pad_bytes.get(idx) {
                body.push(*pad);
            }
        }
    }
    let outer_size = match asset.size_mode {
        RiffSizeMode::Inclusive => 8usize.checked_add(body.len()).ok_or(Error::InvalidField {
            what: "RIFF inclusive size overflow",
            value: body.len() as u64,
        })?,
        RiffSizeMode::Exclusive | RiffSizeMode::Auto => body.len(),
    };
    let mut out = Vec::with_capacity(8usize.saturating_add(body.len()));
    out.extend_from_slice(&RIFF_MAGIC);
    out.extend_from_slice(&(outer_size as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

fn pack_scr_code(asset: &RiffScrAsset) -> Result<Vec<u8>> {
    if asset.chunks.len() != asset.chunk_payloads.len() || asset.chunks.is_empty() {
        return Err(Error::InvalidField {
            what: "SCR CODE chunk count",
            value: asset.chunks.len() as u64,
        });
    }
    let spec = ChunkStreamSpec::new(
        4,
        8,
        IntFieldSpec::u32_le(4),
        ChunkSizeMode::PayloadOnly,
        MAX_JUMP_CHUNKS + 2,
    )
    .with_tag(0, 4);
    let inputs: Vec<ChunkInput<'_>> = asset
        .chunks
        .iter()
        .zip(asset.chunk_payloads.iter())
        .map(|(ch, payload)| ChunkInput {
            tag: &ch.id,
            payload: payload.as_slice(),
            header_template: None,
            trailer: None,
        })
        .collect();
    pack_chunk_stream(&spec, &PackPolicy::TIGHT, MAGIC_SCR, &inputs).map_err(|_| {
        Error::InvalidField {
            what: "SCR CODE pack_chunk_stream",
            value: asset.chunks.len() as u64,
        }
    })
}

fn pack_scr_table(asset: &RiffScrAsset) -> Result<Vec<u8>> {
    let spec = AlignedStringBankSpec::aligned(4, 4, 1024)
        .with_encoding(StringBankEncoding::PrintableAscii)
        .with_termination(StringBankTermination::StopOnInvalid)
        .with_max_entry_len(24);
    let slices: Vec<&[u8]> = asset.table_entries.iter().map(Vec::as_slice).collect();
    pack_aligned_string_bank(&spec, &PackPolicy::TIGHT, MAGIC_SCR, &slices).map_err(|_| {
        Error::InvalidField {
            what: "SCR table pack_aligned_string_bank",
            value: asset.table_entries.len() as u64,
        }
    })
}

/// Returns `true` if `data` starts with a valid `RIFF`, `SCR CODE`, or `SCR ` name-table asset.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse a `RIFF`, `SCR CODE`, or `SCR ` name-table asset from the start of `data`.
pub fn parse(data: &[u8]) -> Result<RiffScrAsset> {
    if data.starts_with(&RIFF_MAGIC) {
        parse_riff(data)
    } else if data.starts_with(MAGIC_SCR_CODE) {
        parse_scr_code(data)
    } else if data.starts_with(MAGIC_SCR) {
        parse_scr_table(data)
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
            expected: u32::from_be_bytes(RIFF_MAGIC),
            found,
        })
    }
}

fn parse_riff(data: &[u8]) -> Result<RiffScrAsset> {
    if data.len() < 20 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 20,
            available: data.len(),
        });
    }

    let validate_form = |form: formatkit_layout::IffForm| -> Option<formatkit_layout::IffForm> {
        if !is_printable_fourcc(&form.form_type) || form.chunks.is_empty() {
            return None;
        }
        if form.chunks.iter().any(|ch| !is_printable_fourcc(&ch.id)) {
            return None;
        }
        Some(form)
    };

    // 1. Try standard exclusive RIFF framing first, validating printable FourCCs.
    let standard_form = parse_riff_form_with_options(
        data,
        RiffParseOptions::new().with_size_mode(RiffSizeMode::Exclusive),
    )
    .ok()
    .and_then(validate_form);

    // 2. Fall back to header-inclusive framing via FormatKit 0.1.17 `RiffSizeMode::Auto` bounded
    //    to `raw_size` (handles retail header-inclusive `RIFF` `SCR ` inside larger ROMs where
    //    trailing ROM bytes would otherwise look like unprintable chunks under exclusive framing).
    let (form, size_mode) = match standard_form {
        Some(f) => (f, RiffSizeMode::Exclusive),
        None => {
            let raw_size = u32_at(data, 4)? as usize;
            if raw_size < 20 || raw_size > data.len() {
                return Err(Error::InvalidField {
                    what: "RIFF size",
                    value: raw_size as u64,
                });
            }
            let f = parse_riff_form_with_options(
                &data[..raw_size],
                RiffParseOptions::new().with_size_mode(RiffSizeMode::Auto),
            )
            .ok()
            .and_then(validate_form)
            .ok_or(Error::InvalidField {
                what: "RIFF form_type or chunks",
                value: raw_size as u64,
            })?;
            let mode = if raw_size == 8usize.saturating_add(f.body_len as usize) {
                RiffSizeMode::Inclusive
            } else {
                RiffSizeMode::Exclusive
            };
            (f, mode)
        }
    };

    let consumed_end = 8usize
        .checked_add(form.body_len as usize)
        .filter(|&e| e <= data.len())
        .ok_or(Error::InvalidField {
            what: "RIFF consumed_end",
            value: u64::from(form.body_len),
        })?;

    let mut chunk_payloads = Vec::with_capacity(form.chunks.len());
    let mut chunk_pad_bytes = Vec::with_capacity(form.chunks.len());
    for ch in &form.chunks {
        let end = ch
            .payload_offset
            .checked_add(ch.payload_len)
            .filter(|&e| e <= consumed_end)
            .ok_or(Error::InvalidField {
                what: "RIFF chunk bounds",
                value: ch.payload_offset as u64,
            })?;
        chunk_payloads.push(data[ch.payload_offset..end].to_vec());
        if (ch.payload_len & 1) == 1 && end < consumed_end {
            chunk_pad_bytes.push(Some(data[end]));
        } else {
            chunk_pad_bytes.push(None);
        }
    }

    if &form.form_type == MAGIC_SCR {
        // Validate inner SCR CODE / STR structure when present inside RIFF SCR.
        let mut code_len = 0usize;
        let mut jump_chunk_count = 0usize;
        let mut str_table = None;
        for (ch, payload) in form.chunks.iter().zip(chunk_payloads.iter()) {
            if &ch.id == TAG_CODE {
                if payload.len() < 4 {
                    return Err(Error::InvalidField {
                        what: "RIFF SCR CODE payload",
                        value: payload.len() as u64,
                    });
                }
                let inner_len = u32_at(payload, 0)? as usize;
                if inner_len.checked_add(4) != Some(payload.len()) || (inner_len & 3) != 0 {
                    return Err(Error::InvalidField {
                        what: "RIFF SCR CODE inner length",
                        value: inner_len as u64,
                    });
                }
                code_len = inner_len;
            } else if &ch.id == TAG_JUMP {
                jump_chunk_count += 1;
            } else if &ch.id == TAG_STR {
                str_table = Some(validate_str_payload(payload)?);
            }
        }
        return Ok(RiffScrAsset {
            kind: RiffScrKind::RiffScr,
            form_type: Some(form.form_type),
            size_mode,
            chunks: form.chunks,
            chunk_payloads,
            chunk_pad_bytes,
            code_len,
            jump_chunk_count,
            str_table,
            table_entry_count: 0,
            table_entries: Vec::new(),
            consumed_end,
        });
    }

    Ok(RiffScrAsset {
        kind: RiffScrKind::RiffOther,
        form_type: Some(form.form_type),
        size_mode,
        chunks: form.chunks,
        chunk_payloads,
        chunk_pad_bytes,
        code_len: 0,
        jump_chunk_count: 0,
        str_table: None,
        table_entry_count: 0,
        table_entries: Vec::new(),
        consumed_end,
    })
}

fn parse_scr_code(data: &[u8]) -> Result<RiffScrAsset> {
    if data.len() < 24 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 24,
            available: data.len(),
        });
    }
    let section_rest = u32_at(data, 8)? as usize;
    let code_len = u32_at(data, 12)? as usize;
    if code_len.checked_add(4) != Some(section_rest) || (code_len & 3) != 0 {
        return Err(Error::InvalidField {
            what: "SCR CODE length invariant",
            value: code_len as u64,
        });
    }
    let mut pos = 16usize.checked_add(code_len).ok_or(Error::InvalidField {
        what: "SCR CODE body end",
        value: code_len as u64,
    })?;
    if pos > data.len() {
        return Err(Error::Truncated {
            offset: 16,
            needed: code_len,
            available: data.len().saturating_sub(16),
        });
    }

    let mut chunks = vec![IffChunk {
        id: *TAG_CODE,
        payload_offset: 12,
        payload_len: section_rest,
    }];
    let mut chunk_payloads = vec![data[12..pos].to_vec()];
    let mut jump_chunk_count = 0usize;

    // Walk 0+ JUMP chunks, then require exactly one STR chunk.
    while jump_chunk_count < MAX_JUMP_CHUNKS {
        let span =
            read_payload_chunk(data, pos, Endian::Little).map_err(|_| Error::InvalidField {
                what: "SCR CODE trailing chunk",
                value: pos as u64,
            })?;
        if &span.tag == TAG_JUMP {
            chunks.push(IffChunk {
                id: span.tag,
                payload_offset: span.payload.start,
                payload_len: span.payload.len(),
            });
            chunk_payloads.push(data[span.payload.clone()].to_vec());
            jump_chunk_count += 1;
            pos = span.payload.end;
            continue;
        }
        if &span.tag == TAG_STR {
            let str_payload = &data[span.payload.clone()];
            let str_info = validate_str_payload(str_payload)?;
            chunks.push(IffChunk {
                id: span.tag,
                payload_offset: span.payload.start,
                payload_len: span.payload.len(),
            });
            chunk_payloads.push(str_payload.to_vec());
            let chunk_pad_bytes = vec![None; chunks.len()];
            return Ok(RiffScrAsset {
                kind: RiffScrKind::ScrCode,
                form_type: Some(*MAGIC_SCR),
                size_mode: RiffSizeMode::Exclusive,
                chunks,
                chunk_payloads,
                chunk_pad_bytes,
                code_len,
                jump_chunk_count,
                str_table: Some(str_info),
                table_entry_count: 0,
                table_entries: Vec::new(),
                consumed_end: span.payload.end,
            });
        }
        return Err(Error::InvalidField {
            what: "SCR CODE unexpected chunk tag",
            value: u64::from(u32::from_be_bytes(span.tag)),
        });
    }

    Err(Error::InvalidField {
        what: "SCR CODE missing STR chunk",
        value: jump_chunk_count as u64,
    })
}

fn validate_str_payload(payload: &[u8]) -> Result<StrTableInfo> {
    if payload.len() < 4 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 4,
            available: payload.len(),
        });
    }
    let count_u32 = u32_at(payload, 0)?;
    let count = count_u32 as usize;
    if count > MAX_STR_COUNT {
        return Err(Error::InvalidField {
            what: "STR string count",
            value: u64::from(count_u32),
        });
    }
    if count == 0 {
        if payload.len() != 4 {
            return Err(Error::InvalidField {
                what: "STR empty table payload length",
                value: payload.len() as u64,
            });
        }
        return Ok(StrTableInfo {
            string_count: 0,
            payload_size: payload.len(),
        });
    }

    let spec = CountedOffsetArraySpec::new(4, count, IntWidth::U32, Endian::Little, MAX_STR_COUNT)
        .with_offset_base(OffsetBase::TableEnd)
        .with_order_policy(OffsetOrderPolicy::RequireMonotonic);

    let spans = parse_counted_offset_array(payload, &spec).map_err(|_| Error::InvalidField {
        what: "STR offset array",
        value: count as u64,
    })?;

    let table_end = 4usize
        .checked_add(count.checked_mul(4).ok_or(Error::InvalidField {
            what: "STR table size",
            value: count as u64,
        })?)
        .ok_or(Error::InvalidField {
            what: "STR table end",
            value: count as u64,
        })?;
    if spans.first().is_none_or(|s| s.offset as usize != table_end) {
        return Err(Error::InvalidField {
            what: "STR first offset != 0",
            value: spans.first().map_or(0, |s| s.offset),
        });
    }

    for span in &spans {
        let Some(rec) = span.slice(payload) else {
            return Err(Error::InvalidField {
                what: "STR record bounds",
                value: span.offset,
            });
        };
        if rec.is_empty() || *rec.last().unwrap_or(&1) != 0 {
            return Err(Error::InvalidField {
                what: "STR record NUL terminator",
                value: span.offset,
            });
        }
        if rec[..rec.len() - 1].contains(&0) {
            return Err(Error::InvalidField {
                what: "STR record interior NUL",
                value: span.offset,
            });
        }
    }

    Ok(StrTableInfo {
        string_count: count,
        payload_size: payload.len(),
    })
}

fn parse_scr_table(data: &[u8]) -> Result<RiffScrAsset> {
    if data.len() < 20 || !data.starts_with(MAGIC_SCR) || data.starts_with(MAGIC_SCR_CODE) {
        return Err(Error::InvalidField {
            what: "SCR table header",
            value: data.len() as u64,
        });
    }

    let spec = AlignedStringBankSpec::aligned(4, 4, 1024)
        .with_encoding(StringBankEncoding::PrintableAscii)
        .with_termination(StringBankTermination::StopOnInvalid)
        .with_max_entry_len(24);

    let bank = parse_aligned_string_bank(data, &spec).map_err(|_| Error::InvalidField {
        what: "SCR aligned name table",
        value: 0,
    })?;

    if bank.entries.len() < 4 {
        return Err(Error::InvalidField {
            what: "SCR name table count",
            value: bank.entries.len() as u64,
        });
    }

    let table_entries = bank
        .entries
        .iter()
        .map(|e| e.bytes.clone())
        .collect::<Vec<_>>();

    Ok(RiffScrAsset {
        kind: RiffScrKind::ScrTable,
        form_type: Some(*MAGIC_SCR),
        size_mode: RiffSizeMode::Exclusive,
        chunks: Vec::new(),
        chunk_payloads: Vec::new(),
        chunk_pad_bytes: Vec::new(),
        code_len: 0,
        jump_chunk_count: 0,
        str_table: None,
        table_entry_count: table_entries.len(),
        table_entries,
        consumed_end: bank.consumed_end,
    })
}

/// Scan `rom` on 4-byte aligned offsets for verified `studio.riff-scr` containers and scripts.
pub fn scan_riff_scr(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective = &rom[..rom.len().min(MAX_ROM_SIZE)];
    if effective.len() < HEADER_SIZE + 20 {
        return Vec::new();
    }

    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + 8 <= effective.len() {
        let tag = &effective[off..off + 4];
        let parsed = if tag == RIFF_MAGIC {
            parse_riff(&effective[off..])
        } else if effective[off..].starts_with(MAGIC_SCR_CODE) {
            parse_scr_code(&effective[off..])
        } else if tag == MAGIC_SCR {
            parse_scr_table(&effective[off..])
        } else {
            off += 4;
            continue;
        };

        if let Ok(asset) = parsed {
            let size = asset.consumed_end;
            if size > 0 && off.checked_add(size).is_some_and(|e| e <= effective.len()) {
                found.push(DetectedFormatExtent {
                    format_id: FORMAT_ID_RIFF_SCR,
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

    fn make_str_chunk(strings: &[&[u8]]) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        let mut offset = 0u32;
        for s in strings {
            payload.extend_from_slice(&offset.to_le_bytes());
            offset += (s.len() + 1) as u32;
        }
        for s in strings {
            payload.extend_from_slice(s);
            payload.push(0);
        }
        let mut chunk = Vec::new();
        chunk.extend_from_slice(TAG_STR);
        chunk.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        chunk.extend_from_slice(&payload);
        chunk
    }

    pub(crate) fn make_valid_riff_scr_inclusive() -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(MAGIC_SCR);
        // CODE chunk: inner code_len = 16 -> payload = 20
        body.extend_from_slice(TAG_CODE);
        body.extend_from_slice(&20u32.to_le_bytes());
        body.extend_from_slice(&16u32.to_le_bytes());
        body.extend_from_slice(&[0x23u8; 16]);
        // STR chunk with 2 strings
        let str_chunk = make_str_chunk(&[b"hello", b"world"]);
        body.extend_from_slice(&str_chunk);
        if (str_chunk.len() & 1) == 1 {
            body.push(0);
        }

        // Header-inclusive outer size = 8 + body.len()
        let total = 8 + body.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&RIFF_MAGIC);
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn make_valid_scr_code() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC_SCR_CODE);
        out.extend_from_slice(&20u32.to_le_bytes()); // code_len + 4
        out.extend_from_slice(&16u32.to_le_bytes()); // code_len
        out.extend_from_slice(&[0x12u8; 16]);
        // Optional JUMP chunk
        out.extend_from_slice(TAG_JUMP);
        out.extend_from_slice(&8u32.to_le_bytes());
        out.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0]);
        // Required STR chunk
        out.extend_from_slice(&make_str_chunk(&[b"line1", b"line2"]));
        out
    }

    fn make_valid_scr_table() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC_SCR);
        for name in ["SCENE_001", "SCENE_002", "SCENE_003", "SCENE_004"] {
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            while !out.len().is_multiple_of(4) {
                out.push(0);
            }
        }
        out
    }

    #[test]
    fn parses_riff_inclusive_scr_code_and_scr_table() {
        let mut riff = make_valid_riff_scr_inclusive();
        let riff_len = riff.len();
        riff.extend_from_slice(&[0x00, 0x00]); // trailing pad
        assert!(detect(&riff));
        let r = parse(&riff).unwrap();
        assert_eq!(r.kind, RiffScrKind::RiffScr);
        assert_eq!(r.consumed_end, riff_len);
        assert_eq!(r.code_len, 16);
        assert_eq!(r.str_table.as_ref().map(|s| s.string_count), Some(2));

        let scr_code = make_valid_scr_code();
        assert!(detect(&scr_code));
        let sc = parse(&scr_code).unwrap();
        assert_eq!(sc.kind, RiffScrKind::ScrCode);
        assert_eq!(sc.jump_chunk_count, 1);
        assert_eq!(sc.str_table.as_ref().map(|s| s.string_count), Some(2));
        assert_eq!(sc.consumed_end, scr_code.len());

        // Empty STR table (count == 0, len == 4) is also valid
        let mut empty_str = Vec::new();
        empty_str.extend_from_slice(MAGIC_SCR_CODE);
        empty_str.extend_from_slice(&16u32.to_le_bytes());
        empty_str.extend_from_slice(&12u32.to_le_bytes());
        empty_str.extend_from_slice(&[0u8; 12]);
        empty_str.extend_from_slice(TAG_STR);
        empty_str.extend_from_slice(&4u32.to_le_bytes());
        empty_str.extend_from_slice(&0u32.to_le_bytes());
        assert!(detect(&empty_str));
        assert_eq!(parse(&empty_str).unwrap().consumed_end, empty_str.len());

        let tbl = make_valid_scr_table();
        assert!(detect(&tbl));
        let t = parse(&tbl).unwrap();
        assert_eq!(t.kind, RiffScrKind::ScrTable);
        assert_eq!(t.table_entry_count, 4);
        assert_eq!(t.consumed_end, tbl.len());
    }

    #[test]
    fn roundtrips_riff_scr_code_and_scr_table_byte_identically() {
        let riff = make_valid_riff_scr_inclusive();
        let r = parse(&riff).unwrap();
        assert_eq!(r.to_bytes().unwrap(), riff[..r.consumed_end]);

        // Also test standard exclusive RIFF framing (WAVE)
        let mut wav = Vec::new();
        wav.extend_from_slice(&RIFF_MAGIC);
        wav.extend_from_slice(&20u32.to_le_bytes()); // exclusive body len = 4 + 8 + 8
        wav.extend_from_slice(b"WAVE");
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&8u32.to_le_bytes());
        wav.extend_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let w = parse(&wav).unwrap();
        assert_eq!(w.size_mode, RiffSizeMode::Exclusive);
        assert_eq!(w.to_bytes().unwrap(), wav[..w.consumed_end]);

        let scr_code = make_valid_scr_code();
        let sc = parse(&scr_code).unwrap();
        assert_eq!(sc.to_bytes().unwrap(), scr_code[..sc.consumed_end]);

        let tbl = make_valid_scr_table();
        let t = parse(&tbl).unwrap();
        assert_eq!(t.to_bytes().unwrap(), tbl[..t.consumed_end]);
    }

    #[test]
    fn rejects_malformed_truncated_and_random_inputs() {
        assert!(!detect(&[]));
        assert!(!detect(&make_valid_riff_scr_inclusive()[..16]));

        let mut bad_scr = make_valid_scr_code();
        bad_scr[8] = 99; // breaks section_rest == code_len + 4
        assert!(!detect(&bad_scr));

        let mut bad_tbl = Vec::new();
        bad_tbl.extend_from_slice(MAGIC_SCR);
        bad_tbl.extend_from_slice(b"ONLY_ONE\0\0\0\0");
        assert!(!detect(&bad_tbl)); // fewer than 4 entries

        let mut seed = 0x55AA_1234u64;
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
