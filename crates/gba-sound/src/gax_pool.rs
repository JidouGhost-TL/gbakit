//! Shin'en GAX Sound Engine sample pool scanning, 8-bit signed PCM extraction,
//! and SoundFont 2.04 soundbank generation.

#![deny(unsafe_code)]

use std::collections::HashSet;
use formatkit_core::{Error, Result};
use gax::{GaxSample, GaxVersion};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title,
    RawSoundFontSample, SoundFontExport,
};

/// GBA ROM Bus Window Base Address.
const ROM_BUS_START: u32 = 0x0800_0000;

/// Default sample playback rate in Hz for GAX samples when unspecified.
pub const GAX_DEFAULT_SAMPLE_RATE: u32 = 18_158;

/// 32-byte ARM7TDMI prologue of the Shin'en GAX v1.x voice mixing kernel.
const GAX_ARM_MIX_V1_SIG: [u8; 32] = [
    0xf1, 0x03, 0x2d, 0xe9, 0x18, 0x70, 0x90, 0xe5, 0x1c, 0x80, 0x90, 0xe5, 0x04, 0x10, 0x90,
    0xe5, 0x08, 0x20, 0x90, 0xe5, 0x0c, 0x30, 0x90, 0xe5, 0x83, 0x35, 0xa0, 0xe1, 0x10, 0x40,
    0x90, 0xe5,
];

/// 32-byte ARM7TDMI prologue of the Shin'en GAX v2.x voice mixing kernel.
const GAX_ARM_MIX_V2_SIG: [u8; 32] = [
    0xf0, 0x01, 0x2d, 0xe9, 0xc8, 0x50, 0x9f, 0xe5, 0x00, 0x40, 0x90, 0xe5, 0x0c, 0x30, 0x90,
    0xe5, 0x10, 0x20, 0x90, 0xe5, 0x08, 0x10, 0x90, 0xe5, 0x04, 0x00, 0x90, 0xe5, 0x00, 0x40,
    0x84, 0xe0,
];

/// 32-byte ARM7TDMI prologue of the Shin'en GAX v3.x unsigned `ldrb` SIMD mixing kernel.
const GAX_ARM_MIX_V3_SIG: [u8; 32] = [
    0xf0, 0x0f, 0x2d, 0xe9, 0xfe, 0x00, 0x90, 0xe8, 0xc4, 0x85, 0xd1, 0xe7, 0x07, 0x40, 0x94,
    0xe0, 0x05, 0x40, 0x44, 0x50, 0xc4, 0x95, 0xd1, 0xe7, 0x07, 0x40, 0x94, 0xe0, 0x05, 0x40,
    0x44, 0x50,
];

/// 32-byte ARM7TDMI prologue of the Shin'en GAX v3.x DirectSound FIFO clamp kernel.
const GAX_ARM_CLAMP_V3_SIG: [u8; 32] = [
    0xf0, 0x1f, 0x2d, 0xe9, 0x0f, 0x00, 0xb0, 0xe8, 0x03, 0x32, 0xa0, 0xe1, 0x01, 0x20, 0x82,
    0xe0, 0x01, 0xa8, 0xa0, 0xe3, 0x01, 0xa0, 0x4a, 0xe2, 0xff, 0xb0, 0xa0, 0xe3, 0x8b, 0xc2,
    0xa0, 0xe1,
];

#[inline]
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && needle.len() <= haystack.len()
        && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Detect Shin'en GAX engine version from verified ARM7TDMI mixer binary signatures.
#[must_use]
pub fn detect_gax_arm_mixer_version(rom: &[u8]) -> Option<GaxVersion> {
    if contains_subslice(rom, &GAX_ARM_MIX_V3_SIG) && contains_subslice(rom, &GAX_ARM_CLAMP_V3_SIG)
    {
        return Some(GaxVersion::V3);
    }
    if contains_subslice(rom, &GAX_ARM_MIX_V2_SIG) {
        return Some(GaxVersion::V2);
    }
    if contains_subslice(rom, &GAX_ARM_MIX_V1_SIG) {
        return Some(GaxVersion::V1);
    }
    None
}

/// Returns `true` if `rom` contains a verified Shin'en GAX ARM7TDMI software mixer kernel.
#[must_use]
pub fn has_gax_arm_mixer(rom: &[u8]) -> bool {
    detect_gax_arm_mixer_version(rom).is_some()
}

/// Extracted Shin'en GAX PCM sample entry from ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GaxSampleEntry {
    /// Sample index within the detected table.
    pub index: usize,
    /// Absolute ROM file offset where the audio PCM data begins.
    pub rom_offset: usize,
    /// 32-bit GBA bus address of the audio PCM data.
    pub bus_address: u32,
    /// Sample length in bytes.
    pub size: usize,
    /// Playback sample rate in Hz.
    pub sample_rate: u32,
    /// Extracted 8-bit signed linear PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Scan a GBA ROM for Shin'en GAX contiguous sample pointer tables.
///
/// In GAX soundbanks, samples are laid out sequentially in ROM and referenced
/// by a table of `(bus_pointer: u32, size_bytes: u32)` pairs.
pub fn scan_gax_soundpool(rom: &[u8]) -> Vec<GaxSampleEntry> {
    if rom.len() < 32 {
        return Vec::new();
    }

    let mut detected_samples = Vec::new();
    let mut seen_bus_addrs = HashSet::new();
    let detected_version =
        GaxVersion::detect_from_bytes(rom).or_else(|| detect_gax_arm_mixer_version(rom));
    let rom_bus_end = u32::try_from(rom.len())
        .ok()
        .and_then(|len| ROM_BUS_START.checked_add(len))
        .unwrap_or(0x0A00_0000)
        .min(0x0A00_0000);

    let mut emit_chain = |valid_chain: &[(u32, usize)]| {
        if valid_chain.len() < 4 {
            return;
        }
        // Use the detected GAX engine version when an engine header is present
        // (GAX 3.x uses unsigned 0x80-centered PCM; GAX 1.x/2.x use signed PCM).
        // Fall back to the waveform continuity MAD heuristic only when no engine header is found.
        let pool_version = detected_version.unwrap_or_else(|| {
            if detect_unsigned_pcm_pool(rom, valid_chain) {
                GaxVersion::V3
            } else {
                GaxVersion::V1
            }
        });

        for &(ptr, sz) in valid_chain {
            if seen_bus_addrs.insert(ptr) {
                let rom_offset = (ptr - ROM_BUS_START) as usize;
                if rom_offset
                    .checked_add(sz)
                    .is_some_and(|end| end <= rom.len())
                {
                    let idx = detected_samples.len();
                    let sample = GaxSample::from_rom_bytes(
                        idx as u32,
                        GAX_DEFAULT_SAMPLE_RATE,
                        pool_version,
                        &rom[rom_offset..rom_offset + sz],
                    );

                    detected_samples.push(GaxSampleEntry {
                        index: idx,
                        rom_offset,
                        bus_address: ptr,
                        size: sz,
                        sample_rate: sample.sample_rate,
                        pcm_data: sample.pcm_data,
                    });
                }
            }
        }
    };

    // Scan each 8-byte-entry alignment parity (0 mod 8 and 4 mod 8) in O(N) time,
    // always advancing past failed contiguous pointer runs instead of re-scanning O(N²).
    for parity in [0usize, 4usize] {
        let mut i = parity;
        while i < rom.len().saturating_sub(16) {
            let mut entries = Vec::new();
            let mut off = i;

            while off + 8 <= rom.len() {
                let ptr = u32::from_le_bytes([rom[off], rom[off + 1], rom[off + 2], rom[off + 3]]);
                let sz = u32::from_le_bytes([rom[off + 4], rom[off + 5], rom[off + 6], rom[off + 7]])
                    as usize;

                if ptr < ROM_BUS_START || ptr >= rom_bus_end {
                    break;
                }
                if sz > 2_000_000 {
                    break;
                }
                entries.push((ptr, sz));
                off += 8;
            }

            // Validate contiguous chains of audio samples within this run in a single linear pass
            let mut valid_chain: Vec<(u32, usize)> = Vec::new();
            for (p, s) in entries {
                if s == 0 {
                    continue;
                }
                if let Some(&(last_p, last_s)) = valid_chain.last() {
                    let expected = last_p.saturating_add(last_s as u32);
                    let aligned_expected = expected.saturating_add(3) & !3;
                    if p == expected || p == aligned_expected {
                        valid_chain.push((p, s));
                    } else {
                        emit_chain(&valid_chain);
                        valid_chain.clear();
                        valid_chain.push((p, s));
                    }
                } else {
                    valid_chain.push((p, s));
                }
            }
            emit_chain(&valid_chain);

            i = if off > i { off } else { i + 8 };
        }
    }

    detected_samples
}

/// Distinguish GAX 3.0+ unsigned 8-bit PCM (`0x80` center) from GAX 1.x/2.x signed 8-bit PCM (`0x00` center).
///
/// Signed waveforms crossing zero jump between `0xFF` and `0x00` (small delta as `i8`, huge delta as `u8`),
/// while unsigned waveforms crossing silence jump between `0x7F` and `0x80` (small delta as `u8`, huge delta as `i8`).
fn detect_unsigned_pcm_pool(rom: &[u8], chain: &[(u32, usize)]) -> bool {
    let mut signed_delta_sum: u64 = 0;
    let mut unsigned_delta_sum: u64 = 0;
    let mut pairs: u64 = 0;

    for &(ptr, sz) in chain.iter().take(16) {
        let off = (ptr - ROM_BUS_START) as usize;
        if off + sz > rom.len() || sz < 2 {
            continue;
        }
        let sample_len = sz.min(4096);
        let slice = &rom[off..off + sample_len];
        for w in slice.windows(2) {
            let s0 = w[0] as i8 as i32;
            let s1 = w[1] as i8 as i32;
            signed_delta_sum += (s1 - s0).unsigned_abs() as u64;

            let u0 = (w[0] ^ 0x80) as i8 as i32;
            let u1 = (w[1] ^ 0x80) as i8 as i32;
            unsigned_delta_sum += (u1 - u0).unsigned_abs() as u64;
            pairs += 1;
        }
    }

    pairs > 0 && unsigned_delta_sum < signed_delta_sum
}

/// Extract all GAX samples from a ROM and encode them as standard 16-bit RIFF WAV files.
///
/// Returns a list of `(sample_index, recommended_filename, wav_bytes)`.
pub fn export_gax_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_gax_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_gax_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!("gax_sample_{:03}_{:06X}.wav", s.index, s.rom_offset);
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Shin'en GAX samples into a standard SoundFont 2.04 (`.sf2`) soundbank.
pub fn compile_gax_soundfont(rom: &[u8], title_override: Option<&str>) -> Result<SoundFontExport> {
    let samples = scan_gax_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_gax_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (GAX)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            RawSoundFontSample {
                name: format!("GAX_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: 60,
                loop_range: None,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(&bank_title, "Shin'en GAX", &raw_samples)
}
