//! `studio.nitro-gba` — Nitro-GBA 2D cell, animation, tile, and label container parser,
//! symmetric byte-identical writer, and ROM scanner.
//!
//! Supports the byte-reversed FourCC BOM-framed Nitro-GBA family:
//! - `RECN` (cell bank: `KBEC` + `LBAL` + `TXEU`)
//! - `RNAN` (animation bank: `KNBA` + `LBAL` + `TXEU`)
//! - `RGCN` (tile bank: `RAHC` + optional `SOPC`)
//! - Standalone `KNBA` animation bank
//! - Standalone `LBAL` label table (Rev 1 with offset array)
//!
//! Built on `formatkit-layout` `ChunkStreamSpec::with_exact_chunks`,
//! `FixedRecordTableSpec`, and `AlignedStringBankSpec`.

use formatkit_core::{u16_at, u32_at, Error, Result};
use formatkit_layout::{
    pack_aligned_string_bank, pack_chunk_stream, parse_aligned_string_bank,
    parse_chunk_stream_with_extent, AlignedStringBankSpec, ChunkInput, ChunkSizeMode,
    ChunkStreamSpec, FixedRecordTableSpec, IntFieldSpec, PackPolicy, SizeMode, StringBankEncoding,
    StringBankTermination, TableTermination,
};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_NITRO_GBA};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_RECN: &[u8; 4] = b"RECN";
const MAGIC_RNAN: &[u8; 4] = b"RNAN";
const MAGIC_RGCN: &[u8; 4] = b"RGCN";
const MAGIC_KNBA: &[u8; 4] = b"KNBA";
const MAGIC_LBAL: &[u8; 4] = b"LBAL";
const MAGIC_KBEC: &[u8; 4] = b"KBEC";
const MAGIC_TXEU: &[u8; 4] = b"TXEU";
const MAGIC_RAHC: &[u8; 4] = b"RAHC";
const MAGIC_SOPC: &[u8; 4] = b"SOPC";

const BOM_LE: u16 = 0xFEFF;
const HEADER_LEN: usize = 16;
const MAX_ASSET_LEN: usize = 4 * 1024 * 1024;

/// A single raw chunk preserved inside a BOM-framed Nitro-GBA container (`RECN`, `RNAN`, `RGCN`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NitroBlock {
    /// Four-byte reversed chunk tag (e.g. `b"KBEC"`, `b"KNBA"`, `b"LBAL"`, `b"TXEU"`, `b"RAHC"`, `b"SOPC"`).
    pub tag: [u8; 4],
    /// Chunk payload following the 8-byte `(tag, size)` header.
    pub payload: Vec<u8>,
}

/// BOM-framed 16-byte header + exact-count chunk stream (`RECN`, `RNAN`, `RGCN`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NitroContainer {
    /// Four-byte container magic (`RECN`, `RNAN`, or `RGCN`).
    pub magic: [u8; 4],
    /// Version halfword (`0x0100` or `0x0101`).
    pub version: u16,
    /// Declared logical asset length (`total_size`).
    pub total_size: u32,
    /// Exact sequence of blocks decoded via `ChunkStreamSpec::with_exact_chunks`.
    pub blocks: Vec<NitroBlock>,
    /// Validated primary record/tile count.
    pub record_count: usize,
    /// Exact consumed byte extent.
    pub consumed_end: usize,
}

/// Standalone `KNBA` animation bank block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnbaBlock {
    /// Animation sequence count `N`.
    pub seq_count: u16,
    /// Frame command count `M`.
    pub cmd_count: u16,
    /// Raw payload bytes following the 8-byte `(b"KNBA", total)` header.
    pub payload: Vec<u8>,
    /// Exact consumed byte extent.
    pub consumed_end: usize,
}

/// Standalone `LBAL` label table (Rev 1 offset table + NUL-terminated ASCII pool).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LbalBlock {
    /// Decoded ASCII labels.
    pub labels: Vec<Vec<u8>>,
    /// Exact consumed byte extent.
    pub consumed_end: usize,
}

/// Parsed `studio.nitro-gba` asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NitroGbaAsset {
    /// `RECN` cell bank (`KBEC` + `LBAL` + `TXEU`).
    Recn(NitroContainer),
    /// `RNAN` animation bank (`KNBA` + `LBAL` + `TXEU`).
    Rnan(NitroContainer),
    /// `RGCN` tile bank (`RAHC` + optional `SOPC`).
    Rgcn(NitroContainer),
    /// Standalone `KNBA` animation bank.
    Knba(KnbaBlock),
    /// Standalone `LBAL` label pool.
    Lbal(LbalBlock),
}

impl NitroGbaAsset {
    /// Exact consumed byte extent of this asset.
    #[must_use]
    pub fn consumed_end(&self) -> usize {
        match self {
            Self::Recn(c) | Self::Rnan(c) | Self::Rgcn(c) => c.consumed_end,
            Self::Knba(k) => k.consumed_end,
            Self::Lbal(l) => l.consumed_end,
        }
    }

    /// Sub-format identifier for `DetectedFormatExtent`.
    #[must_use]
    pub fn subkind(&self) -> &'static str {
        match self {
            Self::Recn(_) => "recn",
            Self::Rnan(_) => "rnan",
            Self::Rgcn(_) => "rgcn",
            Self::Knba(_) => "knba",
            Self::Lbal(_) => "lbal",
        }
    }

    /// Serialize this asset back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Fast, panic-free structural detector for `studio.nitro-gba` assets.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse a `studio.nitro-gba` asset (`RECN`, `RNAN`, `RGCN`, `KNBA`, or `LBAL`) at `data[0..]`.
pub fn parse(data: &[u8]) -> Result<NitroGbaAsset> {
    if data.len() < 8 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 8,
            available: data.len(),
        });
    }
    match &data[..4] {
        b"RECN" => parse_recn(data).map(NitroGbaAsset::Recn),
        b"RNAN" => parse_rnan(data).map(NitroGbaAsset::Rnan),
        b"RGCN" => parse_rgcn(data).map(NitroGbaAsset::Rgcn),
        b"KNBA" => parse_knba(data).map(NitroGbaAsset::Knba),
        b"LBAL" => parse_lbal(data).map(NitroGbaAsset::Lbal),
        _ => Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_RECN),
            found: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        }),
    }
}

fn parse_bom_container(
    data: &[u8],
    expected_magic: &[u8; 4],
    allowed_versions: &[u16],
    allowed_block_counts: &[u16],
) -> Result<(u16, u32, Vec<NitroBlock>, usize)> {
    if data.len() < HEADER_LEN {
        return Err(Error::Truncated {
            offset: 0,
            needed: HEADER_LEN,
            available: data.len(),
        });
    }
    if &data[..4] != expected_magic {
        return Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*expected_magic),
            found: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        });
    }
    let bom = u16_at(data, 4)?;
    let version = u16_at(data, 6)?;
    let total_size = u32_at(data, 8)?;
    let hdr_size = u16_at(data, 12)?;
    let block_count = u16_at(data, 14)?;

    if bom != BOM_LE
        || !allowed_versions.contains(&version)
        || hdr_size != HEADER_LEN as u16
        || !allowed_block_counts.contains(&block_count)
    {
        return Err(Error::InvalidField {
            what: "Nitro-GBA BOM header",
            value: u64::from(bom),
        });
    }
    let total = total_size as usize;
    if total < HEADER_LEN || total > data.len() || total > MAX_ASSET_LEN {
        return Err(Error::InvalidField {
            what: "Nitro-GBA total_size",
            value: u64::from(total_size),
        });
    }

    let spec = ChunkStreamSpec::new(
        HEADER_LEN,
        8,
        IntFieldSpec::u32_le(4),
        ChunkSizeMode::IncludesHeader,
        8,
    )
    .with_tag(0, 4)
    .with_exact_chunks(block_count as usize)
    .with_require_eof_closure(false);

    let res = parse_chunk_stream_with_extent(&data[..total], &spec).map_err(|_| {
        Error::InvalidField {
            what: "Nitro-GBA chunk stream",
            value: u64::from(block_count),
        }
    })?;
    if res.consumed_end != total {
        return Err(Error::InvalidField {
            what: "Nitro-GBA chunk tiling",
            value: res.consumed_end as u64,
        });
    }

    let mut blocks = Vec::with_capacity(res.chunks.len());
    for ch in &res.chunks {
        if ch.tag.len() != 4 {
            return Err(Error::InvalidField {
                what: "Nitro-GBA chunk tag",
                value: ch.tag.len() as u64,
            });
        }
        let payload = ch.payload(&data[..total]).ok_or(Error::InvalidField {
            what: "Nitro-GBA chunk payload",
            value: ch.payload_size as u64,
        })?;
        blocks.push(NitroBlock {
            tag: [ch.tag[0], ch.tag[1], ch.tag[2], ch.tag[3]],
            payload: payload.to_vec(),
        });
    }

    Ok((version, total_size, blocks, total))
}

fn validate_lbal_payload(payload: &[u8], expected_count: Option<usize>) -> Result<Vec<Vec<u8>>> {
    if payload.len() < 5 {
        return Err(Error::InvalidField {
            what: "LBAL payload length",
            value: payload.len() as u64,
        });
    }
    // Offset table: u32 LE offsets starting at 0, strictly increasing; pool starts at 4 * count.
    let first_off = u32_at(payload, 0)?;
    if first_off != 0 {
        return Err(Error::InvalidField {
            what: "LBAL first offset",
            value: u64::from(first_off),
        });
    }

    // Count offsets by finding where the u32 offset array meets the ASCII string pool.
    let mut offsets = Vec::new();
    if let Some(n) = expected_count {
        let pool_start = n.checked_mul(4).ok_or(Error::InvalidField {
            what: "LBAL offset table",
            value: n as u64,
        })?;
        if pool_start >= payload.len() {
            return Err(Error::InvalidField {
                what: "LBAL pool_start",
                value: pool_start as u64,
            });
        }
        for i in 0..n {
            offsets.push(u32_at(payload, i * 4)? as usize);
        }
        return decode_lbal_pool(&payload[pool_start..], &offsets);
    }

    // Standalone/uncounted LBAL: discover k offsets where offset[0] == 0 and offset[i] is
    // strictly increasing and < remaining pool length.
    let max_n = (payload.len() / 4).min(4096);
    offsets.push(0);
    let mut n = 1usize;
    while n < max_n {
        let cand = u32_at(payload, n * 4)? as usize;
        let pool_start = (n + 1) * 4;
        if pool_start >= payload.len() {
            break;
        }
        let pool_len = payload.len() - pool_start;
        if let Some(&prev) = offsets.last() {
            if cand > prev && cand < pool_len {
                offsets.push(cand);
                n += 1;
                continue;
            }
        }
        break;
    }

    // Try from longest candidate offset table downward until pool matches offsets exactly.
    for count in (1..=offsets.len()).rev() {
        let pool_start = count * 4;
        if let Ok(labels) = decode_lbal_pool(&payload[pool_start..], &offsets[..count]) {
            return Ok(labels);
        }
    }
    Err(Error::InvalidField {
        what: "LBAL label pool",
        value: payload.len() as u64,
    })
}

fn decode_lbal_pool(pool: &[u8], offsets: &[usize]) -> Result<Vec<Vec<u8>>> {
    if pool.is_empty() || offsets.is_empty() {
        return Err(Error::InvalidField {
            what: "LBAL pool empty",
            value: 0,
        });
    }
    let spec = AlignedStringBankSpec::aligned(0, 1, 4096)
        .with_encoding(StringBankEncoding::PrintableAscii)
        .with_termination(StringBankTermination::Counted(offsets.len()));
    let res = parse_aligned_string_bank(pool, &spec).map_err(|_| Error::InvalidField {
        what: "LBAL string bank",
        value: offsets.len() as u64,
    })?;
    if res.consumed_end != pool.len() {
        return Err(Error::InvalidField {
            what: "LBAL pool trailing bytes",
            value: res.consumed_end as u64,
        });
    }
    let mut labels = Vec::with_capacity(res.entries.len());
    for (entry, &expected_off) in res.entries.iter().zip(offsets.iter()) {
        if entry.offset != expected_off || entry.bytes.is_empty() {
            return Err(Error::InvalidField {
                what: "LBAL offset mismatch",
                value: entry.offset as u64,
            });
        }
        labels.push(entry.bytes.clone());
    }
    Ok(labels)
}

fn parse_recn(data: &[u8]) -> Result<NitroContainer> {
    let (version, total_size, blocks, total) =
        parse_bom_container(data, MAGIC_RECN, &[0x0100], &[3])?;
    if &blocks[0].tag != MAGIC_KBEC
        || &blocks[1].tag != MAGIC_LBAL
        || &blocks[2].tag != MAGIC_TXEU
    {
        return Err(Error::InvalidField {
            what: "RECN block tags",
            value: 0,
        });
    }
    let kbec = &blocks[0].payload;
    if kbec.len() < 24 {
        return Err(Error::InvalidField {
            what: "KBEC header",
            value: kbec.len() as u64,
        });
    }
    let count = u16_at(kbec, 0)? as usize;
    let ver_flag = u16_at(kbec, 2)?;
    let hdr_len = u32_at(kbec, 4)?;
    if count == 0 || ver_flag != 1 || hdr_len != 24 {
        return Err(Error::InvalidField {
            what: "KBEC counts",
            value: count as u64,
        });
    }
    let table_spec = FixedRecordTableSpec::new(
        16,
        24,
        8192,
        None,
        SizeMode::FixedSlot(0),
        TableTermination::Counted(count),
    )
    .with_offset_base(formatkit_layout::OffsetBase::TableEnd);
    let rows = formatkit_layout::parse_fixed_record_table(kbec, &table_spec).map_err(|_| {
        Error::InvalidField {
            what: "KBEC cell table",
            value: count as u64,
        }
    })?;
    let frame_region_start = 24 + count * 16;
    let frame_region_len = kbec.len().saturating_sub(frame_region_start);
    let mut prev_frame_off = 0u32;
    for row in &rows {
        let rec = &kbec[row.record_offset..row.record_offset + 16];
        let ptr = u32_at(rec, 0)?;
        let frame_off = u32_at(rec, 4)?;
        if !(0x0800_0000..0x1000_0000).contains(&ptr)
            || frame_off < prev_frame_off
            || (frame_off as usize) > frame_region_len
        {
            return Err(Error::InvalidField {
                what: "KBEC cell record",
                value: u64::from(ptr),
            });
        }
        prev_frame_off = frame_off;
    }

    let _ = validate_lbal_payload(&blocks[1].payload, None)?;
    if blocks[2].payload.len() != 4 {
        return Err(Error::InvalidField {
            what: "RECN TXEU payload",
            value: blocks[2].payload.len() as u64,
        });
    }

    Ok(NitroContainer {
        magic: *MAGIC_RECN,
        version,
        total_size,
        blocks,
        record_count: count,
        consumed_end: total,
    })
}

fn validate_knba_payload(payload: &[u8]) -> Result<(u16, u16)> {
    if payload.len() < 24 || (payload.len() & 3) != 0 {
        return Err(Error::InvalidField {
            what: "KNBA payload length",
            value: payload.len() as u64,
        });
    }
    let n1 = u16_at(payload, 0)?;
    let n2 = u16_at(payload, 2)?;
    let hdr_tag = u32_at(payload, 4)?;
    let t1 = u32_at(payload, 8)?;
    let t2 = u32_at(payload, 12)?;
    let res0 = u32_at(payload, 16)?;
    let res1 = u32_at(payload, 20)?;
    if n1 == 0 || n2 == 0 || hdr_tag != 24 || res0 != 0 || res1 != 0 {
        return Err(Error::InvalidField {
            what: "KNBA header fields",
            value: u64::from(hdr_tag),
        });
    }
    let expected_t1 = 24u32
        .checked_add(u32::from(n1).saturating_mul(16))
        .ok_or(Error::InvalidField {
            what: "KNBA t1",
            value: u64::from(t1),
        })?;
    let expected_t2 = expected_t1
        .checked_add(u32::from(n2).saturating_mul(8))
        .ok_or(Error::InvalidField {
            what: "KNBA t2",
            value: u64::from(t2),
        })?;
    if t1 != expected_t1 || t2 != expected_t2 {
        return Err(Error::InvalidField {
            what: "KNBA table offsets",
            value: u64::from(t1),
        });
    }
    let rec_start = 24usize + (n1 as usize) * 16;
    let tail_start = rec_start + (n2 as usize) * 8;
    if tail_start + 2 > payload.len() || ((payload.len() - tail_start) & 1) != 0 {
        return Err(Error::InvalidField {
            what: "KNBA tail framing",
            value: payload.len() as u64,
        });
    }

    // Validate entry rows (16B) partitioning the M command rows (8B).
    let mut running_off = 0u32;
    for i in 0..(n1 as usize) {
        let base = 24 + i * 16;
        let cnt = u32_at(payload, base)?;
        let flags = u32_at(payload, base + 4)?;
        let unit = u32_at(payload, base + 8)?;
        let byte_off = u32_at(payload, base + 12)?;
        if (flags >> 16) != 1
            || (flags & 0xFFFF) > 2
            || !(unit == 1 || unit == 2)
            || byte_off != running_off
        {
            return Err(Error::InvalidField {
                what: "KNBA entry row",
                value: u64::from(flags),
            });
        }
        running_off = running_off
            .checked_add(cnt.checked_mul(8).ok_or(Error::InvalidField {
                what: "KNBA entry count",
                value: u64::from(cnt),
            })?)
            .ok_or(Error::InvalidField {
                what: "KNBA offset chain",
                value: u64::from(running_off),
            })?;
    }
    if running_off != u32::from(n2) * 8 {
        return Err(Error::InvalidField {
            what: "KNBA entry sum",
            value: u64::from(running_off),
        });
    }

    // Validate command rows (8B): even slot + 0xBEEF sentinel.
    for i in 0..(n2 as usize) {
        let base = rec_start + i * 8;
        let slot = u32_at(payload, base)?;
        let sentinel = u16_at(payload, base + 6)?;
        if (slot & 1) != 0 || sentinel != 0xBEEF {
            return Err(Error::InvalidField {
                what: "KNBA command sentinel",
                value: u64::from(sentinel),
            });
        }
    }

    Ok((n1, n2))
}

fn parse_rnan(data: &[u8]) -> Result<NitroContainer> {
    let (version, total_size, blocks, total) =
        parse_bom_container(data, MAGIC_RNAN, &[0x0100], &[3])?;
    if &blocks[0].tag != MAGIC_KNBA
        || &blocks[1].tag != MAGIC_LBAL
        || &blocks[2].tag != MAGIC_TXEU
    {
        return Err(Error::InvalidField {
            what: "RNAN block tags",
            value: 0,
        });
    }
    let (n1, _n2) = validate_knba_payload(&blocks[0].payload)?;
    let _ = validate_lbal_payload(&blocks[1].payload, Some(n1 as usize))?;
    if blocks[2].payload.len() != 4 {
        return Err(Error::InvalidField {
            what: "RNAN TXEU payload",
            value: blocks[2].payload.len() as u64,
        });
    }

    Ok(NitroContainer {
        magic: *MAGIC_RNAN,
        version,
        total_size,
        blocks,
        record_count: n1 as usize,
        consumed_end: total,
    })
}

fn parse_rgcn(data: &[u8]) -> Result<NitroContainer> {
    let (version, total_size, blocks, total) =
        parse_bom_container(data, MAGIC_RGCN, &[0x0100, 0x0101], &[1, 2])?;
    let expected_blocks = usize::from(version - 0x0100 + 1);
    if blocks.len() != expected_blocks || &blocks[0].tag != MAGIC_RAHC {
        return Err(Error::InvalidField {
            what: "RGCN block layout",
            value: blocks.len() as u64,
        });
    }
    let rahc = &blocks[0].payload;
    if rahc.len() < 24 {
        return Err(Error::InvalidField {
            what: "RAHC header",
            value: rahc.len() as u64,
        });
    }
    let flags = u32_at(rahc, 0)?;
    let bpp_section = u32_at(rahc, 4)?;
    let data_len = u32_at(rahc, 16)? as usize;
    let ext_hdr = u32_at(rahc, 20)?;
    let stride = match bpp_section {
        3 => 32usize,
        4 => 64usize,
        _ => {
            return Err(Error::InvalidField {
                what: "RAHC section marker",
                value: u64::from(bpp_section),
            });
        }
    };
    if ext_hdr != 0x18
        || data_len == 0
        || !data_len.is_multiple_of(32)
        || 24 + data_len != rahc.len()
    {
        return Err(Error::InvalidField {
            what: "RAHC tile data length",
            value: data_len as u64,
        });
    }
    if blocks.len() == 1 {
        if flags != 0xFFFF_FFFF {
            return Err(Error::InvalidField {
                what: "RGCN v0x0100 flags",
                value: u64::from(flags),
            });
        }
    } else {
        if &blocks[1].tag != MAGIC_SOPC || blocks[1].payload.len() != 8 {
            return Err(Error::InvalidField {
                what: "RGCN SOPC block",
                value: blocks[1].payload.len() as u64,
            });
        }
        let sopc = &blocks[1].payload;
        let width = u16_at(sopc, 4)? as usize;
        let height = u16_at(sopc, 6)? as usize;
        let expected_flags = ((width as u32) << 16) | (height as u32);
        if flags != expected_flags
            || width
                .checked_mul(height)
                .and_then(|n| n.checked_mul(stride))
                != Some(data_len)
        {
            return Err(Error::InvalidField {
                what: "RGCN SOPC dimensions",
                value: u64::from(flags),
            });
        }
    }

    Ok(NitroContainer {
        magic: *MAGIC_RGCN,
        version,
        total_size,
        blocks,
        record_count: data_len / stride,
        consumed_end: total,
    })
}

fn parse_knba(data: &[u8]) -> Result<KnbaBlock> {
    if data.len() < 32 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 32,
            available: data.len(),
        });
    }
    if &data[..4] != MAGIC_KNBA {
        return Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_KNBA),
            found: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        });
    }
    let total = u32_at(data, 4)? as usize;
    if total < 32 || total > data.len() || total > MAX_ASSET_LEN || (total & 3) != 0 {
        return Err(Error::InvalidField {
            what: "KNBA total",
            value: total as u64,
        });
    }
    let payload = &data[8..total];
    let (seq_count, cmd_count) = validate_knba_payload(payload)?;
    Ok(KnbaBlock {
        seq_count,
        cmd_count,
        payload: payload.to_vec(),
        consumed_end: total,
    })
}

fn parse_lbal(data: &[u8]) -> Result<LbalBlock> {
    if data.len() < 13 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 13,
            available: data.len(),
        });
    }
    if &data[..4] != MAGIC_LBAL {
        return Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_LBAL),
            found: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        });
    }
    let total = u32_at(data, 4)? as usize;
    if total < 13 || total > data.len() || total > MAX_ASSET_LEN {
        return Err(Error::InvalidField {
            what: "LBAL total",
            value: total as u64,
        });
    }
    let labels = validate_lbal_payload(&data[8..total], None)?;
    Ok(LbalBlock {
        labels,
        consumed_end: total,
    })
}

/// Symmetrically pack a [`NitroGbaAsset`] back into its exact binary representation.
pub fn pack(asset: &NitroGbaAsset) -> Result<Vec<u8>> {
    match asset {
        NitroGbaAsset::Recn(c) | NitroGbaAsset::Rnan(c) | NitroGbaAsset::Rgcn(c) => {
            pack_bom_container(c)
        }
        NitroGbaAsset::Knba(k) => {
            let total = u32::try_from(8usize.saturating_add(k.payload.len())).map_err(|_| {
                Error::InvalidField {
                    what: "KNBA pack size",
                    value: k.payload.len() as u64,
                }
            })?;
            let mut out = Vec::with_capacity(total as usize);
            out.extend_from_slice(MAGIC_KNBA);
            out.extend_from_slice(&total.to_le_bytes());
            out.extend_from_slice(&k.payload);
            Ok(out)
        }
        NitroGbaAsset::Lbal(l) => {
            let refs: Vec<&[u8]> = l.labels.iter().map(Vec::as_slice).collect();
            let bank_spec = AlignedStringBankSpec::aligned(0, 1, 4096)
                .with_encoding(StringBankEncoding::PrintableAscii)
                .with_termination(StringBankTermination::Counted(refs.len()));
            let pool = pack_aligned_string_bank(&bank_spec, &PackPolicy::TIGHT, &[], &refs)
                .map_err(|_| Error::InvalidField {
                    what: "LBAL pack pool",
                    value: refs.len() as u64,
                })?;
            let total = 8usize + refs.len() * 4 + pool.len();
            let total_u32 = u32::try_from(total).map_err(|_| Error::InvalidField {
                what: "LBAL pack size",
                value: total as u64,
            })?;
            let mut out = Vec::with_capacity(total);
            out.extend_from_slice(MAGIC_LBAL);
            out.extend_from_slice(&total_u32.to_le_bytes());
            let mut cursor = 0u32;
            for lbl in &l.labels {
                out.extend_from_slice(&cursor.to_le_bytes());
                cursor = cursor.saturating_add(lbl.len() as u32 + 1);
            }
            out.extend_from_slice(&pool);
            Ok(out)
        }
    }
}

fn pack_bom_container(c: &NitroContainer) -> Result<Vec<u8>> {
    let body_len: usize = c.blocks.iter().map(|b| 8 + b.payload.len()).sum();
    let total = HEADER_LEN + body_len;
    let total_u32 = u32::try_from(total).map_err(|_| Error::InvalidField {
        what: "Nitro-GBA pack total",
        value: total as u64,
    })?;
    let mut header = [0u8; HEADER_LEN];
    header[0..4].copy_from_slice(&c.magic);
    header[4..6].copy_from_slice(&BOM_LE.to_le_bytes());
    header[6..8].copy_from_slice(&c.version.to_le_bytes());
    header[8..12].copy_from_slice(&total_u32.to_le_bytes());
    header[12..14].copy_from_slice(&(HEADER_LEN as u16).to_le_bytes());
    header[14..16].copy_from_slice(&(c.blocks.len() as u16).to_le_bytes());

    let spec = ChunkStreamSpec::new(
        HEADER_LEN,
        8,
        IntFieldSpec::u32_le(4),
        ChunkSizeMode::IncludesHeader,
        8,
    )
    .with_tag(0, 4)
    .with_exact_chunks(c.blocks.len())
    .with_require_eof_closure(false);

    let inputs: Vec<ChunkInput<'_>> = c
        .blocks
        .iter()
        .map(|b| ChunkInput {
            tag: &b.tag,
            payload: &b.payload,
            header_template: None,
            trailer: None,
        })
        .collect();

    pack_chunk_stream(&spec, &PackPolicy::TIGHT, &header, &inputs).map_err(|_| {
        Error::InvalidField {
            what: "Nitro-GBA pack chunks",
            value: c.blocks.len() as u64,
        }
    })
}

/// Scan a GBA ROM image for `studio.nitro-gba` containers (`RECN`, `RNAN`, `RGCN`).
pub fn scan_nitro_gba(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective_len = rom.len().min(MAX_ROM_SIZE);
    if effective_len < HEADER_SIZE + HEADER_LEN {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + HEADER_LEN <= effective_len {
        let tag = &rom[off..off + 4];
        if (tag == MAGIC_RECN || tag == MAGIC_RNAN || tag == MAGIC_RGCN)
            && rom[off + 4] == 0xFF
            && rom[off + 5] == 0xFE
        {
            if let Ok(asset) = parse(&rom[off..effective_len]) {
                let size = asset.consumed_end();
                found.push(DetectedFormatExtent {
                    format_id: FORMAT_ID_NITRO_GBA,
                    subkind: asset.subkind(),
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

    pub(crate) fn make_valid_rgcn() -> Vec<u8> {
        let tile_bytes = vec![0x33u8; 64]; // 2 tiles @ 4bpp
        let mut rahc_payload = Vec::new();
        rahc_payload.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // flags
        rahc_payload.extend_from_slice(&3u32.to_le_bytes()); // 4bpp
        rahc_payload.extend_from_slice(&0x10u32.to_le_bytes());
        rahc_payload.extend_from_slice(&256u32.to_le_bytes());
        rahc_payload.extend_from_slice(&(tile_bytes.len() as u32).to_le_bytes());
        rahc_payload.extend_from_slice(&0x18u32.to_le_bytes());
        rahc_payload.extend_from_slice(&tile_bytes);

        let container = NitroContainer {
            magic: *MAGIC_RGCN,
            version: 0x0100,
            total_size: (16 + 8 + rahc_payload.len()) as u32,
            blocks: vec![NitroBlock {
                tag: *MAGIC_RAHC,
                payload: rahc_payload,
            }],
            record_count: 2,
            consumed_end: 16 + 8 + 24 + 64,
        };
        pack(&NitroGbaAsset::Rgcn(container)).unwrap()
    }

    pub(crate) fn make_valid_rnan() -> Vec<u8> {
        let mut knba_payload = Vec::new();
        knba_payload.extend_from_slice(&1u16.to_le_bytes()); // n1 = 1
        knba_payload.extend_from_slice(&1u16.to_le_bytes()); // n2 = 1
        knba_payload.extend_from_slice(&24u32.to_le_bytes()); // tag
        knba_payload.extend_from_slice(&40u32.to_le_bytes()); // t1 = 24 + 16
        knba_payload.extend_from_slice(&48u32.to_le_bytes()); // t2 = 40 + 8
        knba_payload.extend_from_slice(&0u32.to_le_bytes());
        knba_payload.extend_from_slice(&0u32.to_le_bytes());
        // Entry 0 (16B): count=1, flags=0x00010000, unit=1, byte_offset=0
        knba_payload.extend_from_slice(&1u32.to_le_bytes());
        knba_payload.extend_from_slice(&0x0001_0000u32.to_le_bytes());
        knba_payload.extend_from_slice(&1u32.to_le_bytes());
        knba_payload.extend_from_slice(&0u32.to_le_bytes());
        // Command 0 (8B): slot=0, bank=1, sentinel=0xBEEF
        knba_payload.extend_from_slice(&0u32.to_le_bytes());
        knba_payload.extend_from_slice(&1u16.to_le_bytes());
        knba_payload.extend_from_slice(&0xBEEFu16.to_le_bytes());
        // Tail (4B)
        knba_payload.extend_from_slice(&0u16.to_le_bytes());
        knba_payload.extend_from_slice(&0xCCCCu16.to_le_bytes());

        let mut lbal_payload = Vec::new();
        lbal_payload.extend_from_slice(&0u32.to_le_bytes());
        lbal_payload.extend_from_slice(b"idle\0");

        let txeu_payload = vec![0u8; 4];
        let total = 16 + (8 + knba_payload.len()) + (8 + lbal_payload.len()) + 12;
        let container = NitroContainer {
            magic: *MAGIC_RNAN,
            version: 0x0100,
            total_size: total as u32,
            blocks: vec![
                NitroBlock {
                    tag: *MAGIC_KNBA,
                    payload: knba_payload,
                },
                NitroBlock {
                    tag: *MAGIC_LBAL,
                    payload: lbal_payload,
                },
                NitroBlock {
                    tag: *MAGIC_TXEU,
                    payload: txeu_payload,
                },
            ],
            record_count: 1,
            consumed_end: total,
        };
        pack(&NitroGbaAsset::Rnan(container)).unwrap()
    }

    #[test]
    fn round_trips_rgcn_rnan_knba_and_lbal() {
        let mut rgcn = make_valid_rgcn();
        let exact_len = rgcn.len();
        rgcn.extend_from_slice(b"TRAILING_ROM_DATA");
        let parsed = parse(&rgcn).unwrap();
        assert_eq!(parsed.consumed_end(), exact_len);
        assert_eq!(parsed.to_bytes().unwrap(), rgcn[..exact_len]);

        let rnan = make_valid_rnan();
        let parsed_rnan = parse(&rnan).unwrap();
        assert_eq!(parsed_rnan.to_bytes().unwrap(), rnan);

        // Standalone LBAL
        let lbal = pack(&NitroGbaAsset::Lbal(LbalBlock {
            labels: vec![b"Standby".to_vec(), b"Attack".to_vec()],
            consumed_end: 0,
        }))
        .unwrap();
        let parsed_lbal = parse(&lbal).unwrap();
        assert_eq!(parsed_lbal.to_bytes().unwrap(), lbal);
    }

    #[test]
    fn rejects_malformed_nitro_gba() {
        assert!(!detect(b""));
        assert!(!detect(b"RECN\0\0\0\0\0\0\0\0\0\0\0\0"));
        assert!(!detect(b"RGCN\xFF\xFE\x00\x01\x10\0\0\0\x10\0\x01\0"));
    }
}
