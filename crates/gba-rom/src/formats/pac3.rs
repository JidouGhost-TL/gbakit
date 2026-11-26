//! `PAC3` tagged control-word stream parser, symmetric writer, and ROM scanner.
//!
//! Layout (little-endian):
//! - `+0x00`: `b"PAC3"`
//! - `+0x04`: `version` (`4`, or `0x8000_0000 | n` where `n <= 16`)
//! - `+0x08`: `flags` (`0xB000_0000 | n` where `n <= 16`, or `<= 0xFFFF`)
//! - `+0x0C`: `count` (`<= 0x10000`, or `0xA000_0000 | n` where `n <= 16`)
//! - `+0x10`: `u32 LE` control-word stream containing at least one high-bit tagged control word
//!   (`word & 0x8000_0000 != 0`), typically terminated by `(0xFFFF_FFFF, 0x0000_0000)`.

use formatkit_core::{u32_at, Error, Result};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_PAC3};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_PAC3: &[u8; 4] = b"PAC3";
const HEADER_LEN: usize = 16;
const MAX_LEN: usize = 8 * 1024 * 1024;
const ROM_SCAN_MAX_WORDS: usize = 16384;

/// Parsed `studio.pac3` tagged control-word stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pac3Asset {
    pub version: u32,
    pub flags: u32,
    pub count: u32,
    pub tagged_word_count: usize,
    pub words: Vec<u32>,
    pub consumed_end: usize,
}

impl Pac3Asset {
    /// Serialize this stream back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Fast, panic-free structural detector for `studio.pac3` streams.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse a `PAC3` tagged control-word stream from the start of `data`.
pub fn parse(data: &[u8]) -> Result<Pac3Asset> {
    parse_inner(data, false)
}

fn valid_version(v: u32) -> bool {
    v == 4 || ((v & 0x8000_0000) != 0 && (v & 0x7FFF_FFFF) <= 16)
}

fn valid_flags(w: u32) -> bool {
    ((w & 0xF000_0000) == 0xB000_0000 && (w & 0x0FFF_FFFF) <= 16) || w <= 0xFFFF
}

fn valid_count(w: u32) -> bool {
    w <= 0x1_0000 || ((w & 0xF000_0000) == 0xA000_0000 && (w & 0x0FFF_FFFF) <= 16)
}

fn parse_inner(data: &[u8], rom_scan_mode: bool) -> Result<Pac3Asset> {
    if data.len() > MAX_LEN {
        return Err(Error::InvalidField {
            what: "PAC3 size limit",
            value: data.len() as u64,
        });
    }
    if data.len() < HEADER_LEN {
        return Err(Error::Truncated {
            offset: 0,
            needed: HEADER_LEN,
            available: data.len(),
        });
    }
    if &data[..4] != MAGIC_PAC3 {
        let found = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        return Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_PAC3),
            found,
        });
    }
    let version = u32_at(data, 4)?;
    let flags = u32_at(data, 8)?;
    let count = u32_at(data, 12)?;
    if !valid_version(version) || !valid_flags(flags) || !valid_count(count) {
        return Err(Error::InvalidField {
            what: "PAC3 header fields",
            value: u64::from(version),
        });
    }

    let body = &data[HEADER_LEN..];
    if body.len() < 4 {
        return Err(Error::Truncated {
            offset: HEADER_LEN,
            needed: 4,
            available: body.len(),
        });
    }

    let max_words = if rom_scan_mode {
        (body.len() / 4).min(ROM_SCAN_MAX_WORDS)
    } else {
        body.len() / 4
    };
    let mut words = Vec::new();
    let mut tagged_word_count = 0usize;
    let mut saw_terminator = false;
    let mut i = 0usize;
    while i < max_words {
        let w = u32_at(body, i * 4)?;
        if (w & 0x8000_0000) != 0 {
            tagged_word_count += 1;
        }
        words.push(w);
        i += 1;
        if w == 0xFFFF_FFFF && i < max_words {
            let next_w = u32_at(body, i * 4)?;
            if next_w == 0 {
                words.push(next_w);
                saw_terminator = true;
                break;
            }
        }
    }

    if tagged_word_count == 0 {
        return Err(Error::InvalidField {
            what: "PAC3 tagged word count",
            value: 0,
        });
    }
    if rom_scan_mode && !saw_terminator {
        return Err(Error::InvalidField {
            what: "PAC3 ROM scan terminator",
            value: 0,
        });
    }

    let consumed_end = HEADER_LEN
        .checked_add(words.len().checked_mul(4).ok_or(Error::InvalidField {
            what: "PAC3 body bytes",
            value: words.len() as u64,
        })?)
        .ok_or(Error::InvalidField {
            what: "PAC3 consumed_end",
            value: words.len() as u64,
        })?;

    Ok(Pac3Asset {
        version,
        flags,
        count,
        tagged_word_count,
        words,
        consumed_end,
    })
}

/// Serialize a parsed `studio.pac3` asset back into its exact binary representation.
pub fn pack(asset: &Pac3Asset) -> Result<Vec<u8>> {
    if !valid_version(asset.version)
        || !valid_flags(asset.flags)
        || !valid_count(asset.count)
        || asset.words.is_empty()
    {
        return Err(Error::InvalidField {
            what: "PAC3 pack validation",
            value: u64::from(asset.version),
        });
    }
    let body_bytes = asset
        .words
        .len()
        .checked_mul(4)
        .ok_or(Error::InvalidField {
            what: "PAC3 body bytes",
            value: asset.words.len() as u64,
        })?;
    let mut out = Vec::with_capacity(HEADER_LEN.saturating_add(body_bytes));
    out.extend_from_slice(MAGIC_PAC3);
    out.extend_from_slice(&asset.version.to_le_bytes());
    out.extend_from_slice(&asset.flags.to_le_bytes());
    out.extend_from_slice(&asset.count.to_le_bytes());
    for &w in &asset.words {
        out.extend_from_slice(&w.to_le_bytes());
    }
    Ok(out)
}

/// Scan `rom` on 4-byte aligned offsets for verified `studio.pac3` streams.
pub fn scan_pac3(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective = &rom[..rom.len().min(MAX_ROM_SIZE)];
    if effective.len() < HEADER_SIZE + HEADER_LEN + 8 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + HEADER_LEN + 8 <= effective.len() {
        if &effective[off..off + 4] == MAGIC_PAC3 {
            if let Ok(asset) = parse_inner(&effective[off..], true) {
                let size = asset.consumed_end;
                if size > 0 && off.checked_add(size).is_some_and(|e| e <= effective.len()) {
                    found.push(DetectedFormatExtent {
                        format_id: FORMAT_ID_PAC3,
                        subkind: "pac3",
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

    pub(crate) fn make_valid_pac3() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC_PAC3);
        out.extend_from_slice(&4u32.to_le_bytes());
        out.extend_from_slice(&0xB000_0001u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        for w in [0x8000_0001u32, 0u32, 0xFFFF_FFFFu32, 0u32] {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out
    }

    #[test]
    fn parses_and_roundtrips_pac3_byte_identically() {
        let mut data = make_valid_pac3();
        let exact_len = data.len();
        data.extend_from_slice(&[0xAA; 16]); // trailing ROM bytes after terminator
        assert!(detect(&data));
        let parsed = parse(&data).unwrap();
        assert_eq!(parsed.tagged_word_count, 2);
        assert_eq!(parsed.consumed_end, exact_len);
        assert_eq!(parsed.to_bytes().unwrap(), data[..exact_len]);

        // High-bit version variant without terminator
        let mut alt = Vec::new();
        alt.extend_from_slice(MAGIC_PAC3);
        alt.extend_from_slice(&0x8000_0006u32.to_le_bytes());
        alt.extend_from_slice(&0x485u32.to_le_bytes());
        alt.extend_from_slice(&0xA000_0009u32.to_le_bytes());
        for w in [0x9000_0000u32, 1, 2, 3] {
            alt.extend_from_slice(&w.to_le_bytes());
        }
        let parsed_alt = parse(&alt).unwrap();
        assert_eq!(parsed_alt.consumed_end, alt.len());
        assert_eq!(parsed_alt.to_bytes().unwrap(), alt);
    }

    #[test]
    fn rejects_malformed_truncated_and_random_inputs() {
        assert!(!detect(&[]));
        assert!(parse(&[]).is_err());
        let good = make_valid_pac3();
        assert!(!detect(&good[..15]));
        assert!(parse(&good[..16]).is_err());

        let mut bad_ver = good.clone();
        bad_ver[4] = 5;
        assert!(!detect(&bad_ver));
        assert!(parse(&bad_ver).is_err());

        let mut no_tags = good[..16].to_vec();
        no_tags.extend_from_slice(&[1, 0, 0, 0, 2, 0, 0, 0]);
        assert!(!detect(&no_tags));
        assert!(parse(&no_tags).is_err());

        let mut seed = 0x6BEF_1234u64;
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
