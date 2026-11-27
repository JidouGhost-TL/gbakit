//! Full Fat GBA Mono DirectSound audio engine detection, soundbank/sample/sequence
//! scanning, WAV16 export, and SoundFont 2.04 compilation.
//!
//! Thin adapter delegating detection, zero-copy bank parsing, and 4-bit ADPCM decoding
//! to [`fullfat_audio`].

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use fullfat_audio::{Adpcm4Decoder, Revision, SampleFormat, MAX_TOTAL_PCM_BYTES};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// Base sample rate in Hz (`0x2B11`) used by the Full Fat pitch-table generator.
pub const FULLFAT_BASE_SAMPLE_RATE_HZ: u32 = fullfat_audio::BASE_SAMPLE_RATE_HZ;

/// MIDI root key corresponding to the engine's 261 Hz pitch reference (Middle C).
pub const FULLFAT_ROOT_KEY: u8 = fullfat_audio::ROOT_KEY;

/// Detected Full Fat engine build revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FullFatRevision {
    /// Early build revision (`FinalizeBuffer` uses `bpl`, Timer 0 clock 16,780,000 Hz).
    RevA,
    /// Later build revision (`FinalizeBuffer` uses `bgt`, Timer 0 clock 16,777,216 Hz).
    RevB,
    /// Core mixer and frequency table matched, finalize loop revision unclassified.
    Unknown,
}

impl From<Revision> for FullFatRevision {
    fn from(rev: Revision) -> Self {
        match rev {
            Revision::RevA => Self::RevA,
            Revision::RevB => Self::RevB,
            Revision::Unknown | _ => Self::Unknown,
        }
    }
}

/// Extracted Full Fat signed 8-bit PCM sample entry (PCM8 native or decoded from Adpcm4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullFatSampleEntry {
    /// Sequential sample index across all banks.
    pub index: usize,
    /// Sound bank index.
    pub bank_index: usize,
    /// Absolute ROM file offset of the 16-byte sample header.
    pub rom_offset: usize,
    /// Absolute ROM file offset of the sample payload.
    pub sample_start_offset: usize,
    /// Decoded sample count (bytes for PCM8, `2 * payload_len` for Adpcm4).
    pub size: usize,
    /// Base sample rate in Hz (`11_025`).
    pub sample_rate: u32,
    /// MIDI root key (`60` = Middle C).
    pub root_key: u8,
    /// Optional loop range `(loop_start, loop_end)` in samples.
    pub loop_range: Option<(u32, u32)>,
    /// Extracted signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Extracted Full Fat song descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullFatSongEntry {
    /// Song index within its bank.
    pub index: usize,
    /// Sound bank index.
    pub bank_index: usize,
    /// ROM file offset of sequence start.
    pub start_offset: usize,
    /// ROM file offset of sequence loop target (`0` if unspecified).
    pub loop_offset: usize,
}

/// Complete scan summary for a Full Fat Mono DirectSound ROM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullFatScanSummary {
    /// Detected build revision.
    pub revision: FullFatRevision,
    /// ROM file offset of the ARM `MixChannel` inner loop.
    pub mixer_offset: usize,
    /// ROM file offset of the 128-entry 16.16 MIDI frequency table.
    pub freq_table_offset: usize,
    /// Number of discovered sound banks.
    pub bank_count: usize,
    /// Extracted PCM samples across all banks.
    pub samples: Vec<FullFatSampleEntry>,
    /// Extracted song entries across all banks.
    pub songs: Vec<FullFatSongEntry>,
}

/// Detect whether `rom` contains the Full Fat Mono DirectSound audio engine.
#[must_use]
pub fn detect_fullfat_engine(rom: &[u8]) -> Option<FullFatRevision> {
    fullfat_audio::detect_details(rom).map(|d| d.revision.into())
}

/// Scan a Full Fat Mono DirectSound ROM for all sound banks, PCM/ADPCM samples, and songs.
pub fn scan_fullfat_engine(rom: &[u8]) -> Option<FullFatScanSummary> {
    let audio = fullfat_audio::parse(rom).ok()?;

    let mut samples = Vec::new();
    let mut songs = Vec::new();
    let mut total_pcm_bytes = 0usize;

    for (bank_idx, bank) in audio.banks().iter().enumerate() {
        for s_idx in 0..(bank.song_count as usize) {
            if let Some(song) = audio.bank_song(bank_idx, s_idx) {
                songs.push(FullFatSongEntry {
                    index: s_idx,
                    bank_index: bank_idx,
                    start_offset: song.start_offset as usize,
                    loop_offset: song.loop_offset as usize,
                });
            }
        }

        for sample in audio.bank_samples(bank_idx) {
            let decoded_len = match sample.header.format {
                SampleFormat::Pcm8Signed => sample.pcm.len(),
                SampleFormat::Adpcm4 => sample.pcm.len().saturating_mul(2),
                _ => continue,
            };

            if total_pcm_bytes.saturating_add(decoded_len) > MAX_TOTAL_PCM_BYTES {
                break;
            }
            total_pcm_bytes += decoded_len;

            let (pcm_data, loop_range) = match sample.header.format {
                SampleFormat::Pcm8Signed => {
                    let pcm: Vec<i8> = sample.pcm.iter().map(|&b| b as i8).collect();
                    let lr = sample
                        .header
                        .is_looped()
                        .then_some((sample.header.loop_start, sample.header.length));
                    (pcm, lr)
                }
                SampleFormat::Adpcm4 => {
                    let mut dec = Adpcm4Decoder::new();
                    let mut pcm = Vec::with_capacity(decoded_len);
                    for &byte in sample.pcm {
                        let [p0, p1] = dec.decode_byte_12bit(byte);
                        pcm.push((p0 >> 4).clamp(-128, 127) as i8);
                        pcm.push((p1 >> 4).clamp(-128, 127) as i8);
                    }
                    let lr = sample.header.is_looped().then_some((
                        sample.header.loop_start.saturating_mul(2),
                        (sample.pcm.len() as u32).saturating_mul(2),
                    ));
                    (pcm, lr)
                }
                _ => continue,
            };

            let rom_offset = sample.header.rom_offset as usize;
            let index = samples.len();
            samples.push(FullFatSampleEntry {
                index,
                bank_index: bank_idx,
                rom_offset,
                sample_start_offset: rom_offset.saturating_add(16),
                size: pcm_data.len(),
                sample_rate: FULLFAT_BASE_SAMPLE_RATE_HZ,
                root_key: FULLFAT_ROOT_KEY,
                loop_range,
                pcm_data,
            });
        }
    }

    Some(FullFatScanSummary {
        revision: audio.revision().into(),
        mixer_offset: audio.mixer_offset() as usize,
        freq_table_offset: audio.freq_table_offset() as usize,
        bank_count: audio.banks().len(),
        samples,
        songs,
    })
}

/// Scan a GBA ROM for Full Fat Mono DirectSound signed 8-bit PCM samples.
pub fn scan_fullfat_soundpool(rom: &[u8]) -> Vec<FullFatSampleEntry> {
    scan_fullfat_engine(rom).map(|s| s.samples).unwrap_or_default()
}

/// Extract all Full Fat samples as 16-bit mono RIFF WAV files.
pub fn export_fullfat_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_fullfat_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_fullfat_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!("fullfat_sample_{:03}_{:06X}.wav", s.index, s.rom_offset);
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Full Fat Mono DirectSound samples into a SoundFont 2.04 (`.sf2`).
pub fn compile_fullfat_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_fullfat_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_fullfat_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Full Fat DirectSound)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            RawSoundFontSample {
                name: format!("FFAT_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range: s.loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(
        &bank_title,
        "Full Fat Mono DirectSound Engine",
        &raw_samples,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;
    use fullfat_audio::detect::{FREQ_TABLE_PREFIX, MIX_LOOP_SIG};
    use fullfat_audio::GBA_ROM_BASE;

    const FINALIZE_LOOP_REVA: [u8; 36] = [
        0xF2, 0x00, 0xD2, 0xE0, 0x40, 0x02, 0xA0, 0xE1, 0x80, 0x00, 0x70, 0xE3, 0x7F, 0x00, 0xE0,
        0xB3, 0x7F, 0x00, 0x50, 0xE3, 0x7F, 0x00, 0xA0, 0xC3, 0x01, 0x00, 0xCE, 0xE4, 0x01, 0xC0,
        0x5C, 0xE2, 0xF6, 0xFF, 0xFF, 0x5A,
    ];
    const FINALIZE_LOOP_REVB: [u8; 36] = [
        0xF2, 0x00, 0xD2, 0xE0, 0x40, 0x02, 0xA0, 0xE1, 0x80, 0x00, 0x70, 0xE3, 0x7F, 0x00, 0xE0,
        0xB3, 0x7F, 0x00, 0x50, 0xE3, 0x7F, 0x00, 0xA0, 0xC3, 0x01, 0x00, 0xCE, 0xE4, 0x01, 0xC0,
        0x5C, 0xE2, 0xF6, 0xFF, 0xFF, 0xCA,
    ];

    fn build_synthetic_rom(rev_b: bool) -> Vec<u8> {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"FULLFATTEST\0");

        rom[0x200..0x200 + MIX_LOOP_SIG.len()].copy_from_slice(&MIX_LOOP_SIG);
        let fin = if rev_b {
            &FINALIZE_LOOP_REVB
        } else {
            &FINALIZE_LOOP_REVA
        };
        rom[0x240..0x240 + fin.len()].copy_from_slice(fin);

        rom[0x400..0x400 + FREQ_TABLE_PREFIX.len()].copy_from_slice(&FREQ_TABLE_PREFIX);
        rom[0x400 + 69 * 4..0x400 + 69 * 4 + 4].copy_from_slice(&28_835_840u32.to_le_bytes());

        // 1 bank @ 0x800: 1 song, 2 samples (1 PCM8 + 1 Adpcm4)
        let bank_off = 0x800usize;
        let bank_addr = GBA_ROM_BASE + bank_off as u32;
        rom[bank_off..bank_off + 4].copy_from_slice(&1u32.to_le_bytes());
        rom[bank_off + 4..bank_off + 8].copy_from_slice(&2u32.to_le_bytes());
        rom[bank_off + 8..bank_off + 12].copy_from_slice(&(bank_addr + 16).to_le_bytes());
        rom[bank_off + 12..bank_off + 16].copy_from_slice(&(bank_addr + 24).to_le_bytes());
        rom[bank_off + 16..bank_off + 20].copy_from_slice(&(GBA_ROM_BASE + 0x900).to_le_bytes());
        rom[bank_off + 24..bank_off + 28].copy_from_slice(&(GBA_ROM_BASE + 0xA00).to_le_bytes());
        rom[bank_off + 28..bank_off + 32].copy_from_slice(&(GBA_ROM_BASE + 0xA60).to_le_bytes());

        // Sample 0 @ 0xA00: Adpcm4 (fmt=0), len=32 bytes -> 64 decoded samples, loop=0
        rom[0xA00..0xA04].copy_from_slice(&0u32.to_le_bytes());
        rom[0xA04..0xA08].copy_from_slice(&32u32.to_le_bytes());
        // Sample 1 @ 0xA60: PCM8 (fmt=1), len=64, loop=16
        rom[0xA60..0xA64].copy_from_slice(&1u32.to_le_bytes());
        rom[0xA64..0xA68].copy_from_slice(&64u32.to_le_bytes());
        rom[0xA68..0xA6C].copy_from_slice(&16u32.to_le_bytes());

        rom
    }

    #[test]
    fn detects_and_exports_fullfat_reva_revb_and_adpcm4() {
        let rom_a = build_synthetic_rom(false);
        assert_eq!(detect_fullfat_engine(&rom_a), Some(FullFatRevision::RevA));

        let rom_b = build_synthetic_rom(true);
        assert_eq!(detect_fullfat_engine(&rom_b), Some(FullFatRevision::RevB));

        let summary = scan_fullfat_engine(&rom_b).expect("valid summary");
        assert_eq!(summary.bank_count, 1);
        assert_eq!(summary.songs.len(), 1);
        assert_eq!(summary.samples.len(), 2);
        assert_eq!(summary.samples[0].size, 64); // 32 ADPCM4 bytes -> 64 decoded samples
        assert_eq!(summary.samples[0].loop_range, None);
        assert_eq!(summary.samples[1].loop_range, Some((16, 64)));

        let wavs = export_fullfat_samples_wav16(&rom_b).expect("wav export");
        assert_eq!(wavs.len(), 2);
        assert_eq!(&wavs[0].2[0..4], b"RIFF");

        let sf2 = compile_fullfat_soundfont(&rom_b, Some("Synthetic Full Fat")).expect("sf2");
        let val = validate_soundfont(&sf2.sf2_bytes).expect("valid sf2");
        assert_eq!(val.sample_count, 2);
    }

    #[test]
    fn rejects_empty_and_malformed() {
        assert_eq!(detect_fullfat_engine(&[]), None);
        assert!(scan_fullfat_soundpool(&[0u8; 512]).is_empty());
    }
}
