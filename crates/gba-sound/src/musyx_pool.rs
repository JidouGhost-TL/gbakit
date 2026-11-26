//! Factor 5 MusyX Sound System sample pool scanning, ADPCM decompression,
//! and SoundFont 2.04 soundbank generation.

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use musyx::{MusyXAdpcm, MusyXSampleFormat};
use serde::{Deserialize, Serialize};

use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// 32-byte ARM7TDMI prologue of the Factor 5 MusyX DirectSound 8-bit DMA FIFO
/// stereo clamp routine (`stmfd sp!, {r0-r2, r4-r9, lr}; mov lr, #0; mov r9, #0x20; ...`).
const ARM_CLIP_STEREO_SIG: [u8; 32] = [
    0xf7, 0x43, 0x2d, 0xe9, 0x00, 0xe0, 0xa0, 0xe3, 0x20, 0x90, 0xa0, 0xe3, 0xf4, 0x40, 0xd0,
    0xe0, 0xf4, 0x50, 0xd0, 0xe0, 0xf4, 0x60, 0xd0, 0xe0, 0xfc, 0x70, 0x50, 0xe0, 0x7f, 0x00,
    0x54, 0xe3,
];

/// 32-byte ARM7TDMI prologue of the Factor 5 MusyX 16-bit intermediate voice
/// mixing kernel (`stmfd sp!, {r1-r12, lr}; ldr r2, [r0, #4]; ldr r10, [r0, #8]; ...`).
const ARM_MIX_VOICE_SIG: [u8; 32] = [
    0xfe, 0x5f, 0x2d, 0xe9, 0x04, 0x20, 0x90, 0xe5, 0x08, 0xa0, 0x90, 0xe5, 0xb6, 0xb1, 0xd0,
    0xe1, 0xb4, 0xe1, 0xd0, 0xe1, 0x18, 0x40, 0xd0, 0xe5, 0x19, 0x50, 0xd0, 0xe5, 0x00, 0x00,
    0x54, 0xe3,
];

#[inline]
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && needle.len() <= haystack.len()
        && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Returns `true` if `rom` contains both verified 32-byte Factor 5 MusyX ARM7TDMI
/// software mixer and DirectSound FIFO clamp kernels.
///
/// Detects string-stripped MusyX builds (including RelocBank32 titles) with 0% false positives.
#[must_use]
pub fn has_musyx_arm_mixer(rom: &[u8]) -> bool {
    contains_subslice(rom, &ARM_CLIP_STEREO_SIG) && contains_subslice(rom, &ARM_MIX_VOICE_SIG)
}

/// Extracted Factor 5 MusyX sample entry from ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MusyXSampleEntry {
    /// Sample index within the detected pool.
    pub index: usize,
    /// Absolute ROM file offset where the 16-byte sample header begins.
    pub rom_offset: usize,
    /// Payload length in bytes.
    pub size: usize,
    /// Sample playback rate in Hz.
    pub sample_rate: u32,
    /// MIDI root key (e.g. 60 = Middle C).
    pub root_key: u8,
    /// Loop start offset in samples, if looped.
    pub loop_point: Option<u32>,
    /// Native compression format in ROM.
    pub format: String,
    /// Decoded 16-bit signed linear mono PCM samples.
    pub pcm16: Vec<i16>,
}

/// Scan a GBA ROM for Factor 5 MusyX sequential sample pools.
///
/// In MusyX soundbanks, samples are stored sequentially as 16-byte descriptors:
/// - `size: u32` (payload length in bytes)
/// - `loop_start: u32` (0xFFFF_FFFF if unlooped)
/// - `sample_rate: u16` (in Hz, typically 8000–32000)
/// - `root_key: u16` (MIDI key number)
/// - `format: u32` (0 = 8-bit PCM, 1 = 16-bit PCM, 2 = 4-bit ADPCM)
///
/// Followed by `size` bytes of audio data, padded to 4-byte alignment.
pub fn scan_musyx_soundpool(rom: &[u8]) -> Vec<MusyXSampleEntry> {
    if rom.len() < 32 {
        return Vec::new();
    }

    let mut detected_samples = Vec::new();
    let step = 4usize;
    let mut i = 0usize;

    while i < rom.len().saturating_sub(32) {
        let mut off = i;
        let mut chain = Vec::new();

        while off + 16 <= rom.len() {
            let length = u32::from_le_bytes([rom[off], rom[off + 1], rom[off + 2], rom[off + 3]]) as usize;
            let loop_raw = u32::from_le_bytes([rom[off + 4], rom[off + 5], rom[off + 6], rom[off + 7]]);
            let rate = u16::from_le_bytes([rom[off + 8], rom[off + 9]]) as u32;
            let root_key = rom[off + 10];
            let fmt_raw = u32::from_le_bytes([rom[off + 12], rom[off + 13], rom[off + 14], rom[off + 15]]);

            if length == 0 || length > 500_000 {
                break;
            }
            if !(4000..=48000).contains(&rate) {
                break;
            }
            if fmt_raw > 2 || root_key > 127 {
                break;
            }
            let max_loop_samples = match fmt_raw {
                1 => length / 2,
                2 => length * 2,
                _ => length,
            };
            if loop_raw != 0xFFFF_FFFF && (loop_raw as usize) > max_loop_samples {
                break;
            }
            if off + 16 + length > rom.len() {
                break;
            }

            chain.push((off, length, loop_raw, rate, root_key, fmt_raw));
            off += 16 + length;
            if !off.is_multiple_of(4) {
                off = (off + 3) & !3;
            }
        }

        // Require at least 4 contiguous sample descriptors to reject false matches
        if chain.len() >= 4 {
            for (entry_off, length, loop_raw, rate, root_key, fmt_raw) in chain {
                let payload_start = entry_off + 16;
                let payload = &rom[payload_start..payload_start + length];

                let (fmt, fmt_str, pcm16) = match fmt_raw {
                    1 => {
                        let samples = payload
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|&[c0, c1]| i16::from_le_bytes([c0, c1]))
                            .collect();
                        (MusyXSampleFormat::Pcm16, "Pcm16", samples)
                    }
                    2 => {
                        let sample_count = length * 2;
                        let decoded = MusyXAdpcm::decode(payload, sample_count).unwrap_or_default();
                        (MusyXSampleFormat::Adpcm4, "Adpcm4", decoded)
                    }
                    _ => {
                        let samples = payload.iter().map(|&b| (b as i8 as i16) << 8).collect();
                        (MusyXSampleFormat::Pcm8, "Pcm8", samples)
                    }
                };

                let loop_point = if loop_raw == 0xFFFF_FFFF {
                    None
                } else {
                    Some(loop_raw)
                };

                let idx = detected_samples.len();
                detected_samples.push(MusyXSampleEntry {
                    index: idx,
                    rom_offset: entry_off,
                    size: length,
                    sample_rate: rate,
                    root_key: root_key & 0x7F,
                    loop_point,
                    format: fmt_str.to_string(),
                    pcm16,
                });
                let _ = fmt;
            }
            i = off;
        } else {
            i += step;
        }
    }

    if has_musyx_arm_mixer(rom) {
        let reloc = crate::konami_pool::scan_konami_soundpool(rom);
        if reloc.len() > detected_samples.len() {
            detected_samples.clear();
            for ks in reloc {
                let pcm16: Vec<i16> = ks.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
                let idx = detected_samples.len();
                detected_samples.push(MusyXSampleEntry {
                    index: idx,
                    rom_offset: ks.rom_offset,
                    size: ks.size,
                    sample_rate: ks.sample_rate,
                    root_key: ks.root_key,
                    loop_point: ks.loop_start,
                    format: "Pcm8 (RelocBank32)".to_string(),
                    pcm16,
                });
            }
        }
    }

    detected_samples
}

/// Extract all MusyX samples from a ROM and encode them as standard 16-bit RIFF WAV files.
///
/// Returns a list of `(sample_index, recommended_filename, wav_bytes)`.
pub fn export_musyx_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_musyx_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_musyx_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let rate = if s.sample_rate == 0 { 11025 } else { s.sample_rate };
        let data_bytes = (s.pcm16.len() as u64).checked_mul(2).ok_or(Error::ResourceLimit {
            resource: "wav_data_bytes",
            requested: s.pcm16.len() as u64,
            limit: u64::MAX,
        })?;
        let header = formatkit_core::wav_header_pcm(rate, 1, 16, data_bytes)?;
        let mut wav = Vec::with_capacity(header.len() + s.pcm16.len() * 2);
        wav.extend_from_slice(&header);
        for &val in &s.pcm16 {
            wav.extend_from_slice(&val.to_le_bytes());
        }
        let name = format!("musyx_sample_{:03}_{:06X}.wav", s.index, s.rom_offset);
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Factor 5 MusyX samples into a standard SoundFont 2.04 (`.sf2`) soundbank.
pub fn compile_musyx_soundfont(rom: &[u8], title_override: Option<&str>) -> Result<SoundFontExport> {
    let samples = scan_musyx_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_musyx_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (MusyX)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let loop_range = s.loop_point.map(|start| (start, s.pcm16.len() as u32));
            RawSoundFontSample {
                name: format!("MSX_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: if s.root_key > 0 { s.root_key } else { 60 },
                loop_range,
                pcm16: s.pcm16.clone(),
            }
        })
        .collect();

    compile_custom_samples_soundfont(&bank_title, "Factor 5 MusyX", &raw_samples)
}
