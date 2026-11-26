//! Unified `IS-ASC03` / `IS-ACG03` / `IS-MAP01` chunked map and graphics container parser.
//!
//! Consolidates the four entry-point tags observed across GBA retail ROMs:
//! - **`CLRF`**: 1bpp collision mask followed by `LINK`, optional `CMNT`/`CLRC`, `MODE`,
//!   `VER ` (`IS-ASC03`/`IS-ACG03`), and `END `.
//! - **`CMNT`**: Comment-led metadata chunk run (`CMNT`, `CLRC`, `MODE`, `VER `, `END `, etc.).
//! - **`VER `**: 24-byte `VER ` (`IS-ASC03`/`IS-ACG03`) + `END ` header, tilemap body, and
//!   trailing `CLRF`/`LINK`/`CMNT`/`CLRC`/`MODE` metadata footer.
//! - **`PNL `**: Extended 10-byte `PNL ` panel header followed by `MAP ` and standard 8-byte
//!   metadata chunks (`CMNT`, `CLRF`, `ATTR`, `NAME`, `ATRC`, `LINK`, `CLRC`, `MODE`, `VER `, `END `).
//!
//! Built directly on `formatkit-layout` `0.1.17` [`ChunkStreamSpec`] (`with_allowed_tags`,
//! `with_terminator_tag`, `with_emit_terminator`, `with_stop_on_unknown_tag`,
//! [`parse_chunk_stream_with_extent`]).

use formatkit_core::{u16_at, u32_at, Error, Result};
use formatkit_layout::{
    pack_chunk_stream, parse_chunk_stream_with_extent, ChunkEntry, ChunkInput, ChunkSizeMode,
    ChunkStreamSpec, IntFieldSpec, PackPolicy,
};

use super::{DetectedFormatExtent, FORMAT_ID_IS_ASC03};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const TAG_CLRF: &[u8; 4] = b"CLRF";
const TAG_CMNT: &[u8; 4] = b"CMNT";
const TAG_VER: &[u8; 4] = b"VER ";
const TAG_PNL: &[u8; 4] = b"PNL ";
const TAG_MAP: &[u8; 4] = b"MAP ";
#[cfg(test)]
const TAG_LINK: &[u8; 4] = b"LINK";
#[cfg(test)]
const TAG_CLRC: &[u8; 4] = b"CLRC";
const TAG_MODE: &[u8; 4] = b"MODE";
const TAG_END: &[u8; 4] = b"END ";

const ALLOWED_STANDARD_TAGS: &[&[u8]] = &[
    b"CLRF", b"LINK", b"CMNT", b"CLRC", b"MODE", b"VER ", b"MAP ", b"SIZE", b"ATTR", b"NAME",
    b"ATRC",
];

const VER_IS_ASC03: &[u8; 8] = b"IS-ASC03";
const VER_IS_ACG03: &[u8; 8] = b"IS-ACG03";
const VER_IS_MAP01: &[u8; 8] = b"IS-MAP01";

const MAX_CHUNKS: usize = 256;
const VER_FOOTER_SEARCH_LIMIT: usize = 64 * 1024;

const STANDARD_CHUNK_SPEC: ChunkStreamSpec = ChunkStreamSpec::new(
    0,
    8,
    IntFieldSpec::u32_le(4),
    ChunkSizeMode::PayloadOnly,
    MAX_CHUNKS,
)
.with_tag(0, 4)
.with_allowed_tags(ALLOWED_STANDARD_TAGS)
.with_terminator_tag(TAG_END)
.with_emit_terminator(true)
.with_stop_on_unknown_tag(true)
.with_require_eof_closure(false);

/// Entry-point variant of an `IS-ASC03` family container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsAsc03Kind {
    Clrf,
    Cmnt,
    Ver,
    Pnl,
}

impl IsAsc03Kind {
    pub const fn subkind(self) -> &'static str {
        match self {
            Self::Clrf => "clrf",
            Self::Cmnt => "cmnt",
            Self::Ver => "ver",
            Self::Pnl => "pnl",
        }
    }
}

/// Summary of one parsed chunk inside an `IS-ASC03` container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsAscChunk {
    pub tag: [u8; 4],
    pub header_offset: usize,
    pub payload_offset: usize,
    pub payload_size: usize,
    /// Extra header bytes beyond the standard 8-byte `(tag, u32 size)` header (2 bytes for `PNL `).
    pub extra_header: Vec<u8>,
    pub payload: Vec<u8>,
}

/// Parsed `IS-ASC03` / `IS-ACG03` / `IS-MAP01` container metadata and exact byte extent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsAsc03Asset {
    pub kind: IsAsc03Kind,
    pub version: Option<[u8; 8]>,
    pub chunks: Vec<IsAscChunk>,
    /// Opaque body bytes situated between `VER `+`END ` and the `CLRF..MODE` footer (`IsAsc03Kind::Ver`).
    pub inter_body: Vec<u8>,
    pub consumed_end: usize,
}

impl IsAsc03Asset {
    /// Serialize this container back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Serialize a parsed `studio.is-asc03` container back into its exact binary representation.
pub fn pack(asset: &IsAsc03Asset) -> Result<Vec<u8>> {
    match asset.kind {
        IsAsc03Kind::Clrf | IsAsc03Kind::Cmnt => {
            let inputs: Vec<ChunkInput<'_>> = asset
                .chunks
                .iter()
                .map(|c| ChunkInput {
                    tag: &c.tag,
                    payload: &c.payload,
                    header_template: None,
                    trailer: None,
                })
                .collect();
            pack_chunk_stream(&STANDARD_CHUNK_SPEC, &PackPolicy::TIGHT, &[], &inputs).map_err(
                |_| Error::InvalidField {
                    what: "IS-ASC03 pack_chunk_stream",
                    value: asset.chunks.len() as u64,
                },
            )
        }
        IsAsc03Kind::Ver => {
            if asset.chunks.len() < 5 {
                return Err(Error::InvalidField {
                    what: "IS-ASC03 VER chunk count",
                    value: asset.chunks.len() as u64,
                });
            }
            let head_inputs: Vec<ChunkInput<'_>> = asset.chunks[..2]
                .iter()
                .map(|c| ChunkInput {
                    tag: &c.tag,
                    payload: &c.payload,
                    header_template: None,
                    trailer: None,
                })
                .collect();
            let mut out =
                pack_chunk_stream(&STANDARD_CHUNK_SPEC, &PackPolicy::TIGHT, &[], &head_inputs)
                    .map_err(|_| Error::InvalidField {
                        what: "IS-ASC03 VER head pack",
                        value: 2,
                    })?;
            out.extend_from_slice(&asset.inter_body);
            const FOOTER_SPEC: ChunkStreamSpec = ChunkStreamSpec::new(
                0,
                8,
                IntFieldSpec::u32_le(4),
                ChunkSizeMode::PayloadOnly,
                16,
            )
            .with_tag(0, 4)
            .with_allowed_tags(&[b"CLRF", b"LINK", b"CMNT", b"CLRC"])
            .with_terminator_tag(TAG_MODE)
            .with_emit_terminator(true)
            .with_stop_on_unknown_tag(true)
            .with_require_eof_closure(false);
            let foot_inputs: Vec<ChunkInput<'_>> = asset.chunks[2..]
                .iter()
                .map(|c| ChunkInput {
                    tag: &c.tag,
                    payload: &c.payload,
                    header_template: None,
                    trailer: None,
                })
                .collect();
            let footer = pack_chunk_stream(&FOOTER_SPEC, &PackPolicy::TIGHT, &[], &foot_inputs)
                .map_err(|_| Error::InvalidField {
                    what: "IS-ASC03 VER footer pack",
                    value: foot_inputs.len() as u64,
                })?;
            out.extend_from_slice(&footer);
            Ok(out)
        }
        IsAsc03Kind::Pnl => {
            let mut out = Vec::new();
            let mut i = 0usize;
            while i < asset.chunks.len() {
                if &asset.chunks[i].tag == TAG_PNL {
                    let ch = &asset.chunks[i];
                    if ch.extra_header.len() != 2 {
                        return Err(Error::InvalidField {
                            what: "IS-ASC03 PNL extra header",
                            value: ch.extra_header.len() as u64,
                        });
                    }
                    out.extend_from_slice(TAG_PNL);
                    out.extend_from_slice(&(ch.payload.len() as u32).to_le_bytes());
                    out.extend_from_slice(&ch.extra_header);
                    out.extend_from_slice(&ch.payload);
                    i += 1;
                } else {
                    let start = i;
                    while i < asset.chunks.len() && &asset.chunks[i].tag != TAG_PNL {
                        i += 1;
                    }
                    let run: Vec<ChunkInput<'_>> = asset.chunks[start..i]
                        .iter()
                        .map(|c| ChunkInput {
                            tag: &c.tag,
                            payload: &c.payload,
                            header_template: None,
                            trailer: None,
                        })
                        .collect();
                    let packed =
                        pack_chunk_stream(&STANDARD_CHUNK_SPEC, &PackPolicy::TIGHT, &[], &run)
                            .map_err(|_| Error::InvalidField {
                                what: "IS-ASC03 PNL standard run pack",
                                value: run.len() as u64,
                            })?;
                    out.extend_from_slice(&packed);
                }
            }
            Ok(out)
        }
    }
}

/// Returns `true` if `data` begins with a valid `IS-ASC03` / `IS-ACG03` / `IS-MAP01` container.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse an `IS-ASC03` / `IS-ACG03` / `IS-MAP01` container from the start of `data`.
pub fn parse(data: &[u8]) -> Result<IsAsc03Asset> {
    let Some(magic) = data.get(..4) else {
        return Err(Error::Truncated {
            offset: 0,
            needed: 4,
            available: data.len(),
        });
    };
    if magic == TAG_CLRF {
        parse_clrf(data)
    } else if magic == TAG_CMNT {
        parse_cmnt(data)
    } else if magic == TAG_VER {
        parse_ver(data)
    } else if magic == TAG_PNL {
        parse_pnl(data)
    } else {
        let found = u32::from_be_bytes([magic[0], magic[1], magic[2], magic[3]]);
        Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*TAG_CLRF),
            found,
        })
    }
}

fn is_known_version(bytes: &[u8]) -> bool {
    bytes == VER_IS_ASC03 || bytes == VER_IS_ACG03 || bytes == VER_IS_MAP01
}

fn to_is_asc_chunk(entry: &ChunkEntry, data: &[u8]) -> Option<IsAscChunk> {
    if entry.tag.len() != 4 {
        return None;
    }
    let payload = entry.payload(data)?.to_vec();
    Some(IsAscChunk {
        tag: [entry.tag[0], entry.tag[1], entry.tag[2], entry.tag[3]],
        header_offset: entry.header_offset,
        payload_offset: entry.payload_offset,
        payload_size: entry.payload_size,
        extra_header: Vec::new(),
        payload,
    })
}

fn parse_clrf(data: &[u8]) -> Result<IsAsc03Asset> {
    let stream = parse_chunk_stream_with_extent(data, &STANDARD_CHUNK_SPEC).map_err(|_| {
        Error::InvalidField {
            what: "IS-ASC03 CLRF chunk stream",
            value: 0,
        }
    })?;

    if stream.chunks.len() < 4 {
        return Err(Error::InvalidField {
            what: "IS-ASC03 CLRF chunk count",
            value: stream.chunks.len() as u64,
        });
    }

    let first = &stream.chunks[0];
    if first.tag.as_slice() != TAG_CLRF || first.payload_size == 0 {
        return Err(Error::InvalidField {
            what: "IS-ASC03 CLRF first chunk",
            value: first.payload_size as u64,
        });
    }

    let mut has_link = false;
    let mut mode_dims: Option<(u8, u8)> = None;
    let mut version: Option<[u8; 8]> = None;
    let mut has_end = false;

    for ch in &stream.chunks {
        let payload = ch.payload(data).ok_or(Error::InvalidField {
            what: "IS-ASC03 chunk bounds",
            value: ch.payload_offset as u64,
        })?;
        match ch.tag.as_slice() {
            b"LINK" => {
                if payload.is_empty() || payload.len() > 64 {
                    return Err(Error::InvalidField {
                        what: "IS-ASC03 LINK length",
                        value: payload.len() as u64,
                    });
                }
                has_link = true;
            }
            b"CLRC" => {
                if payload.len() != 2 {
                    return Err(Error::InvalidField {
                        what: "IS-ASC03 CLRC length",
                        value: payload.len() as u64,
                    });
                }
            }
            b"MODE" => {
                if payload.len() != 4 {
                    return Err(Error::InvalidField {
                        what: "IS-ASC03 MODE length",
                        value: payload.len() as u64,
                    });
                }
                let w = payload[0];
                let h = payload[1];
                if w == 0 || h == 0 || w > 128 || h > 128 {
                    return Err(Error::InvalidField {
                        what: "IS-ASC03 MODE dimensions",
                        value: (u64::from(w) << 8) | u64::from(h),
                    });
                }
                mode_dims = Some((w, h));
            }
            b"VER " => {
                if payload.len() != 8 || !is_known_version(payload) {
                    return Err(Error::InvalidField {
                        what: "IS-ASC03 VER payload",
                        value: payload.len() as u64,
                    });
                }
                let mut v = [0u8; 8];
                v.copy_from_slice(payload);
                version = Some(v);
            }
            b"END " => {
                if !payload.is_empty() {
                    return Err(Error::InvalidField {
                        what: "IS-ASC03 END length",
                        value: payload.len() as u64,
                    });
                }
                has_end = true;
            }
            _ => {}
        }
    }

    let Some((w, h)) = mode_dims else {
        return Err(Error::InvalidField {
            what: "IS-ASC03 missing MODE",
            value: 0,
        });
    };
    let expected_clrf_len = ((w as usize) * (h as usize)) / 8;
    if first.payload_size != expected_clrf_len || !has_link || version.is_none() || !has_end {
        return Err(Error::InvalidField {
            what: "IS-ASC03 CLRF invariants",
            value: first.payload_size as u64,
        });
    }

    let chunks = stream
        .chunks
        .iter()
        .filter_map(|ch| to_is_asc_chunk(ch, data))
        .collect::<Vec<_>>();

    Ok(IsAsc03Asset {
        kind: IsAsc03Kind::Clrf,
        version,
        chunks,
        inter_body: Vec::new(),
        consumed_end: stream.consumed_end,
    })
}

fn parse_cmnt(data: &[u8]) -> Result<IsAsc03Asset> {
    if data.len() < 9 || data[8] != 0x01 {
        return Err(Error::InvalidField {
            what: "IS-ASC03 CMNT marker",
            value: u64::from(*data.get(8).unwrap_or(&0)),
        });
    }
    let stream = parse_chunk_stream_with_extent(data, &STANDARD_CHUNK_SPEC).map_err(|_| {
        Error::InvalidField {
            what: "IS-ASC03 CMNT chunk stream",
            value: 0,
        }
    })?;
    if stream.chunks.len() < 3 {
        return Err(Error::InvalidField {
            what: "IS-ASC03 CMNT chunk count",
            value: stream.chunks.len() as u64,
        });
    }

    let mut version: Option<[u8; 8]> = None;
    let mut has_end = false;
    for ch in &stream.chunks {
        let payload = ch.payload(data).ok_or(Error::InvalidField {
            what: "IS-ASC03 CMNT chunk bounds",
            value: ch.payload_offset as u64,
        })?;
        if ch.tag.as_slice() == TAG_VER {
            if payload.len() != 8 || !is_known_version(payload) {
                return Err(Error::InvalidField {
                    what: "IS-ASC03 VER payload",
                    value: payload.len() as u64,
                });
            }
            let mut v = [0u8; 8];
            v.copy_from_slice(payload);
            version = Some(v);
        } else if ch.tag.as_slice() == TAG_END {
            if !payload.is_empty() {
                return Err(Error::InvalidField {
                    what: "IS-ASC03 END length",
                    value: payload.len() as u64,
                });
            }
            has_end = true;
        }
    }

    if version.is_none() || !has_end {
        return Err(Error::InvalidField {
            what: "IS-ASC03 CMNT missing VER/END",
            value: 0,
        });
    }

    let chunks = stream
        .chunks
        .iter()
        .filter_map(|ch| to_is_asc_chunk(ch, data))
        .collect::<Vec<_>>();

    Ok(IsAsc03Asset {
        kind: IsAsc03Kind::Cmnt,
        version,
        chunks,
        inter_body: Vec::new(),
        consumed_end: stream.consumed_end,
    })
}

fn parse_ver(data: &[u8]) -> Result<IsAsc03Asset> {
    if data.len() < 40 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 40,
            available: data.len(),
        });
    }
    let ver_len = u32_at(data, 4)?;
    if ver_len != 8 {
        return Err(Error::InvalidField {
            what: "IS-ASC03 VER length",
            value: u64::from(ver_len),
        });
    }
    let ver_bytes = &data[8..16];
    if !is_known_version(ver_bytes) {
        return Err(Error::InvalidField {
            what: "IS-ASC03 VER string",
            value: 0,
        });
    }
    if &data[16..20] != TAG_END || u32_at(data, 20)? != 0 {
        return Err(Error::InvalidField {
            what: "IS-ASC03 VER END header",
            value: 0,
        });
    }

    let mut version = [0u8; 8];
    version.copy_from_slice(ver_bytes);

    // Search forward (bounded) for a valid CLRF..MODE footer chain.
    let search_limit = data.len().min(VER_FOOTER_SEARCH_LIMIT);
    let mut pos = 24usize;
    while pos + 8 <= search_limit {
        if &data[pos..pos + 4] == TAG_CLRF {
            if let Ok(footer) = parse_ver_footer(&data[pos..search_limit], pos) {
                let mut chunks = vec![
                    IsAscChunk {
                        tag: *TAG_VER,
                        header_offset: 0,
                        payload_offset: 8,
                        payload_size: 8,
                        extra_header: Vec::new(),
                        payload: version.to_vec(),
                    },
                    IsAscChunk {
                        tag: *TAG_END,
                        header_offset: 16,
                        payload_offset: 24,
                        payload_size: 0,
                        extra_header: Vec::new(),
                        payload: Vec::new(),
                    },
                ];
                chunks.extend(footer.0);
                return Ok(IsAsc03Asset {
                    kind: IsAsc03Kind::Ver,
                    version: Some(version),
                    chunks,
                    inter_body: data[24..pos].to_vec(),
                    consumed_end: footer.1,
                });
            }
        }
        pos += 1;
    }

    Err(Error::InvalidField {
        what: "IS-ASC03 VER footer chain",
        value: 0,
    })
}

fn parse_ver_footer(slice: &[u8], base_offset: usize) -> Result<(Vec<IsAscChunk>, usize)> {
    const FOOTER_SPEC: ChunkStreamSpec = ChunkStreamSpec::new(
        0,
        8,
        IntFieldSpec::u32_le(4),
        ChunkSizeMode::PayloadOnly,
        16,
    )
    .with_tag(0, 4)
    .with_allowed_tags(&[b"CLRF", b"LINK", b"CMNT", b"CLRC"])
    .with_terminator_tag(TAG_MODE)
    .with_emit_terminator(true)
    .with_stop_on_unknown_tag(true)
    .with_require_eof_closure(false);

    let stream =
        parse_chunk_stream_with_extent(slice, &FOOTER_SPEC).map_err(|_| Error::InvalidField {
            what: "IS-ASC03 VER footer stream",
            value: 0,
        })?;
    if stream.chunks.len() < 3 {
        return Err(Error::InvalidField {
            what: "IS-ASC03 VER footer chunk count",
            value: stream.chunks.len() as u64,
        });
    }
    let first = &stream.chunks[0];
    let last = &stream.chunks[stream.chunks.len() - 1];
    if first.tag.as_slice() != TAG_CLRF || last.tag.as_slice() != TAG_MODE {
        return Err(Error::InvalidField {
            what: "IS-ASC03 VER footer framing",
            value: 0,
        });
    }
    let mode_payload = last.payload(slice).ok_or(Error::InvalidField {
        what: "IS-ASC03 VER MODE payload",
        value: 0,
    })?;
    if mode_payload.len() != 4 {
        return Err(Error::InvalidField {
            what: "IS-ASC03 VER MODE length",
            value: mode_payload.len() as u64,
        });
    }
    let w = mode_payload[0];
    let h = mode_payload[1];
    if w == 0 || h == 0 || w > 128 || h > 128 {
        return Err(Error::InvalidField {
            what: "IS-ASC03 VER MODE dims",
            value: (u64::from(w) << 8) | u64::from(h),
        });
    }
    if first.payload_size != ((w as usize) * (h as usize)) / 8 {
        return Err(Error::InvalidField {
            what: "IS-ASC03 VER CLRF size mismatch",
            value: first.payload_size as u64,
        });
    }

    let mut out = Vec::with_capacity(stream.chunks.len());
    for ch in &stream.chunks {
        let mut c = to_is_asc_chunk(ch, slice).ok_or(Error::InvalidField {
            what: "IS-ASC03 VER chunk tag",
            value: 0,
        })?;
        c.header_offset = base_offset
            .checked_add(c.header_offset)
            .ok_or(Error::InvalidField {
                what: "IS-ASC03 VER header offset",
                value: base_offset as u64,
            })?;
        c.payload_offset = base_offset
            .checked_add(c.payload_offset)
            .ok_or(Error::InvalidField {
                what: "IS-ASC03 VER payload offset",
                value: base_offset as u64,
            })?;
        out.push(c);
    }
    let end = base_offset
        .checked_add(stream.consumed_end)
        .ok_or(Error::InvalidField {
            what: "IS-ASC03 VER footer end",
            value: base_offset as u64,
        })?;
    Ok((out, end))
}

fn parse_pnl(data: &[u8]) -> Result<IsAsc03Asset> {
    let mut pos = 0usize;
    let mut chunks = Vec::new();
    let mut has_map = false;
    let mut version: Option<[u8; 8]> = None;

    while pos + 8 <= data.len() && chunks.len() < MAX_CHUNKS {
        let tag = &data[pos..pos + 4];
        if tag == TAG_PNL {
            if pos + 14 > data.len() {
                return Err(Error::Truncated {
                    offset: pos,
                    needed: 14,
                    available: data.len() - pos,
                });
            }
            let size = u32_at(data, pos + 4)? as usize;
            let kind = u16_at(data, pos + 8)?;
            let elem = u16_at(data, pos + 10)?;
            let count = u16_at(data, pos + 12)?;
            if !(kind == 1 || kind == 2) || elem != 2 {
                return Err(Error::InvalidField {
                    what: "IS-ASC03 PNL kind/elem",
                    value: (u64::from(kind) << 16) | u64::from(elem),
                });
            }
            let expected_size = (count as usize)
                .checked_mul(kind as usize)
                .and_then(|v| v.checked_mul(4))
                .and_then(|v| v.checked_add(4))
                .ok_or(Error::InvalidField {
                    what: "IS-ASC03 PNL size overflow",
                    value: u64::from(count),
                })?;
            if size != expected_size {
                return Err(Error::InvalidField {
                    what: "IS-ASC03 PNL size formula",
                    value: size as u64,
                });
            }
            let section_len = 10usize.checked_add(size).ok_or(Error::InvalidField {
                what: "IS-ASC03 PNL section len",
                value: size as u64,
            })?;
            let next = pos.checked_add(section_len).ok_or(Error::InvalidField {
                what: "IS-ASC03 PNL end",
                value: pos as u64,
            })?;
            if next > data.len() {
                return Err(Error::Truncated {
                    offset: pos,
                    needed: section_len,
                    available: data.len() - pos,
                });
            }
            chunks.push(IsAscChunk {
                tag: *TAG_PNL,
                header_offset: pos,
                payload_offset: pos + 10,
                payload_size: size,
                extra_header: data[pos + 8..pos + 10].to_vec(),
                payload: data[pos + 10..next].to_vec(),
            });
            pos = next;
            continue;
        }

        // Parse any run of standard 8-byte chunks (MAP , CMNT, CLRF, ..., END ) via ChunkStreamSpec.
        let tail = &data[pos..];
        let stream = parse_chunk_stream_with_extent(tail, &STANDARD_CHUNK_SPEC).map_err(|_| {
            Error::InvalidField {
                what: "IS-ASC03 PNL trailing chunk stream",
                value: pos as u64,
            }
        })?;
        if stream.chunks.is_empty() {
            break;
        }
        let mut hit_end = false;
        for ch in &stream.chunks {
            let payload = ch.payload(tail).ok_or(Error::InvalidField {
                what: "IS-ASC03 PNL child bounds",
                value: ch.payload_offset as u64,
            })?;
            if ch.tag.as_slice() == TAG_MAP {
                if payload.len() < 4 {
                    return Err(Error::InvalidField {
                        what: "IS-ASC03 MAP payload",
                        value: payload.len() as u64,
                    });
                }
                let w = u16_at(payload, 0)? as usize;
                let h = u16_at(payload, 2)? as usize;
                let expected = w
                    .checked_mul(h)
                    .and_then(|v| v.checked_mul(2))
                    .and_then(|v| v.checked_add(4))
                    .ok_or(Error::InvalidField {
                        what: "IS-ASC03 MAP size overflow",
                        value: w as u64,
                    })?;
                if w == 0 || h == 0 || payload.len() != expected {
                    return Err(Error::InvalidField {
                        what: "IS-ASC03 MAP size formula",
                        value: payload.len() as u64,
                    });
                }
                has_map = true;
            } else if ch.tag.as_slice() == TAG_VER
                && payload.len() == 8
                && is_known_version(payload)
            {
                let mut v = [0u8; 8];
                v.copy_from_slice(payload);
                version = Some(v);
            } else if ch.tag.as_slice() == TAG_END {
                hit_end = true;
            }
            if let Some(mut c) = to_is_asc_chunk(ch, tail) {
                c.header_offset = pos
                    .checked_add(c.header_offset)
                    .ok_or(Error::InvalidField {
                        what: "IS-ASC03 PNL child offset",
                        value: pos as u64,
                    })?;
                c.payload_offset = pos
                    .checked_add(c.payload_offset)
                    .ok_or(Error::InvalidField {
                        what: "IS-ASC03 PNL child payload offset",
                        value: pos as u64,
                    })?;
                chunks.push(c);
            }
        }
        pos = pos
            .checked_add(stream.consumed_end)
            .ok_or(Error::InvalidField {
                what: "IS-ASC03 PNL cursor",
                value: pos as u64,
            })?;
        if hit_end {
            break;
        }
    }

    if chunks.len() < 2 || !has_map {
        return Err(Error::InvalidField {
            what: "IS-ASC03 PNL requires >=2 chunks and MAP",
            value: chunks.len() as u64,
        });
    }

    Ok(IsAsc03Asset {
        kind: IsAsc03Kind::Pnl,
        version,
        chunks,
        inter_body: Vec::new(),
        consumed_end: pos,
    })
}

/// Scan `rom` for verified `studio.is-asc03` containers.
pub fn scan_is_asc03(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective = &rom[..rom.len().min(MAX_ROM_SIZE)];
    if effective.len() < HEADER_SIZE + 24 {
        return Vec::new();
    }

    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + 8 <= effective.len() {
        let b = effective[off];
        if b != b'C' && b != b'V' && b != b'P' {
            off += 1;
            continue;
        }
        let tag = &effective[off..off + 4];
        let parsed = if tag == TAG_CLRF {
            parse_clrf(&effective[off..])
        } else if tag == TAG_CMNT {
            parse_cmnt(&effective[off..])
        } else if tag == TAG_VER {
            parse_ver(&effective[off..])
        } else if tag == TAG_PNL {
            parse_pnl(&effective[off..])
        } else {
            off += 1;
            continue;
        };

        if let Ok(asset) = parsed {
            let size = asset.consumed_end;
            if size > 0 && off.checked_add(size).is_some_and(|e| e <= effective.len()) {
                found.push(DetectedFormatExtent {
                    format_id: FORMAT_ID_IS_ASC03,
                    subkind: asset.kind.subkind(),
                    offset: off,
                    size,
                });
                off = off.saturating_add(size).max(off + 1);
                continue;
            }
        }
        off += 1;
    }
    found
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn push_chunk(out: &mut Vec<u8>, tag: &[u8; 4], payload: &[u8]) {
        out.extend_from_slice(tag);
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
    }

    pub(crate) fn make_valid_clrf() -> Vec<u8> {
        let mut out = Vec::new();
        // 32x32 / 8 = 128 bytes
        push_chunk(&mut out, TAG_CLRF, &[0u8; 128]);
        push_chunk(&mut out, TAG_LINK, b"bg_01.acg\0");
        push_chunk(&mut out, TAG_CMNT, &[0x01, 0x01]);
        push_chunk(&mut out, TAG_CLRC, &[0x00, 0x00]);
        push_chunk(&mut out, TAG_MODE, &[32, 32, 0, 2]);
        push_chunk(&mut out, TAG_VER, VER_IS_ASC03);
        push_chunk(&mut out, TAG_END, &[]);
        out
    }

    fn make_valid_cmnt() -> Vec<u8> {
        let mut out = Vec::new();
        push_chunk(&mut out, TAG_CMNT, &[0x01, 0x01, b'A']);
        push_chunk(&mut out, TAG_CLRC, &[0x00, 0x00]);
        push_chunk(&mut out, TAG_MODE, &[30, 20, 0, 3]);
        push_chunk(&mut out, TAG_VER, VER_IS_ASC03);
        push_chunk(&mut out, TAG_END, &[]);
        out
    }

    fn make_valid_ver() -> Vec<u8> {
        let mut out = Vec::new();
        push_chunk(&mut out, TAG_VER, VER_IS_ASC03);
        push_chunk(&mut out, TAG_END, &[]);
        // 16 bytes tilemap body
        out.extend_from_slice(&[0x01, 0x00, 0x02, 0x00, 0x03, 0x00, 0x04, 0x00]);
        // Footer: CLRF (8*8/8 = 8 bytes), LINK, CLRC, MODE(8, 8, 0, 2)
        push_chunk(&mut out, TAG_CLRF, &[0u8; 8]);
        push_chunk(&mut out, TAG_LINK, b"\0");
        push_chunk(&mut out, TAG_CLRC, &[0x00, 0x00]);
        push_chunk(&mut out, TAG_MODE, &[8, 8, 0, 2]);
        out
    }

    fn make_valid_pnl() -> Vec<u8> {
        let mut out = Vec::new();
        // PNL: kind=2, elem=2, count=3 -> size = 4 + 3*2*4 = 28, section = 38
        out.extend_from_slice(TAG_PNL);
        out.extend_from_slice(&28u32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&3u16.to_le_bytes());
        out.extend_from_slice(&[0x11u8; 24]);
        // MAP: 4x4 -> size = 4 + 32 = 36
        let mut map_payload = Vec::new();
        map_payload.extend_from_slice(&4u16.to_le_bytes());
        map_payload.extend_from_slice(&4u16.to_le_bytes());
        map_payload.extend_from_slice(&[0u8; 32]);
        push_chunk(&mut out, TAG_MAP, &map_payload);
        push_chunk(&mut out, TAG_VER, VER_IS_MAP01);
        push_chunk(&mut out, TAG_END, &[]);
        out
    }

    #[test]
    fn parses_all_four_is_asc03_entry_points() {
        let mut clrf = make_valid_clrf();
        let clrf_len = clrf.len();
        clrf.extend_from_slice(b"TRAILING_ROM_BYTES");
        let parsed = parse(&clrf).unwrap();
        assert_eq!(parsed.kind, IsAsc03Kind::Clrf);
        assert_eq!(parsed.consumed_end, clrf_len);
        assert_eq!(parsed.version, Some(*VER_IS_ASC03));

        let cmnt = make_valid_cmnt();
        let parsed = parse(&cmnt).unwrap();
        assert_eq!(parsed.kind, IsAsc03Kind::Cmnt);
        assert_eq!(parsed.consumed_end, cmnt.len());

        let mut ver = make_valid_ver();
        let ver_len = ver.len();
        ver.extend_from_slice(&[0xFF; 32]);
        let parsed = parse(&ver).unwrap();
        assert_eq!(parsed.kind, IsAsc03Kind::Ver);
        assert_eq!(parsed.consumed_end, ver_len);

        let pnl = make_valid_pnl();
        let parsed = parse(&pnl).unwrap();
        assert_eq!(parsed.kind, IsAsc03Kind::Pnl);
        assert_eq!(parsed.consumed_end, pnl.len());
    }

    #[test]
    fn roundtrips_all_four_is_asc03_entry_points_byte_identically() {
        for raw in [
            make_valid_clrf(),
            make_valid_cmnt(),
            make_valid_ver(),
            make_valid_pnl(),
        ] {
            let parsed = parse(&raw).unwrap();
            let repacked = parsed.to_bytes().unwrap();
            assert_eq!(repacked, raw[..parsed.consumed_end]);
        }
    }

    #[test]
    fn rejects_corrupt_truncated_and_random_inputs() {
        assert!(!detect(&[]));
        assert!(!detect(&make_valid_clrf()[..40]));

        let mut bad_clrf = make_valid_clrf();
        // Corrupt CLRF length so it no longer matches 32*32/8
        bad_clrf[4] = 100;
        assert!(!detect(&bad_clrf));

        let mut bad_pnl = make_valid_pnl();
        // Corrupt elem != 2
        bad_pnl[10] = 3;
        assert!(!detect(&bad_pnl));

        let mut seed = 0xCAFE_BABEu64;
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
