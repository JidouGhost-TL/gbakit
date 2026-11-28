//! Destination / Skyworks 10.5 kHz Hybrid PSG+PCM GBA audio engine detection,
//! sample/song pool scanning, WAV16 export, and SoundFont 2.04 compilation.
//!
//! Thin adapter delegating detection and ROM parsing to [`skyworks_hybrid`].

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// Engine revision of the Destination / Skyworks Hybrid PSG+PCM driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SkyworksHybridRevision {
    /// 6 music channels (4 PSG + 2 PCM), 28-byte song table entries.
    Rev1,
    /// 8 music channels (4 PSG + 4 PCM), 36-byte song table entries.
    Rev2,
}

impl From<skyworks_hybrid::Revision> for SkyworksHybridRevision {
    fn from(r: skyworks_hybrid::Revision) -> Self {
        match r {
            skyworks_hybrid::Revision::Rev1 => Self::Rev1,
            skyworks_hybrid::Revision::Rev2 => Self::Rev2,
            _ => Self::Rev2,
        }
    }
}

/// Extracted Destination / Skyworks Hybrid signed 8-bit PCM sample entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkyworksHybridSampleEntry {
    /// Sequential sample index within the scan output.
    pub index: usize,
    /// Whether this sample comes from the SFX (`FIFO_B`) bank vs music (`FIFO_A`) bank.
    pub is_sfx: bool,
    /// Absolute ROM file offset where the signed 8-bit PCM payload begins.
    pub rom_offset: usize,
    /// Payload length in bytes.
    pub size: usize,
    /// Native playback sample rate in Hz (`10_512` music / `10_261` SFX).
    pub sample_rate: u32,
    /// MIDI root key (`60` = Middle C).
    pub root_key: u8,
    /// Optional loop range `(loop_start, loop_end)` in samples.
    pub loop_range: Option<(u32, u32)>,
    /// Extracted signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Complete scan summary for a Destination / Skyworks Hybrid PSG+PCM ROM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkyworksHybridScanSummary {
    /// Detected engine revision (`Rev1` or `Rev2`).
    pub revision: SkyworksHybridRevision,
    /// Number of music PCM samples.
    pub music_sample_count: usize,
    /// Number of valid SFX PCM samples.
    pub sfx_sample_count: usize,
    /// Number of parsed songs.
    pub song_count: usize,
    /// Number of parsed PSG instruments.
    pub psg_instrument_count: usize,
    /// Combined music + SFX signed 8-bit PCM samples.
    pub samples: Vec<SkyworksHybridSampleEntry>,
}

/// Detect whether `rom` contains the Destination / Skyworks Hybrid PSG+PCM audio engine.
#[must_use]
pub fn detect_skyworks_hybrid_engine(rom: &[u8]) -> Option<SkyworksHybridRevision> {
    skyworks_hybrid::Revision::detect_from_bytes(rom).map(Into::into)
}

/// Scan a Destination / Skyworks Hybrid PSG+PCM ROM for samples, songs, and PSG instruments.
pub fn scan_skyworks_hybrid_engine(rom: &[u8]) -> Option<SkyworksHybridScanSummary> {
    let bank = skyworks_hybrid::parse(rom).ok()?;
    let revision: SkyworksHybridRevision = bank.revision().into();
    let mut samples = Vec::with_capacity(bank.total_pcm_samples());

    for s in bank.music_samples() {
        let index = samples.len();
        let loop_range = if s.is_looped && s.loop_end > s.loop_start {
            Some((s.loop_start, s.loop_end))
        } else {
            None
        };
        samples.push(SkyworksHybridSampleEntry {
            index,
            is_sfx: false,
            rom_offset: s.offset,
            size: s.pcm.len(),
            sample_rate: skyworks_hybrid::MUSIC_SAMPLE_RATE_HZ,
            root_key: 60,
            loop_range,
            pcm_data: s.pcm.iter().map(|&b| b as i8).collect(),
        });
    }

    for s in bank.sfx_samples() {
        let index = samples.len();
        let loop_range = if s.is_looped && !s.pcm.is_empty() {
            Some((0, s.pcm.len() as u32))
        } else {
            None
        };
        samples.push(SkyworksHybridSampleEntry {
            index,
            is_sfx: true,
            rom_offset: s.offset,
            size: s.pcm.len(),
            sample_rate: skyworks_hybrid::SFX_SAMPLE_RATE_HZ,
            root_key: 60,
            loop_range,
            pcm_data: s.pcm.iter().map(|&b| b as i8).collect(),
        });
    }

    Some(SkyworksHybridScanSummary {
        revision,
        music_sample_count: bank.music_sample_count(),
        sfx_sample_count: bank.sfx_valid_count(),
        song_count: bank.song_count(),
        psg_instrument_count: bank.psg_instrument_count(),
        samples,
    })
}

/// Scan a GBA ROM for Destination / Skyworks Hybrid signed 8-bit PCM samples.
pub fn scan_skyworks_hybrid_soundpool(rom: &[u8]) -> Vec<SkyworksHybridSampleEntry> {
    scan_skyworks_hybrid_engine(rom)
        .map(|s| s.samples)
        .unwrap_or_default()
}

/// Extract all Destination / Skyworks Hybrid samples as 16-bit mono RIFF WAV files.
pub fn export_skyworks_hybrid_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_skyworks_hybrid_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_skyworks_hybrid_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let kind = if s.is_sfx { "sfx" } else { "music" };
        let name = format!(
            "skyworks_hybrid_{kind}_{:03}_{:06X}.wav",
            s.index, s.rom_offset
        );
        out.push((s.index, name, wav));
    }
    Ok(out)
}

/// Compile all extracted Destination / Skyworks Hybrid samples into a SoundFont 2.04 (`.sf2`).
pub fn compile_skyworks_hybrid_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_skyworks_hybrid_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_skyworks_hybrid_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Skyworks Hybrid)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            let prefix = if s.is_sfx { "SWH_SFX" } else { "SWH_MUS" };
            RawSoundFontSample {
                name: format!("{prefix}_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range: s.loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(
        &bank_title,
        "Destination / Skyworks Hybrid PSG+PCM",
        &raw_samples,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;

    fn put_u32(buf: &mut [u8], off: usize, v: u32) {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn build_synthetic_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0xA0..0xAC].copy_from_slice(b"SKYHYBTEST\0\0");
        let base = skyworks_hybrid::GBA_ROM_BASE;

        // DMA/Timer literal pool
        let pool = 0x1000usize;
        put_u32(&mut rom, pool, 0x0400_0100);
        put_u32(&mut rom, pool + 4, 0x0400_00C4);
        put_u32(&mut rom, pool + 8, 0x0400_00C0);
        put_u32(&mut rom, pool + 12, 0x0400_00A0);
        put_u32(&mut rom, pool + 0x14, 0x0080_F9C4);
        put_u32(&mut rom, pool + 0x18, 0xBE40_0000);
        put_u32(&mut rom, pool + 0x28, 0x0080_F99D);

        // PCM pitch table slice at 0x1200 + 24*4
        let slice: [u32; 8] = [
            0xFD64, 0xEF3E, 0xE1C8, 0xD51B, 0xC932, 0xBDD2, 0xB327, 0xA902,
        ];
        for (i, &v) in slice.iter().enumerate() {
            put_u32(&mut rom, 0x1200 + (24 + i) * 4, v);
        }

        // Literal pool ref to PCM pitch table + Music PCM table (0x2000)
        put_u32(&mut rom, 0x1400, base + 0x1200);
        put_u32(&mut rom, 0x1404, base + 0x2000);

        // Music PCM sample entry at 0x2000 -> PCM at 0x3000 (256 bytes, looped)
        put_u32(&mut rom, 0x2000, base + 0x3000);
        put_u32(&mut rom, 0x2004, 256);
        put_u32(&mut rom, 0x2008, 0);
        put_u32(&mut rom, 0x200C, 0);
        for (i, b) in rom[0x3000..0x3100].iter_mut().enumerate() {
            *b = (((i as i32 % 16) - 8) * 6) as i8 as u8;
        }

        // PSG pitch table at 0x2400 -> song table at 0x24C4
        let psg_prefix: [u8; 16] = [
            0x9D, 0x07, 0x6B, 0xCA, 0x23, 0x78, 0xC7, 0x12,
            0x59, 0x9C, 0xDB, 0x17, 0x4F, 0x84, 0xB6, 0xE5,
        ];
        rom[0x2400..0x2410].copy_from_slice(&psg_prefix);
        put_u32(&mut rom, 0x2600, 0x61); // Track End
        put_u32(&mut rom, 0x2640, 12); // Duration table
        for c in 0..8usize {
            put_u32(&mut rom, 0x24C4 + c * 4, base + 0x2600);
        }
        put_u32(&mut rom, 0x24C4 + 8 * 4, base + 0x2640);
        rom
    }

    #[test]
    fn detects_and_exports_skyworks_hybrid_wav_and_sf2() {
        let rom = build_synthetic_rom();
        assert_eq!(
            detect_skyworks_hybrid_engine(&rom),
            Some(SkyworksHybridRevision::Rev2)
        );
        let summary = scan_skyworks_hybrid_engine(&rom).expect("valid summary");
        assert_eq!(summary.music_sample_count, 1);
        assert_eq!(summary.song_count, 1);

        let wavs = export_skyworks_hybrid_samples_wav16(&rom).expect("wav");
        assert_eq!(wavs.len(), 1);
        assert_eq!(&wavs[0].2[0..4], b"RIFF");

        let sf2 = compile_skyworks_hybrid_soundfont(&rom, Some("Synthetic SWH")).expect("sf2");
        let val = validate_soundfont(&sf2.sf2_bytes).expect("valid sf2");
        assert_eq!(val.sample_count, 1);
    }

    #[test]
    fn rejects_empty_rom() {
        assert_eq!(detect_skyworks_hybrid_engine(&[]), None);
        assert!(scan_skyworks_hybrid_soundpool(&[0u8; 512]).is_empty());
    }
}
