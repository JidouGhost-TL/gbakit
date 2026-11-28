//! Skyworks GBA 4-Channel DirectSound A Streaming PCM Mixer detection,
//! clip scanning, WAV16 export, and SoundFont 2.04 compilation.
//!
//! Thin adapter delegating detection and ROM parsing to [`skyworks_pcm_stream`].

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// Engine revision of the Skyworks 4-Channel DirectSound A Streaming PCM Mixer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SkyworksPcmRevision {
    /// 8,001 Hz 1:1 stepping (`TM0CNT_L = 0xF7CF`).
    RevA,
    /// 11,030 Hz 8.8 fixed-point pitch stepping (`TM0CNT_L = 0xFA0F`).
    RevB,
}

impl From<skyworks_pcm_stream::SkyworksPcmVersion> for SkyworksPcmRevision {
    fn from(v: skyworks_pcm_stream::SkyworksPcmVersion) -> Self {
        match v {
            skyworks_pcm_stream::SkyworksPcmVersion::RevA => Self::RevA,
            skyworks_pcm_stream::SkyworksPcmVersion::RevB => Self::RevB,
            _ => Self::RevA,
        }
    }
}

/// Extracted Skyworks streaming PCM clip entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkyworksPcmSampleEntry {
    /// Sequential clip index within the scan output.
    pub index: usize,
    /// Whether this clip is classified as a looped stream/BGM vs one-shot SFX.
    pub is_stream: bool,
    /// Absolute ROM file offset where the signed 8-bit PCM payload begins.
    pub rom_offset: usize,
    /// Payload length in bytes.
    pub size: usize,
    /// Native playback sample rate in Hz (`8_001` or `11_030`).
    pub sample_rate: u32,
    /// MIDI root key (`60` = Middle C).
    pub root_key: u8,
    /// Optional loop range `(loop_start, loop_end)` in samples.
    pub loop_range: Option<(u32, u32)>,
    /// Extracted signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Complete scan summary for a Skyworks 4-Channel Streaming PCM ROM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkyworksPcmScanSummary {
    /// Primary detected engine revision.
    pub revision: SkyworksPcmRevision,
    /// Number of detected engine instances (1 for standard ROMs, >1 for multicarts).
    pub instance_count: usize,
    /// Extracted signed 8-bit PCM clips.
    pub samples: Vec<SkyworksPcmSampleEntry>,
}

/// Detect whether `rom` contains the Skyworks 4-Channel DirectSound A Streaming PCM Mixer.
#[must_use]
pub fn detect_skyworks_pcm_engine(rom: &[u8]) -> Option<SkyworksPcmRevision> {
    skyworks_pcm_stream::SkyworksPcmVersion::detect_from_bytes(rom).map(Into::into)
}

/// Scan a Skyworks 4-Channel Streaming PCM ROM for engine instances and clips.
pub fn scan_skyworks_pcm_engine(rom: &[u8]) -> Option<SkyworksPcmScanSummary> {
    let bank = skyworks_pcm_stream::parse(rom).ok()?;
    let instances = bank.instances();
    let revision: SkyworksPcmRevision = instances
        .first()
        .map(|i| i.version.into())
        .unwrap_or(SkyworksPcmRevision::RevA);

    let mut samples = Vec::with_capacity(bank.clips().len());
    for (idx, clip) in bank.clips().iter().enumerate() {
        let Some(pcm) = clip.pcm(rom) else {
            continue;
        };
        let is_stream = clip.is_looped();
        let loop_range = if is_stream && !pcm.is_empty() {
            Some((0, pcm.len() as u32))
        } else {
            None
        };
        samples.push(SkyworksPcmSampleEntry {
            index: idx,
            is_stream,
            rom_offset: clip.start_offset,
            size: pcm.len(),
            sample_rate: clip.sample_rate_hz,
            root_key: 60,
            loop_range,
            pcm_data: pcm.iter().map(|&b| b as i8).collect(),
        });
    }

    Some(SkyworksPcmScanSummary {
        revision,
        instance_count: instances.len(),
        samples,
    })
}

/// Scan a GBA ROM for Skyworks 4-Channel Streaming PCM clips.
pub fn scan_skyworks_pcm_soundpool(rom: &[u8]) -> Vec<SkyworksPcmSampleEntry> {
    scan_skyworks_pcm_engine(rom)
        .map(|s| s.samples)
        .unwrap_or_default()
}

/// Extract all Skyworks 4-Channel Streaming PCM clips as 16-bit mono RIFF WAV files.
pub fn export_skyworks_pcm_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_skyworks_pcm_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_skyworks_pcm_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let kind = if s.is_stream { "stream" } else { "sfx" };
        let name = format!(
            "skyworks_pcm_{kind}_{:03}_{:06X}.wav",
            s.index, s.rom_offset
        );
        out.push((s.index, name, wav));
    }
    Ok(out)
}

/// Compile all extracted Skyworks 4-Channel Streaming PCM clips into a SoundFont 2.04 (`.sf2`).
pub fn compile_skyworks_pcm_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_skyworks_pcm_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_skyworks_pcm_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Skyworks PCM Stream)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            RawSoundFontSample {
                name: format!("SWP_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range: s.loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(
        &bank_title,
        "Skyworks 4-Channel DirectSound A Streaming PCM Mixer",
        &raw_samples,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;

    fn encode_thumb_bl(from: usize, to: usize) -> [u8; 4] {
        let diff = (to as isize) - ((from as isize) + 4);
        let raw = (diff as u32) & 0x007F_FFFF;
        let hi = 0xF000u16 | (((raw >> 12) & 0x07FF) as u16);
        let lo = 0xF800u16 | (((raw >> 1) & 0x07FF) as u16);
        let b1 = hi.to_le_bytes();
        let b2 = lo.to_le_bytes();
        [b1[0], b1[1], b2[0], b2[1]]
    }

    fn build_synthetic_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0xA0..0xAC].copy_from_slice(b"SKYPCMTEST\0\0");
        let base = skyworks_pcm_stream::GBA_ROM_BASE;

        let init_words: [u32; 8] = [
            0x0400_00D4,
            0x8500_0008,
            0x0400_00BC,
            0x0400_0084,
            0x0000_0B0E,
            0x0400_00A0,
            0xF660_0004,
            0x0080_FA0F,
        ];
        for (i, &w) in init_words.iter().enumerate() {
            rom[0x0800 + i * 4..0x0804 + i * 4].copy_from_slice(&w.to_le_bytes());
        }
        // play_sfx at 0x0900
        for (i, &h) in [0xB5F0u16, 0x226C, 0x301C, 0x3038, 0x3034].iter().enumerate() {
            rom[0x0900 + i * 2..0x0902 + i * 2].copy_from_slice(&h.to_le_bytes());
        }
        // Call site at 0x1000
        rom[0x1000..0x1002].copy_from_slice(&0x4903u16.to_le_bytes());
        rom[0x1002..0x1004].copy_from_slice(&0x4A04u16.to_le_bytes());
        rom[0x1004..0x1006].copy_from_slice(&0x1A52u16.to_le_bytes());
        let bl = encode_thumb_bl(0x1006, 0x0900);
        rom[0x1006..0x100A].copy_from_slice(&bl);
        rom[0x1010..0x1014].copy_from_slice(&(base + 0x2000).to_le_bytes());
        rom[0x1014..0x1018].copy_from_slice(&(base + 0x2100).to_le_bytes());
        for (i, b) in rom[0x2000..0x2100].iter_mut().enumerate() {
            *b = (((i as i32 % 16) - 8) * 6) as i8 as u8;
        }
        rom
    }

    #[test]
    fn detects_and_exports_skyworks_pcm_wav_and_sf2() {
        let rom = build_synthetic_rom();
        assert_eq!(
            detect_skyworks_pcm_engine(&rom),
            Some(SkyworksPcmRevision::RevB)
        );
        let summary = scan_skyworks_pcm_engine(&rom).expect("valid summary");
        assert_eq!(summary.samples.len(), 1);

        let wavs = export_skyworks_pcm_samples_wav16(&rom).expect("wav");
        assert_eq!(wavs.len(), 1);
        assert_eq!(&wavs[0].2[0..4], b"RIFF");

        let sf2 = compile_skyworks_pcm_soundfont(&rom, Some("Synthetic SWP")).expect("sf2");
        let val = validate_soundfont(&sf2.sf2_bytes).expect("valid sf2");
        assert_eq!(val.sample_count, 1);
    }

    #[test]
    fn rejects_empty_rom() {
        assert_eq!(detect_skyworks_pcm_engine(&[]), None);
        assert!(scan_skyworks_pcm_soundpool(&[0u8; 512]).is_empty());
    }
}
