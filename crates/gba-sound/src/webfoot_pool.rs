//! Webfoot Technologies GBA Stereo DirectSound audio engine detection,
//! sample/song pool scanning, WAV16 export, and SoundFont 2.04 compilation.
//!
//! Thin adapter delegating detection and ROM parsing to [`webfoot_audio`].

#![deny(unsafe_code)]

use std::collections::HashSet;

use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// Engine revision of the Webfoot Technologies Stereo DirectSound driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WebfootRevision {
    /// Early singleton-state driver with 12-byte song headers.
    RevA,
    /// Object-oriented driver with 20-byte self-contained song headers.
    RevB,
    /// Dual-revision 2-in-1 multicart containing both RevA and RevB sub-games.
    DualRev,
    /// Pitch table detected, revision unclassified.
    Unknown,
}

impl From<webfoot_audio::Revision> for WebfootRevision {
    fn from(r: webfoot_audio::Revision) -> Self {
        match r {
            webfoot_audio::Revision::RevA => Self::RevA,
            webfoot_audio::Revision::RevB => Self::RevB,
            webfoot_audio::Revision::DualRev => Self::DualRev,
            webfoot_audio::Revision::Unknown => Self::Unknown,
            _ => Self::Unknown,
        }
    }
}

/// Extracted Webfoot Technologies signed 8-bit PCM sample entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebfootSampleEntry {
    /// Sequential sample index within the scan output.
    pub index: usize,
    /// Absolute ROM file offset where the signed 8-bit PCM payload begins.
    pub rom_offset: usize,
    /// Payload length in bytes.
    pub size: usize,
    /// Native playback sample rate in Hz.
    pub sample_rate: u32,
    /// MIDI root key (`60` = Middle C).
    pub root_key: u8,
    /// Optional loop range `(loop_start, loop_end)` in samples.
    pub loop_range: Option<(u32, u32)>,
    /// Extracted signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Complete scan summary for a Webfoot Technologies Stereo DirectSound ROM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebfootScanSummary {
    /// Detected engine revision.
    pub revision: WebfootRevision,
    /// Number of discovered sample tables (Music + SFX).
    pub table_count: usize,
    /// Number of discovered tracker songs.
    pub song_count: usize,
    /// Extracted unique signed 8-bit PCM samples.
    pub samples: Vec<WebfootSampleEntry>,
}

/// Detect whether `rom` contains the Webfoot Technologies Stereo DirectSound engine.
#[must_use]
pub fn detect_webfoot_engine(rom: &[u8]) -> Option<WebfootRevision> {
    if !webfoot_audio::detect(rom) {
        return None;
    }
    Some(
        webfoot_audio::Revision::detect_from_bytes(rom)
            .map(Into::into)
            .unwrap_or(WebfootRevision::Unknown),
    )
}

/// Scan a Webfoot Technologies Stereo DirectSound ROM for sample tables, samples, and songs.
pub fn scan_webfoot_engine(rom: &[u8]) -> Option<WebfootScanSummary> {
    let audio = webfoot_audio::parse(rom).ok()?;
    let revision: WebfootRevision = audio.revision().into();
    let table_count = audio.sample_tables().len();
    let song_count = audio.total_songs();

    let mut samples = Vec::new();
    let mut seen_offsets = HashSet::new();
    for tbl_idx in 0..table_count {
        for s in audio.table_samples(tbl_idx) {
            let rom_offset = s.rom_offset as usize;
            if !seen_offsets.insert((rom_offset, s.pcm.len())) {
                continue;
            }
            let rate = if s.header.sample_rate == 0 {
                webfoot_audio::MIXER_SAMPLE_RATE_HZ
            } else {
                u32::from(s.header.sample_rate)
            };
            let loop_range = if s.header.is_looped() && usize::from(s.header.loop_start) < s.pcm.len()
            {
                Some((u32::from(s.header.loop_start), s.pcm.len() as u32))
            } else {
                None
            };
            let index = samples.len();
            samples.push(WebfootSampleEntry {
                index,
                rom_offset,
                size: s.pcm.len(),
                sample_rate: rate,
                root_key: 60,
                loop_range,
                pcm_data: s.pcm.iter().map(|&b| b as i8).collect(),
            });
        }
    }

    Some(WebfootScanSummary {
        revision,
        table_count,
        song_count,
        samples,
    })
}

/// Scan a GBA ROM for Webfoot Technologies signed 8-bit PCM samples.
pub fn scan_webfoot_soundpool(rom: &[u8]) -> Vec<WebfootSampleEntry> {
    scan_webfoot_engine(rom)
        .map(|s| s.samples)
        .unwrap_or_default()
}

/// Extract all Webfoot Technologies samples as 16-bit mono RIFF WAV files.
pub fn export_webfoot_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_webfoot_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_webfoot_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!("webfoot_sample_{:03}_{:06X}.wav", s.index, s.rom_offset);
        out.push((s.index, name, wav));
    }
    Ok(out)
}

/// Compile all extracted Webfoot Technologies samples into a SoundFont 2.04 (`.sf2`).
pub fn compile_webfoot_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_webfoot_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_webfoot_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Webfoot Stereo)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            RawSoundFontSample {
                name: format!("WFT_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range: s.loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(
        &bank_title,
        "Webfoot Technologies Stereo DirectSound",
        &raw_samples,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;

    fn build_synthetic_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0xA0..0xAC].copy_from_slice(b"WEBFOOTTEST\0");
        let base = webfoot_audio::GBA_ROM_BASE;

        for (i, &val) in webfoot_audio::PITCH_TABLE.iter().enumerate() {
            let off = 0x1000 + i * 4;
            rom[off..off + 4].copy_from_slice(&val.to_le_bytes());
        }

        // 3 contiguous 64-byte PCM samples at 0x2000, descriptors at 0x2400
        for i in 0..3usize {
            let p_off = 0x2000 + i * 64;
            for j in 0..64 {
                rom[p_off + j] = (((j as i32 % 16) - 8) * 6) as i8 as u8;
            }
            let d_off = 0x2400 + i * 12;
            rom[d_off..d_off + 4].copy_from_slice(&(base + p_off as u32).to_le_bytes());
            rom[d_off + 4] = 0x01;
            rom[d_off + 5] = 0;
            rom[d_off + 6..d_off + 8].copy_from_slice(&8u16.to_le_bytes());
            rom[d_off + 8..d_off + 10].copy_from_slice(&64u16.to_le_bytes());
            rom[d_off + 10..d_off + 12].copy_from_slice(&16_000u16.to_le_bytes());
        }
        rom
    }

    #[test]
    fn detects_and_exports_webfoot_wav_and_sf2() {
        let rom = build_synthetic_rom();
        assert!(detect_webfoot_engine(&rom).is_some());
        let summary = scan_webfoot_engine(&rom).expect("valid summary");
        assert_eq!(summary.samples.len(), 3);

        let wavs = export_webfoot_samples_wav16(&rom).expect("wav");
        assert_eq!(wavs.len(), 3);
        assert_eq!(&wavs[0].2[0..4], b"RIFF");

        let sf2 = compile_webfoot_soundfont(&rom, Some("Synthetic Webfoot")).expect("sf2");
        let val = validate_soundfont(&sf2.sf2_bytes).expect("valid sf2");
        assert_eq!(val.sample_count, 3);
    }

    #[test]
    fn rejects_empty_rom() {
        assert_eq!(detect_webfoot_engine(&[]), None);
        assert!(scan_webfoot_soundpool(&[0u8; 512]).is_empty());
    }
}
