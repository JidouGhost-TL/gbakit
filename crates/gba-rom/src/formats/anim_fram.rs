//! Unified `ANIM` / `FRAM` sprite animation package parser, writer, and ROM scanner.
//!
//! Consolidates the `ANIM` and `FRAM` animation container variants observed across GBA ROMs:
//! - **`ANIM` Chunked**: `ANIM` + `flags` + `nseq` + (`XY  `, `FRAM`)* + `PAL ` parsed and
//!   repacked via `formatkit-layout` `0.1.17` [`ChunkStreamSpec`] / [`pack_chunk_stream`],
//!   optionally followed by an `END CHAR` 4bpp tile bank.
//! - **`ANIM` Table**: 76-byte `ANIM` header + `n` * 12-byte ROM-pointer-anchored animation records.
//! - **`ANIM` Inline**: `ANIM` + `nframes` + `ndesc` + `ndesc` * 16-byte descriptors + script body.
//! - **`ANIM` Names**: 4-byte-aligned NUL-terminated `ANIM_*` ASCII name bank via
//!   [`AlignedStringBankSpec`] / [`pack_aligned_string_bank`].
//! - **`FRAM` Table**: `FRAM` + `f1` (`4 + 8 * count`) + `count` + `flags` + `count` * 8-byte records.
//! - **`FRAM` Directory**: Descending `FRAME N `..`FRAME 1 ` 12-byte directory records.
//! - **`FRAM` Pool**: `FRAMEPOOL\0\0\0` NUL-separated string pool (optionally split at
//!   `SPRBMPOOL` with a `PALETTE` trailer).

use formatkit_core::{bytes_at, u16_at, u32_at, Error, Result};
use formatkit_layout::{
    pack_aligned_string_bank, pack_chunk_stream, parse_aligned_string_bank,
    parse_chunk_stream_with_extent, AlignedStringBankSpec, ChunkInput, ChunkSizeMode,
    ChunkStreamSpec, IntFieldSpec, PackPolicy, StringBankEncoding, StringBankTermination,
};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_ANIM_FRAM};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_ANIM: &[u8; 4] = b"ANIM";
const MAGIC_FRAM: &[u8; 4] = b"FRAM";
const MAGIC_FRAMEPOOL: &[u8; 9] = b"FRAMEPOOL";
const MAGIC_SPRBMPOOL: &[u8; 9] = b"SPRBMPOOL";
const MAGIC_PALETTE: &[u8; 7] = b"PALETTE";
const TAG_XY: &[u8; 4] = b"XY  ";
const TAG_PAL: &[u8; 4] = b"PAL ";
const TAG_END_CHAR: &[u8; 8] = b"END CHAR";

const ROM_PTR_MIN: u32 = 0x0800_0000;
const ROM_PTR_MAX: u32 = 0x09FF_FFFF;
const TABLE_HEADER_LEN: usize = 76;
const TABLE_RECORD_LEN: usize = 12;
const INLINE_DESC_LEN: usize = 16;
const MAX_CHUNK_BYTES: u32 = 0x10_0000;
const MAX_CHAR_BYTES: u32 = 0x100_0000;
const FRAM_TABLE_HEADER_LEN: usize = 16;
const FRAM_TABLE_RECORD_LEN: usize = 8;
const FRAM_TABLE_COUNT_MAX: usize = 4096;
const FRAME_DIR_RECORD_LEN: usize = 12;
const FRAMES_MAX: u32 = 64;
const POOL_HEADER_LEN: usize = 12;

const ANIM_CHUNK_ALLOWED_TAGS: &[&[u8]] = &[b"XY  ", b"FRAM", b"PAL "];

/// Entry-point variant within `studio.anim-fram`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimFramKind {
    AnimChunked,
    AnimTable,
    AnimInline,
    AnimNames,
    FramTable,
    FramDir,
    FramPool,
}

impl AnimFramKind {
    pub const fn subkind(self) -> &'static str {
        match self {
            Self::AnimChunked => "anim_chunked",
            Self::AnimTable => "anim_table",
            Self::AnimInline => "anim_inline",
            Self::AnimNames => "anim_names",
            Self::FramTable => "fram_table",
            Self::FramDir => "fram_dir",
            Self::FramPool => "fram_pool",
        }
    }
}

/// One chunk inside an `ANIM` chunked animation container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimChunk {
    pub tag: [u8; 4],
    pub payload: Vec<u8>,
}

/// One 12-byte record inside an `ANIM` table animation container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimTableRecord {
    pub width: u16,
    pub height: u16,
    pub frame: u16,
    pub cmd: u16,
    pub rom_ptr: u32,
}

/// One 16-byte descriptor inside an `ANIM` inline animation container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimInlineDesc {
    pub width: u16,
    pub height: u16,
    pub words: [u32; 3],
}

/// Parsed `studio.anim-fram` asset with exact binary round-trip state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnimFramAsset {
    AnimChunked {
        flags: u32,
        seq_count: u32,
        chunks: Vec<AnimChunk>,
        char_bank: Option<Vec<u8>>,
        consumed_end: usize,
    },
    AnimTable {
        param: u16,
        records: Vec<AnimTableRecord>,
        script_anchor: u32,
        consumed_end: usize,
    },
    AnimInline {
        frame_count: u32,
        descriptors: Vec<AnimInlineDesc>,
        body: Vec<u8>,
        consumed_end: usize,
    },
    AnimNames {
        names: Vec<Vec<u8>>,
        consumed_end: usize,
    },
    FramTable {
        flags: u32,
        records: Vec<(u32, u32)>,
        consumed_end: usize,
    },
    FramDir {
        /// Exact 8-byte frame labels (`FRAME N ` or `FRAME N\0`) in descending order.
        labels: Vec<[u8; 8]>,
        consumed_end: usize,
    },
    FramPool {
        body: Vec<u8>,
        sprbm_section_offset: Option<usize>,
        consumed_end: usize,
    },
}

impl AnimFramAsset {
    pub const fn kind(&self) -> AnimFramKind {
        match self {
            Self::AnimChunked { .. } => AnimFramKind::AnimChunked,
            Self::AnimTable { .. } => AnimFramKind::AnimTable,
            Self::AnimInline { .. } => AnimFramKind::AnimInline,
            Self::AnimNames { .. } => AnimFramKind::AnimNames,
            Self::FramTable { .. } => AnimFramKind::FramTable,
            Self::FramDir { .. } => AnimFramKind::FramDir,
            Self::FramPool { .. } => AnimFramKind::FramPool,
        }
    }

    pub const fn consumed_end(&self) -> usize {
        match self {
            Self::AnimChunked { consumed_end, .. }
            | Self::AnimTable { consumed_end, .. }
            | Self::AnimInline { consumed_end, .. }
            | Self::AnimNames { consumed_end, .. }
            | Self::FramTable { consumed_end, .. }
            | Self::FramDir { consumed_end, .. }
            | Self::FramPool { consumed_end, .. } => *consumed_end,
        }
    }

    /// Serialize this asset back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Fast, panic-free structural detector for `studio.anim-fram` assets.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse an `ANIM` or `FRAM` asset from the start of `data`.
pub fn parse(data: &[u8]) -> Result<AnimFramAsset> {
    parse_inner(data, false)
}

fn parse_inner(data: &[u8], rom_scan_mode: bool) -> Result<AnimFramAsset> {
    let Some(magic) = data.get(..4) else {
        return Err(Error::Truncated {
            offset: 0,
            needed: 4,
            available: data.len(),
        });
    };
    if magic == MAGIC_ANIM {
        if let Ok(a) = parse_anim_chunked(data) {
            return Ok(a);
        }
        if let Ok(a) = parse_anim_table(data) {
            return Ok(a);
        }
        if let Ok(a) = parse_anim_inline(data, rom_scan_mode) {
            return Ok(a);
        }
        if let Ok(a) = parse_anim_names(data) {
            return Ok(a);
        }
        return Err(Error::InvalidField {
            what: "ANIM variant structure",
            value: 0,
        });
    }
    if magic == MAGIC_FRAM {
        if let Ok(a) = parse_fram_table(data) {
            return Ok(a);
        }
        if let Ok(a) = parse_fram_dir(data) {
            return Ok(a);
        }
        if !rom_scan_mode {
            if let Ok(a) = parse_fram_pool(data) {
                return Ok(a);
            }
        } else if let Ok(a) = parse_fram_pool_bounded(data) {
            return Ok(a);
        }
        return Err(Error::InvalidField {
            what: "FRAM variant structure",
            value: 0,
        });
    }
    let found = u32::from_be_bytes([magic[0], magic[1], magic[2], magic[3]]);
    Err(Error::BadMagic {
        offset: 0,
        expected: u32::from_be_bytes(*MAGIC_ANIM),
        found,
    })
}

/// Serialize a parsed `studio.anim-fram` asset back into its exact binary representation.
pub fn pack(asset: &AnimFramAsset) -> Result<Vec<u8>> {
    match asset {
        AnimFramAsset::AnimChunked {
            flags,
            seq_count,
            chunks,
            char_bank,
            ..
        } => {
            let mut header = [0u8; 12];
            header[..4].copy_from_slice(MAGIC_ANIM);
            header[4..8].copy_from_slice(&flags.to_le_bytes());
            header[8..12].copy_from_slice(&seq_count.to_le_bytes());

            let spec = ChunkStreamSpec::new(
                12,
                8,
                IntFieldSpec::u32_le(4),
                ChunkSizeMode::PayloadOnly,
                1024,
            )
            .with_tag(0, 4)
            .with_allowed_tags(ANIM_CHUNK_ALLOWED_TAGS)
            .with_stop_on_unknown_tag(true)
            .with_require_eof_closure(false);

            let inputs: Vec<ChunkInput<'_>> = chunks
                .iter()
                .map(|c| ChunkInput {
                    tag: &c.tag,
                    payload: &c.payload,
                    header_template: None,
                    trailer: None,
                })
                .collect();

            let mut out = pack_chunk_stream(&spec, &PackPolicy::TIGHT, &header, &inputs).map_err(
                |_| Error::InvalidField {
                    what: "ANIM chunked pack_chunk_stream",
                    value: chunks.len() as u64,
                },
            )?;

            if let Some(bank) = char_bank {
                out.extend_from_slice(TAG_END_CHAR);
                out.extend_from_slice(&(bank.len() as u32).to_le_bytes());
                out.extend_from_slice(bank);
            }
            Ok(out)
        }
        AnimFramAsset::AnimTable {
            param,
            records,
            script_anchor,
            ..
        } => {
            if records.is_empty() || records.len() > 4096 {
                return Err(Error::InvalidField {
                    what: "ANIM table record count",
                    value: records.len() as u64,
                });
            }
            let total = TABLE_HEADER_LEN
                .checked_add(records.len().checked_mul(TABLE_RECORD_LEN).ok_or(
                    Error::InvalidField {
                        what: "ANIM table size",
                        value: records.len() as u64,
                    },
                )?)
                .and_then(|v| v.checked_add(4))
                .ok_or(Error::InvalidField {
                    what: "ANIM table total",
                    value: records.len() as u64,
                })?;
            let mut out = Vec::with_capacity(total);
            out.extend_from_slice(MAGIC_ANIM);
            out.extend_from_slice(&(records.len() as u16).to_le_bytes());
            out.extend_from_slice(&param.to_le_bytes());
            out.resize(TABLE_HEADER_LEN, 0);
            for rec in records {
                out.extend_from_slice(&rec.width.to_le_bytes());
                out.extend_from_slice(&rec.height.to_le_bytes());
                out.extend_from_slice(&rec.frame.to_le_bytes());
                out.extend_from_slice(&rec.cmd.to_le_bytes());
                out.extend_from_slice(&rec.rom_ptr.to_le_bytes());
            }
            out.extend_from_slice(&script_anchor.to_le_bytes());
            Ok(out)
        }
        AnimFramAsset::AnimInline {
            frame_count,
            descriptors,
            body,
            ..
        } => {
            if *frame_count == 0 || descriptors.is_empty() || descriptors.len() > 2048 {
                return Err(Error::InvalidField {
                    what: "ANIM inline header",
                    value: descriptors.len() as u64,
                });
            }
            let mut out = Vec::with_capacity(
                12usize
                    .saturating_add(descriptors.len().saturating_mul(INLINE_DESC_LEN))
                    .saturating_add(body.len()),
            );
            out.extend_from_slice(MAGIC_ANIM);
            out.extend_from_slice(&frame_count.to_le_bytes());
            out.extend_from_slice(&(descriptors.len() as u32).to_le_bytes());
            for d in descriptors {
                out.extend_from_slice(&d.width.to_le_bytes());
                out.extend_from_slice(&d.height.to_le_bytes());
                for w in &d.words {
                    out.extend_from_slice(&w.to_le_bytes());
                }
            }
            out.extend_from_slice(body);
            Ok(out)
        }
        AnimFramAsset::AnimNames { names, .. } => {
            let spec = AlignedStringBankSpec::aligned(0, 4, 10000)
                .with_encoding(StringBankEncoding::PrintableAscii)
                .with_termination(StringBankTermination::StopOnInvalid)
                .with_max_entry_len(48);
            let slices: Vec<&[u8]> = names.iter().map(Vec::as_slice).collect();
            pack_aligned_string_bank(&spec, &PackPolicy::TIGHT, &[], &slices).map_err(|_| {
                Error::InvalidField {
                    what: "ANIM names pack_aligned_string_bank",
                    value: names.len() as u64,
                }
            })
        }
        AnimFramAsset::FramTable { flags, records, .. } => {
            let count = records.len();
            if count == 0 || count > FRAM_TABLE_COUNT_MAX {
                return Err(Error::InvalidField {
                    what: "FRAM table count",
                    value: count as u64,
                });
            }
            let f1 = 4u32
                .checked_add((count as u32).checked_mul(8).ok_or(Error::InvalidField {
                    what: "FRAM f1 overflow",
                    value: count as u64,
                })?)
                .ok_or(Error::InvalidField {
                    what: "FRAM f1 overflow",
                    value: count as u64,
                })?;
            let mut out = Vec::with_capacity(
                FRAM_TABLE_HEADER_LEN.saturating_add(count.saturating_mul(FRAM_TABLE_RECORD_LEN)),
            );
            out.extend_from_slice(MAGIC_FRAM);
            out.extend_from_slice(&f1.to_le_bytes());
            out.extend_from_slice(&(count as u32).to_le_bytes());
            out.extend_from_slice(&flags.to_le_bytes());
            for &(a, b) in records {
                out.extend_from_slice(&a.to_le_bytes());
                out.extend_from_slice(&b.to_le_bytes());
            }
            Ok(out)
        }
        AnimFramAsset::FramDir { labels, .. } => {
            if labels.is_empty() || labels.len() > FRAMES_MAX as usize {
                return Err(Error::InvalidField {
                    what: "FRAM dir count",
                    value: labels.len() as u64,
                });
            }
            let mut out = Vec::with_capacity(labels.len().saturating_mul(FRAME_DIR_RECORD_LEN));
            for label in labels {
                out.extend_from_slice(label);
                out.extend_from_slice(&0u32.to_le_bytes());
            }
            Ok(out)
        }
        AnimFramAsset::FramPool { body, .. } => {
            let mut out = Vec::with_capacity(POOL_HEADER_LEN.saturating_add(body.len()));
            out.extend_from_slice(MAGIC_FRAMEPOOL);
            out.extend_from_slice(&[0u8; 3]);
            out.extend_from_slice(body);
            Ok(out)
        }
    }
}

fn in_rom_range(ptr: u32) -> bool {
    (ROM_PTR_MIN..=ROM_PTR_MAX).contains(&ptr)
}

fn parse_anim_chunked(data: &[u8]) -> Result<AnimFramAsset> {
    if data.len() < 12 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 12,
            available: data.len(),
        });
    }
    let flags = u32_at(data, 4)?;
    let nseq = u32_at(data, 8)?;
    if nseq == 0 || nseq > 256 {
        return Err(Error::InvalidField {
            what: "ANIM chunked seq count",
            value: u64::from(nseq),
        });
    }
    let expected_chunks = (nseq as usize)
        .checked_mul(2)
        .and_then(|v| v.checked_add(1))
        .ok_or(Error::InvalidField {
            what: "ANIM chunk count",
            value: u64::from(nseq),
        })?;

    let spec = ChunkStreamSpec::new(
        12,
        8,
        IntFieldSpec::u32_le(4),
        ChunkSizeMode::PayloadOnly,
        expected_chunks,
    )
    .with_tag(0, 4)
    .with_allowed_tags(ANIM_CHUNK_ALLOWED_TAGS)
    .with_stop_on_unknown_tag(true)
    .with_require_eof_closure(false);

    let stream = parse_chunk_stream_with_extent(data, &spec).map_err(|_| Error::InvalidField {
        what: "ANIM chunk stream",
        value: 0,
    })?;

    if stream.chunks.len() != expected_chunks {
        return Err(Error::InvalidField {
            what: "ANIM chunked sequence length",
            value: stream.chunks.len() as u64,
        });
    }

    let mut chunks = Vec::with_capacity(expected_chunks);
    for seq in 0..(nseq as usize) {
        let xy = &stream.chunks[seq * 2];
        let fram = &stream.chunks[seq * 2 + 1];
        if xy.tag.as_slice() != TAG_XY || xy.payload_size != 8 {
            return Err(Error::InvalidField {
                what: "ANIM XY chunk",
                value: xy.payload_size as u64,
            });
        }
        let xy_payload = xy.payload(data).ok_or(Error::InvalidField {
            what: "ANIM XY payload bounds",
            value: xy.payload_offset as u64,
        })?;
        let w = u32_at(xy_payload, 0)?;
        let h = u32_at(xy_payload, 4)?;
        if w == 0 || w > 256 || h == 0 || h > 256 {
            return Err(Error::InvalidField {
                what: "ANIM XY dims",
                value: (u64::from(w) << 16) | u64::from(h),
            });
        }
        if fram.tag.as_slice() != MAGIC_FRAM
            || !fram.payload_size.is_multiple_of(4)
            || fram.payload_size > MAX_CHUNK_BYTES as usize
        {
            return Err(Error::InvalidField {
                what: "ANIM FRAM chunk",
                value: fram.payload_size as u64,
            });
        }
        let fram_payload = fram.payload(data).ok_or(Error::InvalidField {
            what: "ANIM FRAM payload bounds",
            value: fram.payload_offset as u64,
        })?;
        chunks.push(AnimChunk {
            tag: *TAG_XY,
            payload: xy_payload.to_vec(),
        });
        chunks.push(AnimChunk {
            tag: *MAGIC_FRAM,
            payload: fram_payload.to_vec(),
        });
    }

    let pal = &stream.chunks[expected_chunks - 1];
    if pal.tag.as_slice() != TAG_PAL
        || pal.payload_size < 4
        || !(pal.payload_size - 4).is_multiple_of(2)
        || pal.payload_size > MAX_CHUNK_BYTES as usize
    {
        return Err(Error::InvalidField {
            what: "ANIM PAL chunk",
            value: pal.payload_size as u64,
        });
    }
    let pal_payload = pal.payload(data).ok_or(Error::InvalidField {
        what: "ANIM PAL payload bounds",
        value: pal.payload_offset as u64,
    })?;
    chunks.push(AnimChunk {
        tag: *TAG_PAL,
        payload: pal_payload.to_vec(),
    });

    let mut consumed_end = stream.consumed_end;
    let mut char_bank = None;
    if let Ok(tag) = bytes_at(data, consumed_end, 8) {
        if tag == TAG_END_CHAR {
            if let Ok(csize_raw) = u32_at(data, consumed_end.saturating_add(8)) {
                if csize_raw <= MAX_CHAR_BYTES {
                    let csize = csize_raw as usize;
                    if let Some(end) = consumed_end
                        .checked_add(12)
                        .and_then(|v| v.checked_add(csize))
                    {
                        if end <= data.len() {
                            char_bank = Some(data[consumed_end + 12..end].to_vec());
                            consumed_end = end;
                        }
                    }
                }
            }
        }
    }

    Ok(AnimFramAsset::AnimChunked {
        flags,
        seq_count: nseq,
        chunks,
        char_bank,
        consumed_end,
    })
}

fn parse_anim_table(data: &[u8]) -> Result<AnimFramAsset> {
    if data.len() < TABLE_HEADER_LEN {
        return Err(Error::Truncated {
            offset: 0,
            needed: TABLE_HEADER_LEN,
            available: data.len(),
        });
    }
    let n_raw = u16_at(data, 4)?;
    let param = u16_at(data, 6)?;
    if n_raw == 0 || n_raw > 4096 || param > 256 {
        return Err(Error::InvalidField {
            what: "ANIM table header",
            value: u64::from(n_raw),
        });
    }
    if bytes_at(data, 8, TABLE_HEADER_LEN - 8)?
        .iter()
        .any(|&b| b != 0)
    {
        return Err(Error::InvalidField {
            what: "ANIM table reserved bytes",
            value: 1,
        });
    }
    let n = n_raw as usize;
    let table_end = TABLE_HEADER_LEN
        .checked_add(n.checked_mul(TABLE_RECORD_LEN).ok_or(Error::InvalidField {
            what: "ANIM table size",
            value: n as u64,
        })?)
        .ok_or(Error::InvalidField {
            what: "ANIM table end",
            value: n as u64,
        })?;
    let consumed_end = table_end.checked_add(4).ok_or(Error::InvalidField {
        what: "ANIM table anchor end",
        value: table_end as u64,
    })?;
    if consumed_end > data.len() {
        return Err(Error::Truncated {
            offset: 0,
            needed: consumed_end,
            available: data.len(),
        });
    }

    let mut records = Vec::with_capacity(n);
    let mut prev_ptr = 0u32;
    for i in 0..n {
        let base = TABLE_HEADER_LEN + i * TABLE_RECORD_LEN;
        let width = u16_at(data, base)?;
        let height = u16_at(data, base + 2)?;
        let frame = u16_at(data, base + 4)?;
        let cmd = u16_at(data, base + 6)?;
        let rom_ptr = u32_at(data, base + 8)?;
        if !(1..=512).contains(&width)
            || !(1..=512).contains(&height)
            || !(1..=255).contains(&frame)
            || (cmd != 0x42 && cmd != 0xCC)
            || !in_rom_range(rom_ptr)
            || (i > 0 && rom_ptr <= prev_ptr)
        {
            return Err(Error::InvalidField {
                what: "ANIM table record",
                value: u64::from(rom_ptr),
            });
        }
        prev_ptr = rom_ptr;
        records.push(AnimTableRecord {
            width,
            height,
            frame,
            cmd,
            rom_ptr,
        });
    }
    let script_anchor = u32_at(data, table_end)?;
    if !in_rom_range(script_anchor) {
        return Err(Error::InvalidField {
            what: "ANIM table anchor",
            value: u64::from(script_anchor),
        });
    }
    Ok(AnimFramAsset::AnimTable {
        param,
        records,
        script_anchor,
        consumed_end,
    })
}

fn parse_anim_inline(data: &[u8], rom_scan_mode: bool) -> Result<AnimFramAsset> {
    if data.len() < 12 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 12,
            available: data.len(),
        });
    }
    let frame_count = u32_at(data, 4)?;
    let ndesc_raw = u32_at(data, 8)?;
    if frame_count == 0 || frame_count > 65535 || ndesc_raw == 0 || ndesc_raw > 2048 {
        return Err(Error::InvalidField {
            what: "ANIM inline counts",
            value: u64::from(ndesc_raw),
        });
    }
    let ndesc = ndesc_raw as usize;
    let desc_end = 12usize
        .checked_add(ndesc.checked_mul(INLINE_DESC_LEN).ok_or(Error::InvalidField {
            what: "ANIM inline desc size",
            value: ndesc as u64,
        })?)
        .ok_or(Error::InvalidField {
            what: "ANIM inline desc end",
            value: ndesc as u64,
        })?;
    if desc_end.checked_add(4).is_none_or(|e| e > data.len()) {
        return Err(Error::Truncated {
            offset: 0,
            needed: desc_end.saturating_add(4),
            available: data.len(),
        });
    }

    let mut descriptors = Vec::with_capacity(ndesc);
    for i in 0..ndesc {
        let base = 12 + i * INLINE_DESC_LEN;
        let width = u16_at(data, base)?;
        let height = u16_at(data, base + 2)?;
        if !(1..=512).contains(&width) || !(1..=512).contains(&height) {
            return Err(Error::InvalidField {
                what: "ANIM inline desc dims",
                value: (u64::from(width) << 16) | u64::from(height),
            });
        }
        let w1 = u32_at(data, base + 4)?;
        let w2 = u32_at(data, base + 8)?;
        let w3 = u32_at(data, base + 12)?;
        if w1 > 0x00FF_FFFF || w2 > 0x00FF_FFFF || w3 > 0x00FF_FFFF {
            return Err(Error::InvalidField {
                what: "ANIM inline desc word",
                value: u64::from(w1),
            });
        }
        descriptors.push(AnimInlineDesc {
            width,
            height,
            words: [w1, w2, w3],
        });
    }

    let body_end = if rom_scan_mode {
        // In ROM scan mode require a ROM-pointer anchor within 256 bytes and bound extent cleanly.
        let scan_end = data.len().min(desc_end.saturating_add(256));
        let mut anchored_end = None;
        let mut off = desc_end;
        while off + 4 <= scan_end {
            if let Ok(ptr) = u32_at(data, off) {
                if in_rom_range(ptr) {
                    anchored_end = Some(off + 4);
                    break;
                }
            }
            off += 4;
        }
        let Some(end) = anchored_end else {
            return Err(Error::InvalidField {
                what: "ANIM inline ROM anchor",
                value: 0,
            });
        };
        end
    } else {
        data.len()
    };

    Ok(AnimFramAsset::AnimInline {
        frame_count,
        descriptors,
        body: data[desc_end..body_end].to_vec(),
        consumed_end: body_end,
    })
}

fn parse_anim_names(data: &[u8]) -> Result<AnimFramAsset> {
    if data.len() < 16 || !data.starts_with(b"ANIM_") {
        return Err(Error::InvalidField {
            what: "ANIM names prefix",
            value: 0,
        });
    }
    let spec = AlignedStringBankSpec::aligned(0, 4, 10000)
        .with_encoding(StringBankEncoding::PrintableAscii)
        .with_termination(StringBankTermination::StopOnInvalid)
        .with_max_entry_len(48);

    let bank = parse_aligned_string_bank(data, &spec).map_err(|_| Error::InvalidField {
        what: "ANIM names bank",
        value: 0,
    })?;

    let mut names = Vec::new();
    let mut consumed_end = 0usize;
    for entry in &bank.entries {
        let b = entry.bytes.as_slice();
        let valid_prefix = b.len() >= 6
            && b.starts_with(b"ANIM_")
            && b[5..]
                .iter()
                .all(|&c| c == b'_' || c.is_ascii_digit() || c.is_ascii_uppercase());
        if !valid_prefix {
            break;
        }
        names.push(entry.bytes.clone());
        consumed_end = align_up_4(entry.offset.saturating_add(b.len()).saturating_add(1));
    }
    if names.len() < 2 || consumed_end > data.len() {
        return Err(Error::InvalidField {
            what: "ANIM names count",
            value: names.len() as u64,
        });
    }
    Ok(AnimFramAsset::AnimNames {
        names,
        consumed_end,
    })
}

fn parse_fram_table(data: &[u8]) -> Result<AnimFramAsset> {
    if data.len() < FRAM_TABLE_HEADER_LEN {
        return Err(Error::Truncated {
            offset: 0,
            needed: FRAM_TABLE_HEADER_LEN,
            available: data.len(),
        });
    }
    let f1 = u32_at(data, 4)?;
    let count_u32 = u32_at(data, 8)?;
    let flags = u32_at(data, 12)?;
    let count = count_u32 as usize;
    if count == 0 || count > FRAM_TABLE_COUNT_MAX {
        return Err(Error::InvalidField {
            what: "FRAM table count",
            value: u64::from(count_u32),
        });
    }
    let expected_f1 = 4u64 + (FRAM_TABLE_RECORD_LEN as u64) * (count as u64);
    if u64::from(f1) != expected_f1 {
        return Err(Error::InvalidField {
            what: "FRAM table f1 invariant",
            value: u64::from(f1),
        });
    }
    let consumed_end = FRAM_TABLE_HEADER_LEN
        .checked_add(count.checked_mul(FRAM_TABLE_RECORD_LEN).ok_or(
            Error::InvalidField {
                what: "FRAM table size",
                value: count as u64,
            },
        )?)
        .ok_or(Error::InvalidField {
            what: "FRAM table end",
            value: count as u64,
        })?;
    if consumed_end > data.len() {
        return Err(Error::Truncated {
            offset: 0,
            needed: consumed_end,
            available: data.len(),
        });
    }
    let mut records = Vec::with_capacity(count);
    for i in 0..count {
        let off = FRAM_TABLE_HEADER_LEN + i * FRAM_TABLE_RECORD_LEN;
        records.push((u32_at(data, off)?, u32_at(data, off + 4)?));
    }
    Ok(AnimFramAsset::FramTable {
        flags,
        records,
        consumed_end,
    })
}

fn parse_frame_number(name: &[u8]) -> Option<u32> {
    if name.len() < 8 || &name[..6] != b"FRAME " {
        return None;
    }
    let d6 = name[6];
    let d7 = name[7];
    if d6.is_ascii_digit() && d7.is_ascii_digit() {
        let v = u32::from(d6 - b'0') * 10 + u32::from(d7 - b'0');
        return (10..=FRAMES_MAX).contains(&v).then_some(v);
    }
    if (b'1'..=b'9').contains(&d6) && (d7 == b' ' || d7 == 0) {
        return Some(u32::from(d6 - b'0'));
    }
    None
}

fn parse_fram_dir(data: &[u8]) -> Result<AnimFramAsset> {
    let mut offset = 0usize;
    let mut first: Option<u32> = None;
    let mut labels = Vec::new();
    while let Ok(rec) = bytes_at(data, offset, FRAME_DIR_RECORD_LEN) {
        let Some(num) = parse_frame_number(&rec[..8]) else {
            break;
        };
        if rec[8..12] != [0, 0, 0, 0] {
            break;
        }
        match first {
            None => {
                if num == 0 || num > FRAMES_MAX {
                    break;
                }
                first = Some(num);
            }
            Some(expected_first) => {
                let want = expected_first.checked_sub(labels.len() as u32);
                if want != Some(num) {
                    break;
                }
            }
        }
        let mut label = [0u8; 8];
        label.copy_from_slice(&rec[..8]);
        labels.push(label);
        offset = offset.saturating_add(FRAME_DIR_RECORD_LEN);
        if labels.len() > FRAMES_MAX as usize {
            break;
        }
    }
    match first {
        Some(n) if labels.len() == n as usize && !labels.is_empty() => Ok(AnimFramAsset::FramDir {
            labels,
            consumed_end: offset,
        }),
        _ => Err(Error::InvalidField {
            what: "FRAME directory sequence",
            value: labels.len() as u64,
        }),
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_fram_pool(data: &[u8]) -> Result<AnimFramAsset> {
    if data.len() < POOL_HEADER_LEN + 1
        || !data.starts_with(MAGIC_FRAMEPOOL)
        || data[9..12] != [0, 0, 0]
    {
        return Err(Error::InvalidField {
            what: "FRAMEPOOL header",
            value: 0,
        });
    }
    let body = &data[POOL_HEADER_LEN..];
    if !body.contains(&0) {
        return Err(Error::InvalidField {
            what: "FRAMEPOOL NUL separator",
            value: 0,
        });
    }
    let sprbm_section_offset = if let Some(sec) = find_subslice(body, MAGIC_SPRBMPOOL) {
        if sec == 0 || !body[..sec].contains(&0) {
            return Err(Error::InvalidField {
                what: "FRAMEPOOL SPRBMPOOL split",
                value: sec as u64,
            });
        }
        let sec_off = POOL_HEADER_LEN + sec;
        if find_subslice(&data[sec_off..], MAGIC_PALETTE).is_none() {
            return Err(Error::InvalidField {
                what: "FRAMEPOOL missing PALETTE trailer",
                value: sec_off as u64,
            });
        }
        Some(sec_off)
    } else {
        None
    };
    Ok(AnimFramAsset::FramPool {
        body: body.to_vec(),
        sprbm_section_offset,
        consumed_end: data.len(),
    })
}

fn parse_fram_pool_bounded(data: &[u8]) -> Result<AnimFramAsset> {
    if data.len() < POOL_HEADER_LEN + 16
        || !data.starts_with(MAGIC_FRAMEPOOL)
        || data[9..12] != [0, 0, 0]
    {
        return Err(Error::InvalidField {
            what: "FRAMEPOOL header",
            value: 0,
        });
    }
    let spec = AlignedStringBankSpec::aligned(0, 4, 2048)
        .with_encoding(StringBankEncoding::PrintableAscii)
        .with_termination(StringBankTermination::StopOnInvalid)
        .with_max_entry_len(256);
    let bank = parse_aligned_string_bank(data, &spec).map_err(|_| Error::InvalidField {
        what: "FRAMEPOOL aligned string bank",
        value: 0,
    })?;
    if bank.entries.len() < 3 {
        return Err(Error::InvalidField {
            what: "FRAMEPOOL entry count",
            value: bank.entries.len() as u64,
        });
    }
    let slice = &data[..bank.consumed_end];
    parse_fram_pool(slice)
}

/// Scan `rom` on 4-byte aligned offsets for verified `studio.anim-fram` assets.
pub fn scan_anim_fram(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective = &rom[..rom.len().min(MAX_ROM_SIZE)];
    if effective.len() < HEADER_SIZE + 12 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + 12 <= effective.len() {
        let tag = &effective[off..off + 4];
        if tag == MAGIC_ANIM || tag == MAGIC_FRAM {
            if let Ok(asset) = parse_inner(&effective[off..], true) {
                let size = asset.consumed_end();
                if size > 0 && off.checked_add(size).is_some_and(|e| e <= effective.len()) {
                    found.push(DetectedFormatExtent {
                        format_id: FORMAT_ID_ANIM_FRAM,
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

    pub(crate) fn make_valid_anim_chunked() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC_ANIM);
        buf.extend_from_slice(&0xC0u32.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(TAG_XY);
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&64u32.to_le_bytes());
        buf.extend_from_slice(&64u32.to_le_bytes());
        buf.extend_from_slice(MAGIC_FRAM);
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&2u32.to_le_bytes());
        buf.extend_from_slice(TAG_PAL);
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&0x001Fu16.to_le_bytes());
        buf.extend_from_slice(&0x03E0u16.to_le_bytes());
        buf.extend_from_slice(TAG_END_CHAR);
        buf.extend_from_slice(&32u32.to_le_bytes());
        buf.extend_from_slice(&[0x7Fu8; 32]);
        buf
    }

    fn make_valid_anim_table() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC_ANIM);
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.resize(TABLE_HEADER_LEN, 0);
        for (frame, ptr) in [(1u16, 0x0800_0100u32), (2u16, 0x0800_0200u32)] {
            buf.extend_from_slice(&64u16.to_le_bytes());
            buf.extend_from_slice(&64u16.to_le_bytes());
            buf.extend_from_slice(&frame.to_le_bytes());
            buf.extend_from_slice(&0x42u16.to_le_bytes());
            buf.extend_from_slice(&ptr.to_le_bytes());
        }
        buf.extend_from_slice(&0x0800_0300u32.to_le_bytes());
        buf
    }

    fn make_valid_anim_names() -> Vec<u8> {
        let mut buf = Vec::new();
        for name in ["ANIM_IDLE", "ANIM_WALK", "ANIM_JUMP"] {
            buf.extend_from_slice(name.as_bytes());
            buf.push(0);
            while !buf.len().is_multiple_of(4) {
                buf.push(0);
            }
        }
        buf
    }

    fn make_valid_fram_table() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC_FRAM);
        buf.extend_from_slice(&20u32.to_le_bytes()); // 4 + 8 * 2
        buf.extend_from_slice(&2u32.to_le_bytes());
        buf.extend_from_slice(&6u32.to_le_bytes());
        buf.extend_from_slice(&32u32.to_le_bytes());
        buf.extend_from_slice(&7u32.to_le_bytes());
        buf.extend_from_slice(&32u32.to_le_bytes());
        buf.extend_from_slice(&8u32.to_le_bytes());
        buf
    }

    fn make_valid_fram_dir() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"FRAME 2 ");
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(b"FRAME 1 ");
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf
    }

    fn make_valid_fram_pool() -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(MAGIC_FRAMEPOOL);
        buf.extend_from_slice(&[0, 0, 0]);
        buf.extend_from_slice(b"art/test.rbh\0col_BOXES\0\0");
        buf.extend_from_slice(MAGIC_SPRBMPOOL);
        buf.extend_from_slice(&[0, 0, 0]);
        buf.extend_from_slice(b"PALETTE0\0\0\0\0");
        buf
    }

    #[test]
    fn parses_and_roundtrips_all_anim_fram_variants_byte_identically() {
        for (raw, expected_kind) in [
            (make_valid_anim_chunked(), AnimFramKind::AnimChunked),
            (make_valid_anim_table(), AnimFramKind::AnimTable),
            (make_valid_anim_names(), AnimFramKind::AnimNames),
            (make_valid_fram_table(), AnimFramKind::FramTable),
            (make_valid_fram_dir(), AnimFramKind::FramDir),
            (make_valid_fram_pool(), AnimFramKind::FramPool),
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
        let chunked = make_valid_anim_chunked();
        assert!(!detect(&chunked[..24]));
        assert!(parse(&chunked[..24]).is_err());

        let mut bad_xy = chunked.clone();
        bad_xy[16..20].copy_from_slice(&12u32.to_le_bytes());
        assert!(!detect(&bad_xy));
        assert!(parse(&bad_xy).is_err());

        let mut bad_fram = make_valid_fram_table();
        bad_fram[4] = 99;
        assert!(!detect(&bad_fram));
        assert!(parse(&bad_fram).is_err());

        let mut seed = 0x1357_9BDFu64;
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
