//! Bounded, consumed-extent-tracking GBA BIOS and proprietary compression codecs.
//!
//! Supports GBA BIOS hardware SWI formats:
//! - `BitUnPack` (SWI `0x10`) — bit-depth unpacking
//! - LZ77 (`0x10`, SWI `0x11` / `0x12`) — decode & encode
//! - Huffman 4-bit / 8-bit (`0x24` / `0x28`, SWI `0x13`) — decode & encode
//! - RLE (`0x30`, SWI `0x14` / `0x15`) — decode & encode
//! - Delta filters (`0x81` Diff8bit / `0x82` Diff16bit, SWI `0x16`–`0x18`) — decode & encode
//!
//! Supports GBA proprietary and extended codecs:
//! - Capcom / Nintendo Extended LZ11 (`0x11`) — decode & encode
//! - Hudson Soft Variant LZ (`0x48` / `HLZ\x01`) — decode & encode
//! - Konami Dictionary / Huffman (`0x4B`) — decode & encode
//! - Shin'en Mode 7 Affine Delta RLE (`0x53`) — decode & encode
//!
//! The BIOS formats are thin GBA-named wrappers over FormatKit's platform-neutral
//! tagged-header codecs (`formatkit_codec::{tagged_lz77, tagged_huffman, rle, delta,
//! bit_unpack}`); this crate keeps the SWI-oriented API and GBA-specific choices such
//! as the VRAM-safe (`disp >= 2`) LZ77 encoder.

#![deny(unsafe_code)]

use std::sync::OnceLock;

use formatkit_codec::bit_unpack::BitUnpackConfig;
use formatkit_codec::delta::{
    decode_tagged_delta, delta_encode_u8_into, encode_tagged_delta, TaggedDeltaWidth,
};
use formatkit_codec::okumura::BitOrder;
use formatkit_codec::rle::{
    decode_high_bit_rle, decode_tagged_rle, encode_high_bit_rle, encode_tagged_rle,
    HighBitRleConfig,
};
use formatkit_codec::tagged_huffman::{
    decode_tagged_huffman, encode_tagged_huffman, TaggedHuffmanWidth,
};
use formatkit_codec::tagged_lz77::{
    decode_tagged_lz77, decode_tagged_lz77_extended, encode_tagged_lz77,
    encode_tagged_lz77_extended, encode_tagged_lz77_with_min_distance,
};
use formatkit_codec::DecodeOutcome;
use formatkit_core::{Error, Result};

/// Default maximum decoded output size (4 MiB) to guard against malformed headers.
pub const DEFAULT_MAX_OUTPUT: usize = 4 * 1024 * 1024;

pub use formatkit_codec::DecodeOutcome as StreamOutcome;

/// Minimum LZ77 back-reference distance that is safe for `SWI 0x12` (VRAM), which
/// writes 16-bit units and therefore cannot copy from the byte just written.
const VRAM_MIN_DISP: usize = 2;

fn trunc(offset: usize, needed: usize, total: usize) -> Error {
    Error::Truncated {
        offset,
        needed,
        available: total.saturating_sub(offset),
    }
}

/// Decode a GBA BIOS LZ77 (`0x10`, `SWI 0x11` / `0x12`) stream starting at `input[0]`.
pub fn decode_lz77(input: &[u8], max_output: usize) -> Result<DecodeOutcome> {
    decode_tagged_lz77(input, max_output)
}

/// Encode bytes into a valid GBA BIOS LZ77 (`0x10`) stream suitable for WRAM (`SWI 0x11`, `disp >= 1`).
pub fn encode_lz77(data: &[u8]) -> Result<Vec<u8>> {
    encode_tagged_lz77(data)
}

/// Encode bytes into a VRAM-safe GBA BIOS LZ77 (`0x10`) stream (`SWI 0x12`, enforcing `disp >= 2`).
pub fn encode_lz77_vram(data: &[u8]) -> Result<Vec<u8>> {
    encode_tagged_lz77_with_min_distance(data, VRAM_MIN_DISP)
}

/// Decode a Capcom / Nintendo Extended LZ11 (`0x11`) stream starting at `input[0]`.
pub fn decode_lz11(input: &[u8], max_output: usize) -> Result<DecodeOutcome> {
    decode_tagged_lz77_extended(input, max_output)
}

/// Encode bytes into a Capcom / Nintendo Extended LZ11 (`0x11`) stream.
pub fn encode_lz11(data: &[u8]) -> Result<Vec<u8>> {
    encode_tagged_lz77_extended(data)
}

/// Decode a GBA BIOS RLE (`0x30`, `SWI 0x14` / `0x15`) stream starting at `input[0]`.
pub fn decode_rle(input: &[u8], max_output: usize) -> Result<DecodeOutcome> {
    decode_tagged_rle(input, max_output)
}

/// Encode bytes into a canonical GBA BIOS RLE (`0x30`) stream.
pub fn encode_rle(data: &[u8]) -> Result<Vec<u8>> {
    encode_tagged_rle(data)
}

/// Decode a GBA BIOS Huffman 4-bit (`0x24`) or 8-bit (`0x28`) stream (`SWI 0x13`) starting at `input[0]`.
pub fn decode_huffman(input: &[u8], max_output: usize) -> Result<DecodeOutcome> {
    decode_tagged_huffman(input, max_output)
}

/// Encode bytes into a GBA BIOS 4-bit Huffman (`0x24`, `SWI 0x13`) stream.
pub fn encode_huffman_4bit(data: &[u8]) -> Result<Vec<u8>> {
    encode_tagged_huffman(data, TaggedHuffmanWidth::Bits4)
}

/// Encode bytes into a GBA BIOS 8-bit Huffman (`0x28`, `SWI 0x13`) stream.
pub fn encode_huffman_8bit(data: &[u8]) -> Result<Vec<u8>> {
    encode_tagged_huffman(data, TaggedHuffmanWidth::Bits8)
}

/// Configuration for GBA BIOS `BitUnPack` (`SWI 0x10`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitUnPackConfig {
    /// Source unit bit width (`1`, `2`, `4`, or `8`).
    pub src_bpp: u8,
    /// Destination unit bit width (`1`, `2`, `4`, `8`, `16`, or `32`), must be `>= src_bpp`.
    pub dst_bpp: u8,
    /// Offset added to each unpacked non-zero unit (and to zero units when `zero_data_flag` is set).
    pub data_offset: u32,
    /// When `true`, `data_offset` is also added to zero source units.
    pub zero_data_flag: bool,
}

impl From<BitUnPackConfig> for BitUnpackConfig {
    fn from(cfg: BitUnPackConfig) -> Self {
        Self {
            src_bits: cfg.src_bpp,
            dst_bits: cfg.dst_bpp,
            offset: cfg.data_offset,
            offset_zero: cfg.zero_data_flag,
        }
    }
}

/// Unpack a bitstream using GBA BIOS `BitUnPack` (`SWI 0x10`) semantics.
///
/// Extracts `cfg.src_bpp`-bit units LSB-first from each input byte, adds `cfg.data_offset`
/// when `(val != 0 || cfg.zero_data_flag)`, and packs the results into little-endian
/// `cfg.dst_bpp`-bit units.
pub fn bit_unpack(input: &[u8], cfg: BitUnPackConfig, max_output: usize) -> Result<Vec<u8>> {
    formatkit_codec::bit_unpack::bit_unpack(input, cfg.into(), max_output)
}

/// Decode a GBA BIOS delta-filtered stream (`0x81` Diff8bit or `0x82` Diff16bit, `SWI 0x16`–`0x18`).
pub fn decode_diff_filter(input: &[u8], max_output: usize) -> Result<DecodeOutcome> {
    decode_tagged_delta(input, max_output)
}

/// Encode bytes into a GBA BIOS 8-bit delta-filtered (`0x81`, `SWI 0x16` / `0x17`) stream.
pub fn encode_diff8(data: &[u8]) -> Result<Vec<u8>> {
    encode_tagged_delta(data, TaggedDeltaWidth::Bits8)
}

/// Encode even-length bytes into a GBA BIOS 16-bit LE delta-filtered (`0x82`, `SWI 0x18`) stream.
pub fn encode_diff16(data: &[u8]) -> Result<Vec<u8>> {
    encode_tagged_delta(data, TaggedDeltaWidth::Bits16)
}

/// Decode a Hudson Soft variant LZ (`0x48` / `HLZ\x01`) stream starting at `input[0]`.
pub fn decode_hudson_lz(input: &[u8], max_output: usize) -> Result<DecodeOutcome> {
    if input.len() < 4 {
        return Err(trunc(0, 4, input.len()));
    }
    let (expected, header_size) = if input.len() >= 8 && &input[..4] == b"HLZ\x01" {
        let sz = u32::from_le_bytes([input[4], input[5], input[6], input[7]]) as usize;
        (sz, 8)
    } else if input[0] == 0x48 {
        let sz = (input[1] as usize) | ((input[2] as usize) << 8) | ((input[3] as usize) << 16);
        (sz, 4)
    } else {
        return Err(Error::BadMagic {
            offset: 0,
            expected: 0x48,
            found: input[0] as u32,
        });
    };

    if expected == 0 {
        return Err(Error::InvalidField {
            what: "hudson_lz_decoded_size",
            value: 0,
        });
    }
    if expected > max_output {
        return Err(Error::ResourceLimit {
            resource: "hudson_lz_decoded_bytes",
            requested: expected as u64,
            limit: max_output as u64,
        });
    }

    match formatkit_codec::okumura_lzss_consumed_with_limit(
        &input[header_size..],
        expected,
        BitOrder::LsbFirst,
        0x20,
        max_output,
    ) {
        Ok(outcome) => Ok(DecodeOutcome {
            bytes: outcome.bytes,
            consumed: header_size + outcome.consumed,
        }),
        Err(formatkit_codec::okumura::OkumuraDecodeError::OutputLimit { requested, limit }) => {
            Err(Error::ResourceLimit {
                resource: "hudson_lz_decoded_bytes",
                requested: requested as u64,
                limit: limit as u64,
            })
        }
        Err(formatkit_codec::okumura::OkumuraDecodeError::Truncated { offset }) => {
            Err(trunc(header_size + offset, 1, input.len()))
        }
        Err(formatkit_codec::okumura::OkumuraDecodeError::TokenOvershoot {
            produced,
            token_len,
            expected,
        }) => Err(Error::Malformed(format!(
            "Hudson LZ token overshoot: produced {produced} + token {token_len} > expected {expected}"
        ))),
        Err(formatkit_codec::okumura::OkumuraDecodeError::InvalidInitialCursor {
            initial_cursor,
        }) => Err(Error::InvalidField {
            what: "hudson_lz_initial_cursor",
            value: initial_cursor as u64,
        }),
    }
}

/// Encode bytes into a Hudson Soft variant LZ (`HLZ\x01`) stream.
pub fn encode_hudson_lz(data: &[u8]) -> Result<Vec<u8>> {
    if data.is_empty() {
        return Err(Error::InvalidField {
            what: "hudson_lz_input_size",
            value: 0,
        });
    }
    let len_u32 = u32::try_from(data.len()).map_err(|_| Error::InvalidField {
        what: "hudson_lz_input_size",
        value: data.len() as u64,
    })?;
    let body = formatkit_codec::okumura_lzss_compress(data, BitOrder::LsbFirst, 0x20);
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(b"HLZ\x01");
    out.extend_from_slice(&len_u32.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Canonical Konami 256-word tile pattern dictionary.
pub const KONAMI_DICT: [[u8; 2]; 256] = generate_konami_dict();

const fn generate_konami_dict() -> [[u8; 2]; 256] {
    let mut dict = [[0u8; 2]; 256];
    dict[0] = [0x00, 0x00];
    let mut i = 1;
    while i < 16 {
        let b = (i as u8) | ((i as u8) << 4);
        dict[i] = [b, b];
        i += 1;
    }
    while i < 256 {
        dict[i] = [(i as u8).wrapping_mul(17), (i as u8).wrapping_mul(31)];
        i += 1;
    }
    dict
}

/// Decode a Konami Dictionary / Huffman (`0x4B`) stream starting at `input[0]`.
pub fn decode_konami_dict(input: &[u8], max_output: usize) -> Result<DecodeOutcome> {
    if input.len() < 4 {
        return Err(trunc(0, 4, input.len()));
    }
    if input[0] != 0x4B {
        return Err(Error::BadMagic {
            offset: 0,
            expected: 0x4B,
            found: input[0] as u32,
        });
    }
    let expected = (input[1] as usize) | ((input[2] as usize) << 8) | ((input[3] as usize) << 16);
    if expected == 0 {
        return Err(Error::InvalidField {
            what: "konami_dict_decoded_size",
            value: 0,
        });
    }
    if expected > max_output {
        return Err(Error::ResourceLimit {
            resource: "konami_dict_decoded_bytes",
            requested: expected as u64,
            limit: max_output as u64,
        });
    }

    let initial_cap = expected.min(input.len().saturating_mul(16));
    let mut out = Vec::with_capacity(initial_cap);
    let mut pos = 4;

    while out.len() < expected {
        if pos >= input.len() {
            return Err(trunc(pos, 1, input.len()));
        }
        let flags = input[pos];
        pos += 1;

        for bit in (0..8).rev() {
            if out.len() >= expected {
                break;
            }
            if (flags & (1 << bit)) == 0 {
                // Literal byte
                if pos >= input.len() {
                    return Err(trunc(pos, 1, input.len()));
                }
                out.push(input[pos]);
                pos += 1;
            } else {
                // Match token (2 bytes)
                if pos + 2 > input.len() {
                    return Err(trunc(pos, 2, input.len()));
                }
                let b1 = input[pos] as usize;
                let b2 = input[pos + 1] as usize;
                pos += 2;

                if b1 == 0x00 {
                    // Dictionary lookup
                    let pair = KONAMI_DICT[b2];
                    out.push(pair[0]);
                    if out.len() < expected {
                        out.push(pair[1]);
                    }
                } else {
                    // Sliding window match
                    let length = (b1 >> 4) + 3;
                    let distance = (((b1 & 0x0F) << 8) | b2) + 1;
                    let count = length.min(expected - out.len());
                    formatkit_codec::sliding_window::append_match(
                        &mut out,
                        &[],
                        distance,
                        count,
                        expected,
                    )
                    .map_err(|_| {
                        Error::Malformed(format!(
                            "Konami Dict backreference out of bounds: distance {distance} > output length {}",
                            out.len()
                        ))
                    })?;
                }
            }
        }
    }

    Ok(DecodeOutcome {
        bytes: out,
        consumed: pos,
    })
}

fn konami_dict_lookup() -> &'static [u16] {
    static LOOKUP: OnceLock<Vec<u16>> = OnceLock::new();
    LOOKUP.get_or_init(|| {
        let mut table = vec![u16::MAX; 65_536];
        for (idx, &[b0, b1]) in KONAMI_DICT.iter().enumerate() {
            let key = ((b0 as usize) << 8) | (b1 as usize);
            if table[key] == u16::MAX {
                table[key] = idx as u16;
            }
        }
        table
    })
}

/// Encode bytes into a Konami Dictionary / Huffman (`0x4B`) stream.
pub fn encode_konami_dict(data: &[u8]) -> Result<Vec<u8>> {
    if data.is_empty() || data.len() > 0x00FF_FFFF {
        return Err(Error::InvalidField {
            what: "konami_dict_input_size",
            value: data.len() as u64,
        });
    }
    let dict_lookup = konami_dict_lookup();
    let mut out = Vec::with_capacity(data.len() + 4);
    out.push(0x4B);
    out.push((data.len() & 0xFF) as u8);
    out.push(((data.len() >> 8) & 0xFF) as u8);
    out.push(((data.len() >> 16) & 0xFF) as u8);

    let mut idx = 0usize;
    while idx < data.len() {
        let flag_pos = out.len();
        out.push(0);
        let mut flags = 0u8;

        for bit in 0..8 {
            if idx >= data.len() {
                break;
            }

            // Check sliding window match first
            let sw_match = formatkit_codec::find_longest_match(data, idx, 1, 4096, 3, 18);
            if let Some((disp, len)) = sw_match {
                let d = disp - 1;
                let l = len - 3;
                let b1 = ((l << 4) | (d >> 8)) as u8;
                // b1 == 0x00 is reserved for dictionary match
                if b1 != 0x00 {
                    flags |= 0x80 >> bit;
                    out.push(b1);
                    out.push((d & 0xFF) as u8);
                    idx += len;
                    continue;
                }
            }

            // Check dictionary match for 2 bytes in O(1)
            if idx + 1 < data.len() {
                let key = ((data[idx] as usize) << 8) | (data[idx + 1] as usize);
                let pos = dict_lookup[key];
                if pos != u16::MAX {
                    flags |= 0x80 >> bit;
                    out.push(0x00);
                    out.push(pos as u8);
                    idx += 2;
                    continue;
                }
            }

            // Literal byte
            out.push(data[idx]);
            idx += 1;
        }
        out[flag_pos] = flags;
    }

    Ok(out)
}

const SHINEN_RLE_CONFIG: HighBitRleConfig = HighBitRleConfig {
    unit_bytes: 1,
    min_run_len: 2,
    min_literal_len: 1,
};

/// Decode a Shin'en Mode 7 / Affine Delta RLE (`0x53`) stream starting at `input[0]`.
pub fn decode_shinen_mode7(input: &[u8], max_output: usize) -> Result<DecodeOutcome> {
    if input.len() < 4 {
        return Err(trunc(0, 4, input.len()));
    }
    if input[0] != 0x53 {
        return Err(Error::BadMagic {
            offset: 0,
            expected: 0x53,
            found: input[0] as u32,
        });
    }
    let expected = (input[1] as usize) | ((input[2] as usize) << 8) | ((input[3] as usize) << 16);
    if expected == 0 {
        return Err(Error::InvalidField {
            what: "shinen_mode7_decoded_size",
            value: 0,
        });
    }
    if expected > max_output {
        return Err(Error::ResourceLimit {
            resource: "shinen_mode7_decoded_bytes",
            requested: expected as u64,
            limit: max_output as u64,
        });
    }

    let mut outcome = decode_high_bit_rle(&input[4..], expected, SHINEN_RLE_CONFIG).map_err(
        |err| match err {
            Error::Truncated {
                offset,
                needed,
                available,
            } => Error::Truncated {
                offset: offset + 4,
                needed,
                available,
            },
            other => other,
        },
    )?;

    let mut acc = 0u8;
    for b in &mut outcome.bytes {
        acc = acc.wrapping_add(*b);
        *b = acc;
    }

    Ok(DecodeOutcome {
        bytes: outcome.bytes,
        consumed: 4 + outcome.consumed,
    })
}

/// Encode bytes into a Shin'en Mode 7 / Affine Delta RLE (`0x53`) stream.
pub fn encode_shinen_mode7(data: &[u8]) -> Result<Vec<u8>> {
    if data.is_empty() || data.len() > 0x00FF_FFFF {
        return Err(Error::InvalidField {
            what: "shinen_mode7_input_size",
            value: data.len() as u64,
        });
    }
    let mut deltas = Vec::with_capacity(data.len());
    delta_encode_u8_into(data, &mut deltas);
    let body = encode_high_bit_rle(&deltas, SHINEN_RLE_CONFIG)?;

    let mut out = Vec::with_capacity(4 + body.len());
    out.push(0x53);
    out.push((data.len() & 0xFF) as u8);
    out.push(((data.len() >> 8) & 0xFF) as u8);
    out.push(((data.len() >> 16) & 0xFF) as u8);
    out.extend_from_slice(&body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use formatkit_core::MsbInterleavedBitReader;

    #[test]
    fn lz77_round_trip_and_extent() {
        let mut src = Vec::new();
        for _ in 0..16 {
            src.extend_from_slice(b"GBA_LZ77_PATTERN_");
        }
        let mut encoded = encode_lz77(&src).unwrap();
        let exact_len = encoded.len();
        encoded.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        let dec = decode_lz77(&encoded, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec.bytes, src);
        assert_eq!(dec.consumed, exact_len);
    }

    #[test]
    fn rle_round_trip_and_extent() {
        let mut src = vec![0xAA; 64];
        src.extend_from_slice(b"HELLO_GBA");
        src.extend(std::iter::repeat_n(0x00, 32));
        let mut encoded = encode_rle(&src).unwrap();
        let exact_len = encoded.len();
        encoded.extend_from_slice(&[0xFF, 0xFF]);

        let dec = decode_rle(&encoded, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec.bytes, src);
        assert_eq!(dec.consumed, exact_len);
    }

    #[test]
    fn rejects_invalid_lz77_backref() {
        let bad = [0x10, 0x04, 0x00, 0x00, 0x80, 0x00, 0x04];
        assert!(decode_lz77(&bad, 1024).is_err());
    }

    #[test]
    fn lz77_independent_overlapping_and_clipped_vector() {
        // Header: type=0x10, decoded_size=6
        // Flags: 0x40 (bit 0 = literal 'A', bit 1 = backref len=7 (encoded 4+3) disp=1 (encoded 0))
        // Clipped at decoded_size=6 -> "AAAAAA", exact consumed = 8 bytes (before trailing 0x99).
        let stream = [0x10, 0x06, 0x00, 0x00, 0x40, b'A', 0x40, 0x00, 0x99];
        let dec = decode_lz77(&stream, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec.bytes, b"AAAAAA");
        assert_eq!(dec.consumed, 8);
    }

    #[test]
    fn lz77_vram_encoder_enforces_min_disp_2() {
        let src = vec![0x5A; 32];
        let vram_stream = encode_lz77_vram(&src).unwrap();
        // Verify round-trip AND that no backref in the stream has disp == 1 (encoded d == 0).
        let dec = decode_lz77(&vram_stream, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec.bytes, src);
        assert_eq!(dec.consumed, vram_stream.len());

        let mut r = MsbInterleavedBitReader::at(&vram_stream, 4).unwrap();
        let mut produced = 0usize;
        while produced < src.len() {
            if !r.read_bit().unwrap() {
                r.read_byte().unwrap();
                produced += 1;
            } else {
                let pair = r.read_bytes(2).unwrap();
                let len = (pair[0] as usize >> 4) + 3;
                let disp = (((pair[0] as usize & 0x0F) << 8) | pair[1] as usize) + 1;
                assert!(disp >= 2, "SWI 0x12 VRAM stream must not emit disp=1");
                produced += len;
            }
        }
    }

    #[test]
    fn rle_independent_vector_and_extent() {
        // Header: 0x30, decoded_size=7
        // Compressed run: 0x81 -> (1 + 3 = 4) x 0x42 ('B')
        // Uncompressed run: 0x02 -> (2 + 1 = 3) bytes: "XYZ"
        let stream = [0x30, 0x07, 0x00, 0x00, 0x81, b'B', 0x02, b'X', b'Y', b'Z', 0xEE];
        let dec = decode_rle(&stream, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec.bytes, b"BBBBXYZ");
        assert_eq!(dec.consumed, 10);
    }

    #[test]
    fn huffman_8bit_and_4bit_independent_vectors() {
        // 8-bit Huffman (0x28): decoded_size=4, tree_size=(1+1)*2=4 bytes:
        //   tree[0] = 0x01 (header size byte)
        //   tree[1] = 0xC0 (root: both left & right children are data leaves, offset=0 -> indices 2 & 3)
        //   tree[2] = b'A' (bit 0)
        //   tree[3] = b'B' (bit 1)
        // Bitstream word (u32 LE, read MSB first): 0b0101_0000... = 0x5000_0000 -> "ABAB"
        let h8 = [
            0x28, 0x04, 0x00, 0x00, 0x01, 0xC0, b'A', b'B', 0x00, 0x00, 0x00, 0x50, 0xFF,
        ];
        let dec8 = decode_huffman(&h8, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec8.bytes, b"ABAB");
        assert_eq!(dec8.consumed, 12);

        // 4-bit Huffman (0x24): decoded_size=2 (4 nibbles), same 2-leaf tree:
        //   bit 0 -> nibble 0x3, bit 1 -> nibble 0xA
        //   bits 0, 1, 1, 0 = 0x6000_0000 -> bytes [0xA3, 0x3A] (low nibble first)
        let h4 = [
            0x24, 0x02, 0x00, 0x00, 0x01, 0xC0, 0x03, 0x0A, 0x00, 0x00, 0x00, 0x60, 0xFF,
        ];
        let dec4 = decode_huffman(&h4, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec4.bytes, [0xA3, 0x3A]);
        assert_eq!(dec4.consumed, 12);
    }

    #[test]
    fn truncated_huge_header_fails_without_overallocating() {
        // Claims 4 MiB output in 5 bytes — must fail cleanly with Truncated.
        let bad_lz = [0x10, 0x00, 0x00, 0x40, 0x00];
        assert!(decode_lz77(&bad_lz, DEFAULT_MAX_OUTPUT).is_err());
        let bad_rle = [0x30, 0x00, 0x00, 0x40, 0x80];
        assert!(decode_rle(&bad_rle, DEFAULT_MAX_OUTPUT).is_err());
        let bad_diff = [0x81, 0x00, 0x00, 0x40, 0x00];
        assert!(decode_diff_filter(&bad_diff, DEFAULT_MAX_OUTPUT).is_err());
    }

    #[test]
    fn bit_unpack_1bpp_to_4bpp_with_offset_and_zero_flag() {
        // 0b1010_0101 LSB-first bits: 1, 0, 1, 0, 0, 1, 0, 1
        let input = [0b1010_0101u8];

        // zero_data_flag = false: 0 stays 0, 1 becomes 1 + 2 = 3
        let out_skip_zero = bit_unpack(
            &input,
            BitUnPackConfig {
                src_bpp: 1,
                dst_bpp: 4,
                data_offset: 2,
                zero_data_flag: false,
            },
            DEFAULT_MAX_OUTPUT,
        )
        .unwrap();
        assert_eq!(out_skip_zero, [0x03, 0x03, 0x30, 0x30]);

        // zero_data_flag = true: 0 becomes 2, 1 becomes 3
        let out_add_zero = bit_unpack(
            &input,
            BitUnPackConfig {
                src_bpp: 1,
                dst_bpp: 4,
                data_offset: 2,
                zero_data_flag: true,
            },
            DEFAULT_MAX_OUTPUT,
        )
        .unwrap();
        assert_eq!(out_add_zero, [0x23, 0x23, 0x32, 0x32]);
    }

    #[test]
    fn bit_unpack_various_widths_and_le_packing() {
        // 2bpp -> 8bpp: byte 0b11_10_01_00 -> units [0, 1, 2, 3], offset 10, skip zero -> [0, 11, 12, 13]
        let out_8 = bit_unpack(
            &[0b11_10_01_00],
            BitUnPackConfig {
                src_bpp: 2,
                dst_bpp: 8,
                data_offset: 10,
                zero_data_flag: false,
            },
            DEFAULT_MAX_OUTPUT,
        )
        .unwrap();
        assert_eq!(out_8, [0, 11, 12, 13]);

        // 4bpp -> 16bpp LE: byte 0xA0 -> units [0, 0xA], offset 0x1000, zero_data_flag = true
        let out_16 = bit_unpack(
            &[0xA0],
            BitUnPackConfig {
                src_bpp: 4,
                dst_bpp: 16,
                data_offset: 0x1000,
                zero_data_flag: true,
            },
            DEFAULT_MAX_OUTPUT,
        )
        .unwrap();
        assert_eq!(out_16, [0x00, 0x10, 0x0A, 0x10]);

        // 8bpp -> 32bpp LE: [0x00, 0x05], offset 0x0102_0300, skip zero
        let out_32 = bit_unpack(
            &[0x00, 0x05],
            BitUnPackConfig {
                src_bpp: 8,
                dst_bpp: 32,
                data_offset: 0x0102_0300,
                zero_data_flag: false,
            },
            DEFAULT_MAX_OUTPUT,
        )
        .unwrap();
        assert_eq!(out_32, [0x00, 0x00, 0x00, 0x00, 0x05, 0x03, 0x02, 0x01]);

        // 1bpp -> 2bpp: 0b0000_1010 -> bits 0,1,0,1,0,0,0,0 -> 2bpp [0b01_00_01_00, 0x00] = [0x44, 0x00]
        let out_2 = bit_unpack(
            &[0b0000_1010],
            BitUnPackConfig {
                src_bpp: 1,
                dst_bpp: 2,
                data_offset: 0,
                zero_data_flag: false,
            },
            DEFAULT_MAX_OUTPUT,
        )
        .unwrap();
        assert_eq!(out_2, [0x44, 0x00]);

        // Empty input produces empty output.
        let out_empty = bit_unpack(
            &[],
            BitUnPackConfig {
                src_bpp: 1,
                dst_bpp: 4,
                data_offset: 0,
                zero_data_flag: false,
            },
            DEFAULT_MAX_OUTPUT,
        )
        .unwrap();
        assert!(out_empty.is_empty());
    }

    #[test]
    fn bit_unpack_validation_and_bounds() {
        let base = BitUnPackConfig {
            src_bpp: 1,
            dst_bpp: 4,
            data_offset: 0,
            zero_data_flag: false,
        };
        // Invalid src_bpp
        assert!(bit_unpack(&[0], BitUnPackConfig { src_bpp: 3, ..base }, 1024).is_err());
        assert!(bit_unpack(&[0], BitUnPackConfig { src_bpp: 16, ..base }, 1024).is_err());
        // Invalid dst_bpp
        assert!(bit_unpack(&[0], BitUnPackConfig { dst_bpp: 3, ..base }, 1024).is_err());
        assert!(bit_unpack(&[0], BitUnPackConfig { dst_bpp: 64, ..base }, 1024).is_err());
        // dst_bpp < src_bpp
        assert!(bit_unpack(
            &[0],
            BitUnPackConfig {
                src_bpp: 4,
                dst_bpp: 2,
                ..base
            },
            1024,
        )
        .is_err());
        // Exceeds max_output (2 bytes * 4 = 8 > 7)
        assert!(bit_unpack(&[0, 0], base, 7).is_err());
    }

    #[test]
    fn diff8_and_diff16_independent_vectors_and_extent() {
        // Diff8 (0x81): decoded_size=5, deltas=[10, 5, 251 (-5), 0, 246 (-10)]
        // Expected: [10, 15, 10, 10, 0], consumed = 9 (before trailing 0xEE).
        let d8 = [0x81, 0x05, 0x00, 0x00, 10, 5, 251, 0, 246, 0xEE];
        let dec8 = decode_diff_filter(&d8, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec8.bytes, [10, 15, 10, 10, 0]);
        assert_eq!(dec8.consumed, 9);

        // Diff16 (0x82): decoded_size=6 (3 LE u16s)
        // deltas: 0x0100, 0x0050, 0xFFFE (-2)
        // Expected u16s: 0x0100, 0x0150, 0x014E -> [0x00, 0x01, 0x50, 0x01, 0x4E, 0x01], consumed = 10.
        let d16 = [
            0x82, 0x06, 0x00, 0x00, 0x00, 0x01, 0x50, 0x00, 0xFE, 0xFF, 0xDD,
        ];
        let dec16 = decode_diff_filter(&d16, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec16.bytes, [0x00, 0x01, 0x50, 0x01, 0x4E, 0x01]);
        assert_eq!(dec16.consumed, 10);
    }

    #[test]
    fn diff8_and_diff16_round_trip_and_errors() {
        let src: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(37).wrapping_add(11)).collect();
        let mut enc8 = encode_diff8(&src).unwrap();
        let exact8 = enc8.len();
        enc8.extend_from_slice(&[0xAA, 0xBB]);
        let dec8 = decode_diff_filter(&enc8, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec8.bytes, src);
        assert_eq!(dec8.consumed, exact8);

        let mut enc16 = encode_diff16(&src).unwrap();
        let exact16 = enc16.len();
        enc16.extend_from_slice(&[0xCC, 0xDD]);
        let dec16 = decode_diff_filter(&enc16, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec16.bytes, src);
        assert_eq!(dec16.consumed, exact16);

        // Error cases
        assert!(encode_diff8(&[]).is_err());
        assert!(encode_diff16(&[]).is_err());
        assert!(encode_diff16(&[1, 2, 3]).is_err()); // odd length
        assert!(decode_diff_filter(&[0x82, 0x03, 0x00, 0x00, 1, 2, 3], 1024).is_err()); // odd diff16 header
        assert!(decode_diff_filter(&[0x80, 0x02, 0x00, 0x00, 1, 2], 1024).is_err()); // bad magic
        assert!(decode_diff_filter(&[0x81, 0x00, 0x00, 0x00], 1024).is_err()); // zero size
        assert!(decode_diff_filter(&[0x81, 0x04, 0x00, 0x00, 1, 2], 1024).is_err()); // truncated
        assert!(decode_diff_filter(&[0x81, 0x04, 0x00, 0x00, 1, 2, 3, 4], 3).is_err()); // max_output
    }

    #[test]
    fn huffman_4bit_round_trips_edge_cases_and_full_alphabet() {
        assert!(encode_huffman_4bit(&[]).is_err());

        // 1-symbol (all nibbles identical: 0x77 -> nibbles 7, 7)
        let single = vec![0x77u8; 31];
        // 2-symbol (nibbles 0x2 and 0xE)
        let two_sym = vec![0xE2u8; 25];
        // Skewed frequencies
        let mut skewed = Vec::new();
        for nib in 0..16u8 {
            let count = (1usize << (nib.min(8))) + 1;
            for _ in 0..count {
                skewed.push((nib << 4) | (15 - nib));
            }
        }
        // Full 16-symbol uniform alphabet
        let full_uniform: Vec<u8> = (0..=255u8).collect();

        for case in [&single, &two_sym, &skewed, &full_uniform] {
            let mut enc = encode_huffman_4bit(case).unwrap();
            let exact = enc.len();
            assert_eq!(enc[0], 0x24);
            enc.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

            let dec = decode_huffman(&enc, DEFAULT_MAX_OUTPUT).unwrap();
            assert_eq!(dec.bytes, *case);
            assert_eq!(dec.consumed, exact);
        }
    }

    #[test]
    fn huffman_8bit_round_trips_edge_cases_and_full_256_alphabet() {
        assert!(encode_huffman_8bit(&[]).is_err());

        // 1-symbol input
        let single = vec![0x5Au8; 43];
        // 2-symbol input
        let two_sym: Vec<u8> = (0..50).map(|i| if i % 3 == 0 { 0x11 } else { 0xEE }).collect();
        // Full 256-symbol uniform alphabet (balanced 256-leaf tree: stresses 6-bit relative offset layout)
        let full_uniform: Vec<u8> = (0..=255u8).collect();
        // Full 256-symbol skewed alphabet
        let mut full_skewed = Vec::new();
        for sym in 0..=255u8 {
            let count = if sym < 8 {
                (sym as usize + 1) * 40
            } else {
                (sym as usize % 5) + 1
            };
            full_skewed.extend(std::iter::repeat_n(sym, count));
        }
        // Pseudo-random 4 KiB buffer covering all 256 byte values
        let mut state = 0x1234_5678_9ABC_DEF0u64;
        let mut prng_buf = Vec::with_capacity(4096);
        for b in 0..=255u8 {
            prng_buf.push(b);
        }
        while prng_buf.len() < 4096 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            prng_buf.push((state >> 56) as u8);
        }

        for case in [&single, &two_sym, &full_uniform, &full_skewed, &prng_buf] {
            let mut enc = encode_huffman_8bit(case).unwrap();
            let exact = enc.len();
            assert_eq!(enc[0], 0x28);
            enc.extend_from_slice(&[0xCA, 0xFE, 0xBA, 0xBE]);

            let dec = decode_huffman(&enc, DEFAULT_MAX_OUTPUT).unwrap();
            assert_eq!(dec.bytes, *case);
            assert_eq!(dec.consumed, exact);
        }
    }

    #[test]
    fn lz11_round_trip_modes_and_extent() {
        // Short match (< 18 bytes), medium match (18..272 bytes), and long match (> 272 bytes)
        let mut src = Vec::new();
        // Short runs
        for _ in 0..10 {
            src.extend_from_slice(b"SHORT_MATCH_1234");
        }
        // Medium run (100 identical bytes)
        src.extend_from_slice(&[0x42; 100]);
        // Long run (1000 identical bytes)
        src.extend_from_slice(&[0x99; 1000]);

        let mut enc = encode_lz11(&src).unwrap();
        let exact = enc.len();
        assert_eq!(enc[0], 0x11);
        enc.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);

        let dec = decode_lz11(&enc, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec.bytes, src);
        assert_eq!(dec.consumed, exact);

        // Boundary tests for match lengths 16, 17, and 18
        for match_len in [16, 17, 18] {
            let mut test_src = vec![0x5Au8; 20];
            test_src.extend_from_slice(&vec![0x7Bu8; match_len]);
            test_src.extend_from_slice(&vec![0x7Bu8; match_len]);
            let test_enc = encode_lz11(&test_src).unwrap();
            let test_dec = decode_lz11(&test_enc, DEFAULT_MAX_OUTPUT).unwrap();
            assert_eq!(test_dec.bytes, test_src, "failed at match_len {match_len}");
        }

        // Error cases
        assert!(encode_lz11(&[]).is_err());
        assert!(decode_lz11(&[0x10, 0x04, 0x00, 0x00], 1024).is_err()); // bad magic
        assert!(decode_lz11(&[0x11, 0x00, 0x00, 0x00], 1024).is_err()); // zero size
        assert!(decode_lz11(&[0x11, 0x10, 0x00, 0x00, 0x00], 1024).is_err()); // truncated
        assert!(decode_lz11(&enc, 100).is_err()); // resource limit exceeded
    }

    #[test]
    fn hudson_lz_round_trip_and_extent() {
        let mut src = Vec::new();
        for _ in 0..20 {
            src.extend_from_slice(b"HUDSON_BOMBERMAN_LZ_TEST_PATTERN_");
        }
        let mut enc = encode_hudson_lz(&src).unwrap();
        let exact = enc.len();
        assert!(enc.starts_with(b"HLZ\x01"));
        enc.extend_from_slice(&[0x12, 0x34, 0x56, 0x78]);

        let dec = decode_hudson_lz(&enc, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec.bytes, src);
        assert_eq!(dec.consumed, exact);

        // Also test 4-byte 0x48 header format
        let mut enc_4byte = vec![
            0x48,
            (src.len() & 0xFF) as u8,
            ((src.len() >> 8) & 0xFF) as u8,
            ((src.len() >> 16) & 0xFF) as u8,
        ];
        enc_4byte.extend_from_slice(&enc[8..exact]);
        let dec_4byte = decode_hudson_lz(&enc_4byte, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec_4byte.bytes, src);
        assert_eq!(dec_4byte.consumed, enc_4byte.len());

        // Error cases
        assert!(encode_hudson_lz(&[]).is_err());
        assert!(decode_hudson_lz(&[0x00, 0x04, 0x00, 0x00], 1024).is_err()); // bad magic
        assert!(decode_hudson_lz(&[0x48, 0x00, 0x00, 0x00], 1024).is_err()); // zero size
        assert!(decode_hudson_lz(&enc, 50).is_err()); // resource limit
    }

    #[test]
    fn konami_dict_round_trip_and_extent() {
        let mut src = Vec::new();
        // Include common dictionary patterns: zeroes, 0x11, 0xFF, etc.
        for _ in 0..16 {
            src.extend_from_slice(&[0x00, 0x00, 0x11, 0x11, 0xFF, 0xFF, 0x22, 0x22]);
            src.extend_from_slice(b"CASTLEVANIA_HOD_");
        }

        let mut enc = encode_konami_dict(&src).unwrap();
        let exact = enc.len();
        assert_eq!(enc[0], 0x4B);
        enc.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

        let dec = decode_konami_dict(&enc, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec.bytes, src);
        assert_eq!(dec.consumed, exact);

        // Error cases
        assert!(encode_konami_dict(&[]).is_err());
        assert!(decode_konami_dict(&[0x4C, 0x04, 0x00, 0x00], 1024).is_err()); // bad magic
        assert!(decode_konami_dict(&[0x4B, 0x00, 0x00, 0x00], 1024).is_err()); // zero size
        assert!(decode_konami_dict(&enc, 50).is_err()); // resource limit
    }

    #[test]
    fn shinen_mode7_round_trip_and_extent() {
        let mut src = Vec::new();
        // Linear ramp (constant delta = 1, testing repeat run)
        for i in 0..128u8 {
            src.push(i);
        }
        // Identical values (constant delta = 0, testing repeat run)
        src.extend_from_slice(&[128u8; 64]);
        // Arbitrary affine transformation bytes
        for i in 0..100u8 {
            src.push(i.wrapping_mul(7).wrapping_add(13));
        }

        let mut enc = encode_shinen_mode7(&src).unwrap();
        let exact = enc.len();
        assert_eq!(enc[0], 0x53);
        enc.extend_from_slice(&[0x99, 0x88, 0x77, 0x66]);

        let dec = decode_shinen_mode7(&enc, DEFAULT_MAX_OUTPUT).unwrap();
        assert_eq!(dec.bytes, src);
        assert_eq!(dec.consumed, exact);

        // Error cases
        assert!(encode_shinen_mode7(&[]).is_err());
        assert!(decode_shinen_mode7(&[0x54, 0x04, 0x00, 0x00], 1024).is_err()); // bad magic
        assert!(decode_shinen_mode7(&[0x53, 0x00, 0x00, 0x00], 1024).is_err()); // zero size
        assert!(decode_shinen_mode7(&enc, 50).is_err()); // resource limit
    }
}
