//! Declarative Game Boy Advance save-format engine.
//!
//! Supports JSON-defined GBA save layouts across SRAM, Flash (512K/1M), and
//! EEPROM (512B/8K) media, with magic-signature checks, standard additive,
//! XOR, and CRC-16 checksum verification/repair, and typed field decoding
//! and mutation.

#![deny(unsafe_code)]

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use formatkit_core::checksum::{Crc16, Crc16Params, FoldChecksum, FoldOp, FoldUnit};
pub use formatkit_core::padding::{classify_padding, PaddingKind, PaddingSummary};
pub use formatkit_layout::edit::{patch_bytes, patched_bytes};
use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

/// Maximum accepted save image size (1 MiB) to prevent OOM allocations from crafted definitions.
pub const MAX_SAVE_SIZE: usize = 1024 * 1024;

/// Returns `true` if `bytes` consists entirely of uniform unwritten/blank memory
/// (either all `0x00` as in cleared SRAM, or all `0xFF` as in erased Flash/EEPROM),
/// of at least `min_bytes` length.
pub fn is_blank_save(bytes: &[u8], min_bytes: usize) -> bool {
    if let Some(summary) = classify_padding(bytes, min_bytes) {
        matches!(summary.kind, PaddingKind::AllZero | PaddingKind::RepeatedByte(0xFF))
    } else {
        false
    }
}

/// Classify the uniform padding pattern of save memory `bytes`, if any.
pub fn classify_save_padding(bytes: &[u8], min_bytes: usize) -> Option<PaddingSummary> {
    classify_padding(bytes, min_bytes)
}

/// Game Boy Advance backup save media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveMediaKind {
    Sram,
    Flash512,
    Flash1m,
    Eeprom512,
    Eeprom8k,
}

impl SaveMediaKind {
    /// Standard media capacity in bytes.
    pub const fn default_size(self) -> usize {
        match self {
            Self::Sram => 32_768,
            Self::Flash512 => 65_536,
            Self::Flash1m => 131_072,
            Self::Eeprom512 => 512,
            Self::Eeprom8k => 8_192,
        }
    }

    /// Canonical lowercase identifier string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sram => "sram",
            Self::Flash512 => "flash512",
            Self::Flash1m => "flash1m",
            Self::Eeprom512 => "eeprom512",
            Self::Eeprom8k => "eeprom8k",
        }
    }

    /// Look up the standard [`SaveMediaKind`] matching `size` bytes, if any.
    pub const fn from_size(size: usize) -> Option<Self> {
        match size {
            512 => Some(Self::Eeprom512),
            8_192 => Some(Self::Eeprom8k),
            32_768 => Some(Self::Sram),
            65_536 => Some(Self::Flash512),
            131_072 => Some(Self::Flash1m),
            _ => None,
        }
    }
}

impl fmt::Display for SaveMediaKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Backing storage geometry information for a GBA save image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveGeometry {
    pub media: SaveMediaKind,
    pub raw_size: usize,
    pub detected_canonical_size: usize,
    pub has_trailing_padding: bool,
    pub padding_byte: Option<u8>,
}

impl SaveGeometry {
    /// Construct geometry from known canonical capacity and media kind.
    pub fn for_canonical_size(bytes: &[u8], media: SaveMediaKind, canonical_size: usize) -> Self {
        let raw_size = bytes.len();
        let (has_trailing_padding, padding_byte) = if raw_size > canonical_size {
            let tail = &bytes[canonical_size..];
            if tail.iter().all(|&b| b == 0xFF) {
                (true, Some(0xFF))
            } else if tail.iter().all(|&b| b == 0x00) {
                (true, Some(0x00))
            } else {
                (false, None)
            }
        } else {
            (false, None)
        };

        Self {
            media,
            raw_size,
            detected_canonical_size: canonical_size,
            has_trailing_padding,
            padding_byte,
        }
    }

    /// Construct geometry for `bytes` relative to a known [`SaveDefinition`].
    pub fn for_definition(bytes: &[u8], def: &SaveDefinition) -> Self {
        Self::for_canonical_size(bytes, def.media, def.size_bytes)
    }

    /// Heuristically detect geometry of an arbitrary save image without a definition.
    pub fn detect(bytes: &[u8]) -> Self {
        const CANDIDATES: &[(usize, SaveMediaKind)] = &[
            (512, SaveMediaKind::Eeprom512),
            (8_192, SaveMediaKind::Eeprom8k),
            (32_768, SaveMediaKind::Sram),
            (65_536, SaveMediaKind::Flash512),
            (131_072, SaveMediaKind::Flash1m),
        ];

        let raw_size = bytes.len();
        if raw_size == 0 {
            return Self {
                media: SaveMediaKind::Eeprom512,
                raw_size: 0,
                detected_canonical_size: 512,
                has_trailing_padding: false,
                padding_byte: None,
            };
        }

        // Check each canonical capacity to see if trailing bytes past that capacity
        // are uniform emulator padding (0xFF or 0x00), while the preceding payload
        // contains actual non-padding data.
        for &(cand_size, cand_media) in CANDIDATES {
            if raw_size > cand_size {
                let tail = &bytes[cand_size..];
                let head = &bytes[..cand_size];
                if tail.iter().all(|&b| b == 0xFF) && head.iter().any(|&b| b != 0xFF) {
                    return Self {
                        media: cand_media,
                        raw_size,
                        detected_canonical_size: cand_size,
                        has_trailing_padding: true,
                        padding_byte: Some(0xFF),
                    };
                }
                if tail.iter().all(|&b| b == 0x00) && head.iter().any(|&b| b != 0x00) {
                    return Self {
                        media: cand_media,
                        raw_size,
                        detected_canonical_size: cand_size,
                        has_trailing_padding: true,
                        padding_byte: Some(0x00),
                    };
                }
            }
        }

        // If no smaller payload with trailing padding matched, check if raw_size matches
        // an exact canonical size directly.
        if let Some(media) = SaveMediaKind::from_size(raw_size) {
            return Self {
                media,
                raw_size,
                detected_canonical_size: raw_size,
                has_trailing_padding: false,
                padding_byte: None,
            };
        }

        // For non-standard sizes, pick the closest canonical capacity <= raw_size
        // or default to Eeprom512.
        let (best_size, best_media) = CANDIDATES
            .iter()
            .rev()
            .find(|&&(sz, _)| sz <= raw_size)
            .copied()
            .unwrap_or((512, SaveMediaKind::Eeprom512));

        Self::for_canonical_size(bytes, best_media, best_size)
    }
}

/// Supported standard checksum algorithms for GBA save files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksumAlgorithm {
    /// 8-bit wrapping byte sum (`sum8`).
    Sum8,
    /// One's complement of 8-bit wrapping byte sum (`~sum8`).
    Sum8Inv,
    /// Two's complement of 8-bit wrapping byte sum (`-sum8`).
    Sum8Neg,
    /// 16-bit wrapping byte sum, stored little-endian.
    Sum16Le,
    /// 16-bit wrapping byte sum, stored big-endian.
    Sum16Be,
    /// One's complement of 16-bit wrapping byte sum, stored little-endian.
    Sum16LeInv,
    /// 16-bit wrapping sum of little-endian `u16` words, stored little-endian.
    Sum16LeWords,
    /// 16-bit wrapping sum of big-endian `u16` words, stored big-endian.
    Sum16BeWords,
    /// Two's complement of 16-bit wrapping sum of little-endian `u16` words, stored little-endian.
    Sum16LeNeg,
    /// Two's complement of 16-bit wrapping sum of big-endian `u16` words, stored big-endian.
    Sum16BeNeg,
    /// One's complement of 16-bit wrapping sum of big-endian `u16` words, stored big-endian.
    Sum16BeInv,
    /// 32-bit wrapping byte sum, stored little-endian.
    Sum32Le,
    /// 32-bit wrapping byte sum, stored big-endian.
    Sum32Be,
    /// 32-bit wrapping sum of little-endian `u32` words, stored little-endian.
    Sum32LeWords,
    /// Two's complement of 32-bit wrapping sum of little-endian `u32` words, stored little-endian.
    Sum32LeNeg,
    /// 8-bit byte-wise XOR.
    Xor8,
    /// 16-bit word-wise little-endian XOR, stored little-endian.
    Xor16Le,
    /// 32-bit word-wise little-endian XOR, stored little-endian.
    Xor32Le,
    /// 16-bit wrapping sum plus XOR of little-endian `u16` words (`(sum + xor) & 0xFFFF`), stored little-endian.
    SumXor16Le,
    /// 32-bit rotate-left-1-and-add byte hash (`h = h.rotate_left(1).wrapping_add(b)`), stored little-endian.
    Rol32AddLe,
    /// CRC-16/ARC (`poly=0xA001` reflected), stored little-endian.
    Crc16Arc,
    /// CRC-16/KERMIT (`poly=0x8408` reflected), stored little-endian.
    Crc16Kermit,
    /// CRC-16/X-25 (`poly=0x8408` reflected, init `0xFFFF`, xorout `0xFFFF`), stored little-endian.
    Crc16X25,
    /// CRC-16/BUYPASS (`poly=0x8005` MSB-first), stored little-endian.
    Crc16Buypass,
    /// CRC-16/XMODEM (`poly=0x1021` MSB-first, init `0x0000`, xorout `0x0000`), stored little-endian.
    Crc16Xmodem,
    /// Nintendo GBA Classic NES Series / Famicom Mini reflected CRC-16 (`poly=0xD0A2`,
    /// `init=0x0000`) over little-endian `u32` words with one trailing zero word,
    /// stored little-endian.
    Crc16NesGba,
}

impl ChecksumAlgorithm {
    /// Width of the stored checksum field in bytes (1, 2, or 4).
    pub const fn width(self) -> usize {
        match self {
            Self::Sum8 | Self::Sum8Inv | Self::Sum8Neg | Self::Xor8 => 1,
            Self::Sum16Le
            | Self::Sum16Be
            | Self::Sum16LeInv
            | Self::Sum16LeWords
            | Self::Sum16BeWords
            | Self::Sum16LeNeg
            | Self::Sum16BeNeg
            | Self::Sum16BeInv
            | Self::Xor16Le
            | Self::SumXor16Le
            | Self::Crc16Arc
            | Self::Crc16Kermit
            | Self::Crc16X25
            | Self::Crc16Buypass
            | Self::Crc16Xmodem
            | Self::Crc16NesGba => 2,
            Self::Sum32Le
            | Self::Sum32Be
            | Self::Sum32LeWords
            | Self::Sum32LeNeg
            | Self::Xor32Le
            | Self::Rol32AddLe => 4,
        }
    }

    /// Compute the checksum over `bytes` with optional `seed` and `xor_out`.
    pub fn compute(self, bytes: &[u8], seed: Option<u32>, xor_out: Option<u32>) -> u32 {
        match self {
            Self::Sum8 => {
                let acc = formatkit_core::checksum::sum_bytes_u8(bytes, seed.unwrap_or(0) as u8);
                u32::from(acc ^ (xor_out.unwrap_or(0) as u8))
            }
            Self::Sum8Inv => {
                let acc = formatkit_core::checksum::sum_bytes_u8(bytes, seed.unwrap_or(0) as u8);
                u32::from((!acc) ^ (xor_out.unwrap_or(0) as u8))
            }
            Self::Sum8Neg => {
                let acc = formatkit_core::checksum::sum_bytes_u8(bytes, seed.unwrap_or(0) as u8);
                u32::from(0u8.wrapping_sub(acc) ^ (xor_out.unwrap_or(0) as u8))
            }
            Self::Sum16Le | Self::Sum16Be => {
                let acc = formatkit_core::checksum::sum_bytes_u16(bytes, seed.unwrap_or(0) as u16);
                u32::from(acc ^ (xor_out.unwrap_or(0) as u16))
            }
            Self::Sum16LeInv => {
                let acc = formatkit_core::checksum::sum_bytes_u16(bytes, seed.unwrap_or(0) as u16);
                u32::from((!acc) ^ (xor_out.unwrap_or(0) as u16))
            }
            Self::Sum16LeWords | Self::Sum16LeNeg => {
                let mut acc =
                    formatkit_core::checksum::sum_u16_le_words(bytes, seed.unwrap_or(0) as u16);
                if let Some(&last) = bytes.as_chunks::<2>().1.first() {
                    acc = acc.wrapping_add(u16::from(last));
                }
                let val = if matches!(self, Self::Sum16LeNeg) {
                    acc.wrapping_neg()
                } else {
                    acc
                };
                u32::from(val ^ (xor_out.unwrap_or(0) as u16))
            }
            Self::Sum16BeWords | Self::Sum16BeNeg | Self::Sum16BeInv => {
                let mut acc =
                    formatkit_core::checksum::sum_u16_be_words(bytes, seed.unwrap_or(0) as u16);
                if let Some(&last) = bytes.as_chunks::<2>().1.first() {
                    acc = acc.wrapping_add(u16::from_be_bytes([last, 0]));
                }
                let val = match self {
                    Self::Sum16BeNeg => acc.wrapping_neg(),
                    Self::Sum16BeInv => !acc,
                    _ => acc,
                };
                u32::from(val ^ (xor_out.unwrap_or(0) as u16))
            }
            Self::Sum32Le | Self::Sum32Be => {
                let acc = formatkit_core::checksum::sum_bytes_u32(bytes, seed.unwrap_or(0));
                acc ^ xor_out.unwrap_or(0)
            }
            Self::Sum32LeWords | Self::Sum32LeNeg => {
                let mut acc = formatkit_core::checksum::sum_u32_le_words(bytes, seed.unwrap_or(0));
                let rem = bytes.as_chunks::<4>().1;
                if !rem.is_empty() {
                    let mut tail = [0u8; 4];
                    tail[..rem.len()].copy_from_slice(rem);
                    acc = acc.wrapping_add(u32::from_le_bytes(tail));
                }
                let val = if matches!(self, Self::Sum32LeNeg) {
                    acc.wrapping_neg()
                } else {
                    acc
                };
                val ^ xor_out.unwrap_or(0)
            }
            Self::Xor8 => {
                let acc = formatkit_core::checksum::xor_bytes_u8(bytes, seed.unwrap_or(0) as u8);
                u32::from(acc ^ (xor_out.unwrap_or(0) as u8))
            }
            Self::Xor16Le => {
                let mut acc =
                    formatkit_core::checksum::xor_u16_le_words(bytes, seed.unwrap_or(0) as u16);
                if let Some(&last) = bytes.as_chunks::<2>().1.first() {
                    acc ^= u16::from(last);
                }
                u32::from(acc ^ (xor_out.unwrap_or(0) as u16))
            }
            Self::Xor32Le => {
                let mut acc = formatkit_core::checksum::xor_u32_le_words(bytes, seed.unwrap_or(0));
                let rem = bytes.as_chunks::<4>().1;
                if !rem.is_empty() {
                    let mut tail = [0u8; 4];
                    tail[..rem.len()].copy_from_slice(rem);
                    acc ^= u32::from_le_bytes(tail);
                }
                acc ^ xor_out.unwrap_or(0)
            }
            Self::SumXor16Le => {
                let mut sum =
                    formatkit_core::checksum::sum_u16_le_words(bytes, seed.unwrap_or(0) as u16);
                let mut xor = formatkit_core::checksum::xor_u16_le_words(bytes, 0);
                if let Some(&last) = bytes.as_chunks::<2>().1.first() {
                    sum = sum.wrapping_add(u16::from(last));
                    xor ^= u16::from(last);
                }
                u32::from(sum.wrapping_add(xor) ^ (xor_out.unwrap_or(0) as u16))
            }
            Self::Rol32AddLe => {
                let fold = FoldChecksum {
                    unit: FoldUnit::Byte,
                    op: FoldOp::RotateLeftThenSum(1),
                    bits: 32,
                    seed: u64::from(seed.unwrap_or(0)),
                    xor_out: u64::from(xor_out.unwrap_or(0)),
                };
                fold.compute(bytes)
                    .expect("FoldUnit::Byte with bits=32 is infallible") as u32
            }
            Self::Crc16Arc
            | Self::Crc16Kermit
            | Self::Crc16X25
            | Self::Crc16Buypass
            | Self::Crc16Xmodem => {
                let base = match self {
                    Self::Crc16Arc => Crc16Params::ARC,
                    Self::Crc16Kermit => Crc16Params::KERMIT,
                    Self::Crc16X25 => Crc16Params::X25,
                    Self::Crc16Buypass => Crc16Params::BUYPASS,
                    Self::Crc16Xmodem => Crc16Params::XMODEM,
                    _ => unreachable!(),
                };
                let params = Crc16Params {
                    seed: seed.map_or(base.seed, |s| s as u16),
                    xor_out: xor_out.map_or(base.xor_out, |x| x as u16),
                    ..base
                };
                let mut crc = Crc16::new(params);
                crc.update(bytes);
                u32::from(crc.finish())
            }
            Self::Crc16NesGba => {
                let mut crc: u32 = seed.unwrap_or(0);
                let (chunks, rem) = bytes.as_chunks::<4>();
                for chunk in chunks {
                    crc ^= u32::from_le_bytes(*chunk);
                    for _ in 0..32 {
                        let lsb = crc & 1;
                        crc >>= 1;
                        if lsb != 0 {
                            crc ^= 0xD0A2;
                        }
                    }
                }
                let mut tail = [0u8; 4];
                tail[..rem.len()].copy_from_slice(rem);
                crc ^= u32::from_le_bytes(tail);
                for _ in 0..32 {
                    let lsb = crc & 1;
                    crc >>= 1;
                    if lsb != 0 {
                        crc ^= 0xD0A2;
                    }
                }
                u32::from((crc as u16) ^ (xor_out.unwrap_or(0) as u16))
            }
        }
    }

    /// Read a stored checksum value from `data` at `offset`.
    pub fn read_stored(self, data: &[u8], offset: usize) -> Result<u32> {
        let width = self.width();
        let end = offset.checked_add(width).ok_or(Error::InvalidField {
            what: "checksum_stored_offset",
            value: offset as u64,
        })?;
        if end > data.len() {
            return Err(Error::Truncated {
                offset,
                needed: width,
                available: data.len().saturating_sub(offset),
            });
        }
        let slice = &data[offset..end];
        Ok(match self {
            Self::Sum8 | Self::Sum8Inv | Self::Sum8Neg | Self::Xor8 => u32::from(slice[0]),
            Self::Sum16Be | Self::Sum16BeWords | Self::Sum16BeNeg | Self::Sum16BeInv => {
                u32::from(u16::from_be_bytes([slice[0], slice[1]]))
            }
            Self::Sum16Le
            | Self::Sum16LeInv
            | Self::Sum16LeWords
            | Self::Sum16LeNeg
            | Self::Xor16Le
            | Self::SumXor16Le
            | Self::Crc16Arc
            | Self::Crc16Kermit
            | Self::Crc16X25
            | Self::Crc16Buypass
            | Self::Crc16Xmodem
            | Self::Crc16NesGba => u32::from(u16::from_le_bytes([slice[0], slice[1]])),
            Self::Sum32Be => u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]),
            Self::Sum32Le
            | Self::Sum32LeWords
            | Self::Sum32LeNeg
            | Self::Xor32Le
            | Self::Rol32AddLe => u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]),
        })
    }

    /// Write a checksum value to `data` at `offset`.
    pub fn write_stored(self, data: &mut [u8], offset: usize, value: u32) -> Result<()> {
        let width = self.width();
        let end = offset.checked_add(width).ok_or(Error::InvalidField {
            what: "checksum_stored_offset",
            value: offset as u64,
        })?;
        if end > data.len() {
            return Err(Error::Truncated {
                offset,
                needed: width,
                available: data.len().saturating_sub(offset),
            });
        }
        let slice = &mut data[offset..end];
        match self {
            Self::Sum8 | Self::Sum8Inv | Self::Sum8Neg | Self::Xor8 => {
                slice[0] = value as u8;
            }
            Self::Sum16Be | Self::Sum16BeWords | Self::Sum16BeNeg | Self::Sum16BeInv => {
                slice.copy_from_slice(&(value as u16).to_be_bytes());
            }
            Self::Sum16Le
            | Self::Sum16LeInv
            | Self::Sum16LeWords
            | Self::Sum16LeNeg
            | Self::Xor16Le
            | Self::SumXor16Le
            | Self::Crc16Arc
            | Self::Crc16Kermit
            | Self::Crc16X25
            | Self::Crc16Buypass
            | Self::Crc16Xmodem
            | Self::Crc16NesGba => {
                slice.copy_from_slice(&(value as u16).to_le_bytes());
            }
            Self::Sum32Be => {
                slice.copy_from_slice(&value.to_be_bytes());
            }
            Self::Sum32Le
            | Self::Sum32LeWords
            | Self::Sum32LeNeg
            | Self::Xor32Le
            | Self::Rol32AddLe => {
                slice.copy_from_slice(&value.to_le_bytes());
            }
        }
        Ok(())
    }
}

/// A fixed magic-byte sequence expected at a given offset in the save image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MagicSignature {
    pub offset: usize,
    pub bytes: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Declarative specification of one checksum covering a byte range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecksumSpec {
    pub name: String,
    pub algorithm: ChecksumAlgorithm,
    pub covered_start: usize,
    pub covered_end: usize,
    pub stored_offset: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xor_out: Option<u32>,
}

impl ChecksumSpec {
    /// Compute this checksum over `data`.
    ///
    /// If the stored checksum field `[stored_offset .. stored_offset + width)`
    /// overlaps `[covered_start .. covered_end)`, the overlapping stored bytes
    /// are treated as `0x00` during computation.
    pub fn compute_over(&self, data: &[u8]) -> Result<u32> {
        if self.covered_start > self.covered_end {
            return Err(Error::InvalidField {
                what: "checksum_covered_range",
                value: self.covered_start as u64,
            });
        }
        if self.covered_end > data.len() {
            return Err(Error::Truncated {
                offset: self.covered_start,
                needed: self.covered_end - self.covered_start,
                available: data.len().saturating_sub(self.covered_start),
            });
        }
        let width = self.algorithm.width();
        let stored_end = self
            .stored_offset
            .checked_add(width)
            .ok_or(Error::InvalidField {
                what: "checksum_stored_offset",
                value: self.stored_offset as u64,
            })?;
        let slice = &data[self.covered_start..self.covered_end];
        let zero_range = if self.stored_offset < self.covered_end && stored_end > self.covered_start
        {
            let zero_start = self.stored_offset.saturating_sub(self.covered_start);
            let zero_end = stored_end
                .min(self.covered_end)
                .saturating_sub(self.covered_start);
            zero_start..zero_end
        } else {
            0..0
        };
        formatkit_core::checksum::compute_with_zeroed_range(slice, zero_range, |buf| {
            Ok(self.algorithm.compute(buf, self.seed, self.xor_out))
        })
    }
}

/// Supported primitive types for declarative save fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    U8,
    U16Le,
    U32Le,
    I16Le,
    Bitflags,
    Ascii,
    Bytes,
    #[serde(rename = "bitfield_array", alias = "packed_bits")]
    BitfieldArray,
}

/// Specification of a packed bitfield array within a save field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitfieldArraySpec {
    #[serde(default = "default_bit_width")]
    pub bit_width: usize,
    pub count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

fn default_bit_width() -> usize {
    1
}

/// Comparison operator for conditional fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionOp {
    #[serde(rename = "eq", alias = "==")]
    Eq,
    #[serde(rename = "ne", alias = "!=")]
    Ne,
    #[serde(rename = "gt", alias = ">")]
    Gt,
    #[serde(rename = "gte", alias = ">=")]
    Gte,
    #[serde(rename = "lt", alias = "<")]
    Lt,
    #[serde(rename = "lte", alias = "<=")]
    Lte,
    BitSet,
    BitClear,
}

fn default_condition_op() -> ConditionOp {
    ConditionOp::Eq
}

/// Declarative condition specifying when a save field is active / valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldCondition {
    pub field: String,
    #[serde(default = "default_condition_op")]
    pub op: ConditionOp,
    pub value: serde_json::Value,
}

/// Provenance confidence level for a save field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldConfidence {
    #[serde(
        alias = "CONFIRMED",
        alias = "CONFIRMED_VERIFIED",
        alias = "confirmed_verified"
    )]
    Confirmed,
    #[serde(
        alias = "LIKELY",
        alias = "CONFIRMED_STATIC",
        alias = "confirmed_static"
    )]
    Likely,
    #[serde(alias = "GUESSED", alias = "HYPOTHESIS", alias = "hypothesis")]
    Guessed,
    #[serde(alias = "UNKNOWN")]
    Unknown,
}

/// Declarative domain constraint enforced on field edits and validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum FieldConstraint {
    /// Inclusive unsigned integer range `[minimum, maximum]`.
    UnsignedRange { minimum: u64, maximum: u64 },
    /// Inclusive signed integer range `[minimum, maximum]`.
    SignedRange { minimum: i64, maximum: i64 },
    /// Discrete set of valid integer values with optional human-readable labels.
    Enum {
        values: Vec<u64>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        labels: BTreeMap<String, String>,
    },
    /// Valid bitmask for `bitflags` fields plus optional bit-index labels.
    Bitmask {
        valid_mask: u64,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        flag_labels: BTreeMap<String, String>,
    },
    /// Packed Binary-Coded Decimal (2 decimal digits per byte).
    PackedBcd {
        digits: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        minimum: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<u64>,
    },
    /// Allowed character set and/or maximum length for `ascii` fields.
    AsciiCharset {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allowed: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_len: Option<usize>,
    },
}

/// Group of non-overlapping mirror/backup copies of the same logical field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyGroup {
    pub name: String,
    pub fields: Vec<String>,
}

/// Declarative specification of one named field inside a save image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldSpec {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitfield_array: Option<BitfieldArraySpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<FieldCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constraint: Option<FieldConstraint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<FieldConfidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Reverse every aligned 8-byte block in `data` in place.
///
/// GBA EEPROM dumps produced by standard emulators and flashcarts store each
/// 64-bit EEPROM word in reversed byte order (`file_offset = (logical & !7) | (7 - (logical & 7))`).
/// This transformation is an involution: applying it twice yields the original bytes.
pub fn reverse_eeprom_blocks_8(data: &mut [u8]) {
    for chunk in data.as_chunks_mut::<8>().0 {
        chunk.reverse();
    }
}

fn to_rev8(data: &[u8]) -> Vec<u8> {
    let mut buf = data.to_vec();
    reverse_eeprom_blocks_8(&mut buf);
    buf
}

/// Declarative GBA save-format definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveDefinition {
    pub id: String,
    pub game_code: String,
    pub title: String,
    pub media: SaveMediaKind,
    pub size_bytes: usize,
    #[serde(default, skip_serializing_if = "is_false")]
    pub eeprom_rev8: bool,
    #[serde(default)]
    pub magic_signatures: Vec<MagicSignature>,
    #[serde(default)]
    pub checksums: Vec<ChecksumSpec>,
    #[serde(default)]
    pub fields: Vec<FieldSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub copy_groups: Vec<CopyGroup>,
}

impl SaveDefinition {
    /// Parse and validate a `SaveDefinition` from JSON text.
    pub fn from_json(json: &str) -> Result<Self> {
        let def: Self = serde_json::from_str(json)
            .map_err(|e| Error::Malformed(format!("invalid save definition JSON: {e}")))?;
        def.validate()?;
        Ok(def)
    }

    /// Validate structural invariants of this definition.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty() {
            return Err(Error::Malformed("save definition id must not be empty".into()));
        }
        if self.game_code.len() != 4 || !self.game_code.is_ascii() {
            return Err(Error::Malformed(format!(
                "game_code must be 4 ASCII characters, got {:?}",
                self.game_code
            )));
        }
        if self.size_bytes == 0 || self.size_bytes > MAX_SAVE_SIZE {
            return Err(Error::InvalidField {
                what: "size_bytes",
                value: self.size_bytes as u64,
            });
        }

        for sig in &self.magic_signatures {
            if sig.bytes.is_empty() {
                return Err(Error::Malformed("magic signature bytes must not be empty".into()));
            }
            let end = sig
                .offset
                .checked_add(sig.bytes.len())
                .ok_or(Error::InvalidField {
                    what: "magic_offset",
                    value: sig.offset as u64,
                })?;
            if end > self.size_bytes {
                return Err(Error::InvalidField {
                    what: "magic_end",
                    value: end as u64,
                });
            }
        }

        for cs in &self.checksums {
            if cs.name.is_empty() {
                return Err(Error::Malformed("checksum name must not be empty".into()));
            }
            if cs.covered_start > cs.covered_end || cs.covered_end > self.size_bytes {
                return Err(Error::InvalidField {
                    what: "checksum_covered_range",
                    value: cs.covered_end as u64,
                });
            }
            let stored_end = cs
                .stored_offset
                .checked_add(cs.algorithm.width())
                .ok_or(Error::InvalidField {
                    what: "checksum_stored_offset",
                    value: cs.stored_offset as u64,
                })?;
            if stored_end > self.size_bytes {
                return Err(Error::InvalidField {
                    what: "checksum_stored_end",
                    value: stored_end as u64,
                });
            }
        }

        let mut seen_fields = HashSet::new();
        for field in &self.fields {
            if field.name.is_empty() {
                return Err(Error::Malformed("field name must not be empty".into()));
            }
            if !seen_fields.insert(field.name.as_str()) {
                return Err(Error::Malformed(format!(
                    "duplicate field name {:?}",
                    field.name
                )));
            }
            if field.size == 0 {
                return Err(Error::InvalidField {
                    what: "field_size",
                    value: 0,
                });
            }
            let end = field
                .offset
                .checked_add(field.size)
                .ok_or(Error::InvalidField {
                    what: "field_offset",
                    value: field.offset as u64,
                })?;
            if end > self.size_bytes {
                return Err(Error::InvalidField {
                    what: "field_end",
                    value: end as u64,
                });
            }
            let expected_size = match field.field_type {
                FieldType::U8 => Some(1),
                FieldType::U16Le | FieldType::I16Le => Some(2),
                FieldType::U32Le => Some(4),
                FieldType::Bitflags => {
                    if !matches!(field.size, 1 | 2 | 4 | 8) {
                        return Err(Error::InvalidField {
                            what: "bitflags_size",
                            value: field.size as u64,
                        });
                    }
                    None
                }
                FieldType::BitfieldArray => {
                    if let Some(ba) = &field.bitfield_array {
                        if ba.bit_width == 0 || ba.bit_width > 32 {
                            return Err(Error::InvalidField {
                                what: "bitfield_array_bit_width",
                                value: ba.bit_width as u64,
                            });
                        }
                        let total_bits = ba.bit_width.checked_mul(ba.count).ok_or(Error::InvalidField {
                            what: "bitfield_array_total_bits",
                            value: 0,
                        })?;
                        let min_bytes = total_bits.div_ceil(8);
                        if min_bytes > field.size {
                            return Err(Error::InvalidField {
                                what: "bitfield_array_size",
                                value: field.size as u64,
                            });
                        }
                    }
                    None
                }
                FieldType::Ascii | FieldType::Bytes => None,
            };
            if let Some(req) = expected_size {
                if field.size != req {
                    return Err(Error::InvalidField {
                        what: "field_size",
                        value: field.size as u64,
                    });
                }
            }
            if let Some(cond) = &field.condition {
                if cond.field == field.name {
                    return Err(Error::Malformed(format!(
                        "field '{}' cannot depend conditionally on itself",
                        field.name
                    )));
                }
            }
            if let Some(constraint) = &field.constraint {
                validate_field_constraint_spec(field, constraint)?;
            }
        }

        // Validate copy_groups: unique non-empty names, >=2 unique existing fields,
        // no field in multiple groups, identical field_type & size, non-overlapping spans.
        let field_map: HashMap<&str, &FieldSpec> =
            self.fields.iter().map(|f| (f.name.as_str(), f)).collect();
        let mut seen_groups = HashSet::new();
        let mut assigned_fields = HashSet::new();
        for group in &self.copy_groups {
            if group.name.is_empty() {
                return Err(Error::Malformed("copy_group name must not be empty".into()));
            }
            if !seen_groups.insert(group.name.as_str()) {
                return Err(Error::Malformed(format!(
                    "duplicate copy_group name {:?}",
                    group.name
                )));
            }
            if group.fields.len() < 2 {
                return Err(Error::Malformed(format!(
                    "copy_group {:?} must reference at least 2 fields",
                    group.name
                )));
            }
            let mut group_specs: Vec<&FieldSpec> = Vec::with_capacity(group.fields.len());
            let mut group_members = HashSet::new();
            for fname in &group.fields {
                if !group_members.insert(fname.as_str()) {
                    return Err(Error::Malformed(format!(
                        "copy_group {:?} lists duplicate field {:?}",
                        group.name, fname
                    )));
                }
                if !assigned_fields.insert(fname.as_str()) {
                    return Err(Error::Malformed(format!(
                        "field {:?} appears in multiple copy_groups",
                        fname
                    )));
                }
                let spec = field_map.get(fname.as_str()).copied().ok_or_else(|| {
                    Error::Malformed(format!(
                        "copy_group {:?} references unknown field {:?}",
                        group.name, fname
                    ))
                })?;
                group_specs.push(spec);
            }
            let first = group_specs[0];
            for spec in &group_specs[1..] {
                if spec.field_type != first.field_type || spec.size != first.size {
                    return Err(Error::Malformed(format!(
                        "copy_group {:?} field {:?} ({:?}, {} bytes) does not match {:?} ({:?}, {} bytes)",
                        group.name,
                        spec.name,
                        spec.field_type,
                        spec.size,
                        first.name,
                        first.field_type,
                        first.size
                    )));
                }
            }
            for i in 0..group_specs.len() {
                for j in (i + 1)..group_specs.len() {
                    let a = group_specs[i];
                    let b = group_specs[j];
                    let a_end = a.offset + a.size;
                    let b_end = b.offset + b.size;
                    if a.offset < b_end && b.offset < a_end {
                        return Err(Error::Malformed(format!(
                            "copy_group {:?} fields {:?} and {:?} overlap",
                            group.name, a.name, b.name
                        )));
                    }
                }
            }
        }

        Ok(())
    }
}

fn unsigned_type_max(ft: FieldType) -> Option<u64> {
    match ft {
        FieldType::U8 => Some(u64::from(u8::MAX)),
        FieldType::U16Le => Some(u64::from(u16::MAX)),
        FieldType::U32Le => Some(u64::from(u32::MAX)),
        _ => None,
    }
}

fn validate_field_constraint_spec(field: &FieldSpec, constraint: &FieldConstraint) -> Result<()> {
    match constraint {
        FieldConstraint::UnsignedRange { minimum, maximum } => {
            let max_allowed = unsigned_type_max(field.field_type).ok_or_else(|| {
                Error::Malformed(format!(
                    "unsigned-range constraint on field {:?} requires u8/u16_le/u32_le, got {:?}",
                    field.name, field.field_type
                ))
            })?;
            if minimum > maximum || *maximum > max_allowed {
                return Err(Error::Malformed(format!(
                    "invalid unsigned-range [{minimum}, {maximum}] for field {:?} (max {max_allowed})",
                    field.name
                )));
            }
        }
        FieldConstraint::SignedRange { minimum, maximum } => {
            if field.field_type != FieldType::I16Le {
                return Err(Error::Malformed(format!(
                    "signed-range constraint on field {:?} requires i16_le, got {:?}",
                    field.name, field.field_type
                )));
            }
            if minimum > maximum
                || *minimum < i64::from(i16::MIN)
                || *maximum > i64::from(i16::MAX)
            {
                return Err(Error::Malformed(format!(
                    "invalid signed-range [{minimum}, {maximum}] for field {:?}",
                    field.name
                )));
            }
        }
        FieldConstraint::Enum { values, .. } => {
            let max_allowed = unsigned_type_max(field.field_type).ok_or_else(|| {
                Error::Malformed(format!(
                    "enum constraint on field {:?} requires u8/u16_le/u32_le, got {:?}",
                    field.name, field.field_type
                ))
            })?;
            if values.is_empty() {
                return Err(Error::Malformed(format!(
                    "enum constraint on field {:?} must have at least one value",
                    field.name
                )));
            }
            if let Some(&bad) = values.iter().find(|&&v| v > max_allowed) {
                return Err(Error::Malformed(format!(
                    "enum value {bad} exceeds type max {max_allowed} for field {:?}",
                    field.name
                )));
            }
        }
        FieldConstraint::Bitmask { valid_mask, .. } => {
            if field.field_type != FieldType::Bitflags {
                return Err(Error::Malformed(format!(
                    "bitmask constraint on field {:?} requires bitflags, got {:?}",
                    field.name, field.field_type
                )));
            }
            let max_mask = if field.size >= 8 {
                u64::MAX
            } else {
                (1u64 << (field.size * 8)) - 1
            };
            if *valid_mask > max_mask {
                return Err(Error::Malformed(format!(
                    "bitmask 0x{valid_mask:X} exceeds width of field {:?} ({} bytes)",
                    field.name, field.size
                )));
            }
        }
        FieldConstraint::PackedBcd {
            digits,
            minimum,
            maximum,
        } => {
            if !matches!(
                field.field_type,
                FieldType::Bytes | FieldType::U8 | FieldType::U16Le | FieldType::U32Le
            ) {
                return Err(Error::Malformed(format!(
                    "packed-bcd constraint on field {:?} requires bytes or unsigned integer, got {:?}",
                    field.name, field.field_type
                )));
            }
            if *digits == 0 || *digits > 16 || usize::from(*digits) > field.size * 2 {
                return Err(Error::Malformed(format!(
                    "packed-bcd digits {} invalid for field {:?} ({} bytes)",
                    digits, field.name, field.size
                )));
            }
            if let (Some(min), Some(max)) = (minimum, maximum) {
                if min > max {
                    return Err(Error::Malformed(format!(
                        "packed-bcd minimum {min} > maximum {max} for field {:?}",
                        field.name
                    )));
                }
            }
        }
        FieldConstraint::AsciiCharset { max_len, .. } => {
            if field.field_type != FieldType::Ascii {
                return Err(Error::Malformed(format!(
                    "ascii-charset constraint on field {:?} requires ascii, got {:?}",
                    field.name, field.field_type
                )));
            }
            if let Some(m) = max_len {
                if *m > field.size {
                    return Err(Error::Malformed(format!(
                        "ascii-charset max_len {m} exceeds field {:?} size {}",
                        field.name, field.size
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Decode a packed BCD byte sequence (big-endian byte order, high nibble then low nibble)
/// into its decimal `u64` value, rejecting any nibble > 9 or non-zero leading unused nibbles.
fn decode_packed_bcd_bytes(bytes: &[u8], digits: u8) -> std::result::Result<u64, String> {
    let total_nibbles = bytes.len() * 2;
    let d = usize::from(digits);
    if d == 0 || d > 16 || d > total_nibbles {
        return Err(format!("invalid packed-bcd digit count {digits} for {} bytes", bytes.len()));
    }
    let unused_leading = total_nibbles - d;
    let mut acc = 0u64;
    for (idx, &byte) in bytes.iter().enumerate() {
        let hi = byte >> 4;
        let lo = byte & 0x0F;
        for (sub, nib) in [(0usize, hi), (1usize, lo)] {
            let nibble_pos = idx * 2 + sub;
            if nibble_pos < unused_leading {
                if nib != 0 {
                    return Err(format!(
                        "non-zero unused leading BCD nibble 0x{nib:X} at nibble {nibble_pos}"
                    ));
                }
            } else {
                if nib > 9 {
                    return Err(format!(
                        "invalid BCD nibble 0x{nib:X} at nibble {nibble_pos} (expected 0..=9)"
                    ));
                }
                acc = acc * 10 + u64::from(nib);
            }
        }
    }
    Ok(acc)
}

/// Validate `value` against `spec.constraint` (if any).
///
/// Returns `Ok(())` when `spec.constraint` is `None` or when `value` satisfies the constraint.
pub fn validate_field_value(spec: &FieldSpec, value: &FieldValue) -> Result<()> {
    let Some(constraint) = &spec.constraint else {
        return Ok(());
    };
    match constraint {
        FieldConstraint::UnsignedRange { minimum, maximum } => {
            let v = match value {
                FieldValue::U8(x) => u64::from(*x),
                FieldValue::U16(x) => u64::from(*x),
                FieldValue::U32(x) => u64::from(*x),
                other => {
                    return Err(Error::Malformed(format!(
                        "unsigned-range constraint on {:?} expected unsigned int, got {other:?}",
                        spec.name
                    )));
                }
            };
            if v < *minimum || v > *maximum {
                return Err(Error::Malformed(format!(
                    "field {:?} value {v} out of unsigned-range [{minimum}, {maximum}]",
                    spec.name
                )));
            }
        }
        FieldConstraint::SignedRange { minimum, maximum } => {
            let v = match value {
                FieldValue::I16(x) => i64::from(*x),
                other => {
                    return Err(Error::Malformed(format!(
                        "signed-range constraint on {:?} expected i16, got {other:?}",
                        spec.name
                    )));
                }
            };
            if v < *minimum || v > *maximum {
                return Err(Error::Malformed(format!(
                    "field {:?} value {v} out of signed-range [{minimum}, {maximum}]",
                    spec.name
                )));
            }
        }
        FieldConstraint::Enum { values, .. } => {
            let v = match value {
                FieldValue::U8(x) => u64::from(*x),
                FieldValue::U16(x) => u64::from(*x),
                FieldValue::U32(x) => u64::from(*x),
                other => {
                    return Err(Error::Malformed(format!(
                        "enum constraint on {:?} expected unsigned int, got {other:?}",
                        spec.name
                    )));
                }
            };
            if !values.contains(&v) {
                return Err(Error::Malformed(format!(
                    "field {:?} value {v} not in allowed enum values {values:?}",
                    spec.name
                )));
            }
        }
        FieldConstraint::Bitmask { valid_mask, .. } => {
            let v = match value {
                FieldValue::Bitflags(x) => *x,
                other => {
                    return Err(Error::Malformed(format!(
                        "bitmask constraint on {:?} expected bitflags, got {other:?}",
                        spec.name
                    )));
                }
            };
            if (v & !*valid_mask) != 0 {
                return Err(Error::Malformed(format!(
                    "field {:?} value 0x{v:X} sets bits outside valid_mask 0x{valid_mask:X}",
                    spec.name
                )));
            }
        }
        FieldConstraint::PackedBcd {
            digits,
            minimum,
            maximum,
        } => {
            let raw_bytes: Vec<u8> = match value {
                FieldValue::Bytes(b) => b.clone(),
                FieldValue::U8(x) => vec![*x],
                FieldValue::U16(x) => x.to_be_bytes().to_vec(),
                FieldValue::U32(x) => x.to_be_bytes().to_vec(),
                other => {
                    return Err(Error::Malformed(format!(
                        "packed-bcd constraint on {:?} expected bytes or unsigned int, got {other:?}",
                        spec.name
                    )));
                }
            };
            let decoded = decode_packed_bcd_bytes(&raw_bytes, *digits).map_err(|msg| {
                Error::Malformed(format!("field {:?} invalid packed-bcd: {msg}", spec.name))
            })?;
            if let Some(min) = minimum {
                if decoded < *min {
                    return Err(Error::Malformed(format!(
                        "field {:?} packed-bcd value {decoded} below minimum {min}",
                        spec.name
                    )));
                }
            }
            if let Some(max) = maximum {
                if decoded > *max {
                    return Err(Error::Malformed(format!(
                        "field {:?} packed-bcd value {decoded} above maximum {max}",
                        spec.name
                    )));
                }
            }
        }
        FieldConstraint::AsciiCharset { allowed, max_len } => {
            let s = match value {
                FieldValue::Ascii(s) => s,
                other => {
                    return Err(Error::Malformed(format!(
                        "ascii-charset constraint on {:?} expected ascii, got {other:?}",
                        spec.name
                    )));
                }
            };
            let limit = max_len.unwrap_or(spec.size).min(spec.size);
            if s.len() > limit {
                return Err(Error::Malformed(format!(
                    "field {:?} ascii length {} exceeds max_len {limit}",
                    spec.name,
                    s.len()
                )));
            }
            if let Some(allowed_chars) = allowed {
                if let Some(bad) = s.chars().find(|&c| !allowed_chars.contains(c)) {
                    return Err(Error::Malformed(format!(
                        "field {:?} contains disallowed character {bad:?}",
                        spec.name
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Typed value of a decoded or mutated save field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum FieldValue {
    U8(u8),
    U16(u16),
    U32(u32),
    I16(i16),
    Bitflags(u64),
    Ascii(String),
    Bytes(Vec<u8>),
    BitfieldArray(Vec<u32>),
}

impl fmt::Display for FieldValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::U8(v) => write!(f, "{v} (0x{v:02X})"),
            Self::U16(v) => write!(f, "{v} (0x{v:04X})"),
            Self::U32(v) => write!(f, "{v} (0x{v:08X})"),
            Self::I16(v) => write!(f, "{v}"),
            Self::Bitflags(v) => write!(f, "0x{v:X}"),
            Self::Ascii(s) => write!(f, "\"{s}\""),
            Self::Bytes(b) => {
                if b.len() <= 16 {
                    let hex_str: Vec<String> = b.iter().map(|byte| format!("{byte:02X}")).collect();
                    write!(f, "[{}]", hex_str.join(" "))
                } else {
                    let hex_head: Vec<String> = b[..8].iter().map(|byte| format!("{byte:02X}")).collect();
                    write!(f, "[{} ... ({} bytes)]", hex_head.join(" "), b.len())
                }
            }
            Self::BitfieldArray(arr) => {
                if arr.len() <= 16 {
                    let items: Vec<String> = arr.iter().map(|v| v.to_string()).collect();
                    write!(f, "[{}]", items.join(", "))
                } else {
                    let head: Vec<String> = arr[..8].iter().map(|v| v.to_string()).collect();
                    write!(f, "[{}, ... ({} elements)]", head.join(", "), arr.len())
                }
            }
        }
    }
}

/// Verification result for one magic signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MagicCheckResult {
    pub offset: usize,
    pub expected: Vec<u8>,
    pub actual: Vec<u8>,
    pub valid: bool,
}

/// Verification or repair result for one checksum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecksumResult {
    pub name: String,
    pub algorithm: ChecksumAlgorithm,
    pub stored_offset: usize,
    pub expected: u32,
    pub actual: u32,
    pub valid: bool,
}

/// Full verification report returned by [`verify_save`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationReport {
    pub valid: bool,
    pub size_matches: bool,
    pub actual_size: usize,
    pub expected_size: usize,
    pub magics: Vec<MagicCheckResult>,
    pub checksums: Vec<ChecksumResult>,
}

/// One decoded field value from a save file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodedField {
    pub name: String,
    pub offset: usize,
    pub size: usize,
    pub value: FieldValue,
}

/// Decoded save image returned by [`decode_save`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodedSave {
    pub id: String,
    pub game_code: String,
    pub title: String,
    pub media: SaveMediaKind,
    pub verification: VerificationReport,
    pub fields: Vec<DecodedField>,
}

fn verify_save_logical(def: &SaveDefinition, data: &[u8]) -> Result<VerificationReport> {
    let size_matches = data.len() == def.size_bytes;

    let mut magics = Vec::with_capacity(def.magic_signatures.len());
    for sig in &def.magic_signatures {
        let end = sig.offset + sig.bytes.len();
        if end > data.len() {
            return Err(Error::Truncated {
                offset: sig.offset,
                needed: sig.bytes.len(),
                available: data.len().saturating_sub(sig.offset),
            });
        }
        let actual = data[sig.offset..end].to_vec();
        let valid = actual == sig.bytes;
        magics.push(MagicCheckResult {
            offset: sig.offset,
            expected: sig.bytes.clone(),
            actual,
            valid,
        });
    }

    let mut checksums = Vec::with_capacity(def.checksums.len());
    for cs in &def.checksums {
        let expected = cs.compute_over(data)?;
        let actual = cs.algorithm.read_stored(data, cs.stored_offset)?;
        checksums.push(ChecksumResult {
            name: cs.name.clone(),
            algorithm: cs.algorithm,
            stored_offset: cs.stored_offset,
            expected,
            actual,
            valid: expected == actual,
        });
    }

    let valid = size_matches
        && magics.iter().all(|m| m.valid)
        && checksums.iter().all(|c| c.valid);

    Ok(VerificationReport {
        valid,
        size_matches,
        actual_size: data.len(),
        expected_size: def.size_bytes,
        magics,
        checksums,
    })
}

/// Verify all magic signatures and checksums of `data` against `def`.
pub fn verify_save(def: &SaveDefinition, data: &[u8]) -> Result<VerificationReport> {
    def.validate()?;
    if def.eeprom_rev8 {
        let logical = to_rev8(data);
        verify_save_logical(def, &logical)
    } else {
        verify_save_logical(def, data)
    }
}

fn read_field(spec: &FieldSpec, data: &[u8]) -> Result<FieldValue> {
    let end = spec
        .offset
        .checked_add(spec.size)
        .ok_or(Error::InvalidField {
            what: "field_offset",
            value: spec.offset as u64,
        })?;
    if end > data.len() {
        return Err(Error::Truncated {
            offset: spec.offset,
            needed: spec.size,
            available: data.len().saturating_sub(spec.offset),
        });
    }
    let slice = &data[spec.offset..end];
    Ok(match spec.field_type {
        FieldType::U8 => {
            if slice.len() != 1 {
                return Err(Error::InvalidField {
                    what: "field_size",
                    value: slice.len() as u64,
                });
            }
            FieldValue::U8(slice[0])
        }
        FieldType::U16Le => {
            if slice.len() != 2 {
                return Err(Error::InvalidField {
                    what: "field_size",
                    value: slice.len() as u64,
                });
            }
            FieldValue::U16(u16::from_le_bytes([slice[0], slice[1]]))
        }
        FieldType::U32Le => {
            if slice.len() != 4 {
                return Err(Error::InvalidField {
                    what: "field_size",
                    value: slice.len() as u64,
                });
            }
            FieldValue::U32(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
        }
        FieldType::I16Le => {
            if slice.len() != 2 {
                return Err(Error::InvalidField {
                    what: "field_size",
                    value: slice.len() as u64,
                });
            }
            FieldValue::I16(i16::from_le_bytes([slice[0], slice[1]]))
        }
        FieldType::Bitflags => {
            if !matches!(slice.len(), 1 | 2 | 4 | 8) {
                return Err(Error::InvalidField {
                    what: "bitflags_size",
                    value: slice.len() as u64,
                });
            }
            let mut buf = [0u8; 8];
            buf[..slice.len()].copy_from_slice(slice);
            FieldValue::Bitflags(u64::from_le_bytes(buf))
        }
        FieldType::Ascii => {
            let trimmed = slice
                .iter()
                .take_while(|&&b| b != 0)
                .copied()
                .collect::<Vec<u8>>();
            FieldValue::Ascii(String::from_utf8_lossy(&trimmed).into_owned())
        }
        FieldType::Bytes => FieldValue::Bytes(slice.to_vec()),
        FieldType::BitfieldArray => {
            let bit_width = spec
                .bitfield_array
                .as_ref()
                .map(|ba| ba.bit_width)
                .unwrap_or(1);
            if bit_width == 0 || bit_width > 32 {
                return Err(Error::InvalidField {
                    what: "bitfield_array_bit_width",
                    value: bit_width as u64,
                });
            }
            let slice_bits = slice.len().checked_mul(8).ok_or(Error::InvalidField {
                what: "bitfield_array_size",
                value: slice.len() as u64,
            })?;
            let count = match &spec.bitfield_array {
                Some(ba) => {
                    let total_bits = ba.count.checked_mul(bit_width).ok_or(Error::InvalidField {
                        what: "bitfield_array_total_bits",
                        value: ba.count as u64,
                    })?;
                    if total_bits > slice_bits {
                        return Err(Error::InvalidField {
                            what: "bitfield_array_size",
                            value: slice.len() as u64,
                        });
                    }
                    ba.count
                }
                None => slice_bits / bit_width,
            };
            let mut values = Vec::with_capacity(count);
            for i in 0..count {
                let Some(bit_start) = i.checked_mul(bit_width) else {
                    break;
                };
                let mut val = 0u32;
                for b in 0..bit_width {
                    let Some(cur_bit) = bit_start.checked_add(b) else {
                        break;
                    };
                    let byte_idx = cur_bit / 8;
                    let bit_idx = cur_bit % 8;
                    if byte_idx < slice.len() {
                        let bit = ((slice[byte_idx] >> bit_idx) & 1) as u32;
                        val |= bit << b;
                    }
                }
                values.push(val);
            }
            FieldValue::BitfieldArray(values)
        }
    })
}

fn decode_save_logical(def: &SaveDefinition, data: &[u8]) -> Result<DecodedSave> {
    let verification = verify_save_logical(def, data)?;
    let mut fields = Vec::with_capacity(def.fields.len());
    for spec in &def.fields {
        let value = read_field(spec, data)?;
        fields.push(DecodedField {
            name: spec.name.clone(),
            offset: spec.offset,
            size: spec.size,
            value,
        });
    }
    Ok(DecodedSave {
        id: def.id.clone(),
        game_code: def.game_code.clone(),
        title: def.title.clone(),
        media: def.media,
        verification,
        fields,
    })
}

/// Verify and decode all declared fields of `data` using `def`.
pub fn decode_save(def: &SaveDefinition, data: &[u8]) -> Result<DecodedSave> {
    def.validate()?;
    if def.eeprom_rev8 {
        let logical = to_rev8(data);
        decode_save_logical(def, &logical)
    } else {
        decode_save_logical(def, data)
    }
}

fn fix_checksums_logical(def: &SaveDefinition, data: &mut [u8]) -> Result<Vec<ChecksumResult>> {
    let mut results = Vec::with_capacity(def.checksums.len());
    for cs in &def.checksums {
        let previous = cs.algorithm.read_stored(data, cs.stored_offset)?;
        let computed = cs.compute_over(data)?;
        cs.algorithm.write_stored(data, cs.stored_offset, computed)?;
        results.push(ChecksumResult {
            name: cs.name.clone(),
            algorithm: cs.algorithm,
            stored_offset: cs.stored_offset,
            expected: computed,
            actual: previous,
            valid: previous == computed,
        });
    }
    Ok(results)
}

/// Recompute and write all checksums declared in `def` into `data`.
///
/// Returns the updated [`ChecksumResult`] for each checksum (with `actual`
/// reflecting the previously stored value and `expected` the newly written
/// value).
pub fn fix_checksums(def: &SaveDefinition, data: &mut [u8]) -> Result<Vec<ChecksumResult>> {
    def.validate()?;
    if def.eeprom_rev8 {
        let mut logical = to_rev8(data);
        let results = fix_checksums_logical(def, &mut logical)?;
        reverse_eeprom_blocks_8(&mut logical);
        data.copy_from_slice(&logical);
        Ok(results)
    } else {
        fix_checksums_logical(def, data)
    }
}

fn set_field_logical(
    def: &SaveDefinition,
    data: &mut [u8],
    field_name: &str,
    value: FieldValue,
    repair: bool,
    check_constraint: bool,
) -> Result<()> {
    let spec = def
        .fields
        .iter()
        .find(|f| f.name == field_name)
        .ok_or_else(|| Error::Malformed(format!("unknown field {field_name:?}")))?;

    if check_constraint {
        validate_field_value(spec, &value)?;
    }

    let end = spec.offset + spec.size;
    if end > data.len() {
        return Err(Error::Truncated {
            offset: spec.offset,
            needed: spec.size,
            available: data.len().saturating_sub(spec.offset),
        });
    }
    let slice = &mut data[spec.offset..end];

    match (spec.field_type, value) {
        (FieldType::U8, FieldValue::U8(v)) => slice[0] = v,
        (FieldType::U16Le, FieldValue::U16(v)) => slice.copy_from_slice(&v.to_le_bytes()),
        (FieldType::U32Le, FieldValue::U32(v)) => slice.copy_from_slice(&v.to_le_bytes()),
        (FieldType::I16Le, FieldValue::I16(v)) => slice.copy_from_slice(&v.to_le_bytes()),
        (FieldType::Bitflags, FieldValue::Bitflags(v)) => {
            let bytes = v.to_le_bytes();
            slice.copy_from_slice(&bytes[..spec.size]);
        }
        (FieldType::Ascii, FieldValue::Ascii(s)) => {
            let raw = s.as_bytes();
            if raw.len() > spec.size {
                return Err(Error::InvalidField {
                    what: "ascii_field_len",
                    value: raw.len() as u64,
                });
            }
            slice.fill(0);
            slice[..raw.len()].copy_from_slice(raw);
        }
        (FieldType::Bytes, FieldValue::Bytes(b)) => {
            if b.len() != spec.size {
                return Err(Error::InvalidField {
                    what: "bytes_field_len",
                    value: b.len() as u64,
                });
            }
            slice.copy_from_slice(&b);
        }
        (FieldType::BitfieldArray, FieldValue::BitfieldArray(values)) => {
            let bit_width = spec
                .bitfield_array
                .as_ref()
                .map(|ba| ba.bit_width)
                .unwrap_or(1);
            if bit_width == 0 || bit_width > 32 {
                return Err(Error::InvalidField {
                    what: "bitfield_array_bit_width",
                    value: bit_width as u64,
                });
            }
            for (i, &val) in values.iter().enumerate() {
                let Some(bit_start) = i.checked_mul(bit_width) else {
                    break;
                };
                for b in 0..bit_width {
                    let Some(cur_bit) = bit_start.checked_add(b) else {
                        break;
                    };
                    let byte_idx = cur_bit / 8;
                    let bit_idx = cur_bit % 8;
                    if byte_idx < slice.len() {
                        let bit = ((val >> b) & 1) as u8;
                        slice[byte_idx] &= !(1 << bit_idx);
                        slice[byte_idx] |= bit << bit_idx;
                    }
                }
            }
        }
        (expected, actual) => {
            return Err(Error::Malformed(format!(
                "type mismatch for field {field_name:?}: expected {expected:?}, got {actual:?}"
            )));
        }
    }

    if repair {
        fix_checksums_logical(def, data)?;
    }
    Ok(())
}

/// Set a named field in `data`, validating any declared [`FieldConstraint`]
/// and optionally repairing all checksums afterward.
pub fn set_field(
    def: &SaveDefinition,
    data: &mut [u8],
    field_name: &str,
    value: FieldValue,
    repair: bool,
) -> Result<()> {
    def.validate()?;
    if def.eeprom_rev8 {
        let mut logical = to_rev8(data);
        set_field_logical(def, &mut logical, field_name, value, repair, true)?;
        reverse_eeprom_blocks_8(&mut logical);
        data.copy_from_slice(&logical);
        Ok(())
    } else {
        set_field_logical(def, data, field_name, value, repair, true)
    }
}

/// Match confidence returned by [`probe_loose_save`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchConfidence {
    /// Magic signature(s) matched at canonical offsets, but no checksums matched.
    MagicOnly,
    /// Checksum(s) matched expected values, but no magic signatures matched.
    ChecksumOnly,
    /// Both magic signature(s) and checksum(s) matched.
    Exact,
}

impl MatchConfidence {
    /// Canonical lowercase string identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::ChecksumOnly => "checksum_only",
            Self::MagicOnly => "magic_only",
        }
    }
}

impl fmt::Display for MatchConfidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Match result produced by [`probe_loose_save`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeMatch<'a> {
    pub def: &'a SaveDefinition,
    pub magic_matched: bool,
    pub checksums_valid: bool,
    pub confidence: MatchConfidence,
    pub geometry: SaveGeometry,
}

/// Probe loose save `bytes` against a list of known save definitions.
///
/// Returns all definitions where at least one magic signature matched at its
/// canonical offset or at least one declared checksum matched its expected value,
/// ordered with highest confidence first.
pub fn probe_loose_save<'a>(
    bytes: &[u8],
    definitions: &'a [SaveDefinition],
) -> Vec<ProbeMatch<'a>> {
    if bytes.is_empty() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    let mut rev8_full: Option<Vec<u8>> = None;

    for def in definitions {
        let probe_len = def.size_bytes.min(bytes.len());
        let probe_data: Cow<'_, [u8]> = if def.eeprom_rev8 {
            if probe_len.is_multiple_of(8) || probe_len == bytes.len() {
                let cached = rev8_full.get_or_insert_with(|| to_rev8(bytes));
                Cow::Borrowed(&cached[..probe_len])
            } else {
                Cow::Owned(to_rev8(&bytes[..probe_len]))
            }
        } else {
            Cow::Borrowed(&bytes[..probe_len])
        };

        let magic_matched = !def.magic_signatures.is_empty()
            && def.magic_signatures.iter().any(|sig| {
                let end = sig.offset.saturating_add(sig.bytes.len());
                if end <= probe_data.len() {
                    probe_data[sig.offset..end] == sig.bytes
                } else {
                    false
                }
            });

        let checksums_valid = !def.checksums.is_empty()
            && def.checksums.iter().any(|cs| {
                let width = cs.algorithm.width();
                let stored_end = cs.stored_offset.saturating_add(width);
                if cs.covered_end <= probe_data.len() && stored_end <= probe_data.len() {
                    match (
                        cs.compute_over(&probe_data),
                        cs.algorithm.read_stored(&probe_data, cs.stored_offset),
                    ) {
                        (Ok(expected), Ok(actual)) => expected == actual,
                        _ => false,
                    }
                } else {
                    false
                }
            });

        if magic_matched || checksums_valid {
            let confidence = if magic_matched && checksums_valid {
                MatchConfidence::Exact
            } else if checksums_valid {
                MatchConfidence::ChecksumOnly
            } else {
                MatchConfidence::MagicOnly
            };
            let geometry = SaveGeometry::for_definition(bytes, def);

            matches.push(ProbeMatch {
                def,
                magic_matched,
                checksums_valid,
                confidence,
                geometry,
            });
        }
    }

    matches.sort_by(|a, b| {
        b.confidence
            .cmp(&a.confidence)
            .then_with(|| a.def.id.cmp(&b.def.id))
    });

    matches
}

/// Trim trailing emulator or flashcart padding from `bytes` down to `canonical_size`.
///
/// Verifies that all bytes past `canonical_size` are uniform padding (`0xFF` or `0x00`).
/// Returns an error if `canonical_size == 0`, if `bytes.len() < canonical_size`, or if
/// non-padding data would be truncated.
pub fn trim_to_canonical(bytes: &[u8], canonical_size: usize) -> Result<Vec<u8>> {
    trim_to_canonical_with_force(bytes, canonical_size, false)
}

/// Trim trailing padding from `bytes` down to `canonical_size`, optionally bypassing
/// the padding verification if `force` is `true`.
pub fn trim_to_canonical_with_force(
    bytes: &[u8],
    canonical_size: usize,
    force: bool,
) -> Result<Vec<u8>> {
    if canonical_size == 0 {
        return Err(Error::InvalidField {
            what: "canonical_size",
            value: 0,
        });
    }
    if bytes.len() < canonical_size {
        return Err(Error::Truncated {
            offset: bytes.len(),
            needed: canonical_size - bytes.len(),
            available: 0,
        });
    }
    if bytes.len() == canonical_size {
        return Ok(bytes.to_vec());
    }

    let tail = &bytes[canonical_size..];
    let is_pure_ff = tail.iter().all(|&b| b == 0xFF);
    let is_pure_00 = tail.iter().all(|&b| b == 0x00);
    let is_pure_padding = is_pure_ff || is_pure_00;

    if !is_pure_padding && !force {
        return Err(Error::Malformed(
            "cannot trim save: trailing bytes contain non-padding data (expected pure 0xFF or 0x00)"
                .into(),
        ));
    }

    Ok(bytes[..canonical_size].to_vec())
}

fn parse_int_or_hex_or_bool<T>(s: &str) -> Result<T>
where
    T: TryFrom<u64> + std::str::FromStr,
    <T as TryFrom<u64>>::Error: std::fmt::Display,
    <T as std::str::FromStr>::Err: std::fmt::Display,
{
    if s.eq_ignore_ascii_case("true") {
        return T::try_from(1u64)
            .map_err(|e| Error::Malformed(format!("cannot convert true to integer: {e}")));
    }
    if s.eq_ignore_ascii_case("false") {
        return T::try_from(0u64)
            .map_err(|e| Error::Malformed(format!("cannot convert false to integer: {e}")));
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        let clean = hex.replace('_', "");
        let val = u64::from_str_radix(&clean, 16)
            .map_err(|e| Error::Malformed(format!("invalid hex integer {s:?}: {e}")))?;
        return T::try_from(val)
            .map_err(|e| Error::Malformed(format!("hex integer out of range {s:?}: {e}")));
    }
    let clean = s.replace('_', "");
    if let Ok(val) = clean.parse::<T>() {
        return Ok(val);
    }
    if clean.bytes().any(|b| matches!(b, b'a'..=b'f' | b'A'..=b'F')) {
        if let Ok(val) = u64::from_str_radix(&clean, 16) {
            if let Ok(target) = T::try_from(val) {
                return Ok(target);
            }
        }
    }
    clean
        .parse::<T>()
        .map_err(|e| Error::Malformed(format!("invalid integer {s:?}: {e}")))
}

fn parse_signed_int_or_hex(s: &str) -> Result<i16> {
    if s.eq_ignore_ascii_case("true") {
        return Ok(1);
    }
    if s.eq_ignore_ascii_case("false") {
        return Ok(0);
    }
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        let clean = hex.replace('_', "");
        let val = u16::from_str_radix(&clean, 16)
            .map_err(|e| Error::Malformed(format!("invalid hex integer {s:?}: {e}")))?;
        return Ok(val as i16);
    }
    let clean = s.replace('_', "");
    if let Ok(val) = clean.parse::<i16>() {
        return Ok(val);
    }
    if clean.bytes().any(|b| matches!(b, b'a'..=b'f' | b'A'..=b'F')) {
        if let Ok(val) = u16::from_str_radix(&clean, 16) {
            return Ok(val as i16);
        }
    }
    clean
        .parse::<i16>()
        .map_err(|e| Error::Malformed(format!("invalid integer {s:?}: {e}")))
}

fn parse_bytes_literal(s: &str, expected_size: usize) -> Result<Vec<u8>> {
    let mut clean = s.trim();
    if clean.starts_with('[') && clean.ends_with(']') && clean.len() >= 2 {
        clean = clean[1..clean.len() - 1].trim();
    }
    if clean.is_empty() {
        if expected_size == 0 {
            return Ok(Vec::new());
        } else {
            return Err(Error::InvalidField {
                what: "bytes_field_len",
                value: 0,
            });
        }
    }
    // Check if comma-separated or whitespace-separated list of numbers
    if clean.contains(',') || (clean.contains(' ') && clean.split_whitespace().count() > 1) {
        let parts: Vec<&str> = if clean.contains(',') {
            clean.split(',').collect()
        } else {
            clean.split_whitespace().collect()
        };
        let mut bytes = Vec::with_capacity(parts.len());
        for p in parts {
            let p = p.trim();
            if p.is_empty() {
                continue;
            }
            let b = if let Some(hex) = p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")) {
                u8::from_str_radix(hex, 16)
                    .map_err(|e| Error::Malformed(format!("invalid byte hex {p:?}: {e}")))?
            } else if p.bytes().any(|b| matches!(b, b'a'..=b'f' | b'A'..=b'F')) {
                u8::from_str_radix(p, 16)
                    .map_err(|e| Error::Malformed(format!("invalid byte hex {p:?}: {e}")))?
            } else {
                p.parse::<u8>()
                    .map_err(|e| Error::Malformed(format!("invalid byte {p:?}: {e}")))?
            };
            bytes.push(b);
        }
        if bytes.len() != expected_size {
            return Err(Error::InvalidField {
                what: "bytes_field_len",
                value: bytes.len() as u64,
            });
        }
        return Ok(bytes);
    }

    // Hex string without separators, e.g. "0x01020304" or "01020304"
    let hex_candidate = clean
        .strip_prefix("0x")
        .or_else(|| clean.strip_prefix("0X"))
        .unwrap_or(clean);
    let hex_clean: String = hex_candidate.chars().filter(|c| !c.is_whitespace()).collect();
    if hex_clean.len() == expected_size * 2 && hex_clean.chars().all(|c| c.is_ascii_hexdigit()) {
        let mut bytes = Vec::with_capacity(expected_size);
        for chunk in hex_clean.as_bytes().as_chunks::<2>().0 {
            let s_chunk = std::str::from_utf8(chunk).map_err(|e| Error::Malformed(e.to_string()))?;
            let b = u8::from_str_radix(s_chunk, 16)
                .map_err(|e| Error::Malformed(format!("invalid hex byte {s_chunk:?}: {e}")))?;
            bytes.push(b);
        }
        return Ok(bytes);
    }

    // Direct ascii bytes fallback if length matches exactly
    if clean.len() == expected_size {
        return Ok(clean.as_bytes().to_vec());
    }

    Err(Error::Malformed(format!(
        "cannot parse {s:?} as {expected_size}-byte sequence (expected {} hex digits or comma-separated bytes)",
        expected_size * 2
    )))
}

/// Parse a string input into a typed [`FieldValue`] based on the target [`FieldSpec`].
///
/// Supports decimal integers, hex (`0x...` or direct hex digits), booleans (`true`/`false`),
/// quoted or unquoted ASCII strings, and hex/comma-separated byte arrays.
pub fn parse_field_value(spec: &FieldSpec, s: &str) -> Result<FieldValue> {
    let trimmed = s.trim();
    match spec.field_type {
        FieldType::U8 => {
            let val = parse_int_or_hex_or_bool::<u8>(trimmed)?;
            Ok(FieldValue::U8(val))
        }
        FieldType::U16Le => {
            let val = parse_int_or_hex_or_bool::<u16>(trimmed)?;
            Ok(FieldValue::U16(val))
        }
        FieldType::U32Le => {
            let val = parse_int_or_hex_or_bool::<u32>(trimmed)?;
            Ok(FieldValue::U32(val))
        }
        FieldType::I16Le => {
            let val = parse_signed_int_or_hex(trimmed)?;
            Ok(FieldValue::I16(val))
        }
        FieldType::Bitflags => {
            let val = parse_int_or_hex_or_bool::<u64>(trimmed)?;
            Ok(FieldValue::Bitflags(val))
        }
        FieldType::Ascii => {
            let mut val = trimmed;
            if val.len() >= 2
                && ((val.starts_with('"') && val.ends_with('"'))
                    || (val.starts_with('\'') && val.ends_with('\'')))
            {
                val = &val[1..val.len() - 1];
            }
            if val.len() > spec.size {
                return Err(Error::InvalidField {
                    what: "ascii_field_len",
                    value: val.len() as u64,
                });
            }
            Ok(FieldValue::Ascii(val.to_string()))
        }
        FieldType::Bytes => {
            let bytes = parse_bytes_literal(trimmed, spec.size)?;
            Ok(FieldValue::Bytes(bytes))
        }
        FieldType::BitfieldArray => {
            let clean = trimmed.trim_start_matches('[').trim_end_matches(']');
            let bit_width = spec
                .bitfield_array
                .as_ref()
                .map(|ba| ba.bit_width)
                .unwrap_or(1);
            if bit_width == 0 || bit_width > 32 {
                return Err(Error::InvalidField {
                    what: "bitfield_array_bit_width",
                    value: bit_width as u64,
                });
            }
            let count = match &spec.bitfield_array {
                Some(ba) => ba.count,
                None => {
                    spec.size.checked_mul(8).ok_or(Error::InvalidField {
                        what: "bitfield_array_size",
                        value: spec.size as u64,
                    })? / bit_width
                }
            };
            let mut values = Vec::with_capacity(count);
            if !clean.is_empty() {
                for part in clean.split(',') {
                    let p = part.trim();
                    if !p.is_empty() {
                        values.push(parse_int_or_hex_or_bool::<u32>(p)?);
                    }
                }
            }
            while values.len() < count {
                values.push(0);
            }
            Ok(FieldValue::BitfieldArray(values))
        }
    }
}

/// Parse a `serde_json::Value` into a typed `FieldValue` according to `spec`.
pub fn json_val_to_field_value(spec: &FieldSpec, val: &serde_json::Value) -> Result<FieldValue> {
    if let Ok(fv) = serde_json::from_value::<FieldValue>(val.clone()) {
        return Ok(fv);
    }
    if let serde_json::Value::Object(map) = val {
        if let Some(inner) = map.get("value") {
            return json_val_to_field_value(spec, inner);
        }
    }
    match val {
        serde_json::Value::Number(num) => {
            if let Some(u) = num.as_u64() {
                match spec.field_type {
                    FieldType::U8 => Ok(FieldValue::U8(u8::try_from(u).map_err(|_| {
                        Error::InvalidField {
                            what: "u8_field_value",
                            value: u,
                        }
                    })?)),
                    FieldType::U16Le => Ok(FieldValue::U16(u16::try_from(u).map_err(|_| {
                        Error::InvalidField {
                            what: "u16_field_value",
                            value: u,
                        }
                    })?)),
                    FieldType::U32Le => Ok(FieldValue::U32(u32::try_from(u).map_err(|_| {
                        Error::InvalidField {
                            what: "u32_field_value",
                            value: u,
                        }
                    })?)),
                    FieldType::I16Le => Ok(FieldValue::I16(i16::try_from(u).map_err(|_| {
                        Error::InvalidField {
                            what: "i16_field_value",
                            value: u,
                        }
                    })?)),
                    FieldType::Bitflags => Ok(FieldValue::Bitflags(u)),
                    _ => parse_field_value(spec, &u.to_string()),
                }
            } else if let Some(i) = num.as_i64() {
                match spec.field_type {
                    FieldType::I16Le => Ok(FieldValue::I16(i16::try_from(i).map_err(|_| {
                        Error::InvalidField {
                            what: "i16_field_value",
                            value: i as u64,
                        }
                    })?)),
                    _ => parse_field_value(spec, &i.to_string()),
                }
            } else {
                parse_field_value(spec, &num.to_string())
            }
        }
        serde_json::Value::String(s) => parse_field_value(spec, s),
        serde_json::Value::Bool(b) => parse_field_value(spec, if *b { "true" } else { "false" }),
        serde_json::Value::Array(arr) => {
            if spec.field_type == FieldType::BitfieldArray {
                let bit_width = spec
                    .bitfield_array
                    .as_ref()
                    .map(|ba| ba.bit_width)
                    .unwrap_or(1);
                if bit_width == 0 || bit_width > 32 {
                    return Err(Error::InvalidField {
                        what: "bitfield_array_bit_width",
                        value: bit_width as u64,
                    });
                }
                let count = match &spec.bitfield_array {
                    Some(ba) => ba.count,
                    None => {
                        spec.size.checked_mul(8).ok_or(Error::InvalidField {
                            what: "bitfield_array_size",
                            value: spec.size as u64,
                        })? / bit_width
                    }
                };
                let mut values = Vec::with_capacity(count);
                for item in arr.iter().take(count) {
                    if let Some(u) = item.as_u64() {
                        values.push(u32::try_from(u).map_err(|_| Error::InvalidField {
                            what: "bitfield_array_value",
                            value: u,
                        })?);
                    } else if let Some(b) = item.as_bool() {
                        values.push(if b { 1 } else { 0 });
                    } else if let Some(s) = item.as_str() {
                        let b = parse_int_or_hex_or_bool::<u32>(s)?;
                        values.push(b);
                    } else {
                        return Err(Error::InvalidField {
                            what: "array item not a valid bitfield value",
                            value: 0,
                        });
                    }
                }
                while values.len() < count {
                    values.push(0);
                }
                return Ok(FieldValue::BitfieldArray(values));
            }
            let mut bytes = Vec::with_capacity(arr.len());
            for item in arr {
                if let Some(byte) = item.as_u64() {
                    bytes.push(u8::try_from(byte).map_err(|_| Error::InvalidField {
                        what: "bytes_field_value",
                        value: byte,
                    })?);
                } else if let Some(s) = item.as_str() {
                    let b = parse_int_or_hex_or_bool::<u8>(s)?;
                    bytes.push(b);
                } else {
                    return Err(Error::InvalidField {
                        what: "array item not a byte",
                        value: 0,
                    });
                }
            }
            Ok(FieldValue::Bytes(bytes))
        }
        _ => Err(Error::Malformed(format!(
            "unsupported JSON value for field '{}': {:?}",
            spec.name, val
        ))),
    }
}

/// Export a save slice to readable JSON format according to `def`.
pub fn export_save_json(def: &SaveDefinition, data: &[u8]) -> Result<serde_json::Value> {
    let buf = SaveBuffer::from_bytes(data);
    buf.export_json(def)
}

/// Import a JSON value into an existing or fresh save buffer, recalculating all checksums.
pub fn import_save_json(
    def: &SaveDefinition,
    base_data: Option<&[u8]>,
    json: &serde_json::Value,
) -> Result<(Vec<u8>, Vec<ChecksumResult>)> {
    let mut buf = match base_data {
        Some(data) => SaveBuffer::from_bytes(data),
        None => SaveBuffer::create_from_json(def, json)?,
    };
    if base_data.is_some() {
        let results = buf.import_json(def, json)?;
        Ok((buf.into_bytes(), results))
    } else {
        let results = buf.recalculate_checksums(def)?;
        Ok((buf.into_bytes(), results))
    }
}

/// Evaluate whether `cond` is met in `data` against `def`.
pub fn evaluate_condition(cond: &FieldCondition, def: &SaveDefinition, data: &[u8]) -> bool {
    let target_spec = match def.fields.iter().find(|f| f.name == cond.field) {
        Some(s) => s,
        None => return false,
    };
    let target_val = match read_field(target_spec, data) {
        Ok(v) => v,
        Err(_) => return false,
    };

    let target_num: Option<i64> = match &target_val {
        FieldValue::U8(v) => Some(*v as i64),
        FieldValue::U16(v) => Some(*v as i64),
        FieldValue::U32(v) => Some(*v as i64),
        FieldValue::I16(v) => Some(*v as i64),
        FieldValue::Bitflags(v) => Some(*v as i64),
        _ => None,
    };

    let cond_num: Option<i64> = cond.value.as_i64().or_else(|| {
        cond.value.as_u64().map(|v| v as i64).or_else(|| {
            cond.value.as_bool().map(|b| if b { 1 } else { 0 })
        })
    });

    match cond.op {
        ConditionOp::Eq => match (&target_val, &cond.value) {
            (FieldValue::Ascii(s), serde_json::Value::String(expected)) => s == expected,
            _ => match (target_num, cond_num) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            },
        },
        ConditionOp::Ne => match (&target_val, &cond.value) {
            (FieldValue::Ascii(s), serde_json::Value::String(expected)) => s != expected,
            _ => match (target_num, cond_num) {
                (Some(a), Some(b)) => a != b,
                _ => true,
            },
        },
        ConditionOp::Gt => match (target_num, cond_num) {
            (Some(a), Some(b)) => a > b,
            _ => false,
        },
        ConditionOp::Gte => match (target_num, cond_num) {
            (Some(a), Some(b)) => a >= b,
            _ => false,
        },
        ConditionOp::Lt => match (target_num, cond_num) {
            (Some(a), Some(b)) => a < b,
            _ => false,
        },
        ConditionOp::Lte => match (target_num, cond_num) {
            (Some(a), Some(b)) => a <= b,
            _ => false,
        },
        ConditionOp::BitSet => match (target_num, cond_num) {
            (Some(a), Some(b)) => (a & b) != 0,
            _ => false,
        },
        ConditionOp::BitClear => match (target_num, cond_num) {
            (Some(a), Some(b)) => (a & b) == 0,
            _ => false,
        },
    }
}

/// In-memory mutable save buffer with high-level inspection, mutation, and checksum repair methods.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveBuffer {
    data: Vec<u8>,
}

impl SaveBuffer {
    /// Create a new `SaveBuffer` owning `data`.
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    /// Create a new `SaveBuffer` from a byte slice.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            data: bytes.to_vec(),
        }
    }

    /// Return an immutable slice reference to the underlying bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Return a mutable slice reference to the underlying bytes.
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Return an immutable slice reference to the underlying bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Consume this buffer and return the underlying `Vec<u8>`.
    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }

    /// Return the length of the buffer in bytes.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Return `true` if the buffer contains 0 bytes.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Canonical capacity slice for `def`.
    ///
    /// If `self.data.len() >= def.size_bytes`, returns the first `def.size_bytes`
    /// (ignoring any trailing emulator padding).
    /// Returns an error if the buffer is smaller than `def.size_bytes`.
    pub fn canonical_slice(&self, def: &SaveDefinition) -> Result<&[u8]> {
        if self.data.len() < def.size_bytes {
            return Err(Error::Truncated {
                offset: self.data.len(),
                needed: def.size_bytes - self.data.len(),
                available: 0,
            });
        }
        Ok(&self.data[..def.size_bytes])
    }

    /// Mutable canonical capacity slice for `def`.
    pub fn canonical_slice_mut(&mut self, def: &SaveDefinition) -> Result<&mut [u8]> {
        if self.data.len() < def.size_bytes {
            return Err(Error::Truncated {
                offset: self.data.len(),
                needed: def.size_bytes - self.data.len(),
                available: 0,
            });
        }
        Ok(&mut self.data[..def.size_bytes])
    }

    /// Apply a [`SaveDefinition`] to inspect all mapped fields with their decoded values
    /// and verification report.
    pub fn inspect(&self, def: &SaveDefinition) -> Result<DecodedSave> {
        def.validate()?;
        let slice = self.canonical_slice(def)?;
        decode_save(def, slice)
    }

    /// Validate all checksums and magic signatures of the save buffer against `def`.
    pub fn validate_checksums(&self, def: &SaveDefinition) -> Result<VerificationReport> {
        def.validate()?;
        let slice = self.canonical_slice(def)?;
        verify_save(def, slice)
    }

    /// Recompute and write all checksums declared in `def` into the save buffer.
    ///
    /// Returns the updated [`ChecksumResult`] for each checksum.
    pub fn recalculate_checksums(&mut self, def: &SaveDefinition) -> Result<Vec<ChecksumResult>> {
        def.validate()?;
        let slice = self.canonical_slice_mut(def)?;
        fix_checksums(def, slice)
    }

    /// Get the decoded value of a named field.
    pub fn get_field(&self, def: &SaveDefinition, field_name: &str) -> Result<FieldValue> {
        def.validate()?;
        let spec = def
            .fields
            .iter()
            .find(|f| f.name == field_name)
            .ok_or_else(|| Error::Malformed(format!("unknown field {field_name:?}")))?;
        let slice = self.canonical_slice(def)?;
        if def.eeprom_rev8 {
            let logical = to_rev8(slice);
            read_field(spec, &logical)
        } else {
            read_field(spec, slice)
        }
    }

    /// Set a named field by typed [`FieldValue`], validating its [`FieldConstraint`]
    /// if present, recalculating all covering checksums across slots automatically,
    /// and validating that all checksums match after modification.
    pub fn set_field(
        &mut self,
        def: &SaveDefinition,
        field_name: &str,
        value: FieldValue,
    ) -> Result<Vec<ChecksumResult>> {
        def.validate()?;
        let slice = self.canonical_slice_mut(def)?;
        set_field(def, slice, field_name, value, false)?;
        let results = fix_checksums(def, slice)?;
        let report = verify_save(def, slice)?;
        if !report.checksums.iter().all(|c| c.valid) {
            return Err(Error::Malformed(
                "checksum verification failed after modification: covering checksums do not match".into(),
            ));
        }
        Ok(results)
    }

    /// Set a named field by typed [`FieldValue`], validating its [`FieldConstraint`]
    /// (and those of any [`CopyGroup`] peers), atomically updating all mirror peers
    /// in any matching `copy_groups`, and recalculating + verifying all checksums.
    pub fn set_field_sync(
        &mut self,
        def: &SaveDefinition,
        field_name: &str,
        value: FieldValue,
    ) -> Result<Vec<ChecksumResult>> {
        def.validate()?;
        let targets: Vec<&str> = match def
            .copy_groups
            .iter()
            .find(|g| g.fields.iter().any(|f| f == field_name))
        {
            Some(group) => group.fields.iter().map(|s| s.as_str()).collect(),
            None => vec![field_name],
        };
        // Pre-validate every target field before touching `self.data` (atomic on error).
        for &target in &targets {
            let spec = def
                .fields
                .iter()
                .find(|f| f.name == target)
                .ok_or_else(|| Error::Malformed(format!("unknown field {target:?}")))?;
            validate_field_value(spec, &value)?;
        }

        let canonical = self.canonical_slice(def)?;
        let mut staged = if def.eeprom_rev8 {
            to_rev8(canonical)
        } else {
            canonical.to_vec()
        };

        for &target in &targets {
            set_field_logical(def, &mut staged, target, value.clone(), false, true)?;
        }
        let results = fix_checksums_logical(def, &mut staged)?;
        let report = verify_save_logical(def, &staged)?;
        if !report.checksums.iter().all(|c| c.valid) {
            return Err(Error::Malformed(
                "checksum verification failed after synced modification: covering checksums do not match".into(),
            ));
        }
        if def.eeprom_rev8 {
            reverse_eeprom_blocks_8(&mut staged);
        }
        let slice = self.canonical_slice_mut(def)?;
        slice.copy_from_slice(&staged);
        Ok(results)
    }

    /// Set a named field by typed [`FieldValue`] WITHOUT validating its [`FieldConstraint`].
    ///
    /// Intended for counterfactual boundary fuzzing (e.g. writing `max + 1`, invalid
    /// enum codes, unmasked bits, or malformed BCD nibbles) while still recalculating
    /// and verifying covering checksums.
    pub fn set_field_unchecked(
        &mut self,
        def: &SaveDefinition,
        field_name: &str,
        value: FieldValue,
    ) -> Result<Vec<ChecksumResult>> {
        def.validate()?;
        let canonical = self.canonical_slice(def)?;
        let mut staged = if def.eeprom_rev8 {
            to_rev8(canonical)
        } else {
            canonical.to_vec()
        };
        set_field_logical(def, &mut staged, field_name, value, false, false)?;
        let results = fix_checksums_logical(def, &mut staged)?;
        let report = verify_save_logical(def, &staged)?;
        if !report.checksums.iter().all(|c| c.valid) {
            return Err(Error::Malformed(
                "checksum verification failed after unchecked modification: covering checksums do not match".into(),
            ));
        }
        if def.eeprom_rev8 {
            reverse_eeprom_blocks_8(&mut staged);
        }
        let slice = self.canonical_slice_mut(def)?;
        slice.copy_from_slice(&staged);
        Ok(results)
    }

    /// Set a named field by string value (parsing int, hex, bool, string),
    /// recalculating all covering checksums across slots automatically,
    /// and validating that all checksums match after modification.
    pub fn set_field_str(
        &mut self,
        def: &SaveDefinition,
        field_name: &str,
        value_str: &str,
    ) -> Result<Vec<ChecksumResult>> {
        def.validate()?;
        let spec = def
            .fields
            .iter()
            .find(|f| f.name == field_name)
            .ok_or_else(|| Error::Malformed(format!("unknown field {field_name:?}")))?;
        let typed_value = parse_field_value(spec, value_str)?;
        self.set_field(def, field_name, typed_value)
    }

    /// Export the save buffer fields and format metadata to a readable JSON value.
    pub fn export_json(&self, def: &SaveDefinition) -> Result<serde_json::Value> {
        let decoded = self.inspect(def)?;
        let mut fields_map = serde_json::Map::new();
        for f in &decoded.fields {
            let json_val = match &f.value {
                FieldValue::U8(v) => serde_json::Value::from(*v),
                FieldValue::U16(v) => serde_json::Value::from(*v),
                FieldValue::U32(v) => serde_json::Value::from(*v),
                FieldValue::I16(v) => serde_json::Value::from(*v),
                FieldValue::Bitflags(v) => serde_json::Value::from(*v),
                FieldValue::Ascii(s) => serde_json::Value::from(s.clone()),
                FieldValue::Bytes(b) => serde_json::Value::Array(
                    b.iter().map(|byte| serde_json::Value::from(*byte)).collect(),
                ),
                FieldValue::BitfieldArray(arr) => serde_json::Value::Array(
                    arr.iter().map(|&v| serde_json::Value::from(v)).collect(),
                ),
            };
            fields_map.insert(f.name.clone(), json_val);
        }

        let slice = self.canonical_slice(def)?;
        let canonical_bytes = if def.eeprom_rev8 {
            to_rev8(slice)
        } else {
            slice.to_vec()
        };

        let fields_detail: Vec<serde_json::Value> = decoded
            .fields
            .iter()
            .map(|f| {
                let spec = def.fields.iter().find(|s| s.name == f.name);
                let type_name = spec
                    .map(|s| format!("{:?}", s.field_type).to_lowercase())
                    .unwrap_or_else(|| "unknown".into());
                let mut d = serde_json::json!({
                    "name": f.name,
                    "offset": f.offset,
                    "offset_hex": format!("0x{:04X}", f.offset),
                    "size": f.size,
                    "type": type_name,
                    "value": f.value,
                    "display": f.value.to_string(),
                });
                if let Some(s) = spec {
                    if let Some(ba) = &s.bitfield_array {
                        d["bitfield_array"] = serde_json::to_value(ba).unwrap_or_default();
                    }
                    if let Some(c) = &s.condition {
                        d["condition"] = serde_json::to_value(c).unwrap_or_default();
                        d["condition_met"] =
                            serde_json::Value::Bool(evaluate_condition(c, def, &canonical_bytes));
                    }
                    if let Some(constraint) = &s.constraint {
                        d["constraint"] = serde_json::to_value(constraint).unwrap_or_default();
                    }
                    if let Some(confidence) = &s.confidence {
                        d["confidence"] = serde_json::to_value(confidence).unwrap_or_default();
                    }
                    if let Some(evidence) = &s.evidence {
                        d["evidence"] = serde_json::Value::String(evidence.clone());
                    }
                }
                d
            })
            .collect();

        let checksum_items: Vec<serde_json::Value> = decoded
            .verification
            .checksums
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "algorithm": format!("{:?}", c.algorithm).to_lowercase(),
                    "stored_offset": format!("0x{:04X}", c.stored_offset),
                    "expected": format!("0x{:X}", c.expected),
                    "actual": format!("0x{:X}", c.actual),
                    "valid": c.valid,
                })
            })
            .collect();

        let val = serde_json::json!({
            "format": {
                "id": def.id,
                "game_code": def.game_code,
                "title": def.title,
                "media": def.media.as_str(),
                "canonical_size": def.size_bytes,
            },
            "fields": fields_map,
            "fields_detail": fields_detail,
            "verification": {
                "valid": decoded.verification.valid,
                "checksums": checksum_items,
            }
        });
        Ok(val)
    }

    /// Import fields from a JSON value into `self`, applying all matching fields
    /// and recalculating all covering slot checksums automatically.
    pub fn import_json(
        &mut self,
        def: &SaveDefinition,
        json: &serde_json::Value,
    ) -> Result<Vec<ChecksumResult>> {
        def.validate()?;
        if self.data.len() < def.size_bytes {
            let fill = match def.media {
                SaveMediaKind::Sram => 0x00,
                _ => 0xFF,
            };
            self.data.resize(def.size_bytes, fill);
        }

        let mut updates: Vec<(String, serde_json::Value)> = Vec::new();

        if let serde_json::Value::Object(root_map) = json {
            if let Some(serde_json::Value::Object(fields_map)) = root_map.get("fields") {
                for (k, v) in fields_map {
                    updates.push((k.clone(), v.clone()));
                }
            } else if let Some(serde_json::Value::Array(fields_arr)) = root_map.get("fields") {
                for item in fields_arr {
                    if let serde_json::Value::Object(item_map) = item {
                        if let Some(name) = item_map.get("name").and_then(|n| n.as_str()) {
                            if let Some(val) = item_map.get("value") {
                                updates.push((name.to_string(), val.clone()));
                            }
                        }
                    }
                }
            }

            if let Some(serde_json::Value::Array(detail_arr)) = root_map.get("fields_detail") {
                for item in detail_arr {
                    if let serde_json::Value::Object(item_map) = item {
                        if let Some(name) = item_map.get("name").and_then(|n| n.as_str()) {
                            if let Some(val) = item_map.get("value") {
                                if !updates.iter().any(|(k, _)| k == name) {
                                    updates.push((name.to_string(), val.clone()));
                                }
                            }
                        }
                    }
                }
            }

            for spec in &def.fields {
                if !updates.iter().any(|(k, _)| k == &spec.name) {
                    if let Some(val) = root_map.get(&spec.name) {
                        updates.push((spec.name.clone(), val.clone()));
                    }
                }
            }
        }

        let slice = self.canonical_slice_mut(def)?;
        for (field_name, field_val) in updates {
            if let Some(spec) = def.fields.iter().find(|f| f.name == field_name) {
                let typed_val = json_val_to_field_value(spec, &field_val)?;
                set_field(def, slice, &field_name, typed_val, false)?;
            }
        }

        let results = fix_checksums(def, slice)?;
        let report = verify_save(def, slice)?;
        if !report.checksums.iter().all(|c| c.valid) {
            return Err(Error::Malformed(
                "checksum verification failed after import: covering checksums do not match".into(),
            ));
        }

        Ok(results)
    }

    /// Create a new blank save buffer for `def`, writing magic signatures,
    /// applying `json` fields, and recalculating slot checksums.
    pub fn create_from_json(def: &SaveDefinition, json: &serde_json::Value) -> Result<Self> {
        def.validate()?;
        let fill = match def.media {
            SaveMediaKind::Sram => 0x00,
            _ => 0xFF,
        };
        let mut buf = vec![fill; def.size_bytes];
        for sig in &def.magic_signatures {
            let end = sig.offset + sig.bytes.len();
            if end <= buf.len() {
                buf[sig.offset..end].copy_from_slice(&sig.bytes);
            }
        }
        if def.eeprom_rev8 {
            reverse_eeprom_blocks_8(&mut buf);
        }
        let mut save_buf = SaveBuffer::new(buf);
        save_buf.import_json(def, json)?;
        Ok(save_buf)
    }
}

impl From<Vec<u8>> for SaveBuffer {
    fn from(data: Vec<u8>) -> Self {
        Self::new(data)
    }
}

impl From<&[u8]> for SaveBuffer {
    fn from(bytes: &[u8]) -> Self {
        Self::from_bytes(bytes)
    }
}

impl AsRef<[u8]> for SaveBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

impl AsMut<[u8]> for SaveBuffer {
    fn as_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl std::ops::Deref for SaveBuffer {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl std::ops::DerefMut for SaveBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_algorithms_match_known_vectors() {
        let data = b"123456789";
        // Sum of ASCII '1'..'9' = 49+50+...+57 = 477 = 0x01DD
        assert_eq!(ChecksumAlgorithm::Sum8.compute(data, None, None), 0xDD);
        assert_eq!(ChecksumAlgorithm::Sum8Inv.compute(data, None, None), !0xDDu8 as u32);
        assert_eq!(
            ChecksumAlgorithm::Sum8Neg.compute(data, None, None),
            0u8.wrapping_sub(0xDD) as u32
        );
        assert_eq!(ChecksumAlgorithm::Sum16Le.compute(data, None, None), 0x01DD);
        assert_eq!(ChecksumAlgorithm::Sum16Be.compute(data, None, None), 0x01DD);
        assert_eq!(
            ChecksumAlgorithm::Sum16LeInv.compute(data, None, None),
            (!0x01DDu16) as u32
        );
        assert_eq!(ChecksumAlgorithm::Sum32Le.compute(data, None, None), 0x01DD);

        // Word-wise u32_le sum over 8 bytes
        let words = [1u8, 0, 0, 0, 2, 0, 0, 0];
        assert_eq!(ChecksumAlgorithm::Sum32LeWords.compute(&words, None, None), 3);

        // XOR8, XOR16_LE, XOR32_LE, SUM_XOR16_LE, ROL32_ADD_LE
        assert_eq!(ChecksumAlgorithm::Xor8.compute(&[0xAA, 0x55, 0x0F], None, None), 0xF0);
        assert_eq!(
            ChecksumAlgorithm::Xor16Le.compute(&[0x34, 0x12, 0x04, 0x02], None, None),
            0x1030
        );
        assert_eq!(
            ChecksumAlgorithm::Xor32Le.compute(
                &[0x01, 0x02, 0x03, 0x04, 0x10, 0x20, 0x30, 0x40],
                None,
                Some(0xABCD_1234)
            ),
            (0x0403_0201u32 ^ 0x4030_2010u32 ^ 0xABCD_1234u32)
        );
        // SumXor16Le: words 0x0201, 0x0403 -> sum=0x0604, xor=0x0602, sum+xor=0x0C06
        assert_eq!(
            ChecksumAlgorithm::SumXor16Le.compute(&[0x01, 0x02, 0x03, 0x04], None, None),
            0x0C06
        );
        // Rol32AddLe: seed=0xAA55_A5A5, bytes=[0x01, 0x02]
        let expected_rol = 0xAA55_A5A5u32
            .rotate_left(1)
            .wrapping_add(1)
            .rotate_left(1)
            .wrapping_add(2);
        assert_eq!(
            ChecksumAlgorithm::Rol32AddLe.compute(&[0x01, 0x02], Some(0xAA55_A5A5), None),
            expected_rol
        );

        // Standard CRC-16 check values for "123456789"
        assert_eq!(ChecksumAlgorithm::Crc16Arc.compute(data, None, None), 0xBB3D);
        assert_eq!(ChecksumAlgorithm::Crc16Kermit.compute(data, None, None), 0x2189);
        assert_eq!(ChecksumAlgorithm::Crc16X25.compute(data, None, None), 0x906E);
        assert_eq!(ChecksumAlgorithm::Crc16Buypass.compute(data, None, None), 0xFEE8);
        assert_eq!(ChecksumAlgorithm::Crc16Xmodem.compute(data, None, None), 0x31C3);

        // Nintendo GBA Classic NES Series / Famicom Mini CRC-16 (poly=0xD0A2, init=0, +1 zero u32)
        // Verified against live EEPROM dumps:
        //   FSME (Super Mario Bros., 0x18B high-score record) -> stored 0xF6CD
        let fsme_slot: [u8; 24] = [
            0x03, 0x16, 0x18, 0x31, 0x52, 0x49, 0x4F, 0x5F,
            0x34, 0x5F, 0x32, 0x50, 0x02, 0x01, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(ChecksumAlgorithm::Crc16NesGba.compute(&fsme_slot, None, None), 0xF6CD);

        //   FNMJ (Nazo no Murasame Jou, 0x50B FDS record) -> stored 0x7154
        let mut fnmj_slot: [u8; 80] = [0x00; 80];
        fnmj_slot[0..16].copy_from_slice(&[
            0x06, 0x27, 0x02, 0x52, 0x4D, 0x55, 0x52, 0x41,
            0x53, 0x41, 0x4D, 0x45, 0x02, 0x01, 0x54, 0x71,
        ]);
        fnmj_slot[16..20].copy_from_slice(&[0x01, 0x00, 0x00, 0x0A]);
        fnmj_slot[20..43].fill(0xFF);
        fnmj_slot[76..80].copy_from_slice(&[0xA5, 0x5A, 0xFF, 0xFF]);
        let fnmj_spec = ChecksumSpec {
            name: "slot_1_crc16".into(),
            algorithm: ChecksumAlgorithm::Crc16NesGba,
            covered_start: 0,
            covered_end: 80,
            stored_offset: 14,
            seed: None,
            xor_out: None,
        };
        assert_eq!(fnmj_spec.compute_over(&fnmj_slot).unwrap(), 0x7154);

        // Optional seed & xor_out overrides
        assert_eq!(
            ChecksumAlgorithm::Sum8.compute(&[1, 2, 3], Some(10), Some(0xFF)),
            u32::from((16u8) ^ 0xFF)
        );
        assert_eq!(
            ChecksumAlgorithm::Crc16Arc.compute(b"", Some(0x1234), Some(0x00FF)),
            0x12CB
        );
    }

    fn sample_definition() -> SaveDefinition {
        SaveDefinition::from_json(
            r#"{
                "id": "synthetic_sram_test",
                "game_code": "TST1",
                "title": "Synthetic Test Save",
                "media": "sram",
                "size_bytes": 64,
                "magic_signatures": [
                    { "offset": 0, "bytes": [71, 66, 65, 83], "description": "GBAS magic" }
                ],
                "checksums": [
                    {
                        "name": "header_sum8_inv",
                        "algorithm": "sum8_inv",
                        "covered_start": 0,
                        "covered_end": 16,
                        "stored_offset": 15
                    },
                    {
                        "name": "payload_crc16_x25",
                        "algorithm": "crc16_x25",
                        "covered_start": 16,
                        "covered_end": 62,
                        "stored_offset": 62
                    }
                ],
                "fields": [
                    { "name": "magic", "offset": 0, "size": 4, "type": "ascii" },
                    { "name": "slot_flags", "offset": 4, "size": 1, "type": "u8" },
                    { "name": "level", "offset": 6, "size": 2, "type": "u16_le" },
                    { "name": "score", "offset": 8, "size": 4, "type": "u32_le" },
                    { "name": "temp_delta", "offset": 12, "size": 2, "type": "i16_le" },
                    { "name": "unlocks", "offset": 16, "size": 4, "type": "bitflags" },
                    { "name": "player_name", "offset": 20, "size": 8, "type": "ascii" },
                    { "name": "badge_bytes", "offset": 28, "size": 4, "type": "bytes" }
                ]
            }"#,
        )
        .expect("valid sample definition")
    }

    #[test]
    fn synthetic_round_trip_corrupt_and_repair() {
        let def = sample_definition();
        let mut buf = vec![0u8; def.size_bytes];
        buf[0..4].copy_from_slice(b"GBAS");

        // Initially checksums are 0 so verification fails
        let report_before = verify_save(&def, &buf).unwrap();
        assert!(!report_before.valid);
        assert!(report_before.magics[0].valid);

        // Repair checksums -> now valid
        fix_checksums(&def, &mut buf).unwrap();
        let report_ok = verify_save(&def, &buf).unwrap();
        assert!(report_ok.valid);
        assert!(report_ok.checksums.iter().all(|c| c.valid));

        // Corrupt a payload byte -> CRC fails while header sum8_inv stays valid
        buf[40] ^= 0x80;
        let report_corrupt = verify_save(&def, &buf).unwrap();
        assert!(!report_corrupt.valid);
        assert!(report_corrupt.checksums[0].valid);
        assert!(!report_corrupt.checksums[1].valid);

        // Repair again -> valid
        fix_checksums(&def, &mut buf).unwrap();
        assert!(verify_save(&def, &buf).unwrap().valid);
    }

    #[test]
    fn set_field_all_types_with_and_without_repair() {
        let def = sample_definition();
        let mut buf = vec![0u8; def.size_bytes];
        buf[0..4].copy_from_slice(b"GBAS");
        fix_checksums(&def, &mut buf).unwrap();

        set_field(&def, &mut buf, "slot_flags", FieldValue::U8(0x03), true).unwrap();
        set_field(&def, &mut buf, "level", FieldValue::U16(42), true).unwrap();
        set_field(&def, &mut buf, "score", FieldValue::U32(999_999), true).unwrap();
        set_field(&def, &mut buf, "temp_delta", FieldValue::I16(-123), true).unwrap();
        set_field(&def, &mut buf, "unlocks", FieldValue::Bitflags(0xDEAD_BEEF), true).unwrap();
        set_field(
            &def,
            &mut buf,
            "player_name",
            FieldValue::Ascii("HERO".to_string()),
            true,
        )
        .unwrap();
        set_field(
            &def,
            &mut buf,
            "badge_bytes",
            FieldValue::Bytes(vec![1, 2, 3, 4]),
            true,
        )
        .unwrap();

        let decoded = decode_save(&def, &buf).unwrap();
        assert!(decoded.verification.valid);
        let find = |name: &str| {
            decoded
                .fields
                .iter()
                .find(|f| f.name == name)
                .unwrap()
                .value
                .clone()
        };
        assert_eq!(find("magic"), FieldValue::Ascii("GBAS".into()));
        assert_eq!(find("slot_flags"), FieldValue::U8(0x03));
        assert_eq!(find("level"), FieldValue::U16(42));
        assert_eq!(find("score"), FieldValue::U32(999_999));
        assert_eq!(find("temp_delta"), FieldValue::I16(-123));
        assert_eq!(find("unlocks"), FieldValue::Bitflags(0xDEAD_BEEF));
        assert_eq!(find("player_name"), FieldValue::Ascii("HERO".into()));
        assert_eq!(find("badge_bytes"), FieldValue::Bytes(vec![1, 2, 3, 4]));

        // Mutate without repair -> checksum becomes invalid
        set_field(&def, &mut buf, "score", FieldValue::U32(1_000_000), false).unwrap();
        assert!(!verify_save(&def, &buf).unwrap().valid);

        // Repair restores validity
        fix_checksums(&def, &mut buf).unwrap();
        assert!(verify_save(&def, &buf).unwrap().valid);
    }

    #[test]
    fn self_covering_sum16_be_round_trip() {
        let def = SaveDefinition {
            id: "self_covering_test".into(),
            game_code: "SCV1".into(),
            title: "Self-Covering Header".into(),
            media: SaveMediaKind::Eeprom512,
            size_bytes: 512,
            eeprom_rev8: false,
            magic_signatures: vec![],
            checksums: vec![ChecksumSpec {
                name: "hdr_be".into(),
                algorithm: ChecksumAlgorithm::Sum16Be,
                covered_start: 0,
                covered_end: 32,
                stored_offset: 6,
                seed: None,
                xor_out: None,
            }],
            fields: vec![],
            copy_groups: vec![],
        };
        let mut buf = vec![0x11u8; 512];
        fix_checksums(&def, &mut buf).unwrap();
        // 30 non-zero bytes * 0x11 = 510 = 0x01FE, stored big-endian at [6..8]
        assert_eq!(&buf[6..8], &[0x01, 0xFE]);
        assert!(verify_save(&def, &buf).unwrap().valid);
    }

    #[test]
    fn new_word_and_be_checksum_algorithms() {
        // 4 bytes: LE u16 words are 0x0201 + 0x0403 = 0x0604
        //          BE u16 words are 0x0102 + 0x0304 = 0x0406
        let b4 = [0x01u8, 0x02, 0x03, 0x04];
        assert_eq!(ChecksumAlgorithm::Sum16LeWords.compute(&b4, None, None), 0x0604);
        assert_eq!(ChecksumAlgorithm::Sum16BeWords.compute(&b4, None, None), 0x0406);
        assert_eq!(
            ChecksumAlgorithm::Sum16LeNeg.compute(&b4, None, None),
            u32::from(0x0604u16.wrapping_neg())
        );
        assert_eq!(
            ChecksumAlgorithm::Sum16BeNeg.compute(&b4, None, None),
            u32::from(0x0406u16.wrapping_neg())
        );
        assert_eq!(
            ChecksumAlgorithm::Sum16BeInv.compute(&b4, None, None),
            u32::from(!0x0406u16)
        );
        assert_eq!(ChecksumAlgorithm::Sum32Be.compute(&b4, None, None), 10);

        // 8 bytes: LE u32 words are 0x04030201 + 0x08070605 = 0x0C0A0806
        let b8 = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08];
        assert_eq!(
            ChecksumAlgorithm::Sum32LeNeg.compute(&b8, None, None),
            0x0C0A_0806u32.wrapping_neg()
        );

        // Verify big-endian vs little-endian stored byte order
        let mut storage = [0u8; 4];
        ChecksumAlgorithm::Sum16BeNeg
            .write_stored(&mut storage, 0, 0xABCD)
            .unwrap();
        assert_eq!(&storage[0..2], &[0xAB, 0xCD]);
        assert_eq!(
            ChecksumAlgorithm::Sum16BeNeg.read_stored(&storage, 0).unwrap(),
            0xABCD
        );

        ChecksumAlgorithm::Sum16LeNeg
            .write_stored(&mut storage, 0, 0xABCD)
            .unwrap();
        assert_eq!(&storage[0..2], &[0xCD, 0xAB]);
        assert_eq!(
            ChecksumAlgorithm::Sum16LeNeg.read_stored(&storage, 0).unwrap(),
            0xABCD
        );

        ChecksumAlgorithm::Sum32Be
            .write_stored(&mut storage, 0, 0x0102_0304)
            .unwrap();
        assert_eq!(&storage, &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(
            ChecksumAlgorithm::Sum32Be.read_stored(&storage, 0).unwrap(),
            0x0102_0304
        );

        // Self-covering sum16_be_neg makes the u16_be word sum of the covered region 0
        let def = SaveDefinition {
            id: "zero_sum_be_test".into(),
            game_code: "ZSB1".into(),
            title: "Zero Sum BE Test".into(),
            media: SaveMediaKind::Eeprom512,
            size_bytes: 512,
            eeprom_rev8: false,
            magic_signatures: vec![],
            checksums: vec![ChecksumSpec {
                name: "hdr_neg".into(),
                algorithm: ChecksumAlgorithm::Sum16BeNeg,
                covered_start: 0,
                covered_end: 16,
                stored_offset: 14,
                seed: None,
                xor_out: None,
            }],
            fields: vec![],
            copy_groups: vec![],
        };
        let mut buf = vec![0u8; 512];
        buf[0..14].copy_from_slice(b"TESTHEADER1234");
        fix_checksums(&def, &mut buf).unwrap();
        assert!(verify_save(&def, &buf).unwrap().valid);
        let word_sum: u16 = buf[0..16]
            .as_chunks::<2>()
            .0
            .iter()
            .fold(0u16, |acc, &c| acc.wrapping_add(u16::from_be_bytes(c)));
        assert_eq!(word_sum, 0);
    }

    #[test]
    fn eeprom_rev8_round_trip_corrupt_and_repair() {
        let def = SaveDefinition::from_json(
            r#"{
                "id": "synthetic_eeprom_rev8",
                "game_code": "REV8",
                "title": "Synthetic EEPROM rev8 Save",
                "media": "eeprom512",
                "size_bytes": 512,
                "eeprom_rev8": true,
                "magic_signatures": [
                    { "offset": 0, "bytes": [23], "description": "Header magic byte" }
                ],
                "checksums": [
                    {
                        "name": "header_sum16_be",
                        "algorithm": "sum16_be",
                        "covered_start": 3,
                        "covered_end": 64,
                        "stored_offset": 1
                    },
                    {
                        "name": "slot_crc16_xmodem",
                        "algorithm": "crc16_xmodem",
                        "covered_start": 64,
                        "covered_end": 128,
                        "stored_offset": 128
                    }
                ],
                "fields": [
                    { "name": "magic", "offset": 0, "size": 1, "type": "u8" },
                    { "name": "score", "offset": 8, "size": 4, "type": "u32_le" },
                    { "name": "tag", "offset": 64, "size": 4, "type": "ascii" }
                ]
            }"#,
        )
        .expect("valid rev8 definition");
        assert!(def.eeprom_rev8);

        // Start from a raw physical EEPROM image (8-byte blocks reversed)
        let mut raw = vec![0u8; 512];
        // Logical offset 0 lives at physical offset 7 in block 0 (0..8)
        raw[7] = 23;

        // Before fixing checksums, magic passes but checksums fail (or need fixing)
        set_field(&def, &mut raw, "score", FieldValue::U32(0x1122_3344), false).unwrap();
        set_field(
            &def,
            &mut raw,
            "tag",
            FieldValue::Ascii("GBA!".into()),
            false,
        )
        .unwrap();
        // Verify physical bytes in block 1 (bytes 8..16): logical [8..12] = [0x44, 0x33, 0x22, 0x11]
        // maps to physical [15, 14, 13, 12]
        assert_eq!(&raw[12..16], &[0x11, 0x22, 0x33, 0x44]);

        assert!(!verify_save(&def, &raw).unwrap().valid);

        // Repair checksums -> valid
        fix_checksums(&def, &mut raw).unwrap();
        let report_ok = verify_save(&def, &raw).unwrap();
        assert!(report_ok.valid);
        assert!(report_ok.magics[0].valid);
        assert!(report_ok.checksums.iter().all(|c| c.valid));

        // Decode reads logical fields accurately
        let decoded = decode_save(&def, &raw).unwrap();
        assert!(decoded.verification.valid);
        assert_eq!(decoded.fields[0].value, FieldValue::U8(23));
        assert_eq!(decoded.fields[1].value, FieldValue::U32(0x1122_3344));
        assert_eq!(decoded.fields[2].value, FieldValue::Ascii("GBA!".into()));

        // Corrupt a physical byte inside block 8 (logical 64..72) -> CRC fails, header sum stays valid
        raw[68] ^= 0x01;
        let report_corrupt = verify_save(&def, &raw).unwrap();
        assert!(!report_corrupt.valid);
        assert!(report_corrupt.checksums[0].valid);
        assert!(!report_corrupt.checksums[1].valid);

        // Repair restores validity
        fix_checksums(&def, &mut raw).unwrap();
        assert!(verify_save(&def, &raw).unwrap().valid);

        // set_field with repair=true keeps the save valid
        set_field(&def, &mut raw, "score", FieldValue::U32(0xDEAD_BEEF), true).unwrap();
        let decoded2 = decode_save(&def, &raw).unwrap();
        assert!(decoded2.verification.valid);
        assert_eq!(decoded2.fields[1].value, FieldValue::U32(0xDEAD_BEEF));
    }

    #[test]
    fn media_default_sizes() {
        assert_eq!(SaveMediaKind::Sram.default_size(), 32_768);
        assert_eq!(SaveMediaKind::Flash512.default_size(), 65_536);
        assert_eq!(SaveMediaKind::Flash1m.default_size(), 131_072);
        assert_eq!(SaveMediaKind::Eeprom512.default_size(), 512);
        assert_eq!(SaveMediaKind::Eeprom8k.default_size(), 8_192);
    }

    #[test]
    fn detects_blank_save_and_padding_classification() {
        // Cleared SRAM: all 0x00
        let sram_zero = vec![0x00u8; 32_768];
        assert!(is_blank_save(&sram_zero, 512));
        assert_eq!(
            classify_save_padding(&sram_zero, 512),
            Some(PaddingSummary {
                kind: PaddingKind::AllZero,
                byte_len: 32_768,
            })
        );

        // Erased Flash: all 0xFF
        let flash_ff = vec![0xFFu8; 65_536];
        assert!(is_blank_save(&flash_ff, 512));
        assert_eq!(
            classify_save_padding(&flash_ff, 512),
            Some(PaddingSummary {
                kind: PaddingKind::RepeatedByte(0xFF),
                byte_len: 65_536,
            })
        );

        // Active save with data: not blank
        let mut active_save = vec![0x00u8; 512];
        active_save[10] = 0x42;
        assert!(!is_blank_save(&active_save, 64));
        assert_eq!(classify_save_padding(&active_save, 64), None);
    }

    #[test]
    fn test_padded_512b_eeprom_detection_and_trimming_ff() {
        let mut native_512 = vec![0xFFu8; 512];
        native_512[0..4].copy_from_slice(b"SAVE");
        native_512[16] = 0x42;

        let mut padded_8k = native_512.clone();
        padded_8k.resize(8_192, 0xFF);

        let geom = SaveGeometry::detect(&padded_8k);
        assert_eq!(geom.media, SaveMediaKind::Eeprom512);
        assert_eq!(geom.raw_size, 8_192);
        assert_eq!(geom.detected_canonical_size, 512);
        assert!(geom.has_trailing_padding);
        assert_eq!(geom.padding_byte, Some(0xFF));

        let trimmed = trim_to_canonical(&padded_8k, 512).expect("trim 0xFF padding should succeed");
        assert_eq!(trimmed.len(), 512);
        assert_eq!(trimmed, native_512);
    }

    #[test]
    fn test_padded_512b_eeprom_detection_and_trimming_00() {
        let mut native_512 = vec![0x00u8; 512];
        native_512[0..4].copy_from_slice(b"DATA");
        native_512[100] = 0x77;

        let mut padded_64k = native_512.clone();
        padded_64k.resize(65_536, 0x00);

        let geom = SaveGeometry::detect(&padded_64k);
        assert_eq!(geom.media, SaveMediaKind::Eeprom512);
        assert_eq!(geom.raw_size, 65_536);
        assert_eq!(geom.detected_canonical_size, 512);
        assert!(geom.has_trailing_padding);
        assert_eq!(geom.padding_byte, Some(0x00));

        let trimmed = trim_to_canonical(&padded_64k, 512).expect("trim 0x00 padding should succeed");
        assert_eq!(trimmed.len(), 512);
        assert_eq!(trimmed, native_512);
    }

    #[test]
    fn test_trim_protection_against_truncating_non_padding_data() {
        let mut data = vec![0xFFu8; 1024];
        data[0..4].copy_from_slice(b"TEST");
        data[600] = 0x55;

        let err = trim_to_canonical(&data, 512);
        assert!(err.is_err(), "should return error when trailing bytes contain data");

        let err_forced_false = trim_to_canonical_with_force(&data, 512, false);
        assert!(err_forced_false.is_err());

        let forced = trim_to_canonical_with_force(&data, 512, true).expect("force trim should succeed");
        assert_eq!(forced.len(), 512);
        assert_eq!(&forced[0..4], b"TEST");
    }

    #[test]
    fn test_save_title_probe_matching_synthetic_valid_and_invalid() {
        let def = sample_definition();
        let defs = vec![def.clone()];

        // 1. Synthetic valid save (magic + checksum both valid), padded from 64 to 8192 bytes with 0xFF
        let mut valid_save = vec![0u8; 64];
        valid_save[0..4].copy_from_slice(b"GBAS");
        fix_checksums(&def, &mut valid_save).unwrap();
        assert!(verify_save(&def, &valid_save).unwrap().valid);

        let mut padded_valid = valid_save.clone();
        padded_valid.resize(8_192, 0xFF);

        let matches = probe_loose_save(&padded_valid, &defs);
        assert_eq!(matches.len(), 1);
        let m = &matches[0];
        assert_eq!(m.def.id, "synthetic_sram_test");
        assert_eq!(m.def.game_code, "TST1");
        assert_eq!(m.confidence, MatchConfidence::Exact);
        assert!(m.magic_matched);
        assert!(m.checksums_valid);
        assert_eq!(m.geometry.raw_size, 8_192);
        assert_eq!(m.geometry.detected_canonical_size, 64);
        assert!(m.geometry.has_trailing_padding);
        assert_eq!(m.geometry.padding_byte, Some(0xFF));

        // 2. Synthetic invalid save (neither magic nor checksum matches)
        let invalid_save = vec![0xEEu8; 8_192];
        let no_matches = probe_loose_save(&invalid_save, &defs);
        assert!(no_matches.is_empty(), "invalid save should yield 0 probe matches");

        // 3. Magic-only match (magic matches, but all checksums are corrupted)
        let mut magic_only = valid_save.clone();
        magic_only[5] ^= 0x55;  // Corrupt byte in header range (0..16) -> header_sum8_inv fails
        magic_only[40] ^= 0x55; // Corrupt byte in payload range (16..62) -> payload_crc16_x25 fails
        let magic_matches = probe_loose_save(&magic_only, &defs);
        assert_eq!(magic_matches.len(), 1);
        assert_eq!(magic_matches[0].confidence, MatchConfidence::MagicOnly);
        assert!(magic_matches[0].magic_matched);
        assert!(!magic_matches[0].checksums_valid);

        // 4. Checksum-only match (corrupt magic, but repair checksums)
        let mut cs_only = valid_save.clone();
        cs_only[0..4].copy_from_slice(b"XXXX");
        fix_checksums(&def, &mut cs_only).unwrap();
        let cs_matches = probe_loose_save(&cs_only, &defs);
        assert_eq!(cs_matches.len(), 1);
        assert_eq!(cs_matches[0].confidence, MatchConfidence::ChecksumOnly);
        assert!(!cs_matches[0].magic_matched);
        assert!(cs_matches[0].checksums_valid);
    }

    #[test]
    fn test_probe_eeprom_rev8_padded_save() {
        let def = SaveDefinition::from_json(
            r#"{
                "id": "synthetic_eeprom_rev8",
                "game_code": "REV8",
                "title": "Synthetic EEPROM rev8 Save",
                "media": "eeprom512",
                "size_bytes": 512,
                "eeprom_rev8": true,
                "magic_signatures": [
                    { "offset": 0, "bytes": [23], "description": "Header magic byte" }
                ],
                "checksums": [
                    {
                        "name": "header_sum16_be",
                        "algorithm": "sum16_be",
                        "covered_start": 3,
                        "covered_end": 64,
                        "stored_offset": 1
                    }
                ],
                "fields": []
            }"#,
        )
        .expect("valid rev8 definition");

        let mut raw = vec![0u8; 512];
        raw[7] = 23; // Logical offset 0 lives at physical offset 7
        fix_checksums(&def, &mut raw).unwrap();
        assert!(verify_save(&def, &raw).unwrap().valid);

        // Pad to 8KB with 0xFF
        let mut padded = raw.clone();
        padded.resize(8_192, 0xFF);

        let defs = [def];
        let matches = probe_loose_save(&padded, &defs);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].confidence, MatchConfidence::Exact);
        assert!(matches[0].magic_matched);
        assert!(matches[0].checksums_valid);
        assert_eq!(matches[0].geometry.raw_size, 8_192);
        assert_eq!(matches[0].geometry.detected_canonical_size, 512);
        assert!(matches[0].geometry.has_trailing_padding);
        assert_eq!(matches[0].geometry.padding_byte, Some(0xFF));
    }

    #[test]
    fn test_save_buffer_inspect_and_mutate_all_field_types() {
        let def = sample_definition();
        let mut raw = vec![0u8; def.size_bytes];
        raw[0..4].copy_from_slice(b"GBAS");
        fix_checksums(&def, &mut raw).unwrap();

        let mut buf = SaveBuffer::new(raw);
        assert_eq!(buf.len(), 64);
        assert!(!buf.is_empty());

        // Validate initial checksums
        let report = buf.validate_checksums(&def).unwrap();
        assert!(report.valid);
        assert!(report.checksums.iter().all(|c| c.valid));

        // Inspect all fields
        let decoded = buf.inspect(&def).unwrap();
        assert!(decoded.verification.valid);
        assert_eq!(decoded.fields.len(), 8);

        // Mutate fields by string value:
        // 1. level (u16_le): decimal string
        let cs = buf.set_field_str(&def, "level", "99").unwrap();
        assert!(!cs.is_empty());
        assert_eq!(buf.get_field(&def, "level").unwrap(), FieldValue::U16(99));

        // 2. score (u32_le): hex string
        buf.set_field_str(&def, "score", "0x000F4240").unwrap();
        assert_eq!(buf.get_field(&def, "score").unwrap(), FieldValue::U32(1_000_000));

        // 3. temp_delta (i16_le): negative decimal
        buf.set_field_str(&def, "temp_delta", "-50").unwrap();
        assert_eq!(buf.get_field(&def, "temp_delta").unwrap(), FieldValue::I16(-50));

        // 4. slot_flags (u8): bool string "true" -> 1
        buf.set_field_str(&def, "slot_flags", "true").unwrap();
        assert_eq!(buf.get_field(&def, "slot_flags").unwrap(), FieldValue::U8(1));

        // 5. player_name (ascii): string with quotes
        buf.set_field_str(&def, "player_name", "\"LINK\"").unwrap();
        assert_eq!(
            buf.get_field(&def, "player_name").unwrap(),
            FieldValue::Ascii("LINK".to_string())
        );

        // 6. badge_bytes (bytes): hex string
        buf.set_field_str(&def, "badge_bytes", "0xCAFEBABE").unwrap();
        assert_eq!(
            buf.get_field(&def, "badge_bytes").unwrap(),
            FieldValue::Bytes(vec![0xCA, 0xFE, 0xBA, 0xBE])
        );

        // 7. badge_bytes (bytes): comma-separated integers
        buf.set_field_str(&def, "badge_bytes", "10, 20, 30, 40").unwrap();
        assert_eq!(
            buf.get_field(&def, "badge_bytes").unwrap(),
            FieldValue::Bytes(vec![10, 20, 30, 40])
        );

        // Verify that after all modifications, checksums remain completely valid
        let final_report = buf.validate_checksums(&def).unwrap();
        assert!(final_report.valid);
        assert!(final_report.checksums.iter().all(|c| c.valid));

        // Test display formatting of FieldValue
        let val_u16 = FieldValue::U16(99);
        assert_eq!(val_u16.to_string(), "99 (0x0063)");
        let val_ascii = FieldValue::Ascii("LINK".into());
        assert_eq!(val_ascii.to_string(), "\"LINK\"");
    }

    #[test]
    fn test_save_buffer_with_trailing_padding() {
        let def = sample_definition();
        let mut raw = vec![0u8; def.size_bytes];
        raw[0..4].copy_from_slice(b"GBAS");
        fix_checksums(&def, &mut raw).unwrap();

        // Pad to 8192 bytes
        raw.resize(8_192, 0xFF);
        let mut buf = SaveBuffer::from_bytes(&raw);

        // Inspect works on canonical portion
        let decoded = buf.inspect(&def).unwrap();
        assert!(decoded.verification.valid);

        // Mutating a field recalculates checksums and validates
        buf.set_field_str(&def, "level", "50").unwrap();
        assert_eq!(buf.get_field(&def, "level").unwrap(), FieldValue::U16(50));

        // Trailing padding past 64 bytes is preserved
        assert_eq!(buf.len(), 8_192);
        assert!(buf[64..].iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn test_save_buffer_export_and_import_json_round_trip() {
        let def = sample_definition();
        let mut raw = vec![0u8; def.size_bytes];
        raw[0..4].copy_from_slice(b"GBAS");
        raw[4] = 1; // slot_flags
        raw[6..8].copy_from_slice(&99u16.to_le_bytes()); // level
        raw[8..12].copy_from_slice(&100_000u32.to_le_bytes()); // score
        fix_checksums(&def, &mut raw).unwrap();

        let buf = SaveBuffer::from_bytes(&raw);
        let exported = buf.export_json(&def).unwrap();

        assert_eq!(exported["format"]["id"], "synthetic_sram_test");
        assert_eq!(exported["fields"]["level"], 99);
        assert_eq!(exported["fields"]["score"], 100_000);
        assert_eq!(exported["verification"]["valid"], true);

        // Mutate fields in JSON
        let mut mutated_json = exported.clone();
        mutated_json["fields"]["level"] = serde_json::json!(100);
        mutated_json["fields"]["score"] = serde_json::json!(999_999);

        // Import into new buffer from template
        let created_buf = SaveBuffer::create_from_json(&def, &mutated_json).unwrap();
        assert_eq!(created_buf.get_field(&def, "level").unwrap(), FieldValue::U16(100));
        assert_eq!(created_buf.get_field(&def, "score").unwrap(), FieldValue::U32(999_999));
        assert!(created_buf.validate_checksums(&def).unwrap().valid);

        // Round-trip export of the newly created buffer
        let re_exported = created_buf.export_json(&def).unwrap();
        assert_eq!(re_exported["fields"]["level"], 100);
        assert_eq!(re_exported["fields"]["score"], 999_999);
        assert_eq!(re_exported["verification"]["valid"], true);
    }

    #[test]
    fn test_packed_bitfield_array_and_conditional_fields() {
        let def = SaveDefinition {
            id: "bitfield_conditional_test".to_string(),
            game_code: "TEST".to_string(),
            title: "Bitfield & Conditional Test".to_string(),
            media: SaveMediaKind::Sram,
            size_bytes: 64,
            eeprom_rev8: false,
            magic_signatures: vec![MagicSignature {
                offset: 0,
                bytes: b"BCT1".to_vec(),
                description: None,
            }],
            checksums: vec![ChecksumSpec {
                name: "chk".to_string(),
                algorithm: ChecksumAlgorithm::Sum16Le,
                covered_start: 0,
                covered_end: 62,
                stored_offset: 62,
                seed: None,
                xor_out: None,
            }],
            fields: vec![
                FieldSpec {
                    name: "unlocked".to_string(),
                    offset: 4,
                    size: 1,
                    field_type: FieldType::U8,
                    description: None,
                    bitfield_array: None,
                    condition: None,
                    constraint: None,
                    confidence: None,
                    evidence: None,
                },
                FieldSpec {
                    name: "badges".to_string(),
                    offset: 5,
                    size: 2,
                    field_type: FieldType::BitfieldArray,
                    description: Some("16 packed 1-bit badge flags".to_string()),
                    bitfield_array: Some(BitfieldArraySpec {
                        bit_width: 1,
                        count: 16,
                        labels: vec![],
                    }),
                    condition: None,
                    constraint: None,
                    confidence: None,
                    evidence: None,
                },
                FieldSpec {
                    name: "secret_bonus".to_string(),
                    offset: 8,
                    size: 4,
                    field_type: FieldType::U32Le,
                    description: Some("Bonus score visible only when unlocked == 1".to_string()),
                    bitfield_array: None,
                    condition: Some(FieldCondition {
                        field: "unlocked".to_string(),
                        op: ConditionOp::Eq,
                        value: serde_json::json!(1),
                    }),
                    constraint: None,
                    confidence: None,
                    evidence: None,
                },
            ],
            copy_groups: vec![],
        };

        let mut raw = vec![0u8; 64];
        raw[0..4].copy_from_slice(b"BCT1");
        raw[4] = 1; // unlocked = 1
        // badges: bit 0, bit 3, bit 8 set -> 0x01 + 0x08 = 0x09 in byte 0, 0x01 in byte 1 -> 0x0109
        raw[5] = 0x09;
        raw[6] = 0x01;
        raw[8..12].copy_from_slice(&77777u32.to_le_bytes());
        fix_checksums(&def, &mut raw).unwrap();

        let buf = SaveBuffer::from_bytes(&raw);
        let exported = buf.export_json(&def).unwrap();

        // Check bitfield array export
        let badges_arr = exported["fields"]["badges"].as_array().unwrap();
        assert_eq!(badges_arr.len(), 16);
        assert_eq!(badges_arr[0], 1);
        assert_eq!(badges_arr[1], 0);
        assert_eq!(badges_arr[3], 1);
        assert_eq!(badges_arr[8], 1);

        // Check conditional field in fields_detail
        let details = exported["fields_detail"].as_array().unwrap();
        let secret_detail = details.iter().find(|d| d["name"] == "secret_bonus").unwrap();
        assert_eq!(secret_detail["condition_met"], true);

        // Import test with bitfield array mutation
        let mut import_payload = exported.clone();
        let mut new_badges = vec![0; 16];
        new_badges[0] = 1;
        new_badges[15] = 1; // last flag
        import_payload["fields"]["badges"] = serde_json::json!(new_badges);
        import_payload["fields"]["secret_bonus"] = serde_json::json!(99999);

        let imported_buf = SaveBuffer::create_from_json(&def, &import_payload).unwrap();
        assert!(imported_buf.validate_checksums(&def).unwrap().valid);
        assert_eq!(
            imported_buf.get_field(&def, "secret_bonus").unwrap(),
            FieldValue::U32(99999)
        );

        let re_exported = imported_buf.export_json(&def).unwrap();
        let re_badges = re_exported["fields"]["badges"].as_array().unwrap();
        assert_eq!(re_badges[0], 1);
        assert_eq!(re_badges[15], 1);
        assert_eq!(re_badges[1], 0);
        assert_eq!(re_badges[3], 0); // was mutated back to 0
    }

    #[test]
    fn validate_rejects_oversized_save_and_invalid_bit_width() {
        let mut huge = sample_definition();
        huge.size_bytes = usize::MAX;
        assert!(huge.validate().is_err());

        let mut just_over = sample_definition();
        just_over.size_bytes = MAX_SAVE_SIZE + 1;
        assert!(just_over.validate().is_err());

        let mut bad_bw = sample_definition();
        bad_bw.fields.push(FieldSpec {
            name: "bad_bits".to_string(),
            offset: 32,
            size: 8,
            field_type: FieldType::BitfieldArray,
            description: None,
            bitfield_array: Some(BitfieldArraySpec {
                bit_width: 33,
                count: 1,
                labels: vec![],
            }),
            condition: None,
            constraint: None,
            confidence: None,
            evidence: None,
        });
        assert!(bad_bw.validate().is_err());
    }

    #[test]
    fn evaluate_condition_never_panics_on_unvalidated_definition() {
        let def = SaveDefinition {
            id: "unvalidated".to_string(),
            game_code: "TEST".to_string(),
            title: "Unvalidated".to_string(),
            media: SaveMediaKind::Sram,
            size_bytes: 16,
            eeprom_rev8: false,
            magic_signatures: vec![],
            checksums: vec![],
            fields: vec![
                FieldSpec {
                    name: "overflow_off".to_string(),
                    offset: usize::MAX,
                    size: 4,
                    field_type: FieldType::U32Le,
                    description: None,
                    bitfield_array: None,
                    condition: None,
                    constraint: None,
                    confidence: None,
                    evidence: None,
                },
                FieldSpec {
                    name: "short_u32".to_string(),
                    offset: 0,
                    size: 2,
                    field_type: FieldType::U32Le,
                    description: None,
                    bitfield_array: None,
                    condition: None,
                    constraint: None,
                    confidence: None,
                    evidence: None,
                },
                FieldSpec {
                    name: "wide_bitflags".to_string(),
                    offset: 0,
                    size: 16,
                    field_type: FieldType::Bitflags,
                    description: None,
                    bitfield_array: None,
                    condition: None,
                    constraint: None,
                    confidence: None,
                    evidence: None,
                },
                FieldSpec {
                    name: "bad_bw".to_string(),
                    offset: 0,
                    size: 8,
                    field_type: FieldType::BitfieldArray,
                    description: None,
                    bitfield_array: Some(BitfieldArraySpec {
                        bit_width: 33,
                        count: 1,
                        labels: vec![],
                    }),
                    condition: None,
                    constraint: None,
                    confidence: None,
                    evidence: None,
                },
            ],
            copy_groups: vec![],
        };
        let data = vec![0u8; 16];
        for name in ["overflow_off", "short_u32", "wide_bitflags", "bad_bw"] {
            let cond = FieldCondition {
                field: name.to_string(),
                op: ConditionOp::Eq,
                value: serde_json::json!(0),
            };
            assert!(!evaluate_condition(&cond, &def, &data));
        }
    }

    #[test]
    fn create_from_json_with_eeprom_rev8_verifies_and_round_trips() {
        let def = SaveDefinition {
            id: "rev8_eeprom_test".to_string(),
            game_code: "REV8".to_string(),
            title: "Rev8 EEPROM Test".to_string(),
            media: SaveMediaKind::Eeprom512,
            size_bytes: 512,
            eeprom_rev8: true,
            magic_signatures: vec![MagicSignature {
                offset: 0,
                bytes: b"EEP8".to_vec(),
                description: None,
            }],
            checksums: vec![ChecksumSpec {
                name: "chk".to_string(),
                algorithm: ChecksumAlgorithm::Sum16Le,
                covered_start: 0,
                covered_end: 510,
                stored_offset: 510,
                seed: None,
                xor_out: None,
            }],
            fields: vec![
                FieldSpec {
                    name: "magic".to_string(),
                    offset: 0,
                    size: 4,
                    field_type: FieldType::Ascii,
                    description: None,
                    bitfield_array: None,
                    condition: None,
                    constraint: None,
                    confidence: None,
                    evidence: None,
                },
                FieldSpec {
                    name: "score".to_string(),
                    offset: 8,
                    size: 4,
                    field_type: FieldType::U32Le,
                    description: None,
                    bitfield_array: None,
                    condition: None,
                    constraint: None,
                    confidence: None,
                    evidence: None,
                },
            ],
            copy_groups: vec![],
        };

        let json = serde_json::json!({
            "fields": {
                "score": 123456
            }
        });
        let created = SaveBuffer::create_from_json(&def, &json).unwrap();
        let report = created.validate_checksums(&def).unwrap();
        assert!(report.valid, "rev8 save must verify valid: {report:?}");
        assert!(report.magics[0].valid);
        assert_eq!(
            created.get_field(&def, "magic").unwrap(),
            FieldValue::Ascii("EEP8".to_string())
        );
        assert_eq!(
            created.get_field(&def, "score").unwrap(),
            FieldValue::U32(123_456)
        );
    }

    #[test]
    fn json_val_to_field_value_rejects_truncating_integers() {
        let def = sample_definition();
        let u8_spec = def.fields.iter().find(|f| f.name == "slot_flags").unwrap();
        assert!(json_val_to_field_value(u8_spec, &serde_json::json!(256)).is_err());
        let u16_spec = def.fields.iter().find(|f| f.name == "level").unwrap();
        assert!(json_val_to_field_value(u16_spec, &serde_json::json!(70_000)).is_err());
        let u32_spec = def.fields.iter().find(|f| f.name == "score").unwrap();
        assert!(json_val_to_field_value(u32_spec, &serde_json::json!(5_000_000_000u64)).is_err());
        let i16_spec = def.fields.iter().find(|f| f.name == "temp_delta").unwrap();
        assert!(json_val_to_field_value(i16_spec, &serde_json::json!(40_000)).is_err());
        assert!(json_val_to_field_value(i16_spec, &serde_json::json!(-40_000)).is_err());
        let bytes_spec = def.fields.iter().find(|f| f.name == "badge_bytes").unwrap();
        assert!(json_val_to_field_value(bytes_spec, &serde_json::json!([1, 256, 3, 4])).is_err());
    }

    fn schema_v2_definition() -> SaveDefinition {
        SaveDefinition::from_json(
            r#"{
                "id": "schema_v2_test",
                "game_code": "V201",
                "title": "Schema v2 Constraints & CopyGroups Test",
                "media": "sram",
                "size_bytes": 128,
                "magic_signatures": [
                    { "offset": 0, "bytes": [86, 50, 83, 86] }
                ],
                "checksums": [
                    {
                        "name": "slot_sum16",
                        "algorithm": "sum16_le",
                        "covered_start": 0,
                        "covered_end": 126,
                        "stored_offset": 126
                    }
                ],
                "fields": [
                    {
                        "name": "level",
                        "offset": 4,
                        "size": 1,
                        "type": "u8",
                        "confidence": "confirmed",
                        "evidence": "Verified clamp at 0x08001234",
                        "constraint": {
                            "kind": "unsigned-range",
                            "minimum": 1,
                            "maximum": 99
                        }
                    },
                    {
                        "name": "level_mirror",
                        "offset": 68,
                        "size": 1,
                        "type": "u8",
                        "confidence": "confirmed",
                        "constraint": {
                            "kind": "unsigned-range",
                            "minimum": 1,
                            "maximum": 99
                        }
                    },
                    {
                        "name": "temp_offset",
                        "offset": 6,
                        "size": 2,
                        "type": "i16_le",
                        "confidence": "likely",
                        "constraint": {
                            "kind": "signed-range",
                            "minimum": -100,
                            "maximum": 100
                        }
                    },
                    {
                        "name": "difficulty",
                        "offset": 8,
                        "size": 1,
                        "type": "u8",
                        "confidence": "CONFIRMED_VERIFIED",
                        "constraint": {
                            "kind": "enum",
                            "values": [0, 1, 2],
                            "labels": { "0": "Easy", "1": "Normal", "2": "Hard" }
                        }
                    },
                    {
                        "name": "unlock_flags",
                        "offset": 10,
                        "size": 2,
                        "type": "bitflags",
                        "confidence": "HYPOTHESIS",
                        "constraint": {
                            "kind": "bitmask",
                            "valid_mask": 15,
                            "flag_labels": { "0": "SoundTest", "1": "BossRush" }
                        }
                    },
                    {
                        "name": "bcd_score",
                        "offset": 12,
                        "size": 3,
                        "type": "bytes",
                        "confidence": "guessed",
                        "constraint": {
                            "kind": "packed-bcd",
                            "digits": 5,
                            "minimum": 0,
                            "maximum": 99999
                        }
                    },
                    {
                        "name": "hero_name",
                        "offset": 16,
                        "size": 8,
                        "type": "ascii",
                        "confidence": "unknown",
                        "constraint": {
                            "kind": "ascii-charset",
                            "allowed": "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 ",
                            "max_len": 6
                        }
                    }
                ],
                "copy_groups": [
                    {
                        "name": "level_copies",
                        "fields": ["level", "level_mirror"]
                    }
                ]
            }"#,
        )
        .expect("schema v2 definition must parse and validate")
    }

    #[test]
    fn schema_v2_field_constraints_validate_all_variants() {
        let def = schema_v2_definition();

        // Check legacy confidence aliases mapped properly
        let diff_spec = def.fields.iter().find(|f| f.name == "difficulty").unwrap();
        assert_eq!(diff_spec.confidence, Some(FieldConfidence::Confirmed));
        let flags_spec = def.fields.iter().find(|f| f.name == "unlock_flags").unwrap();
        assert_eq!(flags_spec.confidence, Some(FieldConfidence::Guessed));

        // 1. UnsignedRange [1, 99]
        let lvl = def.fields.iter().find(|f| f.name == "level").unwrap();
        assert!(validate_field_value(lvl, &FieldValue::U8(1)).is_ok());
        assert!(validate_field_value(lvl, &FieldValue::U8(99)).is_ok());
        assert!(validate_field_value(lvl, &FieldValue::U8(0)).is_err());
        assert!(validate_field_value(lvl, &FieldValue::U8(100)).is_err());

        // 2. SignedRange [-100, 100]
        let temp = def.fields.iter().find(|f| f.name == "temp_offset").unwrap();
        assert!(validate_field_value(temp, &FieldValue::I16(-100)).is_ok());
        assert!(validate_field_value(temp, &FieldValue::I16(100)).is_ok());
        assert!(validate_field_value(temp, &FieldValue::I16(-101)).is_err());
        assert!(validate_field_value(temp, &FieldValue::I16(101)).is_err());

        // 3. Enum [0, 1, 2]
        assert!(validate_field_value(diff_spec, &FieldValue::U8(0)).is_ok());
        assert!(validate_field_value(diff_spec, &FieldValue::U8(2)).is_ok());
        assert!(validate_field_value(diff_spec, &FieldValue::U8(3)).is_err());

        // 4. Bitmask 0x0F
        assert!(validate_field_value(flags_spec, &FieldValue::Bitflags(0x00)).is_ok());
        assert!(validate_field_value(flags_spec, &FieldValue::Bitflags(0x0F)).is_ok());
        assert!(validate_field_value(flags_spec, &FieldValue::Bitflags(0x10)).is_err());

        // 5. PackedBcd (5 digits in 3 bytes, 0..=99999)
        let bcd = def.fields.iter().find(|f| f.name == "bcd_score").unwrap();
        assert!(validate_field_value(bcd, &FieldValue::Bytes(vec![0x00, 0x00, 0x00])).is_ok());
        assert!(validate_field_value(bcd, &FieldValue::Bytes(vec![0x09, 0x99, 0x99])).is_ok());
        // Non-zero leading unused nibble (6th digit set) -> err
        assert!(validate_field_value(bcd, &FieldValue::Bytes(vec![0x10, 0x00, 0x00])).is_err());
        // Invalid hex nibble 0xA -> err
        assert!(validate_field_value(bcd, &FieldValue::Bytes(vec![0x00, 0x1A, 0x00])).is_err());

        // 6. AsciiCharset (uppercase alnum + space, max_len 6)
        let name = def.fields.iter().find(|f| f.name == "hero_name").unwrap();
        assert!(validate_field_value(name, &FieldValue::Ascii("HERO 1".into())).is_ok());
        assert!(validate_field_value(name, &FieldValue::Ascii("hero".into())).is_err());
        assert!(validate_field_value(name, &FieldValue::Ascii("TOOLONG".into())).is_err());
    }

    #[test]
    fn schema_v2_copy_group_validation_rejects_invalid_definitions() {
        // Unknown field in copy_group
        let mut bad_unknown = schema_v2_definition();
        bad_unknown.copy_groups[0].fields.push("nonexistent".into());
        assert!(bad_unknown.validate().is_err());

        // Mismatched type/size in copy_group
        let mut bad_type = schema_v2_definition();
        bad_type.copy_groups[0].fields = vec!["level".into(), "temp_offset".into()];
        assert!(bad_type.validate().is_err());

        // Fewer than 2 fields in copy_group
        let mut bad_single = schema_v2_definition();
        bad_single.copy_groups[0].fields = vec!["level".into()];
        assert!(bad_single.validate().is_err());

        // Duplicate copy_group name
        let mut bad_dup = schema_v2_definition();
        bad_dup.copy_groups.push(CopyGroup {
            name: "level_copies".into(),
            fields: vec!["difficulty".into(), "level".into()],
        });
        assert!(bad_dup.validate().is_err());

        // Overlapping fields in copy_group
        let mut bad_overlap = schema_v2_definition();
        bad_overlap.fields[1].offset = bad_overlap.fields[0].offset;
        assert!(bad_overlap.validate().is_err());
    }

    #[test]
    fn schema_v2_set_field_sync_and_set_field_unchecked() {
        let def = schema_v2_definition();
        let mut raw = vec![0u8; def.size_bytes];
        raw[0..4].copy_from_slice(b"V2SV");
        raw[4] = 1;
        raw[68] = 1;
        fix_checksums(&def, &mut raw).unwrap();

        let mut buf = SaveBuffer::new(raw);
        assert!(buf.validate_checksums(&def).unwrap().valid);

        // set_field_sync updates both `level` and `level_mirror` atomically and repairs checksums
        buf.set_field_sync(&def, "level", FieldValue::U8(42)).unwrap();
        assert_eq!(buf.get_field(&def, "level").unwrap(), FieldValue::U8(42));
        assert_eq!(buf.get_field(&def, "level_mirror").unwrap(), FieldValue::U8(42));
        assert!(buf.validate_checksums(&def).unwrap().valid);

        // set_field_sync rejects out-of-range values without mutating the buffer
        let before = buf.data().to_vec();
        assert!(buf.set_field_sync(&def, "level", FieldValue::U8(200)).is_err());
        assert_eq!(buf.data(), before.as_slice());

        // set_field also enforces constraints
        assert!(buf.set_field(&def, "difficulty", FieldValue::U8(9)).is_err());

        // set_field_unchecked bypasses constraints for fuzzing while keeping checksums valid
        buf.set_field_unchecked(&def, "level", FieldValue::U8(200)).unwrap();
        assert_eq!(buf.get_field(&def, "level").unwrap(), FieldValue::U8(200));
        assert_eq!(buf.get_field(&def, "level_mirror").unwrap(), FieldValue::U8(42));
        assert!(buf.validate_checksums(&def).unwrap().valid);
    }
}
