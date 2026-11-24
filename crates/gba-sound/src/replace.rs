//! GBA audio sample encoding, WAV input parsing, resampling, and safe ROM write-back.
//!
//! Provides a clean-room, capacity-checked asset replacement workflow for GBA audio drivers:
//! - Standard RIFF PCM WAV decoding (8-bit unsigned & 16-bit signed, mono or stereo downmix)
//!   with optional linear resampling to target GBA DirectSound rates.
//! - Sample encoders for all major GBA codecs:
//!   - Signed 8-bit PCM (M4A, GAX 1.x/2.x, Rareware, Konami KCE, Paragon 5, R&D1 Kaede,
//!     Southpaw, Logik State, EAD-SMA, EIRE).
//!   - Unsigned 8-bit PCM (GAX 3.0+, Krawall, Cosmigo Sonix).
//!   - Pokemon Gen 3 4-bit quadratic DPCM (exact inverse of [`m4a::decode_dpcm_block`]).
//!   - 4-bit IMA ADPCM (standard low-nibble-first and Logik State ARM high-nibble-first).
//! - M4A `WaveData` construction (16-byte header + payload + 1-byte interpolation guard)
//!   and safe in-place or free-space-relocated ROM write-back with automatic 4-byte-aligned
//!   GBA ROM bus pointer (`0x0800_0000`) patching.

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use logik_state::{IMA_INDEX_TABLE, IMA_STEP_TABLE};
use serde::{Deserialize, Serialize};

use crate::m4a::{
    dpcm_encoded_len, M4aSampleView, DPCM_BLOCK_BYTES, DPCM_BLOCK_SAMPLES, DPCM_DELTA_TABLE,
    WAVE_DATA_HEADER_SIZE,
};
use crate::WaveDataHeader;

/// Maximum number of PCM16 output samples permitted by [`resample_pcm_i16_linear`]
/// (16M samples = 32 MiB PCM16) to prevent unbounded memory allocation on crafted WAV headers.
pub const MAX_RESAMPLED_SAMPLES: usize = 16 * 1024 * 1024;

/// Default minimum ROM offset when searching for free space (past the 192-byte GBA header
/// and initial interrupt/CRT0 vectors).
pub const DEFAULT_FREE_SPACE_SEARCH_START: usize = 0x0000_0100;

/// Default alignment in bytes for relocated GBA audio data.
pub const DEFAULT_RELOCATE_ALIGNMENT: usize = 4;

/// IMA ADPCM nibble ordering and step-math variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ImaAdpcmVariant {
    /// Standard low-nibble-first 4-bit IMA ADPCM (`pred` clamped to `[-32768, 32767]`,
    /// `diff = (step >> 3) + ...`). Matches [`crate::logik_pool::decode_ima_adpcm4_to_pcm8`].
    StandardLowNibbleFirst,
    /// Logik State `V3EarlyVol64Adpcm` ARM7TDMI high-nibble-first 4-bit IMA ADPCM
    /// (`diff = (((nib & 7) * step) as u32) >> 2`, no `+ step >> 3` bias).
    /// Matches [`crate::logik_pool::decode_ima_adpcm4_arm_to_pcm8`].
    LogikArmHighNibbleFirst,
}

/// Sample encoding format for an M4A `WaveData` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum M4aWaveCodec {
    /// Uncompressed 8-bit signed PCM (`wave_type = 0`).
    Pcm8Signed,
    /// Pokemon Gen 3 4-bit quadratic DPCM (`wave_type = 1`).
    PokemonGen3Dpcm,
}

/// Options for building a 16-byte M4A `WaveData` header + sample payload + guard byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct M4aWaveBuildOptions {
    /// Native playback sample rate in Hz (`base_freq_q10 = sample_rate_hz * 1024`).
    pub sample_rate_hz: u32,
    /// Optional loop start index in samples (`None` for one-shot).
    pub loop_start: Option<u32>,
    /// Payload codec (`Pcm8Signed` or `PokemonGen3Dpcm`).
    pub codec: M4aWaveCodec,
}

impl M4aWaveBuildOptions {
    /// Construct one-shot 8-bit signed PCM options at `sample_rate_hz`.
    #[inline]
    #[must_use]
    pub const fn pcm8(sample_rate_hz: u32) -> Self {
        Self {
            sample_rate_hz,
            loop_start: None,
            codec: M4aWaveCodec::Pcm8Signed,
        }
    }

    /// Construct one-shot Pokemon Gen 3 4-bit DPCM options at `sample_rate_hz`.
    #[inline]
    #[must_use]
    pub const fn dpcm(sample_rate_hz: u32) -> Self {
        Self {
            sample_rate_hz,
            loop_start: None,
            codec: M4aWaveCodec::PokemonGen3Dpcm,
        }
    }
}

/// Strategy used when writing a replacement audio asset into a ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioReplaceMode {
    /// Replacement fit within the original capacity and was written in-place.
    InPlace,
    /// Replacement exceeded original capacity and was relocated to ROM free space
    /// with 4-byte-aligned ROM pointers updated.
    Relocated,
}

/// Policy controlling capacity checks and optional free-space relocation for ROM write-back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioReplaceOptions {
    /// Allow relocating the sample to ROM free space when `new_bytes.len() > original_capacity`.
    /// When `false`, overflowing replacements fail with [`Error::ResourceLimit`].
    pub allow_relocate: bool,
    /// Minimum ROM file offset to begin scanning for free space.
    pub free_space_search_start: usize,
    /// Required byte alignment for relocated data (power of two, minimum 4).
    pub relocate_alignment: usize,
}

impl Default for AudioReplaceOptions {
    fn default() -> Self {
        Self {
            allow_relocate: false,
            free_space_search_start: DEFAULT_FREE_SPACE_SEARCH_START,
            relocate_alignment: DEFAULT_RELOCATE_ALIGNMENT,
        }
    }
}

impl AudioReplaceOptions {
    /// Convenience options that permit free-space relocation with default alignment.
    #[inline]
    #[must_use]
    pub const fn relocating() -> Self {
        Self {
            allow_relocate: true,
            free_space_search_start: DEFAULT_FREE_SPACE_SEARCH_START,
            relocate_alignment: DEFAULT_RELOCATE_ALIGNMENT,
        }
    }
}

/// Structured report returned by [`replace_m4a_wavedata_in_rom`] and [`replace_pcm8_in_rom`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioReplaceReport {
    /// Whether the replacement was written in-place or relocated.
    pub mode: AudioReplaceMode,
    /// Original ROM file offset of the replaced sample.
    pub original_offset: usize,
    /// Final ROM file offset where the replacement bytes were written.
    pub written_offset: usize,
    /// Original byte capacity at `original_offset`.
    pub original_capacity: usize,
    /// Number of bytes written for the replacement.
    pub written_bytes: usize,
    /// Number of 4-byte-aligned GBA ROM bus pointers rewritten.
    pub patched_pointer_count: usize,
    /// ROM file offsets of every rewritten 32-bit pointer.
    pub patched_pointer_offsets: Vec<usize>,
}

/// Parse a standard RIFF PCM WAV file into `(sample_rate_hz, mono_i16_samples)`.
///
/// Supports:
/// - `WAVE_FORMAT_PCM` (`audio_format == 1`) and `WAVE_FORMAT_EXTENSIBLE` (`0xFFFE`) with PCM subformat.
/// - 8-bit unsigned PCM and 16-bit signed little-endian PCM.
/// - Mono or multi-channel (e.g. stereo) input, downmixed to mono by channel averaging.
pub fn read_wav_pcm_mono_i16(wav_bytes: &[u8]) -> Result<(u32, Vec<i16>)> {
    if wav_bytes.len() < 12 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 12,
            available: wav_bytes.len(),
        });
    }
    if &wav_bytes[0..4] != b"RIFF" {
        return Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_le_bytes(*b"RIFF"),
            found: u32::from_le_bytes([wav_bytes[0], wav_bytes[1], wav_bytes[2], wav_bytes[3]]),
        });
    }
    if &wav_bytes[8..12] != b"WAVE" {
        return Err(Error::BadMagic {
            offset: 8,
            expected: u32::from_le_bytes(*b"WAVE"),
            found: u32::from_le_bytes([wav_bytes[8], wav_bytes[9], wav_bytes[10], wav_bytes[11]]),
        });
    }

    let riff_declared_end = 8usize
        .saturating_add(read_u32_le(wav_bytes, 4) as usize)
        .min(wav_bytes.len());

    let mut fmt_info: Option<(u16, u32, u16)> = None; // (channels, sample_rate, bits_per_sample)
    let mut data_slice: Option<&[u8]> = None;

    let mut pos = 12usize;
    while pos + 8 <= riff_declared_end {
        let chunk_id = &wav_bytes[pos..pos + 4];
        let chunk_size = read_u32_le(wav_bytes, pos + 4) as usize;
        let payload_start = pos + 8;
        let payload_end = payload_start.checked_add(chunk_size).ok_or(Error::InvalidField {
            what: "wav_chunk_size",
            value: chunk_size as u64,
        })?;
        if payload_end > wav_bytes.len() {
            return Err(Error::Truncated {
                offset: payload_start,
                needed: chunk_size,
                available: wav_bytes.len().saturating_sub(payload_start),
            });
        }

        let payload = &wav_bytes[payload_start..payload_end];
        if chunk_id == b"fmt " {
            if payload.len() < 16 {
                return Err(Error::Truncated {
                    offset: payload_start,
                    needed: 16,
                    available: payload.len(),
                });
            }
            let audio_format = read_u16_le(payload, 0);
            let num_channels = read_u16_le(payload, 2);
            let sample_rate = read_u32_le(payload, 4);
            let bits_per_sample = read_u16_le(payload, 14);

            let is_pcm = match audio_format {
                1 => true,
                0xFFFE => {
                    // WAVE_FORMAT_EXTENSIBLE: subformat GUID starts at offset 24;
                    // KSDATAFORMAT_SUBTYPE_PCM begins with 0x00000001 LE.
                    payload.len() >= 40
                        && read_u16_le(payload, 16) >= 22
                        && read_u32_le(payload, 24) == 1
                }
                _ => false,
            };
            if !is_pcm {
                return Err(Error::Unsupported(format!(
                    "WAV audio format {audio_format:#06x} is not uncompressed PCM"
                )));
            }
            if num_channels == 0 {
                return Err(Error::InvalidField {
                    what: "wav_num_channels",
                    value: 0,
                });
            }
            if sample_rate == 0 {
                return Err(Error::InvalidField {
                    what: "wav_sample_rate",
                    value: 0,
                });
            }
            if bits_per_sample != 8 && bits_per_sample != 16 {
                return Err(Error::Unsupported(format!(
                    "WAV bit depth {bits_per_sample} is not supported (expected 8 or 16)"
                )));
            }

            fmt_info = Some((num_channels, sample_rate, bits_per_sample));
        } else if chunk_id == b"data" {
            data_slice = Some(payload);
        }

        // RIFF chunks are padded to an even byte boundary.
        pos = payload_end.saturating_add(chunk_size & 1);
    }

    let (channels, sample_rate, bits_per_sample) = fmt_info.ok_or_else(|| {
        Error::Malformed("WAV file is missing a required 'fmt ' chunk".to_string())
    })?;
    let data = data_slice.ok_or_else(|| {
        Error::Malformed("WAV file is missing a required 'data' chunk".to_string())
    })?;

    let bytes_per_sample = (bits_per_sample / 8) as usize;
    let frame_bytes = (channels as usize).checked_mul(bytes_per_sample).ok_or(Error::InvalidField {
        what: "wav_frame_bytes",
        value: channels as u64,
    })?;
    let frame_count = data.len() / frame_bytes;
    let mut out = Vec::with_capacity(frame_count);

    for frame_idx in 0..frame_count {
        let base = frame_idx * frame_bytes;
        let mut acc: i64 = 0;
        for ch in 0..(channels as usize) {
            let off = base + ch * bytes_per_sample;
            let s16: i16 = if bits_per_sample == 8 {
                ((data[off] as i16) - 128) << 8
            } else {
                i16::from_le_bytes([data[off], data[off + 1]])
            };
            acc += s16 as i64;
        }
        let mono = (acc / (channels as i64)).clamp(i16::MIN as i64, i16::MAX as i64) as i16;
        out.push(mono);
    }

    Ok((sample_rate, out))
}

/// Linearly resample signed 16-bit mono PCM from `src_rate_hz` to `dst_rate_hz`.
pub fn resample_pcm_i16_linear(
    samples: &[i16],
    src_rate_hz: u32,
    dst_rate_hz: u32,
) -> Result<Vec<i16>> {
    if src_rate_hz == 0 {
        return Err(Error::InvalidField {
            what: "src_rate_hz",
            value: 0,
        });
    }
    if dst_rate_hz == 0 {
        return Err(Error::InvalidField {
            what: "dst_rate_hz",
            value: 0,
        });
    }
    if samples.is_empty() || src_rate_hz == dst_rate_hz {
        return Ok(samples.to_vec());
    }

    let out_len_f64 = ((samples.len() as f64) * (dst_rate_hz as f64)) / (src_rate_hz as f64);
    if !out_len_f64.is_finite() || out_len_f64 > MAX_RESAMPLED_SAMPLES as f64 {
        let requested = if out_len_f64.is_finite() && out_len_f64 >= 0.0 {
            out_len_f64.round() as u64
        } else {
            u64::MAX
        };
        return Err(Error::ResourceLimit {
            resource: "resampled_pcm_samples",
            requested,
            limit: MAX_RESAMPLED_SAMPLES as u64,
        });
    }
    let out_len = out_len_f64.round().max(1.0) as usize;
    if out_len > MAX_RESAMPLED_SAMPLES {
        return Err(Error::ResourceLimit {
            resource: "resampled_pcm_samples",
            requested: out_len as u64,
            limit: MAX_RESAMPLED_SAMPLES as u64,
        });
    }
    let ratio = (src_rate_hz as f64) / (dst_rate_hz as f64);
    let last_idx = samples.len() - 1;

    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = (i as f64) * ratio;
        let idx = (pos.floor() as usize).min(last_idx);
        let next = (idx + 1).min(last_idx);
        let frac = pos - (idx as f64);
        let s0 = samples[idx] as f64;
        let s1 = samples[next] as f64;
        let interp = (s0 + (s1 - s0) * frac)
            .round()
            .clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        out.push(interp);
    }

    Ok(out)
}

/// Parse a RIFF PCM WAV file to mono `i16` samples and optionally linearly resample
/// to `target_rate_hz`.
pub fn read_wav_pcm_mono_i16_resampled(
    wav_bytes: &[u8],
    target_rate_hz: Option<u32>,
) -> Result<(u32, Vec<i16>)> {
    let (src_rate, samples) = read_wav_pcm_mono_i16(wav_bytes)?;
    match target_rate_hz {
        Some(dst_rate) if dst_rate != src_rate => {
            let resampled = resample_pcm_i16_linear(&samples, src_rate, dst_rate)?;
            Ok((dst_rate, resampled))
        }
        Some(dst_rate) => Ok((dst_rate, samples)),
        None => Ok((src_rate, samples)),
    }
}

/// Quantize a signed 16-bit PCM sample to signed 8-bit PCM with rounding.
///
/// Exact inverse of `(s8 as i16) << 8`.
#[inline]
#[must_use]
pub fn quantize_i16_to_i8(sample: i16) -> i8 {
    (((sample as i32) + 128) >> 8).clamp(i8::MIN as i32, i8::MAX as i32) as i8
}

/// Encode signed 16-bit PCM samples as signed 8-bit PCM (`i8` bit patterns as `u8`).
///
/// Used by M4A, GAX 1.x/2.x, Rareware, Konami KCE, Paragon 5, R&D1 Kaede,
/// Southpaw, Logik State, EAD-SMA, and EIRE.
#[must_use]
pub fn encode_pcm8_signed(samples: &[i16]) -> Vec<u8> {
    samples
        .iter()
        .map(|&s| quantize_i16_to_i8(s) as u8)
        .collect()
}

/// Encode signed 16-bit PCM samples as unsigned 8-bit PCM (`0x80` = silence).
///
/// Used by GAX 3.0+, Krawall, and Cosmigo Sonix.
#[must_use]
pub fn encode_pcm8_unsigned(samples: &[i16]) -> Vec<u8> {
    samples
        .iter()
        .map(|&s| (quantize_i16_to_i8(s) as i16 + 128) as u8)
        .collect()
}

/// Encode signed 8-bit PCM samples into Pokemon Gen 3 4-bit quadratic DPCM.
///
/// Exact greedy inverse of [`crate::m4a::decode_dpcm_block`]:
/// - 64 samples per 33-byte block (`DPCM_BLOCK_SAMPLES` / `DPCM_BLOCK_BYTES`).
/// - Output length is always [`crate::m4a::dpcm_encoded_len`]`(samples.len())`.
/// - Byte `0` stores the initial signed 8-bit sample.
/// - Byte `1` stores the delta nibble for sample `1` in its low nibble (`high nibble = 0`).
/// - Bytes `2..33` pack samples `2..64` as `(high_nibble << 4) | low_nibble`.
#[must_use]
pub fn encode_m4a_dpcm(samples: &[i8]) -> Vec<u8> {
    if samples.is_empty() {
        return Vec::new();
    }

    let blocks = samples.len().div_ceil(DPCM_BLOCK_SAMPLES);
    let mut out = Vec::with_capacity(blocks.saturating_mul(DPCM_BLOCK_BYTES));

    for blk in 0..blocks {
        let base = blk * DPCM_BLOCK_SAMPLES;
        let sample_at = |local_idx: usize| -> i8 {
            samples
                .get(base + local_idx)
                .copied()
                .unwrap_or_else(|| *samples.last().unwrap_or(&0))
        };

        let mut acc = sample_at(0);
        out.push(acc as u8);

        let (n1, next_acc) = best_dpcm_nibble(acc, sample_at(1));
        acc = next_acc;
        out.push(n1 & 0x0F);

        let mut pos = 2usize;
        while pos < DPCM_BLOCK_SAMPLES {
            let (hi, acc_after_hi) = best_dpcm_nibble(acc, sample_at(pos));
            acc = acc_after_hi;
            let (lo, acc_after_lo) = best_dpcm_nibble(acc, sample_at(pos + 1));
            acc = acc_after_lo;
            out.push((hi << 4) | (lo & 0x0F));
            pos += 2;
        }
    }

    debug_assert_eq!(out.len(), dpcm_encoded_len(samples.len()));
    out
}

/// Select the 4-bit DPCM delta table index minimizing `|wrapping_add(acc, delta) - target|`.
#[inline]
fn best_dpcm_nibble(acc: i8, target: i8) -> (u8, i8) {
    let target_i32 = target as i32;
    let mut best_nib = 0u8;
    let mut best_acc = acc;
    let mut best_err = i32::MAX;

    for (idx, &delta) in DPCM_DELTA_TABLE.iter().enumerate() {
        let cand = acc.wrapping_add(delta);
        let err = (cand as i32 - target_i32).abs();
        if err < best_err {
            best_err = err;
            best_nib = idx as u8;
            best_acc = cand;
            if err == 0 {
                break;
            }
        }
    }

    (best_nib, best_acc)
}

/// Encode signed 16-bit mono PCM samples into 4-bit IMA ADPCM.
///
/// Supports both [`ImaAdpcmVariant::StandardLowNibbleFirst`] and
/// [`ImaAdpcmVariant::LogikArmHighNibbleFirst`]. Odd-length inputs are padded with
/// one trailing copy of the final sample so each output byte holds two 4-bit nibbles.
#[must_use]
pub fn encode_ima_adpcm4(samples: &[i16], variant: ImaAdpcmVariant) -> Vec<u8> {
    if samples.is_empty() {
        return Vec::new();
    }

    let num_bytes = samples.len().div_ceil(2);
    let mut out = Vec::with_capacity(num_bytes);

    match variant {
        ImaAdpcmVariant::StandardLowNibbleFirst => {
            let mut pred: i32 = 0;
            let mut step_idx: i32 = 0;
            for pair in samples.chunks(2) {
                let s0 = pair[0];
                let s1 = *pair.get(1).unwrap_or(&s0);
                let n0 = encode_ima_standard_nibble(s0, &mut pred, &mut step_idx);
                let n1 = encode_ima_standard_nibble(s1, &mut pred, &mut step_idx);
                out.push(((n1 & 0x0F) << 4) | (n0 & 0x0F));
            }
        }
        ImaAdpcmVariant::LogikArmHighNibbleFirst => {
            let mut pred: i32 = 0;
            let mut step_idx: i32 = 0;
            let mut step: i32 = IMA_STEP_TABLE[0];
            for pair in samples.chunks(2) {
                let s0 = pair[0];
                let s1 = *pair.get(1).unwrap_or(&s0);
                let n0 = encode_ima_arm_nibble(s0, &mut pred, &mut step_idx, &mut step);
                let n1 = encode_ima_arm_nibble(s1, &mut pred, &mut step_idx, &mut step);
                out.push(((n0 & 0x0F) << 4) | (n1 & 0x0F));
            }
        }
    }

    out
}

#[inline]
fn encode_ima_standard_nibble(target: i16, pred: &mut i32, step_idx: &mut i32) -> u8 {
    let step = IMA_STEP_TABLE[*step_idx as usize];
    let target_i32 = target as i32;
    let mut best_nib = 0u8;
    let mut best_pred = *pred;
    let mut best_err = i32::MAX;

    for nib in 0u8..16 {
        let mut diff = step >> 3;
        if (nib & 1) != 0 {
            diff += step >> 2;
        }
        if (nib & 2) != 0 {
            diff += step >> 1;
        }
        if (nib & 4) != 0 {
            diff += step;
        }
        let cand = if (nib & 8) != 0 {
            *pred - diff
        } else {
            *pred + diff
        }
        .clamp(-32_768, 32_767);
        let err = (cand - target_i32).abs();
        if err < best_err {
            best_err = err;
            best_nib = nib;
            best_pred = cand;
        }
    }

    *pred = best_pred;
    *step_idx = (*step_idx + IMA_INDEX_TABLE[best_nib as usize]).clamp(0, 88);
    best_nib
}

#[inline]
fn encode_ima_arm_nibble(
    target: i16,
    pred: &mut i32,
    step_idx: &mut i32,
    step: &mut i32,
) -> u8 {
    let target_i32 = target as i32;
    let cur_step = *step;
    let mut best_nib = 0u8;
    let mut best_pred = *pred;
    let mut best_in_range = false;
    let mut best_err = u32::MAX;

    for nib in 0u8..16 {
        let diff = ((((nib & 7) as i32).wrapping_mul(cur_step)) as u32 >> 2) as i32;
        let cand = if (nib & 8) != 0 {
            pred.wrapping_sub(diff)
        } else {
            pred.wrapping_add(diff)
        };
        let in_range = (-32_768..=32_767).contains(&cand);
        let err = cand.abs_diff(target_i32);
        let better = match (in_range, best_in_range) {
            (true, false) => true,
            (false, true) => false,
            _ => err < best_err,
        };
        if better {
            best_in_range = in_range;
            best_err = err;
            best_nib = nib;
            best_pred = cand;
        }
    }

    *pred = best_pred;
    *step_idx = (*step_idx + IMA_INDEX_TABLE[best_nib as usize]).clamp(0, 88);
    *step = IMA_STEP_TABLE[*step_idx as usize];
    best_nib
}

/// Build a complete Nintendo M4A `WaveData` block:
/// 16-byte [`WaveDataHeader`] + encoded sample payload (PCM8 or Pokemon Gen 3 DPCM)
/// + 1-byte linear interpolation guard byte.
pub fn build_m4a_wavedata(pcm_i8: &[i8], opts: M4aWaveBuildOptions) -> Result<Vec<u8>> {
    if pcm_i8.is_empty() {
        return Err(Error::InvalidField {
            what: "m4a_wavedata_size",
            value: 0,
        });
    }
    let size_u32 = u32::try_from(pcm_i8.len()).map_err(|_| Error::ResourceLimit {
        resource: "m4a_wavedata_size",
        requested: pcm_i8.len() as u64,
        limit: u32::MAX as u64,
    })?;
    if opts.sample_rate_hz == 0 {
        return Err(Error::InvalidField {
            what: "m4a_sample_rate_hz",
            value: 0,
        });
    }
    let base_freq_q10 = opts
        .sample_rate_hz
        .checked_mul(1024)
        .ok_or(Error::InvalidField {
            what: "m4a_base_freq_q10",
            value: opts.sample_rate_hz as u64,
        })?;

    let (flags, loop_start) = match opts.loop_start {
        Some(ls) => {
            if ls >= size_u32 {
                return Err(Error::InvalidField {
                    what: "m4a_loop_start",
                    value: ls as u64,
                });
            }
            (0x40u8, ls)
        }
        None => (0x00u8, 0u32),
    };

    let (wave_type, payload, guard_byte): (u16, Vec<u8>, u8) = match opts.codec {
        M4aWaveCodec::Pcm8Signed => {
            let raw: Vec<u8> = pcm_i8.iter().map(|&s| s as u8).collect();
            let guard = if opts.loop_start.is_some() {
                pcm_i8[loop_start as usize] as u8
            } else {
                0u8
            };
            (0u16, raw, guard)
        }
        M4aWaveCodec::PokemonGen3Dpcm => {
            let compressed = encode_m4a_dpcm(pcm_i8);
            (1u16, compressed, 0u8)
        }
    };

    let mut out = Vec::with_capacity(WAVE_DATA_HEADER_SIZE + payload.len() + 1);
    out.extend_from_slice(&wave_type.to_le_bytes());
    out.push(0u8); // stat
    out.push(flags);
    out.extend_from_slice(&base_freq_q10.to_le_bytes());
    out.extend_from_slice(&loop_start.to_le_bytes());
    out.extend_from_slice(&size_u32.to_le_bytes());
    out.extend_from_slice(&payload);
    out.push(guard_byte);
    Ok(out)
}

/// Convenience wrapper around [`build_m4a_wavedata`] that quantizes signed 16-bit PCM input.
pub fn build_m4a_wavedata_from_i16(pcm_i16: &[i16], opts: M4aWaveBuildOptions) -> Result<Vec<u8>> {
    let pcm_i8: Vec<i8> = pcm_i16.iter().map(|&s| quantize_i16_to_i8(s)).collect();
    build_m4a_wavedata(&pcm_i8, opts)
}

/// Find an aligned contiguous free-space region in a GBA ROM of at least `needed_len` bytes.
///
/// Prefers `0xFF` flash-erased padding first, then falls back to `0x00` zero-filled padding.
/// Returns `None` if no suitable region exists within the 32 MiB GBA ROM bus window.
#[must_use]
pub fn find_rom_free_space(
    rom: &[u8],
    needed_len: usize,
    search_start: usize,
    alignment: usize,
) -> Option<usize> {
    gba_rom::find_free_space_prefer_ff(rom, needed_len, alignment.max(4), search_start, None)
}

/// Scan `rom` at every 4-byte-aligned offset past the 192-byte AGB header and rewrite all
/// GBA ROM bus pointers (`0x0800_0000 + old_offset`) to (`0x0800_0000 + new_offset`).
///
/// Returns the sorted list of ROM file offsets where pointers were patched, or an empty
/// vector if either offset falls outside the 32 MiB GBA ROM bus window.
pub fn patch_rom_pointers(rom: &mut [u8], old_offset: usize, new_offset: usize) -> Vec<usize> {
    gba_rom::patch_rom_pointers(rom, old_offset, new_offset, None).unwrap_or_default()
}

/// Safely replace an M4A `WaveData` sample in `rom` at `offset`.
///
/// - Validates the existing 16-byte [`WaveDataHeader`] at `offset` to compute `original_capacity`
///   (header + PCM/DPCM payload + 1-byte guard if present in ROM) and validates `new_wavedata`.
/// - Writes in-place when `new_wavedata.len() <= original_capacity`.
/// - When `new_wavedata.len() > original_capacity`:
///   - If `opts.allow_relocate` is `true`, allocates aligned `0xFF`/`0x00` free space in `rom`,
///     patches all 4-byte-aligned `0x0800_0000 + offset` pointers to the new location, and writes
///     `new_wavedata`.
///   - If `opts.allow_relocate` is `false`, returns [`Error::ResourceLimit`].
pub fn replace_m4a_wavedata_in_rom(
    rom: &mut [u8],
    offset: usize,
    new_wavedata: &[u8],
    opts: AudioReplaceOptions,
) -> Result<AudioReplaceReport> {
    if offset >= rom.len() {
        return Err(Error::Truncated {
            offset,
            needed: WAVE_DATA_HEADER_SIZE,
            available: 0,
        });
    }

    // Validate replacement WaveData before mutating ROM.
    M4aSampleView::from_wavdata_bytes(new_wavedata).map_err(|_| Error::InvalidField {
        what: "replacement_m4a_wavedata",
        value: new_wavedata.len() as u64,
    })?;

    // Parse existing WaveData header at `offset` to determine original capacity.
    let old_hdr = WaveDataHeader::parse(&rom[offset..]).map_err(|_| Error::InvalidField {
        what: "existing_m4a_wavedata_header",
        value: offset as u64,
    })?;
    let old_payload_len = if old_hdr.is_compressed_dpcm() {
        dpcm_encoded_len(old_hdr.size as usize)
    } else {
        old_hdr.size as usize
    };
    let min_original_len = WAVE_DATA_HEADER_SIZE
        .checked_add(old_payload_len)
        .ok_or(Error::InvalidField {
            what: "existing_m4a_wavedata_size",
            value: old_hdr.size as u64,
        })?;
    let available = rom.len() - offset;
    if min_original_len > available {
        return Err(Error::Truncated {
            offset,
            needed: min_original_len,
            available,
        });
    }
    // Include the standard 1-byte M4A interpolation guard when present inside the ROM.
    let original_capacity = min_original_len.saturating_add(1).min(available);

    write_or_relocate_in_rom(
        rom,
        offset,
        original_capacity,
        new_wavedata,
        opts,
        "m4a_wavedata_capacity",
    )
}

/// Safely replace a raw 8-bit PCM (or IMA ADPCM) sample buffer in `rom` at `offset`
/// with a known `original_capacity`.
///
/// Writes in-place when `new_pcm_bytes.len() <= original_capacity`, or relocates to aligned
/// ROM free space and patches all 4-byte-aligned `0x0800_0000 + offset` pointers when
/// `opts.allow_relocate` is `true`.
pub fn replace_pcm8_in_rom(
    rom: &mut [u8],
    offset: usize,
    original_capacity: usize,
    new_pcm_bytes: &[u8],
    opts: AudioReplaceOptions,
) -> Result<AudioReplaceReport> {
    if new_pcm_bytes.is_empty() {
        return Err(Error::InvalidField {
            what: "replacement_pcm8_len",
            value: 0,
        });
    }
    let end = offset.checked_add(original_capacity).ok_or(Error::InvalidField {
        what: "pcm8_replace_offset",
        value: offset as u64,
    })?;
    if end > rom.len() {
        return Err(Error::Truncated {
            offset,
            needed: original_capacity,
            available: rom.len().saturating_sub(offset),
        });
    }

    write_or_relocate_in_rom(
        rom,
        offset,
        original_capacity,
        new_pcm_bytes,
        opts,
        "pcm8_sample_capacity",
    )
}

fn write_or_relocate_in_rom(
    rom: &mut [u8],
    offset: usize,
    original_capacity: usize,
    new_bytes: &[u8],
    opts: AudioReplaceOptions,
    capacity_resource: &'static str,
) -> Result<AudioReplaceReport> {
    if new_bytes.len() <= original_capacity {
        rom[offset..offset + new_bytes.len()].copy_from_slice(new_bytes);
        if new_bytes.len() < original_capacity {
            rom[offset + new_bytes.len()..offset + original_capacity].fill(0);
        }
        return Ok(AudioReplaceReport {
            mode: AudioReplaceMode::InPlace,
            original_offset: offset,
            written_offset: offset,
            original_capacity,
            written_bytes: new_bytes.len(),
            patched_pointer_count: 0,
            patched_pointer_offsets: Vec::new(),
        });
    }

    if !opts.allow_relocate {
        return Err(Error::ResourceLimit {
            resource: capacity_resource,
            requested: new_bytes.len() as u64,
            limit: original_capacity as u64,
        });
    }

    let new_offset = find_rom_free_space(
        rom,
        new_bytes.len(),
        opts.free_space_search_start,
        opts.relocate_alignment,
    )
    .ok_or(Error::ResourceLimit {
        resource: "rom_free_space",
        requested: new_bytes.len() as u64,
        limit: 0,
    })?;

    // Patch ROM pointers before writing payload so we never mutate sample bytes that
    // coincidentally match the old pointer pattern, and validate bus addresses first.
    let patched_pointer_offsets = gba_rom::patch_rom_pointers(rom, offset, new_offset, None)?;
    rom[new_offset..new_offset + new_bytes.len()].copy_from_slice(new_bytes);

    Ok(AudioReplaceReport {
        mode: AudioReplaceMode::Relocated,
        original_offset: offset,
        written_offset: new_offset,
        original_capacity,
        written_bytes: new_bytes.len(),
        patched_pointer_count: patched_pointer_offsets.len(),
        patched_pointer_offsets,
    })
}

#[inline]
fn read_u16_le(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}

#[inline]
fn read_u32_le(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logik_pool::{decode_ima_adpcm4_arm_to_pcm8, decode_ima_adpcm4_to_pcm8};
    use crate::m4a::{decode_dpcm_vec, ROM_BUS_START};
    use crate::{encode_wav_i16_mono, extract_pcm_i8};

    fn make_sine_i16(len: usize, sample_rate: f64, freq_hz: f64, amp: f64) -> Vec<i16> {
        (0..len)
            .map(|i| {
                let phase = 2.0 * std::f64::consts::PI * freq_hz * (i as f64) / sample_rate;
                (phase.sin() * amp).round().clamp(i16::MIN as f64, i16::MAX as f64) as i16
            })
            .collect()
    }

    fn build_stereo_wav16(sample_rate: u32, left: &[i16], right: &[i16]) -> Vec<u8> {
        assert_eq!(left.len(), right.len());
        let data_bytes = (left.len() * 4) as u64;
        let header = formatkit_core::wav_header_pcm(sample_rate, 2, 16, data_bytes)
            .expect("valid stereo wav header");
        let mut out = Vec::with_capacity(44 + data_bytes as usize);
        out.extend_from_slice(&header);
        for (&l, &r) in left.iter().zip(right.iter()) {
            out.extend_from_slice(&l.to_le_bytes());
            out.extend_from_slice(&r.to_le_bytes());
        }
        out
    }

    fn build_mono_wav8(sample_rate: u32, pcm_u8: &[u8]) -> Vec<u8> {
        let header = formatkit_core::wav_header_pcm(sample_rate, 1, 8, pcm_u8.len() as u64)
            .expect("valid 8-bit wav header");
        let mut out = Vec::with_capacity(44 + pcm_u8.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(pcm_u8);
        out
    }

    fn compute_snr_db(reference: &[f64], decoded: &[f64]) -> f64 {
        assert_eq!(reference.len(), decoded.len());
        let sig_pow: f64 = reference.iter().map(|&x| x * x).sum();
        let err_pow: f64 = reference
            .iter()
            .zip(decoded.iter())
            .map(|(&r, &d)| (r - d) * (r - d))
            .sum();
        if err_pow == 0.0 {
            return f64::INFINITY;
        }
        10.0 * (sig_pow / err_pow).log10()
    }

    #[test]
    fn wav16_mono_and_stereo_and_wav8_roundtrip() {
        // Mono WAV16 round-trip via crate::encode_wav_i16_mono
        let original_i8: Vec<i8> = (-64..64).collect();
        let wav_mono = encode_wav_i16_mono(13_379, &original_i8).expect("encode mono wav16");
        let (rate, pcm16) = read_wav_pcm_mono_i16(&wav_mono).expect("decode mono wav16");
        assert_eq!(rate, 13_379);
        assert_eq!(pcm16.len(), original_i8.len());
        let re_signed = encode_pcm8_signed(&pcm16);
        let re_i8: Vec<i8> = re_signed.into_iter().map(|b| b as i8).collect();
        assert_eq!(re_i8, original_i8);

        // Stereo WAV16 downmix to mono
        let left = vec![1000i16, -2000, 30000, -32768];
        let right = vec![3000i16, -4000, 10000, -32768];
        let wav_stereo = build_stereo_wav16(22_050, &left, &right);
        let (s_rate, s_mono) = read_wav_pcm_mono_i16(&wav_stereo).expect("decode stereo wav16");
        assert_eq!(s_rate, 22_050);
        assert_eq!(s_mono, vec![2000, -3000, 20000, -32768]);

        // 8-bit unsigned WAV round-trip
        let u8_samples = vec![0x00u8, 0x40, 0x80, 0xC0, 0xFF];
        let wav8 = build_mono_wav8(10_512, &u8_samples);
        let (r8, p8) = read_wav_pcm_mono_i16(&wav8).expect("decode wav8");
        assert_eq!(r8, 10_512);
        assert_eq!(encode_pcm8_unsigned(&p8), u8_samples);

        // Resampling convenience
        let (r_resamp, p_resamp) =
            read_wav_pcm_mono_i16_resampled(&wav_mono, Some(26_758)).expect("resample 2x");
        assert_eq!(r_resamp, 26_758);
        assert_eq!(p_resamp.len(), original_i8.len() * 2);
    }

    #[test]
    fn m4a_dpcm_encode_decode_roundtrip_quality_and_exact_len() {
        let sine16 = make_sine_i16(200, 13_379.0, 440.0, 24_000.0);
        let sine8: Vec<i8> = sine16.iter().map(|&s| quantize_i16_to_i8(s)).collect();

        let encoded = encode_m4a_dpcm(&sine8);
        assert_eq!(encoded.len(), dpcm_encoded_len(sine8.len()));
        assert_eq!(encoded.len(), 4 * DPCM_BLOCK_BYTES); // ceil(200/64) = 4 blocks = 132 bytes

        let decoded = decode_dpcm_vec(&encoded, sine8.len()).expect("decode dpcm");
        assert_eq!(decoded.len(), sine8.len());
        // First sample of every 64-sample block is stored verbatim
        assert_eq!(decoded[0], sine8[0]);
        assert_eq!(decoded[64], sine8[64]);
        assert_eq!(decoded[128], sine8[128]);
        assert_eq!(decoded[192], sine8[192]);

        let ref_f64: Vec<f64> = sine8.iter().map(|&x| x as f64).collect();
        let dec_f64: Vec<f64> = decoded.iter().map(|&x| x as f64).collect();
        let snr = compute_snr_db(&ref_f64, &dec_f64);
        assert!(snr > 25.0, "expected high DPCM SNR > 25 dB, got {snr:.2} dB");
    }

    #[test]
    fn ima_adpcm4_standard_and_arm_roundtrip() {
        let sine16 = make_sine_i16(512, 10_512.0, 220.0, 20_000.0);
        let ref_f64: Vec<f64> = sine16.iter().map(|&s| (s >> 8) as f64).collect();

        // Standard low-nibble-first
        let enc_std = encode_ima_adpcm4(&sine16, ImaAdpcmVariant::StandardLowNibbleFirst);
        assert_eq!(enc_std.len(), 256);
        let dec_std = decode_ima_adpcm4_to_pcm8(&enc_std);
        assert_eq!(dec_std.len(), 512);
        let dec_std_f64: Vec<f64> = dec_std.iter().map(|&x| x as f64).collect();
        let snr_std_full = compute_snr_db(&ref_f64, &dec_std_f64);
        let snr_std_steady = compute_snr_db(&ref_f64[16..], &dec_std_f64[16..]);
        assert!(
            snr_std_full > 18.0 && snr_std_steady > 28.0,
            "expected standard IMA ADPCM4 SNR > 18 dB full / > 28 dB steady, got {snr_std_full:.2} / {snr_std_steady:.2} dB"
        );

        // Logik State ARM high-nibble-first
        let enc_arm = encode_ima_adpcm4(&sine16, ImaAdpcmVariant::LogikArmHighNibbleFirst);
        assert_eq!(enc_arm.len(), 256);
        let dec_arm = decode_ima_adpcm4_arm_to_pcm8(&enc_arm);
        assert_eq!(dec_arm.len(), 512);
        let dec_arm_f64: Vec<f64> = dec_arm.iter().map(|&x| x as f64).collect();
        let snr_arm_full = compute_snr_db(&ref_f64, &dec_arm_f64);
        let snr_arm_steady = compute_snr_db(&ref_f64[16..], &dec_arm_f64[16..]);
        assert!(
            snr_arm_full > 16.0 && snr_arm_steady > 25.0,
            "expected Logik ARM IMA ADPCM4 SNR > 16 dB full / > 25 dB steady, got {snr_arm_full:.2} / {snr_arm_steady:.2} dB"
        );
    }

    #[test]
    fn replace_m4a_wavedata_in_place_pcm8_and_dpcm() {
        let mut rom = vec![0xFFu8; 0x1000];
        let sample_off = 0x0200usize;

        // Seed an existing 128-sample PCM8 WaveData (16 + 128 + 1 = 145 bytes)
        let orig_pcm: Vec<i8> = (0..128).map(|i| (i as i8).wrapping_sub(64)).collect();
        let orig_wd = build_m4a_wavedata(&orig_pcm, M4aWaveBuildOptions::pcm8(13_379))
            .expect("build original wavedata");
        rom[sample_off..sample_off + orig_wd.len()].copy_from_slice(&orig_wd);

        // 1) In-place PCM8 replacement (shorter, looped)
        let new_pcm: Vec<i8> = (0..96).map(|i| ((i % 16) as i8 - 8) * 10).collect();
        let new_wd = build_m4a_wavedata(
            &new_pcm,
            M4aWaveBuildOptions {
                sample_rate_hz: 15_768,
                loop_start: Some(16),
                codec: M4aWaveCodec::Pcm8Signed,
            },
        )
        .expect("build replacement pcm8 wavedata");

        let report = replace_m4a_wavedata_in_rom(
            &mut rom,
            sample_off,
            &new_wd,
            AudioReplaceOptions::default(),
        )
        .expect("in-place pcm8 replace");

        assert_eq!(report.mode, AudioReplaceMode::InPlace);
        assert_eq!(report.original_offset, sample_off);
        assert_eq!(report.written_offset, sample_off);
        assert_eq!(report.original_capacity, 145);
        assert_eq!(report.written_bytes, new_wd.len());
        assert_eq!(report.patched_pointer_count, 0);

        let (hdr, extracted) = extract_pcm_i8(&rom[sample_off..], 1024).expect("extract replaced");
        assert_eq!(hdr.base_sample_rate_hz(), 15_768);
        assert!(hdr.is_looped());
        assert_eq!(hdr.loop_start, 16);
        assert_eq!(hdr.size, 96);
        assert_eq!(extracted, new_pcm);

        // 2) In-place Pokemon Gen 3 DPCM replacement (64 samples = 16 + 33 + 1 = 50 bytes <= 113)
        let dpcm_src: Vec<i8> = make_sine_i16(64, 13_379.0, 440.0, 16_000.0)
            .into_iter()
            .map(quantize_i16_to_i8)
            .collect();
        let dpcm_wd = build_m4a_wavedata(&dpcm_src, M4aWaveBuildOptions::dpcm(13_379))
            .expect("build dpcm wavedata");

        let dpcm_report = replace_m4a_wavedata_in_rom(
            &mut rom,
            sample_off,
            &dpcm_wd,
            AudioReplaceOptions::default(),
        )
        .expect("in-place dpcm replace");
        assert_eq!(dpcm_report.mode, AudioReplaceMode::InPlace);

        let (dhdr, dextracted) =
            extract_pcm_i8(&rom[sample_off..], 1024).expect("extract replaced dpcm");
        assert!(dhdr.is_compressed_dpcm());
        assert_eq!(dhdr.size, 64);
        assert_eq!(dextracted.len(), 64);
        assert_eq!(dextracted[0], dpcm_src[0]);
    }

    #[test]
    fn replace_m4a_wavedata_relocates_and_patches_voicegroup_pointers() {
        let mut rom = vec![0xAAu8; 0x2000];
        // Carve a free-space region at 0x1000..0x1800 filled with 0xFF
        rom[0x1000..0x1800].fill(0xFF);

        let sample_off = 0x0200usize;
        let orig_pcm = vec![0i8; 32];
        let orig_wd = build_m4a_wavedata(&orig_pcm, M4aWaveBuildOptions::pcm8(13_379))
            .expect("build small wavedata");
        rom[sample_off..sample_off + orig_wd.len()].copy_from_slice(&orig_wd);

        // Place two synthetic M4A voicegroup entries pointing to `0x0800_0200`
        let old_ptr = (ROM_BUS_START + sample_off as u32).to_le_bytes();
        let vg_ptr_off_1 = 0x0404usize;
        let vg_ptr_off_2 = 0x0410usize;
        rom[vg_ptr_off_1..vg_ptr_off_1 + 4].copy_from_slice(&old_ptr);
        rom[vg_ptr_off_2..vg_ptr_off_2 + 4].copy_from_slice(&old_ptr);

        // Build a larger 256-sample WaveData that overflows the 49-byte original capacity
        let big_pcm: Vec<i8> = (0..256).map(|i| ((i % 64) as i8).wrapping_sub(32)).collect();
        let big_wd = build_m4a_wavedata(&big_pcm, M4aWaveBuildOptions::pcm8(18_157))
            .expect("build big wavedata");

        // 1) Overflow rejected when allow_relocate = false
        let err = replace_m4a_wavedata_in_rom(
            &mut rom,
            sample_off,
            &big_wd,
            AudioReplaceOptions::default(),
        )
        .expect_err("overflow must fail when allow_relocate is false");
        assert!(matches!(
            err,
            Error::ResourceLimit {
                resource: "m4a_wavedata_capacity",
                ..
            }
        ));

        // 2) Relocate when allow_relocate = true
        let report = replace_m4a_wavedata_in_rom(
            &mut rom,
            sample_off,
            &big_wd,
            AudioReplaceOptions::relocating(),
        )
        .expect("relocating replace should succeed");

        assert_eq!(report.mode, AudioReplaceMode::Relocated);
        assert_eq!(report.original_offset, sample_off);
        assert_eq!(report.written_offset, 0x1000);
        assert_eq!(report.written_bytes, big_wd.len());
        assert_eq!(report.patched_pointer_count, 2);
        assert_eq!(report.patched_pointer_offsets, vec![vg_ptr_off_1, vg_ptr_off_2]);

        let expected_new_ptr = (ROM_BUS_START + 0x1000).to_le_bytes();
        assert_eq!(&rom[vg_ptr_off_1..vg_ptr_off_1 + 4], &expected_new_ptr);
        assert_eq!(&rom[vg_ptr_off_2..vg_ptr_off_2 + 4], &expected_new_ptr);

        let (hdr, extracted) =
            extract_pcm_i8(&rom[report.written_offset..], 1024).expect("extract relocated sample");
        assert_eq!(hdr.base_sample_rate_hz(), 18_157);
        assert_eq!(extracted, big_pcm);
    }

    #[test]
    fn replace_pcm8_in_rom_in_place_and_relocated() {
        let mut rom = vec![0x55u8; 0x1000];
        rom[0x0800..0x0C00].fill(0x00); // zero-filled free space

        let sample_off = 0x0200usize;
        let old_ptr = (ROM_BUS_START + sample_off as u32).to_le_bytes();
        rom[0x0100..0x0104].copy_from_slice(&old_ptr);

        let replacement = encode_pcm8_signed(&[0, 4096, 8192, -8192]);
        let r1 = replace_pcm8_in_rom(&mut rom, sample_off, 16, &replacement, AudioReplaceOptions::default())
            .expect("in-place pcm8 replace");
        assert_eq!(r1.mode, AudioReplaceMode::InPlace);
        assert_eq!(&rom[sample_off..sample_off + 4], &replacement[..]);
        assert_eq!(&rom[sample_off + 4..sample_off + 16], &[0u8; 12]);

        // Overflow rejected when allow_relocate = false
        let larger = vec![0x7Fu8; 64];
        assert!(replace_pcm8_in_rom(&mut rom, sample_off, 16, &larger, AudioReplaceOptions::default()).is_err());

        // Relocated into 0x00 free space when allow_relocate = true
        let r2 = replace_pcm8_in_rom(&mut rom, sample_off, 16, &larger, AudioReplaceOptions::relocating())
            .expect("relocated pcm8 replace");
        assert_eq!(r2.mode, AudioReplaceMode::Relocated);
        assert_eq!(r2.written_offset, 0x0800);
        assert_eq!(r2.patched_pointer_offsets, vec![0x0100]);
        assert_eq!(&rom[0x0100..0x0104], &(ROM_BUS_START + 0x0800).to_le_bytes());
        assert_eq!(&rom[0x0800..0x0840], &larger[..]);
    }

    #[test]
    fn wav_resampler_rejects_oom_upsampling() {
        // 1,000 samples at 1 Hz resampled to 44,100 Hz would request 44,100,000 samples (> 16M limit).
        let samples = vec![0i16; 1_000];
        let err = resample_pcm_i16_linear(&samples, 1, 44_100)
            .expect_err("extreme upsampling must fail with ResourceLimit");
        assert!(
            matches!(
                err,
                Error::ResourceLimit {
                    resource: "resampled_pcm_samples",
                    limit,
                    ..
                } if limit == MAX_RESAMPLED_SAMPLES as u64
            ),
            "expected ResourceLimit, got {err:?}"
        );

        // Also verify through the WAV file entry point.
        let wav = build_mono_wav8(1, &vec![0x80u8; 1_000]);
        let err_wav = read_wav_pcm_mono_i16_resampled(&wav, Some(44_100))
            .expect_err("crafted 1 Hz WAV resampled to 44.1 kHz must fail");
        assert!(matches!(err_wav, Error::ResourceLimit { .. }));
    }
}
