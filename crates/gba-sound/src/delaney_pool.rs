//! Michael Delaney / Allister Brimble GBA Stereo DirectSound + PSG (`0xB6600004`)
//! audio engine detection, sample/song pool scanning, WAV16 export, and SoundFont 2.04 compilation.
//!
//! Thin adapter delegating detection and ROM parsing to [`delaney_gba`].

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

const GBA_ROM_BASE: u32 = 0x0800_0000;

/// Metadata extracted from the Michael Delaney / Allister Brimble driver's literal pools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelaneyDriverInfo {
    /// ROM file offset of the `(0x0400_00BC, 0xB660_0004)` DMA literal pool pair.
    pub anchor_offset: usize,
    /// Raw 32-bit `TM0CNT` / `TM1CNT` initialization word.
    pub timer_word: u32,
    /// Derived DirectSound playback rate in Hz (`10_512` or `13_379`).
    pub sample_rate_hz: u32,
    /// Byte stride between adjacent IWRAM ring-buffer slots (`0xE0` or `0xB0`).
    pub ring_slot_stride: u32,
    /// ROM address (`0x08xx_xxxx`) of the 16-byte sample descriptor table.
    pub sample_table_addr: u32,
    /// ROM address (`0x08xx_xxxx`) of the 60-byte song descriptor table, if present.
    pub song_table_addr: Option<u32>,
    /// ROM address (`0x08xx_xxxx`) of the 32-bit pattern pointer table, if located.
    pub pattern_table_addr: Option<u32>,
}

impl From<delaney_gba::DriverInfo> for DelaneyDriverInfo {
    fn from(d: delaney_gba::DriverInfo) -> Self {
        Self {
            anchor_offset: d.anchor_offset,
            timer_word: d.timer_word,
            sample_rate_hz: d.sample_rate_hz,
            ring_slot_stride: d.ring_slot_stride,
            sample_table_addr: d.sample_table_addr,
            song_table_addr: d.song_table_addr,
            pattern_table_addr: d.pattern_table_addr,
        }
    }
}

/// Extracted Michael Delaney / Allister Brimble signed 8-bit PCM sample entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelaneySampleEntry {
    /// Sequential sample index within the scan output.
    pub index: usize,
    /// 0-based index within the ROM's 16-byte sample descriptor table.
    pub table_index: u8,
    /// Absolute ROM file offset where the signed 8-bit PCM payload begins.
    pub rom_offset: usize,
    /// Payload length in bytes.
    pub size: usize,
    /// Playback sample rate in Hz (`10_512` or `13_379`).
    pub sample_rate: u32,
    /// MIDI root key (`60` = Middle C).
    pub root_key: u8,
    /// Optional loop range `(loop_start, loop_end)` in samples.
    pub loop_range: Option<(u32, u32)>,
    /// Extracted signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Extracted 60-byte (`0x3C`) 12-channel song descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelaneySongEntry {
    /// 0-based song index.
    pub index: usize,
    /// Absolute ROM file offset of the 60-byte song descriptor.
    pub rom_offset: usize,
    /// Base tick divider (`tempo_ticks`).
    pub tempo_ticks: u16,
    /// 12-bit active channel bitmask.
    pub channel_mask: u16,
    /// Number of active channels.
    pub active_channels: u32,
}

/// Complete scan summary for a Michael Delaney / Allister Brimble Stereo ROM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelaneyScanSummary {
    /// Driver literal-pool metadata.
    pub driver: DelaneyDriverInfo,
    /// Extracted DirectSound signed 8-bit PCM samples.
    pub samples: Vec<DelaneySampleEntry>,
    /// Extracted 12-channel song descriptors.
    pub songs: Vec<DelaneySongEntry>,
}

/// Locate the ROM file offset of the verified `0x0400_00BC` / `0xB660_0004` DMA literal pool pair.
#[must_use]
pub fn find_delaney_driver_anchor(rom: &[u8]) -> Option<usize> {
    delaney_gba::find_driver_anchor(rom)
}

/// Detect whether `rom` contains the Michael Delaney / Allister Brimble Stereo DirectSound + PSG engine.
#[must_use]
pub fn detect_delaney_engine(rom: &[u8]) -> bool {
    delaney_gba::detect(rom)
}

/// Scan a Michael Delaney / Allister Brimble Stereo ROM for driver info, samples, and songs.
pub fn scan_delaney_engine(rom: &[u8]) -> Option<DelaneyScanSummary> {
    let bank = delaney_gba::parse(rom).ok()?;
    let driver: DelaneyDriverInfo = bank.driver.into();

    let samples: Vec<DelaneySampleEntry> = bank
        .samples
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let rom_offset = entry.descriptor.start_addr.saturating_sub(GBA_ROM_BASE) as usize;
            let len = entry.pcm8.len();
            let loop_range = entry
                .descriptor
                .loop_offset_samples()
                .map(|start| (start, len as u32));
            let pcm_data: Vec<i8> = entry.pcm8.iter().map(|&b| b as i8).collect();
            DelaneySampleEntry {
                index,
                table_index: entry.descriptor.index,
                rom_offset,
                size: len,
                sample_rate: driver.sample_rate_hz,
                root_key: 60,
                loop_range,
                pcm_data,
            }
        })
        .collect();

    let song_table_off = driver
        .song_table_addr
        .and_then(|a| a.checked_sub(GBA_ROM_BASE))
        .unwrap_or(0) as usize;
    let songs: Vec<DelaneySongEntry> = bank
        .songs
        .iter()
        .map(|s| DelaneySongEntry {
            index: s.index,
            rom_offset: song_table_off.saturating_add(s.index * delaney_gba::parser::SONG_ENTRY_SIZE),
            tempo_ticks: s.tempo_ticks,
            channel_mask: s.channel_mask,
            active_channels: s.active_channel_count(),
        })
        .collect();

    Some(DelaneyScanSummary {
        driver,
        samples,
        songs,
    })
}

/// Scan a GBA ROM for Michael Delaney / Allister Brimble signed 8-bit PCM samples.
pub fn scan_delaney_soundpool(rom: &[u8]) -> Vec<DelaneySampleEntry> {
    scan_delaney_engine(rom).map(|s| s.samples).unwrap_or_default()
}

/// Extract all Michael Delaney / Allister Brimble samples as 16-bit mono RIFF WAV files.
pub fn export_delaney_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_delaney_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_delaney_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!("delaney_sample_{:03}_{:06X}.wav", s.index, s.rom_offset);
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Michael Delaney / Allister Brimble samples into a SoundFont 2.04 (`.sf2`).
pub fn compile_delaney_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_delaney_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_delaney_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Delaney Stereo)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            RawSoundFontSample {
                name: format!("DLN_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range: s.loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(
        &bank_title,
        "Michael Delaney / Allister Brimble Stereo DirectSound",
        &raw_samples,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;

    fn put_u16(buf: &mut [u8], off: usize, v: u16) {
        buf[off..off + 2].copy_from_slice(&v.to_le_bytes());
    }

    fn put_u32(buf: &mut [u8], off: usize, v: u32) {
        buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn build_synthetic_rom(timer_word: u32) -> Vec<u8> {
        let mut rom = vec![0u8; 0x4000];
        rom[0xA0..0xAC].copy_from_slice(b"DELANEYTEST\0");
        let anchor = 0x1000usize;

        put_u32(&mut rom, anchor, 0x0400_00BC);
        put_u32(&mut rom, anchor + 4, 0xB660_0004);
        put_u32(&mut rom, anchor + 8, 0x0300_0540);
        put_u32(&mut rom, anchor + 12, 0x0300_0542);
        put_u32(&mut rom, anchor + 16, 0x0300_05CC);
        put_u32(&mut rom, anchor + 20, 0x0300_06AC);

        put_u32(&mut rom, anchor + 0x100, 0x29FE_07FF);
        put_u32(&mut rom, anchor + 0x104, 0x28FF_07FF);

        for i in 0..7 {
            put_u32(&mut rom, anchor + 0x200 + i * 4, 0x0800_3000);
        }
        for i in 0..8 {
            put_u32(&mut rom, anchor + 0x240 + i * 4, 0x0800_2800);
        }

        put_u32(&mut rom, anchor + 0x400, timer_word);
        put_u32(&mut rom, anchor + 0x404, 0x0400_00A0);
        put_u32(&mut rom, anchor + 0x408, 0x0400_00A4);

        put_u32(&mut rom, anchor + 0x440, 0x0800_2C00);
        put_u32(&mut rom, anchor + 0x444, 0x0400_0080);
        put_u32(&mut rom, anchor + 0x448, 0x0400_0060);

        put_u32(&mut rom, 0x2800, 0x0800_2900);
        put_u32(&mut rom, 0x2804, 0x0800_2910);

        put_u16(&mut rom, 0x2C00, 7);
        put_u16(&mut rom, 0x2C02, 0x0011);
        put_u32(&mut rom, 0x2C04, 0x0800_2900);
        put_u32(&mut rom, 0x2C04 + 4 * 4, 0x0800_2910);

        put_u32(&mut rom, 0x3010, 0x0800_3200);
        put_u32(&mut rom, 0x3014, 0x0800_3220);
        put_u32(&mut rom, 0x3020, 0x0800_3220);
        put_u32(&mut rom, 0x3024, 0x0800_3260);
        put_u32(&mut rom, 0x3028, 0x0800_3230);

        for (i, b) in rom[0x3200..0x3260].iter_mut().enumerate() {
            *b = ((i as i8).wrapping_mul(7)) as u8;
        }
        rom
    }

    #[test]
    fn detects_and_exports_delaney_samples_and_sf2() {
        let rom = build_synthetic_rom(0x0080_F9C4);
        assert!(detect_delaney_engine(&rom));

        let summary = scan_delaney_engine(&rom).expect("valid summary");
        assert_eq!(summary.driver.sample_rate_hz, 10_512);
        assert_eq!(summary.driver.ring_slot_stride, 0xE0);
        assert_eq!(summary.samples.len(), 2);
        assert_eq!(summary.songs.len(), 1);
        assert_eq!(summary.samples[1].loop_range, Some((16, 64)));

        let wavs = export_delaney_samples_wav16(&rom).expect("wav export");
        assert_eq!(wavs.len(), 2);
        assert_eq!(&wavs[0].2[0..4], b"RIFF");

        let sf2 = compile_delaney_soundfont(&rom, Some("Synthetic Delaney")).expect("sf2");
        let val = validate_soundfont(&sf2.sf2_bytes).expect("valid sf2");
        assert_eq!(val.sample_count, 2);
    }

    #[test]
    fn rejects_empty_and_malformed() {
        assert!(!detect_delaney_engine(&[]));
        assert!(scan_delaney_soundpool(&[0xFFu8; 512]).is_empty());
        assert!(export_delaney_samples_wav16(&[0u8; 512]).is_err());
    }
}
