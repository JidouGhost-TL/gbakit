//! Hudson / Red / Rocket Games JP DirectSound audio engine detection,
//! 7-section archive & standalone bank scanning, WAV16 export, and SoundFont 2.04 compilation.
//!
//! Thin adapter delegating detection and zero-copy archive/bank parsing to [`hudson_jp_audio`].

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use hudson_jp_audio::{SequenceKind, StorageLayout, PCM_HEADER_SIZE};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// Storage layout discovered in a Hudson / Red / Rocket Games JP ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HudsonJpLayout {
    /// Standard 7-section chunked audio archive(s).
    Chunked7Section,
    /// Standalone PCM and song/SFX linked-chain banks.
    StandaloneBanks,
}

impl From<StorageLayout> for HudsonJpLayout {
    fn from(layout: StorageLayout) -> Self {
        match layout {
            StorageLayout::Chunked7Section => Self::Chunked7Section,
            _ => Self::StandaloneBanks,
        }
    }
}

/// Extracted Hudson / Red / Rocket Games JP signed 8-bit PCM sample entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HudsonJpSampleEntry {
    /// Sequential sample index across all banks.
    pub index: usize,
    /// Archive index (or `0` for standalone banks).
    pub archive_index: usize,
    /// Bank index (`0` = BGM PCM, `1` = SFX PCM, or standalone bank index).
    pub bank_index: usize,
    /// Absolute ROM file offset of the 6-byte record header.
    pub rom_offset: usize,
    /// Absolute ROM file offset of the signed 8-bit PCM payload (`rom_offset + 6`).
    pub sample_start_offset: usize,
    /// Sample payload length in bytes.
    pub size: usize,
    /// Native playback sample rate in Hz.
    pub sample_rate: u32,
    /// MIDI root key (`60` = Middle C).
    pub root_key: u8,
    /// Extracted signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Extracted 6-track BGM or SFX sequence descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HudsonJpSongEntry {
    /// Sequence index within its bank.
    pub index: usize,
    /// Archive or standalone bank index.
    pub archive_index: usize,
    /// `true` for SFX, `false` for BGM.
    pub is_sfx: bool,
    /// Absolute ROM file offset of the song header.
    pub rom_offset: usize,
    /// Record size in bytes.
    pub record_size: usize,
}

/// Complete scan summary for a Hudson / Red / Rocket Games JP ROM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HudsonJpScanSummary {
    /// ROM file offset of the Thumb DirectSound FIFO pre-fill routine.
    pub driver_offset: usize,
    /// Discovered storage layout.
    pub layout: HudsonJpLayout,
    /// Number of discovered archives (or standalone PCM/song banks).
    pub archive_count: usize,
    /// All extracted PCM samples.
    pub samples: Vec<HudsonJpSampleEntry>,
    /// All extracted BGM and SFX sequence headers.
    pub songs: Vec<HudsonJpSongEntry>,
}

/// Locate the ROM offset of the Hudson / Red / Rocket Games JP DirectSound FIFO pre-fill routine.
#[must_use]
pub fn find_hudson_jp_driver_offset(rom: &[u8]) -> Option<usize> {
    hudson_jp_audio::find_driver_offset(rom)
}

/// Detect whether `rom` uses the Hudson / Red / Rocket Games JP DirectSound audio engine.
#[must_use]
pub fn detect_hudson_jp_engine(rom: &[u8]) -> bool {
    hudson_jp_audio::detect(rom)
}

/// Scan a Hudson / Red / Rocket Games JP ROM for all PCM samples and sequences.
pub fn scan_hudson_jp_engine(rom: &[u8]) -> Option<HudsonJpScanSummary> {
    let audio = hudson_jp_audio::parse(rom).ok()?;

    let mut samples = Vec::with_capacity(audio.total_pcm_samples());
    let mut songs = Vec::with_capacity(audio.total_sequences());

    let archive_count = match audio.layout() {
        StorageLayout::Chunked7Section => {
            let count = audio.archives().len();
            for arch_idx in 0..count {
                for bank_idx in 0..2 {
                    for s in audio.iter_pcm_bank(arch_idx, bank_idx) {
                        samples.push(HudsonJpSampleEntry {
                            index: samples.len(),
                            archive_index: s.archive_index,
                            bank_index: s.bank_index,
                            rom_offset: s.rom_offset,
                            sample_start_offset: s.rom_offset.saturating_add(PCM_HEADER_SIZE),
                            size: s.data.len(),
                            sample_rate: u32::from(s.sample_rate_hz),
                            root_key: 60,
                            pcm_data: s.iter_i8().collect(),
                        });
                    }
                }
                for (kind, is_sfx) in [(SequenceKind::Bgm, false), (SequenceKind::Sfx, true)] {
                    for seq in audio.iter_sequence_bank(arch_idx, kind) {
                        songs.push(HudsonJpSongEntry {
                            index: seq.song_index,
                            archive_index: seq.archive_index,
                            is_sfx,
                            rom_offset: seq.rom_offset,
                            record_size: usize::from(seq.record_size),
                        });
                    }
                }
            }
            count
        }
        _ => {
            let pcm_bank_count = audio.standalone_pcm_banks().len();
            for bank_idx in 0..pcm_bank_count {
                for s in audio.iter_pcm_bank(0, bank_idx) {
                    samples.push(HudsonJpSampleEntry {
                        index: samples.len(),
                        archive_index: 0,
                        bank_index: bank_idx,
                        rom_offset: s.rom_offset,
                        sample_start_offset: s.rom_offset.saturating_add(PCM_HEADER_SIZE),
                        size: s.data.len(),
                        sample_rate: u32::from(s.sample_rate_hz),
                        root_key: 60,
                        pcm_data: s.iter_i8().collect(),
                    });
                }
            }
            let song_bank_count = audio.standalone_song_banks().len();
            for bank_idx in 0..song_bank_count {
                let kind = if bank_idx % 2 == 0 {
                    SequenceKind::Bgm
                } else {
                    SequenceKind::Sfx
                };
                for seq in audio.iter_sequence_bank(bank_idx / 2, kind) {
                    songs.push(HudsonJpSongEntry {
                        index: seq.song_index,
                        archive_index: bank_idx,
                        is_sfx: bank_idx != 0,
                        rom_offset: seq.rom_offset,
                        record_size: usize::from(seq.record_size),
                    });
                }
            }
            pcm_bank_count.max(song_bank_count)
        }
    };

    Some(HudsonJpScanSummary {
        driver_offset: audio.driver_offset(),
        layout: audio.layout().into(),
        archive_count,
        samples,
        songs,
    })
}

/// Scan a GBA ROM for Hudson / Red / Rocket Games JP signed 8-bit PCM samples.
pub fn scan_hudson_jp_soundpool(rom: &[u8]) -> Vec<HudsonJpSampleEntry> {
    scan_hudson_jp_engine(rom)
        .map(|s| s.samples)
        .unwrap_or_default()
}

/// Extract all Hudson / Red / Rocket Games JP samples as 16-bit mono RIFF WAV files.
pub fn export_hudson_jp_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_hudson_jp_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_hudson_jp_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!("hudson_jp_sample_{:03}_{:06X}.wav", s.index, s.rom_offset);
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Hudson / Red / Rocket Games JP samples into a SoundFont 2.04 (`.sf2`).
pub fn compile_hudson_jp_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_hudson_jp_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_hudson_jp_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Hudson JP DirectSound)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            RawSoundFontSample {
                name: format!("HUD_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range: None,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(
        &bank_title,
        "Hudson / Red / Rocket Games JP DirectSound Engine",
        &raw_samples,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;

    const LITERAL_POOL_A: [u8; 28] = [
        0xa0, 0x00, 0x00, 0x04, 0xa1, 0x00, 0x00, 0x04, 0xa2, 0x00, 0x00, 0x04, 0xa3, 0x00, 0x00,
        0x04, 0xbc, 0x00, 0x00, 0x04, 0xc0, 0x00, 0x00, 0x04, 0x40, 0xb6, 0x00, 0x00,
    ];
    const LITERAL_POOL_B: [u8; 32] = [
        0xa4, 0x00, 0x00, 0x04, 0xa5, 0x00, 0x00, 0x04, 0xa6, 0x00, 0x00, 0x04, 0xa7, 0x00, 0x00,
        0x04, 0xc8, 0x00, 0x00, 0x04, 0xcc, 0x00, 0x00, 0x04, 0x40, 0xb6, 0x00, 0x00, 0xe0, 0x0a,
        0x00, 0x01,
    ];
    const THUMB_PROLOGUE: [u8; 16] = [
        0xf0, 0xb5, 0x47, 0x46, 0x80, 0xb4, 0x03, 0x1c, 0x00, 0x29, 0x02, 0xd0, 0x01, 0x29, 0x41,
        0xd0,
    ];
    const POOL_B_DELTA: usize = 0xa0;
    const PROLOGUE_TO_POOL_A: usize = 0x78;

    fn build_synthetic_rom_with_archive() -> Vec<u8> {
        let mut rom = vec![0u8; 0x600];
        rom[0xA0..0xAC].copy_from_slice(b"HUDSONJPTST\0");

        let drv_off = 0x100usize;
        rom[drv_off..drv_off + THUMB_PROLOGUE.len()].copy_from_slice(&THUMB_PROLOGUE);
        rom[drv_off + PROLOGUE_TO_POOL_A..drv_off + PROLOGUE_TO_POOL_A + LITERAL_POOL_A.len()]
            .copy_from_slice(&LITERAL_POOL_A);
        let pool_b_off = drv_off + PROLOGUE_TO_POOL_A + POOL_B_DELTA;
        rom[pool_b_off..pool_b_off + LITERAL_POOL_B.len()].copy_from_slice(&LITERAL_POOL_B);

        let mut pos = 0x300usize;
        // Section 0: 1 BGM
        rom[pos..pos + 4].copy_from_slice(&28u32.to_le_bytes());
        pos += 4;
        rom[pos..pos + 2].copy_from_slice(&20u16.to_le_bytes());
        for ch in 0..6u16 {
            let o = 14 + ch;
            rom[pos + 2 + usize::from(ch) * 2..pos + 4 + usize::from(ch) * 2]
                .copy_from_slice(&o.to_le_bytes());
        }
        pos += 28;

        // Section 1: 1 SFX
        rom[pos..pos + 4].copy_from_slice(&28u32.to_le_bytes());
        pos += 4;
        rom[pos..pos + 2].copy_from_slice(&20u16.to_le_bytes());
        for ch in 0..6u16 {
            let o = 14 + ch;
            rom[pos + 2 + usize::from(ch) * 2..pos + 4 + usize::from(ch) * 2]
                .copy_from_slice(&o.to_le_bytes());
        }
        pos += 28;

        // Sections 2..4 placeholders
        for _ in 0..3 {
            rom[pos..pos + 4].copy_from_slice(&4u32.to_le_bytes());
            pos += 8;
        }

        // Section 5: PCM Bank 0 (1 sample of 70 bytes + 4-byte zero terminator = 76)
        rom[pos..pos + 4].copy_from_slice(&76u32.to_le_bytes());
        pos += 4;
        rom[pos..pos + 4].copy_from_slice(&70u32.to_le_bytes());
        rom[pos + 4..pos + 6].copy_from_slice(&11025u16.to_le_bytes());
        for i in 0..64usize {
            rom[pos + 6 + i] = (i as u8).wrapping_mul(4);
        }
        pos += 76;

        // Section 6: empty PCM Bank 1
        rom[pos..pos + 4].copy_from_slice(&4u32.to_le_bytes());
        rom
    }

    #[test]
    fn detects_and_exports_hudson_jp_archive() {
        let rom = build_synthetic_rom_with_archive();
        assert!(detect_hudson_jp_engine(&rom));
        assert_eq!(find_hudson_jp_driver_offset(&rom), Some(0x100));

        let summary = scan_hudson_jp_engine(&rom).expect("valid summary");
        assert_eq!(summary.layout, HudsonJpLayout::Chunked7Section);
        assert_eq!(summary.archive_count, 1);
        assert_eq!(summary.samples.len(), 1);
        assert_eq!(summary.songs.len(), 2);
        assert_eq!(summary.samples[0].sample_rate, 11_025);
        assert_eq!(summary.samples[0].size, 64);

        let wavs = export_hudson_jp_samples_wav16(&rom).expect("wav export");
        assert_eq!(wavs.len(), 1);
        assert_eq!(&wavs[0].2[0..4], b"RIFF");

        let sf2 = compile_hudson_jp_soundfont(&rom, Some("Synthetic Hudson JP")).expect("sf2");
        let val = validate_soundfont(&sf2.sf2_bytes).expect("valid sf2");
        assert_eq!(val.sample_count, 1);
    }

    #[test]
    fn rejects_empty_and_malformed() {
        assert!(!detect_hudson_jp_engine(&[]));
        assert!(scan_hudson_jp_soundpool(&[0u8; 512]).is_empty());
    }
}
