//! SGGA LZ + canonical-Huffman / Elias-gamma compression codec.
//!
//! Layout (little-endian):
//! - `+0x00`: `uncompressed_size` (`u32 LE`)
//! - `+0x04`: `mode` (`0x00` = raw copy at `+0x05`, `0x01` = compressed)
//! - `+0x05`: `bitstream_rel_offset` (`u32 LE`, present only when `mode == 1`, distance from `+0x09`)
//! - `+0x09`: 3 nibble-packed canonical Huffman code-length tables (literal run, match len - 2,
//!   match dist - 1; low nibble `0` disables a table and selects unary-1 Elias-gamma coding),
//!   followed by raw literal bytes up to `9 + bitstream_rel_offset`, then the MSB-first bitstream.

use formatkit_codec::DecodeOutcome;
use formatkit_core::{u32_at, Error, Result};

/// Maximum uncompressed payload accepted by the SGGA codec (1 MiB).
pub const SGGA_MAX_OUTPUT: usize = 1024 * 1024;

const MAX_SYMBOLS: usize = 17;
const MAX_CODE_LEN: usize = 15;

/// Parsed 5-byte (raw) or 9-byte (compressed) SGGA stream header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SggaHeader {
    /// Expected uncompressed byte count (`u32 LE` at `+0x00`).
    pub uncompressed_size: u32,
    /// Compression mode (`0x00` = raw copy, `0x01` = LZ + Huffman/Elias-gamma).
    pub mode: u8,
    /// Relative offset from `+0x09` to the MSB-first bitstream (`None` if `mode == 0`).
    pub bitstream_rel_offset: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
struct HuffTable {
    enabled: bool,
    count: usize,
    max_len: u8,
    lengths: [u8; MAX_SYMBOLS],
    codes: [u16; MAX_SYMBOLS],
}

impl HuffTable {
    const fn disabled() -> Self {
        Self {
            enabled: false,
            count: 0,
            max_len: 0,
            lengths: [0; MAX_SYMBOLS],
            codes: [0; MAX_SYMBOLS],
        }
    }

    fn decode_from(data: &[u8], pos: &mut usize) -> Result<Self> {
        let &first = data.get(*pos).ok_or(Error::Truncated {
            offset: *pos,
            needed: 1,
            available: 0,
        })?;
        *pos += 1;
        let n_low = (first & 0x0F) as usize;
        if n_low == 0 {
            return Ok(Self::disabled());
        }
        let count = n_low + 2;
        let mut lengths = [0u8; MAX_SYMBOLS];
        let mut cur = first;
        let mut idx = 0usize;
        while idx < count {
            lengths[idx] = (cur >> 4) & 0x0F;
            idx += 1;
            if idx == count {
                break;
            }
            cur = *data.get(*pos).ok_or(Error::Truncated {
                offset: *pos,
                needed: 1,
                available: 0,
            })?;
            *pos += 1;
            lengths[idx] = cur & 0x0F;
            idx += 1;
        }

        let mut max_len = 0u8;
        let mut kraft_num = 0u32;
        let mut nonzero = 0usize;
        for &l in &lengths[..count] {
            if l > max_len {
                max_len = l;
            }
            if l > 0 {
                nonzero += 1;
                kraft_num = kraft_num.saturating_add(1u32 << (MAX_CODE_LEN as u8 - l));
            }
        }
        if max_len == 0 || nonzero < 2 || kraft_num != (1u32 << MAX_CODE_LEN) {
            return Err(Error::InvalidField {
                what: "SGGA canonical Huffman table",
                value: u64::from(kraft_num),
            });
        }

        let mut codes = [0u16; MAX_SYMBOLS];
        let mut code = 0u32;
        for bit_len in 1..=max_len {
            for sym in 0..count {
                if lengths[sym] == bit_len {
                    codes[sym] = code as u16;
                    code += 1;
                }
            }
            code <<= 1;
        }

        Ok(Self {
            enabled: true,
            count,
            max_len,
            lengths,
            codes,
        })
    }
}

struct MsbBitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bitbuf: u8,
    bits_left: u8,
}

impl<'a> MsbBitReader<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Self {
            data,
            pos,
            bitbuf: 0,
            bits_left: 0,
        }
    }

    #[inline]
    fn read_bit(&mut self) -> Result<u16> {
        if self.bits_left == 0 {
            let &b = self.data.get(self.pos).ok_or(Error::Truncated {
                offset: self.pos,
                needed: 1,
                available: 0,
            })?;
            self.bitbuf = b;
            self.pos += 1;
            self.bits_left = 8;
        }
        self.bits_left -= 1;
        Ok(u16::from((self.bitbuf >> self.bits_left) & 1))
    }

    fn read_bits(&mut self, n: u8) -> Result<usize> {
        let mut val = 0usize;
        for _ in 0..n {
            val = (val << 1) | (self.read_bit()? as usize);
        }
        Ok(val)
    }

    fn read_symbol(&mut self, table: &HuffTable) -> Result<usize> {
        if !table.enabled {
            let mut k = 0u8;
            while self.read_bit()? != 0 {
                k = k.checked_add(1).ok_or(Error::InvalidField {
                    what: "SGGA Elias-gamma prefix",
                    value: u64::from(k),
                })?;
                if k > 15 {
                    return Err(Error::InvalidField {
                        what: "SGGA Elias-gamma prefix",
                        value: u64::from(k),
                    });
                }
            }
            if k == 0 {
                return Ok(0);
            }
            let extra = self.read_bits(k)?;
            return Ok((1usize << k) - 1 + extra);
        }

        let mut code = 0u16;
        for bit_len in 1..=table.max_len {
            code = (code << 1) | self.read_bit()?;
            for sym in 0..table.count {
                if table.lengths[sym] == bit_len && table.codes[sym] == code {
                    if sym < 2 {
                        return Ok(sym);
                    }
                    let extra_bits = (sym - 1) as u8;
                    let extra = self.read_bits(extra_bits)?;
                    return Ok((1usize << extra_bits) + extra);
                }
            }
        }
        Err(Error::InvalidField {
            what: "SGGA Huffman code",
            value: u64::from(code),
        })
    }
}

/// Parse the 5-byte (raw) or 9-byte (compressed) SGGA header.
pub fn parse_sgga_header(data: &[u8]) -> Result<SggaHeader> {
    if data.len() < 5 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 5,
            available: data.len(),
        });
    }
    let uncompressed_size = u32_at(data, 0)?;
    if uncompressed_size == 0 || (uncompressed_size as usize) > SGGA_MAX_OUTPUT {
        return Err(Error::InvalidField {
            what: "SGGA uncompressed_size",
            value: u64::from(uncompressed_size),
        });
    }
    let mode = data[4];
    match mode {
        0 => Ok(SggaHeader {
            uncompressed_size,
            mode,
            bitstream_rel_offset: None,
        }),
        1 => {
            if data.len() < 9 {
                return Err(Error::Truncated {
                    offset: 5,
                    needed: 4,
                    available: data.len().saturating_sub(5),
                });
            }
            let rel = u32_at(data, 5)?;
            if (rel as usize) > data.len().saturating_sub(9) {
                return Err(Error::InvalidField {
                    what: "SGGA bitstream_rel_offset",
                    value: u64::from(rel),
                });
            }
            Ok(SggaHeader {
                uncompressed_size,
                mode,
                bitstream_rel_offset: Some(rel),
            })
        }
        other => Err(Error::InvalidField {
            what: "SGGA mode",
            value: u64::from(other),
        }),
    }
}

/// Returns `true` if `data` starts with a structurally valid compressed SGGA (`mode == 1`) stream.
///
/// Raw (`mode == 0`) streams carry no magic or tree invariants and are excluded from standalone
/// detection to maintain a 0% false-positive rate on arbitrary binary buffers.
pub fn detect_sgga(data: &[u8]) -> bool {
    let Ok(hdr) = parse_sgga_header(data) else {
        return false;
    };
    if hdr.mode != 1 {
        return false;
    }
    decode_sgga(data, SGGA_MAX_OUTPUT).is_ok()
}

/// Decompress an SGGA stream (`mode == 0` raw or `mode == 1` compressed).
pub fn decode_sgga(data: &[u8], max_output: usize) -> Result<DecodeOutcome> {
    let hdr = parse_sgga_header(data)?;
    let target = hdr.uncompressed_size as usize;
    if target > max_output {
        return Err(Error::InvalidField {
            what: "SGGA max_output limit",
            value: target as u64,
        });
    }

    if hdr.mode == 0 {
        let end = 5usize.checked_add(target).ok_or(Error::Truncated {
            offset: 5,
            needed: target,
            available: data.len().saturating_sub(5),
        })?;
        let slice = data.get(5..end).ok_or(Error::Truncated {
            offset: 5,
            needed: target,
            available: data.len().saturating_sub(5),
        })?;
        return Ok(DecodeOutcome {
            bytes: slice.to_vec(),
            consumed: end,
        });
    }

    let bs_rel = hdr.bitstream_rel_offset.ok_or(Error::InvalidField {
        what: "SGGA bitstream_rel_offset",
        value: 0,
    })? as usize;
    let bs_start = 9usize.checked_add(bs_rel).ok_or(Error::InvalidField {
        what: "SGGA bitstream_start",
        value: bs_rel as u64,
    })?;
    if bs_start >= data.len() {
        return Err(Error::InvalidField {
            what: "SGGA bitstream_start",
            value: bs_start as u64,
        });
    }

    let mut pos = 9usize;
    let t0 = HuffTable::decode_from(data, &mut pos)?;
    let t1 = HuffTable::decode_from(data, &mut pos)?;
    let t2 = HuffTable::decode_from(data, &mut pos)?;
    if pos > bs_start {
        return Err(Error::InvalidField {
            what: "SGGA table end past bitstream",
            value: pos as u64,
        });
    }

    let mut lit_pos = pos;
    let mut br = MsbBitReader::new(data, bs_start);
    let mut out = Vec::with_capacity(target.min(65536));

    while out.len() < target {
        let lit_len = br.read_symbol(&t0)?;
        if lit_len > 0 {
            if lit_len > target - out.len() {
                return Err(Error::InvalidField {
                    what: "SGGA literal run overshoot",
                    value: lit_len as u64,
                });
            }
            let lit_end = lit_pos.checked_add(lit_len).ok_or(Error::InvalidField {
                what: "SGGA literal end",
                value: lit_len as u64,
            })?;
            if lit_end > bs_start {
                return Err(Error::InvalidField {
                    what: "SGGA literal stream overrun",
                    value: lit_end as u64,
                });
            }
            out.extend_from_slice(&data[lit_pos..lit_end]);
            lit_pos = lit_end;
        }
        if out.len() >= target {
            break;
        }
        let match_len = br.read_symbol(&t1)? + 2;
        let match_dist = br.read_symbol(&t2)? + 1;
        if match_dist > out.len() || match_len > target - out.len() {
            return Err(Error::InvalidField {
                what: "SGGA match bounds",
                value: match_dist as u64,
            });
        }
        for _ in 0..match_len {
            let b = out[out.len() - match_dist];
            out.push(b);
        }
    }

    if lit_pos != bs_start {
        return Err(Error::InvalidField {
            what: "SGGA unconsumed literal bytes",
            value: (bs_start - lit_pos) as u64,
        });
    }

    Ok(DecodeOutcome {
        bytes: out,
        consumed: br.pos,
    })
}

/// Encode `data` as an uncompressed (`mode == 0`) SGGA stream.
pub fn encode_sgga_raw(data: &[u8]) -> Result<Vec<u8>> {
    if data.is_empty() || data.len() > SGGA_MAX_OUTPUT {
        return Err(Error::InvalidField {
            what: "SGGA raw encode length",
            value: data.len() as u64,
        });
    }
    let len_u32 = u32::try_from(data.len()).map_err(|_| Error::InvalidField {
        what: "SGGA raw encode length",
        value: data.len() as u64,
    })?;
    let mut out = Vec::with_capacity(5 + data.len());
    out.extend_from_slice(&len_u32.to_le_bytes());
    out.push(0);
    out.extend_from_slice(data);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_raw_sgga_and_decodes_synthetic_elias_gamma_stream() {
        let payload = b"HELLO_SGGA_WORLD";
        let raw = encode_sgga_raw(payload).unwrap();
        let dec = decode_sgga(&raw, SGGA_MAX_OUTPUT).unwrap();
        assert_eq!(dec.bytes, payload);
        assert_eq!(dec.consumed, raw.len());
        assert!(!detect_sgga(&raw)); // mode 0 not claimed by standalone detect

        // Synthetic compressed mode=1 stream with all 3 tables disabled (Elias-gamma):
        // Output: b"ABABAB" (6 bytes) -> lit_len=2 ("AB"), match_len=4 (sym=2), match_dist=2 (sym=1)
        // Elias-gamma:
        //   2 = (1<<1)-1 + 1 -> k=1: "1 0 1" (3 bits)
        //   2 = "1 0 1" (3 bits)
        //   1 = (1<<1)-1 + 0 -> k=1: "1 0 0" (3 bits)
        // Bits: 101_101_10 0_0000000 -> 0xB6, 0x00
        let mut comp = Vec::new();
        comp.extend_from_slice(&6u32.to_le_bytes());
        comp.push(1); // mode = 1
        comp.extend_from_slice(&5u32.to_le_bytes()); // 3 table bytes + 2 literal bytes = 5
        comp.extend_from_slice(&[0x00, 0x00, 0x00]); // 3 disabled tables
        comp.extend_from_slice(b"AB"); // literals
        comp.extend_from_slice(&[0xB6, 0x00]); // bitstream

        assert!(detect_sgga(&comp));
        let dec_c = decode_sgga(&comp, SGGA_MAX_OUTPUT).unwrap();
        assert_eq!(dec_c.bytes, b"ABABAB");
        assert_eq!(dec_c.consumed, comp.len());
    }
}
