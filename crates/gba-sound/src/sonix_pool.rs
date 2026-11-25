//! Cosmigo Sonix Sound System (`SNX3`) sample bank scanning, unsigned 8-bit PCM
//! (`0x80` DC bias) conversion, WAV16 export, and SoundFont 2.04 soundbank
//! compilation.
//!
//! Retail Cosmigo Sonix GBA ROMs embed `SNX3` soundbank containers holding a
//! 28-byte header followed by a 16-byte-per-entry sample descriptor table:
//! - `+0x00..+0x04`: `length: u32` (PCM payload byte length)
//! - `+0x04..+0x08`: `loop_len: u32` (loop length in samples)
//! - `+0x08..+0x0A`: `flags: u16` (`bit 5` = `0x0020` indicates looped playback)
//! - `+0x0A..+0x0C`: `sample_rate_hz: u16` (native playback rate in Hz)
//! - `+0x0C..+0x10`: `data_off: u32` (PCM byte offset relative to `"SNX3"`)

#![deny(unsafe_code)]

use std::collections::HashSet;

use cosmigo_sonix::{SonixSampleView, SonixVersion};
use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// 4-byte Cosmigo Sonix v3 soundbank container magic.
pub const SONIX_SNX3_MAGIC: &[u8; 4] = b"SNX3";

/// Size of an `SNX3` header in bytes (`0x1C`).
const SNX3_HEADER_SIZE: usize = 0x1C;

/// Size of a single `SNX3` sample descriptor record in bytes.
const SNX3_SAMPLE_REC_SIZE: usize = 16;

/// Flag bit inside `SNX3` sample descriptor `flags` indicating looped playback.
const SNX3_FLAG_LOOPED: u16 = 0x0020;

/// Maximum individual sample length in bytes accepted by the scanner (`1 MiB`).
const MAX_SAMPLE_BYTES: usize = 1024 * 1024;

/// Maximum number of descriptors in a single `SNX3` bank.
const MAX_BANK_SAMPLES: usize = 4096;

/// Minimum and maximum accepted sample rates in Hz.
const MIN_SAMPLE_RATE_HZ: u32 = 1_000;
const MAX_SAMPLE_RATE_HZ: u32 = 48_000;

/// Extracted Cosmigo Sonix PCM sample entry (converted from unsigned 8-bit
/// `0x80`-biased PCM to centered signed 8-bit `i8`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SonixSampleEntry {
    /// Sample index within the detected pool.
    pub index: usize,
    /// Absolute ROM file offset where the 16-byte `SNX3` descriptor begins.
    pub rom_offset: usize,
    /// Absolute ROM file offset where the unsigned 8-bit PCM payload begins.
    pub sample_start_offset: usize,
    /// Sample payload length in bytes.
    pub size: usize,
    /// Native playback sample rate in Hz.
    pub sample_rate: u32,
    /// MIDI root note (`60` = Middle C / C4).
    pub root_key: u8,
    /// Raw `SNX3` sample flags word.
    pub flags: u16,
    /// Optional loop range `(loop_start, loop_end)` in samples.
    pub loop_range: Option<(u32, u32)>,
    /// Extracted centered signed 8-bit linear mono PCM samples (`u8 - 128`).
    pub pcm_data: Vec<i8>,
}

/// Detect whether a GBA ROM uses the Cosmigo Sonix Sound System.
pub fn detect_sonix_engine(rom: &[u8]) -> bool {
    SonixVersion::detect_from_bytes(rom).is_some()
}

/// Scan a GBA ROM for Cosmigo Sonix `SNX3` sample banks and extract all
/// unsigned 8-bit PCM payloads (converted to centered signed 8-bit PCM).
pub fn scan_sonix_soundpool(rom: &[u8]) -> Vec<SonixSampleEntry> {
    if rom.len() < SNX3_HEADER_SIZE + SNX3_SAMPLE_REC_SIZE {
        return Vec::new();
    }

    let mut samples = Vec::new();
    let mut seen_pcm_offsets = HashSet::new();
    let mut search_from = 0usize;

    while let Some(bank_off) = find_subslice(rom, SONIX_SNX3_MAGIC, search_from) {
        search_from = bank_off + SONIX_SNX3_MAGIC.len();
        if bank_off + SNX3_HEADER_SIZE > rom.len() {
            break;
        }

        let total_size = read_u32_le(rom, bank_off + 0x0C) as usize;
        let table_end = read_u32_le(rom, bank_off + 0x14) as usize;
        let table_start = read_u32_le(rom, bank_off + 0x18) as usize;

        if table_start < SNX3_HEADER_SIZE
            || table_end <= table_start
            || !(table_end - table_start).is_multiple_of(SNX3_SAMPLE_REC_SIZE)
        {
            continue;
        }
        let bank_end = match bank_off.checked_add(total_size) {
            Some(end) if total_size >= table_end && end <= rom.len() => end,
            _ => continue,
        };

        let count = (table_end - table_start) / SNX3_SAMPLE_REC_SIZE;
        if count == 0 || count > MAX_BANK_SAMPLES {
            continue;
        }

        let mut valid_in_bank = Vec::with_capacity(count);
        let mut all_valid = true;
        for i in 0..count {
            let rec_off = bank_off + table_start + i * SNX3_SAMPLE_REC_SIZE;
            if rec_off + SNX3_SAMPLE_REC_SIZE > bank_end {
                all_valid = false;
                break;
            }
            let length = read_u32_le(rom, rec_off) as usize;
            let loop_len = read_u32_le(rom, rec_off + 4);
            let flags = read_u16_le(rom, rec_off + 8);
            let rate_hz = u32::from(read_u16_le(rom, rec_off + 10));
            let data_rel = read_u32_le(rom, rec_off + 12) as usize;

            if length == 0
                || length > MAX_SAMPLE_BYTES
                || !(MIN_SAMPLE_RATE_HZ..=MAX_SAMPLE_RATE_HZ).contains(&rate_hz)
                || data_rel < table_end
            {
                all_valid = false;
                break;
            }

            let Some(pcm_start) = bank_off.checked_add(data_rel) else {
                all_valid = false;
                break;
            };
            let Some(pcm_end) = pcm_start.checked_add(length) else {
                all_valid = false;
                break;
            };
            if pcm_end > bank_end {
                all_valid = false;
                break;
            }

            let (loop_start, loop_end) =
                if (flags & SNX3_FLAG_LOOPED) != 0 && loop_len > 0 && (loop_len as usize) <= length
                {
                    let ls = (length - loop_len as usize) as u32;
                    let le = length as u32;
                    if ls < le {
                        (Some(ls), Some(le))
                    } else {
                        (None, None)
                    }
                } else {
                    (None, None)
                };

            let view = SonixSampleView::from_pcm_bytes(
                rate_hz,
                60,
                loop_start,
                loop_end,
                &rom[pcm_start..pcm_end],
            );
            if view.validate_loop().is_err() {
                all_valid = false;
                break;
            }

            valid_in_bank.push((rec_off, pcm_start, flags, view));
        }

        if !all_valid || valid_in_bank.is_empty() {
            continue;
        }

        for (rec_off, pcm_start, flags, view) in valid_in_bank {
            if !seen_pcm_offsets.insert(pcm_start) {
                continue;
            }
            let pcm_data: Vec<i8> = view
                .pcm_bytes
                .iter()
                .map(|&b| ((b as i16) - 128) as i8)
                .collect();
            let loop_range = view.loop_start.zip(view.loop_end);
            let index = samples.len();
            samples.push(SonixSampleEntry {
                index,
                rom_offset: rec_off,
                sample_start_offset: pcm_start,
                size: view.len(),
                sample_rate: view.sample_rate_hz,
                root_key: view.root_key,
                flags,
                loop_range,
                pcm_data,
            });
        }

        search_from = bank_end;
    }

    samples
}

/// Extract all Cosmigo Sonix samples from a ROM and encode them as standard
/// 16-bit uncompressed mono RIFF WAV files.
///
/// Returns a list of `(sample_index, recommended_filename, wav_bytes)`.
pub fn export_sonix_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_sonix_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_sonix_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!(
            "sonix_sample_{:03}_{:06X}.wav",
            s.index, s.sample_start_offset
        );
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Cosmigo Sonix samples into a standard SoundFont 2.04
/// (`.sf2`) soundbank.
pub fn compile_cosmigo_sonix_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_sonix_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_sonix_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Cosmigo Sonix)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            RawSoundFontSample {
                name: format!("SNX_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range: s.loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(&bank_title, "Cosmigo Sonix Sound System", &raw_samples)
}

#[inline]
fn read_u16_le(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}

#[inline]
fn read_u32_le(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

fn find_subslice(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    if needle.is_empty() || start + needle.len() > haystack.len() {
        return None;
    }
    haystack[start..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|pos| start + pos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;

    fn build_synthetic_sonix_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x1000];
        rom[0xA0..0xAC].copy_from_slice(b"SONIXTEST\0\0\0");

        let bank_off = 0x0200usize;
        let table_start = SNX3_HEADER_SIZE; // 0x1C
        let num_samples = 3usize;
        let table_end = table_start + num_samples * SNX3_SAMPLE_REC_SIZE; // 0x4C
        let pcm_base = 0x0080usize;

        let specs: [(usize, u32, u16, u16); 3] = [
            (64, 16, 0x0200, 10_512), // one-shot
            (128, 96, 0x0220, 16_464), // looped (32..128)
            (96, 48, 0x0120, 8_372),   // looped (48..96)
        ];

        let mut data_cursor = pcm_base;
        for (i, &(len, loop_len, flags, rate)) in specs.iter().enumerate() {
            let rec = bank_off + table_start + i * SNX3_SAMPLE_REC_SIZE;
            rom[rec..rec + 4].copy_from_slice(&(len as u32).to_le_bytes());
            rom[rec + 4..rec + 8].copy_from_slice(&loop_len.to_le_bytes());
            rom[rec + 8..rec + 10].copy_from_slice(&flags.to_le_bytes());
            rom[rec + 10..rec + 12].copy_from_slice(&rate.to_le_bytes());
            rom[rec + 12..rec + 16].copy_from_slice(&(data_cursor as u32).to_le_bytes());

            for k in 0..len {
                let u = (128i32 + (((k as i32) % 16) - 8) * 6) as u8;
                rom[bank_off + data_cursor + k] = u;
            }
            data_cursor += len;
        }

        let total_size = data_cursor as u32;
        rom[bank_off..bank_off + 4].copy_from_slice(SONIX_SNX3_MAGIC);
        rom[bank_off + 0x04..bank_off + 0x08].copy_from_slice(&0x0042_120Eu32.to_le_bytes());
        rom[bank_off + 0x08..bank_off + 0x0C].copy_from_slice(&(table_end as u32).to_le_bytes());
        rom[bank_off + 0x0C..bank_off + 0x10].copy_from_slice(&total_size.to_le_bytes());
        rom[bank_off + 0x10..bank_off + 0x14].copy_from_slice(&0u32.to_le_bytes());
        rom[bank_off + 0x14..bank_off + 0x18].copy_from_slice(&(table_end as u32).to_le_bytes());
        rom[bank_off + 0x18..bank_off + 0x1C].copy_from_slice(&(table_start as u32).to_le_bytes());

        rom
    }

    #[test]
    fn scans_snx3_soundbank_and_converts_dc_biased_pcm() {
        let rom = build_synthetic_sonix_rom();
        let samples = scan_sonix_soundpool(&rom);
        assert_eq!(samples.len(), 3);

        assert_eq!(samples[0].size, 64);
        assert_eq!(samples[0].sample_rate, 10_512);
        assert_eq!(samples[0].loop_range, None);
        // Unsigned 128 - 48 = 80 -> signed -48
        assert_eq!(samples[0].pcm_data[0], -48);

        assert_eq!(samples[1].size, 128);
        assert_eq!(samples[1].sample_rate, 16_464);
        assert_eq!(samples[1].loop_range, Some((32, 128)));

        assert_eq!(samples[2].size, 96);
        assert_eq!(samples[2].sample_rate, 8_372);
        assert_eq!(samples[2].loop_range, Some((48, 96)));
    }

    #[test]
    fn exports_valid_sonix_wav16_and_soundfont() {
        let rom = build_synthetic_sonix_rom();

        let wavs = export_sonix_samples_wav16(&rom).expect("wav export should succeed");
        assert_eq!(wavs.len(), 3);
        assert_eq!(wavs[0].2.len(), 44 + 64 * 2);
        assert_eq!(&wavs[0].2[0..4], b"RIFF");
        assert_eq!(&wavs[0].2[8..12], b"WAVE");

        let sf2 = compile_cosmigo_sonix_soundfont(&rom, Some("Synthetic Sonix"))
            .expect("sf2 compile should succeed");
        let validation = validate_soundfont(&sf2.sf2_bytes).expect("sf2 should validate");
        assert_eq!(validation.sample_count, 3);
        assert_eq!(validation.preset_count, 3);
        assert_eq!(validation.instrument_count, 3);
        assert_eq!(validation.bank_name, "Synthetic Sonix");
        assert_eq!(validation.sound_engine, "Cosmigo Sonix Sound System");
    }

    #[test]
    fn rejects_non_sonix_roms() {
        let mut rom = vec![0u8; 0x800];
        rom[0x100..0x104].copy_from_slice(SONIX_SNX3_MAGIC);
        assert!(scan_sonix_soundpool(&rom).is_empty());
        assert!(export_sonix_samples_wav16(&rom).is_err());
        assert!(compile_cosmigo_sonix_soundfont(&rom, None).is_err());
    }
}
