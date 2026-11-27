//! `studio.scrp` — `SCRP` bytecode & symbol-table script container parser,
//! symmetric byte-identical writer, and ROM scanner.
//!
//! Built on `formatkit-layout` `ChunkStreamSpec::with_tag_size_modes` (`NVAR` header-inclusive,
//! `FUNC` stored = section_len - 4, `SVAR` payload-only = section_len - 8).

use formatkit_core::{u16_at, u32_at, Error, Result};
use formatkit_layout::{
    pack_chunk_stream, parse_chunk_stream_with_extent, ChunkInput, ChunkSizeMode, ChunkStreamSpec,
    IntFieldSpec, PackPolicy,
};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_SCRP};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_SCRP: &[u8; 4] = b"SCRP";
const MAGIC_CODE: &[u8; 4] = b"CODE";
const TAG_NVAR: &[u8; 4] = b"NVAR";
const TAG_SVAR: &[u8; 4] = b"SVAR";
const TAG_FUNC: &[u8; 4] = b"FUNC";

const SCRP_TAG_MODES: &[(&[u8], ChunkSizeMode, i64)] = &[
    (TAG_NVAR, ChunkSizeMode::IncludesHeader, 0),
    (TAG_FUNC, ChunkSizeMode::IncludesHeader, 4),
    (TAG_SVAR, ChunkSizeMode::PayloadOnly, 0),
];
const SCRP_ALLOWED_TAGS: &[&[u8]] = &[TAG_NVAR, TAG_SVAR, TAG_FUNC];

/// One symbol-table section (`NVAR`, `SVAR`, or `FUNC`) inside an `SCRP` container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrpSection {
    pub tag: [u8; 4],
    pub symbol_count: usize,
    pub payload: Vec<u8>,
}

/// Parsed `studio.scrp` script container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrpAsset {
    pub version: u16,
    pub script_type: u8,
    pub sub: u8,
    pub bytecode: Vec<u8>,
    pub sections: Vec<ScrpSection>,
    pub total_symbols: usize,
    pub consumed_end: usize,
}

impl ScrpAsset {
    /// Serialize this `SCRP` container back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Fast, panic-free structural detector for `studio.scrp` containers.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

fn validate_symbol_payload(payload: &[u8], code_len: usize) -> Result<usize> {
    if payload.len() < 7 || payload.last() != Some(&0) {
        return Err(Error::InvalidField {
            what: "SCRP symbol section pad",
            value: payload.len() as u64,
        });
    }
    let body = &payload[..payload.len() - 1];
    let mut pos = 0usize;
    let mut count = 0usize;
    let mut prev_def: Option<u32> = None;

    while pos < body.len() {
        let nul_rel = body[pos..].iter().position(|&b| b == 0).ok_or(Error::InvalidField {
            what: "SCRP symbol name NUL",
            value: pos as u64,
        })?;
        if nul_rel == 0 || nul_rel > 64 {
            return Err(Error::InvalidField {
                what: "SCRP symbol name len",
                value: nul_rel as u64,
            });
        }
        if !body[pos..pos + nul_rel]
            .iter()
            .all(|&b| (0x20..=0x7E).contains(&b))
        {
            return Err(Error::InvalidField {
                what: "SCRP symbol name ASCII",
                value: pos as u64,
            });
        }
        pos += nul_rel + 1;

        let mut addrs_for_sym = 0usize;
        while pos + 4 <= body.len() {
            let addr = u32_at(body, pos)?;
            pos += 4;
            if addr == 0 {
                if addrs_for_sym == 0 {
                    return Err(Error::InvalidField {
                        what: "SCRP empty addr list",
                        value: 0,
                    });
                }
                break;
            }
            if addr < 6 || (addr as usize) >= code_len {
                return Err(Error::InvalidField {
                    what: "SCRP addr out of code bounds",
                    value: u64::from(addr),
                });
            }
            if addrs_for_sym == 0 {
                if let Some(prev) = prev_def {
                    if addr <= prev {
                        return Err(Error::InvalidField {
                            what: "SCRP non-increasing def addr",
                            value: u64::from(addr),
                        });
                    }
                }
                prev_def = Some(addr);
            }
            addrs_for_sym += 1;
        }
        if addrs_for_sym == 0 {
            return Err(Error::InvalidField {
                what: "SCRP missing symbol addr",
                value: pos as u64,
            });
        }
        count += 1;
    }

    if count == 0 {
        return Err(Error::InvalidField {
            what: "SCRP empty symbol section",
            value: 0,
        });
    }
    Ok(count)
}

/// Parse a `studio.scrp` container from the start of `data`.
pub fn parse(data: &[u8]) -> Result<ScrpAsset> {
    if data.len() < 23 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 23,
            available: data.len(),
        });
    }
    if &data[0..4] != MAGIC_SCRP || &data[8..12] != MAGIC_CODE {
        return Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_SCRP),
            found: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        });
    }
    let total_len = u32_at(data, 4)? as usize;
    let code_len = u32_at(data, 12)? as usize;
    let version = u16_at(data, 16)?;
    let script_type = data[18];
    let res0 = data[19];
    let sub = data[20];
    let res1 = data[21];

    if version != 0x0400 || res0 != 0 || res1 != 0 || sub > 1 || code_len < 7 {
        return Err(Error::InvalidField {
            what: "SCRP header fields",
            value: u64::from(version),
        });
    }
    let sections_start = 16usize.checked_add(code_len).ok_or(Error::InvalidField {
        what: "SCRP code_len",
        value: code_len as u64,
    })?;
    if sections_start > total_len || total_len > data.len() {
        return Err(Error::InvalidField {
            what: "SCRP total_len",
            value: total_len as u64,
        });
    }

    let spec = ChunkStreamSpec::new(
        sections_start,
        8,
        IntFieldSpec::u32_le(4),
        ChunkSizeMode::IncludesHeader,
        3,
    )
    .with_tag(0, 4)
    .with_allowed_tags(SCRP_ALLOWED_TAGS)
    .with_tag_size_modes(SCRP_TAG_MODES);

    let res = parse_chunk_stream_with_extent(&data[..total_len], &spec).map_err(|_| {
        Error::InvalidField {
            what: "SCRP symbol chunk stream",
            value: total_len as u64,
        }
    })?;
    if res.chunks.is_empty() || res.consumed_end != total_len {
        return Err(Error::InvalidField {
            what: "SCRP section tiling",
            value: res.consumed_end as u64,
        });
    }

    let mut seen_func = false;
    let mut seen_nvar = false;
    let mut seen_svar = false;
    let mut sections = Vec::with_capacity(res.chunks.len());
    let mut total_symbols = 0usize;

    for (idx, ch) in res.chunks.iter().enumerate() {
        let tag: [u8; 4] = [ch.tag[0], ch.tag[1], ch.tag[2], ch.tag[3]];
        match &tag {
            b"NVAR" => {
                if seen_nvar || seen_func {
                    return Err(Error::InvalidField {
                        what: "SCRP NVAR order",
                        value: idx as u64,
                    });
                }
                seen_nvar = true;
            }
            b"SVAR" => {
                if seen_svar || seen_func {
                    return Err(Error::InvalidField {
                        what: "SCRP SVAR order",
                        value: idx as u64,
                    });
                }
                seen_svar = true;
            }
            b"FUNC" => {
                if seen_func || idx + 1 != res.chunks.len() {
                    return Err(Error::InvalidField {
                        what: "SCRP FUNC order",
                        value: idx as u64,
                    });
                }
                seen_func = true;
            }
            _ => {
                return Err(Error::InvalidField {
                    what: "SCRP unknown tag",
                    value: 0,
                });
            }
        }
        let payload = ch.payload(&data[..total_len]).ok_or(Error::InvalidField {
            what: "SCRP section payload",
            value: ch.payload_size as u64,
        })?;
        let sym_count = validate_symbol_payload(payload, code_len)?;
        total_symbols += sym_count;
        sections.push(ScrpSection {
            tag,
            symbol_count: sym_count,
            payload: payload.to_vec(),
        });
    }
    if !seen_func {
        return Err(Error::InvalidField {
            what: "SCRP missing FUNC section",
            value: 0,
        });
    }

    Ok(ScrpAsset {
        version,
        script_type,
        sub,
        bytecode: data[22..sections_start].to_vec(),
        sections,
        total_symbols,
        consumed_end: total_len,
    })
}

/// Symmetrically pack a [`ScrpAsset`] back into its exact binary representation.
pub fn pack(asset: &ScrpAsset) -> Result<Vec<u8>> {
    let code_len = 6usize + asset.bytecode.len();
    let sections_start = 16usize + code_len;

    let mut prefix = Vec::with_capacity(sections_start);
    prefix.extend_from_slice(MAGIC_SCRP);
    prefix.extend_from_slice(&0u32.to_le_bytes()); // patched after packing chunks
    prefix.extend_from_slice(MAGIC_CODE);
    prefix.extend_from_slice(&(code_len as u32).to_le_bytes());
    prefix.extend_from_slice(&asset.version.to_le_bytes());
    prefix.push(asset.script_type);
    prefix.push(0);
    prefix.push(asset.sub);
    prefix.push(0);
    prefix.extend_from_slice(&asset.bytecode);

    let spec = ChunkStreamSpec::new(
        sections_start,
        8,
        IntFieldSpec::u32_le(4),
        ChunkSizeMode::IncludesHeader,
        3,
    )
    .with_tag(0, 4)
    .with_allowed_tags(SCRP_ALLOWED_TAGS)
    .with_tag_size_modes(SCRP_TAG_MODES);

    let inputs: Vec<ChunkInput<'_>> = asset
        .sections
        .iter()
        .map(|s| ChunkInput {
            tag: &s.tag,
            payload: &s.payload,
            header_template: None,
            trailer: None,
        })
        .collect();

    let mut out = pack_chunk_stream(&spec, &PackPolicy::TIGHT, &prefix, &inputs).map_err(|_| {
        Error::InvalidField {
            what: "SCRP pack chunks",
            value: inputs.len() as u64,
        }
    })?;
    let total_u32 = u32::try_from(out.len()).map_err(|_| Error::InvalidField {
        what: "SCRP pack total",
        value: out.len() as u64,
    })?;
    out[4..8].copy_from_slice(&total_u32.to_le_bytes());
    Ok(out)
}

/// Scan a GBA ROM image for `studio.scrp` containers.
pub fn scan_scrp(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective_len = rom.len().min(MAX_ROM_SIZE);
    if effective_len < HEADER_SIZE + 23 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + 23 <= effective_len {
        if &rom[off..off + 4] == MAGIC_SCRP && &rom[off + 8..off + 12] == MAGIC_CODE {
            if let Ok(asset) = parse(&rom[off..effective_len]) {
                let size = asset.consumed_end;
                found.push(DetectedFormatExtent {
                    format_id: FORMAT_ID_SCRP,
                    subkind: "scrp",
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

    pub(crate) fn make_valid_scrp() -> Vec<u8> {
        let mut nvar_payload = Vec::new();
        nvar_payload.extend_from_slice(b"g_var\0");
        nvar_payload.extend_from_slice(&8u32.to_le_bytes());
        nvar_payload.push(0); // trailing pad

        let mut func_payload = Vec::new();
        func_payload.extend_from_slice(b"OnInit\0");
        func_payload.extend_from_slice(&6u32.to_le_bytes());
        func_payload.extend_from_slice(&0u32.to_le_bytes()); // entry terminator
        func_payload.extend_from_slice(b"OnTick\0");
        func_payload.extend_from_slice(&10u32.to_le_bytes());
        func_payload.push(0); // trailing pad

        pack(&ScrpAsset {
            version: 0x0400,
            script_type: 1,
            sub: 0,
            bytecode: vec![0x29; 16],
            sections: vec![
                ScrpSection {
                    tag: *TAG_NVAR,
                    symbol_count: 1,
                    payload: nvar_payload,
                },
                ScrpSection {
                    tag: *TAG_FUNC,
                    symbol_count: 2,
                    payload: func_payload,
                },
            ],
            total_symbols: 3,
            consumed_end: 0,
        })
        .unwrap()
    }

    #[test]
    fn round_trips_scrp_with_tag_size_modes() {
        let mut raw = make_valid_scrp();
        let exact = raw.len();
        raw.extend_from_slice(b"EXTRA_ROM");
        let parsed = parse(&raw).unwrap();
        assert_eq!(parsed.total_symbols, 3);
        assert_eq!(parsed.consumed_end, exact);
        assert_eq!(parsed.to_bytes().unwrap(), raw[..exact]);
    }

    #[test]
    fn rejects_malformed_scrp() {
        assert!(!detect(b""));
        assert!(!detect(b"SCRP\x18\0\0\0CODE\x08\0\0\0\0\x04\0\0\0\0\x29\x29"));
    }
}
