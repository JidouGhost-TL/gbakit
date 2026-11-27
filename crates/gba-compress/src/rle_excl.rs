//! `RLE!` byte/halfword flag-byte run-length codec.
//!
//! Layout (little-endian):
//! - `+0x00`: `b"RLE!"` (`52 4C 45 21`)
//! - `+0x04`: `out_len` (`u24 LE`, 3 bytes) + `0x00` pad byte at `+0x07`
//! - `+0x08`: `comp_len` (`u32 LE`, compressed stream length)
//! - `+0x0C`: `flags` (`u32 LE`: `0` = 1-byte units, `1` = 2-byte halfword units)
//! - `+0x10`: `comp_len` bytes of flag-byte RLE (`(f & 0x7F) + 1` units; `f & 0x80 == 0` literal, else fill)

use formatkit_codec::DecodeOutcome;
use formatkit_core::{u32_at, Error, Result};

/// Four-byte `RLE!` stream magic.
pub const MAGIC_RLE_EXCL: &[u8; 4] = b"RLE!";

/// Fixed header length in bytes.
pub const RLE_EXCL_HEADER_LEN: usize = 16;

/// Maximum uncompressed payload accepted (8 MiB).
pub const RLE_EXCL_MAX_OUTPUT: usize = 8 * 1024 * 1024;

/// Maximum compressed payload accepted (32 MiB).
pub const RLE_EXCL_MAX_COMP: usize = 32 * 1024 * 1024;

/// Unit width selected by the low two bits of `flags`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RleExclUnit {
    /// `flags == 0`: 1-byte units.
    Byte,
    /// `flags == 1`: 2-byte halfword units (preserved in byte order).
    Halfword,
}

impl RleExclUnit {
    /// Byte length of one unit (`1` or `2`).
    #[must_use]
    pub const fn byte_len(self) -> usize {
        match self {
            Self::Byte => 1,
            Self::Halfword => 2,
        }
    }
}

/// Parsed 16-byte `RLE!` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RleExclHeader {
    /// Expected uncompressed byte length (`u24 LE` at `+0x04`).
    pub out_len: usize,
    /// Compressed stream byte length (`u32 LE` at `+0x08`).
    pub comp_len: usize,
    /// Unit width (`Byte` or `Halfword`).
    pub unit: RleExclUnit,
}

/// Parse and validate the 16-byte `RLE!` header at `input[0..16]`.
pub fn parse_rle_excl_header(input: &[u8]) -> Result<RleExclHeader> {
    if input.len() < RLE_EXCL_HEADER_LEN {
        return Err(Error::Truncated {
            offset: 0,
            needed: RLE_EXCL_HEADER_LEN,
            available: input.len(),
        });
    }
    if &input[..4] != MAGIC_RLE_EXCL {
        return Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_RLE_EXCL),
            found: u32::from_be_bytes([input[0], input[1], input[2], input[3]]),
        });
    }
    if input[7] != 0 {
        return Err(Error::InvalidField {
            what: "RLE! length pad byte",
            value: u64::from(input[7]),
        });
    }
    let out_len = (input[4] as usize) | ((input[5] as usize) << 8) | ((input[6] as usize) << 16);
    if out_len == 0 || out_len > RLE_EXCL_MAX_OUTPUT {
        return Err(Error::InvalidField {
            what: "RLE! out_len",
            value: out_len as u64,
        });
    }
    let comp_len = u32_at(input, 8)? as usize;
    if comp_len == 0 || comp_len > RLE_EXCL_MAX_COMP {
        return Err(Error::InvalidField {
            what: "RLE! comp_len",
            value: comp_len as u64,
        });
    }
    let flags = u32_at(input, 12)?;
    let unit = match flags {
        0 => RleExclUnit::Byte,
        1 => RleExclUnit::Halfword,
        _ => {
            return Err(Error::InvalidField {
                what: "RLE! flags",
                value: u64::from(flags),
            });
        }
    };
    if unit == RleExclUnit::Halfword && (out_len & 1) != 0 {
        return Err(Error::InvalidField {
            what: "RLE! halfword out_len",
            value: out_len as u64,
        });
    }
    let total = RLE_EXCL_HEADER_LEN
        .checked_add(comp_len)
        .ok_or(Error::InvalidField {
            what: "RLE! total length",
            value: comp_len as u64,
        })?;
    if total > input.len() {
        return Err(Error::Truncated {
            offset: RLE_EXCL_HEADER_LEN,
            needed: comp_len,
            available: input.len().saturating_sub(RLE_EXCL_HEADER_LEN),
        });
    }
    Ok(RleExclHeader {
        out_len,
        comp_len,
        unit,
    })
}

/// Returns `true` if `input` starts with a structurally valid `RLE!` stream that decodes cleanly.
pub fn detect_rle_excl(input: &[u8]) -> bool {
    decode_rle_excl(input, RLE_EXCL_MAX_OUTPUT).is_ok()
}

/// Decode an `RLE!` byte or halfword stream starting at `input[0]`.
///
/// Requires exact stream exhaustion (`consumed == 16 + comp_len`) and exact output production.
pub fn decode_rle_excl(input: &[u8], max_output: usize) -> Result<DecodeOutcome> {
    let hdr = parse_rle_excl_header(input)?;
    if hdr.out_len > max_output {
        return Err(Error::InvalidField {
            what: "RLE! max_output limit",
            value: hdr.out_len as u64,
        });
    }

    let unit_len = hdr.unit.byte_len();
    let stream_end = RLE_EXCL_HEADER_LEN + hdr.comp_len;
    let stream = &input[RLE_EXCL_HEADER_LEN..stream_end];
    let mut pos = 0usize;
    let mut out = Vec::with_capacity(hdr.out_len.min(65536));

    while out.len() < hdr.out_len {
        let &flag = stream.get(pos).ok_or(Error::Truncated {
            offset: RLE_EXCL_HEADER_LEN + pos,
            needed: 1,
            available: 0,
        })?;
        pos += 1;
        let count = ((flag & 0x7F) as usize) + 1;
        let produced_bytes = count.checked_mul(unit_len).ok_or(Error::InvalidField {
            what: "RLE! run byte length",
            value: count as u64,
        })?;
        let remaining_out = hdr.out_len - out.len();
        if produced_bytes > remaining_out {
            return Err(Error::InvalidField {
                what: "RLE! run overshoot",
                value: produced_bytes as u64,
            });
        }

        if (flag & 0x80) == 0 {
            let src_end = pos.checked_add(produced_bytes).ok_or(Error::Truncated {
                offset: RLE_EXCL_HEADER_LEN + pos,
                needed: produced_bytes,
                available: stream.len().saturating_sub(pos),
            })?;
            let lit = stream.get(pos..src_end).ok_or(Error::Truncated {
                offset: RLE_EXCL_HEADER_LEN + pos,
                needed: produced_bytes,
                available: stream.len().saturating_sub(pos),
            })?;
            out.extend_from_slice(lit);
            pos = src_end;
        } else {
            let src_end = pos.checked_add(unit_len).ok_or(Error::Truncated {
                offset: RLE_EXCL_HEADER_LEN + pos,
                needed: unit_len,
                available: stream.len().saturating_sub(pos),
            })?;
            let unit_bytes = stream.get(pos..src_end).ok_or(Error::Truncated {
                offset: RLE_EXCL_HEADER_LEN + pos,
                needed: unit_len,
                available: stream.len().saturating_sub(pos),
            })?;
            pos = src_end;
            for _ in 0..count {
                out.extend_from_slice(unit_bytes);
            }
        }
    }

    if pos != stream.len() {
        return Err(Error::InvalidField {
            what: "RLE! unconsumed stream bytes",
            value: (stream.len() - pos) as u64,
        });
    }

    Ok(DecodeOutcome {
        bytes: out,
        consumed: stream_end,
    })
}

fn encode_units(data: &[u8], unit: RleExclUnit) -> Result<Vec<u8>> {
    let unit_len = unit.byte_len();
    if data.is_empty() || data.len() > RLE_EXCL_MAX_OUTPUT || data.len() > 0x00FF_FFFF {
        return Err(Error::InvalidField {
            what: "RLE! encode length",
            value: data.len() as u64,
        });
    }
    if unit == RleExclUnit::Halfword && (data.len() & 1) != 0 {
        return Err(Error::InvalidField {
            what: "RLE! halfword encode length",
            value: data.len() as u64,
        });
    }

    let total_units = data.len() / unit_len;
    let mut stream = Vec::with_capacity(data.len() + 16);
    let mut idx = 0usize;

    while idx < total_units {
        let u0 = &data[idx * unit_len..(idx + 1) * unit_len];
        let mut run = 1usize;
        while idx + run < total_units
            && run < 128
            && &data[(idx + run) * unit_len..(idx + run + 1) * unit_len] == u0
        {
            run += 1;
        }
        if run >= 3 {
            stream.push(0x80 | ((run - 1) as u8));
            stream.extend_from_slice(u0);
            idx += run;
        } else {
            let lit_start = idx;
            idx += 1;
            while idx < total_units && (idx - lit_start) < 128 {
                let cur = &data[idx * unit_len..(idx + 1) * unit_len];
                if idx + 2 < total_units
                    && &data[(idx + 1) * unit_len..(idx + 2) * unit_len] == cur
                    && &data[(idx + 2) * unit_len..(idx + 3) * unit_len] == cur
                {
                    break;
                }
                idx += 1;
            }
            let lit_count = idx - lit_start;
            stream.push((lit_count - 1) as u8);
            stream.extend_from_slice(&data[lit_start * unit_len..idx * unit_len]);
        }
    }

    let comp_len = u32::try_from(stream.len()).map_err(|_| Error::InvalidField {
        what: "RLE! compressed length",
        value: stream.len() as u64,
    })?;
    let mut out = Vec::with_capacity(RLE_EXCL_HEADER_LEN + stream.len());
    out.extend_from_slice(MAGIC_RLE_EXCL);
    out.push((data.len() & 0xFF) as u8);
    out.push(((data.len() >> 8) & 0xFF) as u8);
    out.push(((data.len() >> 16) & 0xFF) as u8);
    out.push(0);
    out.extend_from_slice(&comp_len.to_le_bytes());
    let flags: u32 = match unit {
        RleExclUnit::Byte => 0,
        RleExclUnit::Halfword => 1,
    };
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&stream);
    Ok(out)
}

/// Encode `data` into a valid `RLE!` byte-unit (`flags == 0`) stream.
pub fn encode_rle_excl_byte(data: &[u8]) -> Result<Vec<u8>> {
    encode_units(data, RleExclUnit::Byte)
}

/// Encode even-length `data` into a valid `RLE!` halfword-unit (`flags == 1`) stream.
pub fn encode_rle_excl_halfword(data: &[u8]) -> Result<Vec<u8>> {
    encode_units(data, RleExclUnit::Halfword)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_byte_and_halfword_rle_excl() {
        let mut src = Vec::new();
        src.extend_from_slice(&[0xAA; 10]);
        src.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        src.extend_from_slice(&[0x55; 140]);
        src.extend_from_slice(&[9, 8]);

        let enc_b = encode_rle_excl_byte(&src).unwrap();
        assert!(detect_rle_excl(&enc_b));
        let dec_b = decode_rle_excl(&enc_b, RLE_EXCL_MAX_OUTPUT).unwrap();
        assert_eq!(dec_b.bytes, src);
        assert_eq!(dec_b.consumed, enc_b.len());

        let enc_h = encode_rle_excl_halfword(&src).unwrap();
        assert!(detect_rle_excl(&enc_h));
        let dec_h = decode_rle_excl(&enc_h, RLE_EXCL_MAX_OUTPUT).unwrap();
        assert_eq!(dec_h.bytes, src);
        assert_eq!(dec_h.consumed, enc_h.len());
    }

    #[test]
    fn rejects_malformed_rle_excl() {
        assert!(!detect_rle_excl(b""));
        assert!(!detect_rle_excl(b"RLE!\0\0\0\0\0\0\0\0\0\0\0\0"));
        let mut bad = encode_rle_excl_byte(&[1, 2, 3, 4]).unwrap();
        bad[12] = 2; // invalid variant
        assert!(decode_rle_excl(&bad, RLE_EXCL_MAX_OUTPUT).is_err());
    }
}
