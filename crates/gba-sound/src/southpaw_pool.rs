//! Southpaw GBA SFX/Audio Engine sample pool scanning, Timer0 reload rate
//! calculation, signed 8-bit PCM extraction, WAV16 export, and SoundFont 2.04
//! soundbank generation.
//!
//! Covers GBA titles built on the Southpaw DirectSound SFX engine (*Doom*,
//! *Lucky Luke: Wanted!*, *Gremlins: Stripe vs Gizmo*, *Inspector Gadget:
//! Advance Mission*, *Planet Monsters*, etc.), which store raw signed 8-bit PCM
//! payloads referenced by contiguous tables of 16-byte `SouthpawSampleDesc`
//! descriptors with 16-byte DMA/FIFO block lengths and packed Timer0 control/reload
//! words.

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};
use southpaw_audio::{
    find_sample_table, SouthpawSampleDesc, SouthpawSampleView, SouthpawVersion, SOUTHPAW_DESC_SIZE,
};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// Extracted Southpaw GBA SFX/Audio Engine signed 8-bit PCM sample entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SouthpawSampleEntry {
    /// Sample index within the detected pool.
    pub index: usize,
    /// Absolute ROM file offset where the 16-byte `SouthpawSampleDesc` descriptor begins.
    pub rom_offset: usize,
    /// Absolute ROM file offset where the signed 8-bit PCM payload begins.
    pub sample_start_offset: usize,
    /// Sample payload length in bytes (`num_blocks * 16`).
    pub size: usize,
    /// Length in 16-byte DMA/FIFO blocks.
    pub num_blocks: u32,
    /// Packed Timer0 control (`hi16 = 0x0080`) and reload (`lo16`) word.
    pub timer0_cnt_reload: u32,
    /// Native playback sample rate in Hz derived from `timer0_cnt_reload`.
    pub sample_rate: u32,
    /// MIDI root note (`60` = Middle C / C4).
    pub root_key: u8,
    /// Loop start offset in samples (`Some(0)` for full-sample loop, `None` for one-shot).
    pub loop_start: Option<u32>,
    /// Extracted raw signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Detect whether a GBA ROM uses the Southpaw GBA Sound Engine via either its
/// 16-byte Timer0 SFX sample table or its ARM7TDMI/Thumb driver fingerprint.
pub fn detect_southpaw_engine(rom: &[u8]) -> bool {
    find_sample_table(rom).is_some() || SouthpawVersion::detect_from_bytes(rom).is_some()
}

/// Scan a GBA ROM for the primary contiguous table of 16-byte `SouthpawSampleDesc`
/// descriptors and extract their signed 8-bit PCM payloads.
pub fn scan_southpaw_soundpool(rom: &[u8]) -> Vec<SouthpawSampleEntry> {
    let Some((table_off, count)) = find_sample_table(rom) else {
        return Vec::new();
    };

    let mut samples = Vec::with_capacity(count);
    for i in 0..count {
        let eoff = table_off + i * SOUTHPAW_DESC_SIZE;
        let Ok(desc) = SouthpawSampleDesc::parse(&rom[eoff..eoff + SOUTHPAW_DESC_SIZE]) else {
            continue;
        };
        let index = samples.len();
        let Ok(view) = SouthpawSampleView::from_desc(index as u32, desc, rom) else {
            continue;
        };

        let loop_start = if desc.volume_mask != 0 { Some(0) } else { None };
        let pcm_data: Vec<i8> = view.pcm.iter().map(|&b| b as i8).collect();

        samples.push(SouthpawSampleEntry {
            index,
            rom_offset: eoff,
            sample_start_offset: desc.rom_offset(),
            size: view.len(),
            num_blocks: desc.num_blocks,
            timer0_cnt_reload: desc.timer0_cnt_reload,
            sample_rate: view.sample_rate_hz,
            root_key: 60,
            loop_start,
            pcm_data,
        });
    }

    samples
}

/// Extract all Southpaw GBA samples from a ROM and encode them as standard
/// 16-bit uncompressed mono RIFF WAV files.
///
/// Returns a list of `(sample_index, recommended_filename, wav_bytes)`.
pub fn export_southpaw_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_southpaw_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_southpaw_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!(
            "southpaw_sample_{:03}_{:06X}.wav",
            s.index, s.sample_start_offset
        );
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Southpaw GBA samples into a standard SoundFont 2.04 (`.sf2`) soundbank.
pub fn compile_southpaw_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_southpaw_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_southpaw_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Southpaw)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            let loop_range = s.loop_start.map(|start| (start, s.size as u32));
            RawSoundFontSample {
                name: format!("SPW_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(&bank_title, "Southpaw GBA SFX Engine", &raw_samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;
    use southpaw_audio::{timer_reload_to_hz as timer0_reload_to_hz, DMA_BLOCK_BYTES, GBA_ROM_BUS_BASE};

    fn write_southpaw_desc(
        rom: &mut [u8],
        off: usize,
        sample_start_off: usize,
        num_blocks: u32,
        timer0_cnt_reload: u32,
        loop_flag: i32,
    ) {
        let ptr = GBA_ROM_BUS_BASE + (sample_start_off as u32);
        rom[off..off + 4].copy_from_slice(&ptr.to_le_bytes());
        rom[off + 4..off + 8].copy_from_slice(&num_blocks.to_le_bytes());
        rom[off + 8..off + 12].copy_from_slice(&timer0_cnt_reload.to_le_bytes());
        rom[off + 12..off + 16].copy_from_slice(&loop_flag.to_le_bytes());
    }

    fn build_synthetic_southpaw_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x3000];
        rom[0xA0..0xAC].copy_from_slice(b"SOUTHPAWTST\0");

        let specs: [(usize, u32, u32, i32); 6] = [
            (0x1000, 4, 0x0080_FC15, 0),  // 64 B, 16,727 Hz, one-shot
            (0x1100, 8, 0x0080_F800, -1), // 128 B, 8,192 Hz, looped
            (0x1200, 6, 0x0080_FB1A, 0),  // 96 B, 13,379 Hz, one-shot
            (0x1300, 5, 0x0080_FA00, -1), // 80 B, 10,923 Hz, looped
            (0x1400, 3, 0x0080_FC15, 0),  // 48 B, 16,727 Hz, one-shot
            (0x1500, 7, 0x0080_F800, -1), // 112 B, 8,192 Hz, looped
        ];

        for (i, &(pcm_off, num_blocks, timer0, loop_flag)) in specs.iter().enumerate() {
            let slen = (num_blocks as usize) * DMA_BLOCK_BYTES;
            for k in 0..slen {
                let v = (((k as i32) % 16) - 8) * 8;
                rom[pcm_off + k] = (v as i8) as u8;
            }
            write_southpaw_desc(
                &mut rom,
                0x0400 + i * SOUTHPAW_DESC_SIZE,
                pcm_off,
                num_blocks,
                timer0,
                loop_flag,
            );
        }

        rom
    }

    #[test]
    fn computes_exact_timer0_sample_rates() {
        assert_eq!(timer0_reload_to_hz(0x0080_FC15), 16_727);
        assert_eq!(timer0_reload_to_hz(0x0080_F800), 8_192);
        assert_eq!(timer0_reload_to_hz(0x0080_FB1A), 13_379);
    }

    #[test]
    fn detects_and_scans_southpaw_table() {
        let rom = build_synthetic_southpaw_rom();
        assert!(detect_southpaw_engine(&rom));

        let samples = scan_southpaw_soundpool(&rom);
        assert_eq!(samples.len(), 6);

        assert_eq!(samples[0].index, 0);
        assert_eq!(samples[0].rom_offset, 0x0400);
        assert_eq!(samples[0].sample_start_offset, 0x1000);
        assert_eq!(samples[0].num_blocks, 4);
        assert_eq!(samples[0].size, 64);
        assert_eq!(samples[0].timer0_cnt_reload, 0x0080_FC15);
        assert_eq!(samples[0].sample_rate, 16_727);
        assert_eq!(samples[0].root_key, 60);
        assert_eq!(samples[0].loop_start, None);
        assert_eq!(samples[0].pcm_data.len(), 64);
        assert_eq!(samples[0].pcm_data[0], -64);

        assert_eq!(samples[1].index, 1);
        assert_eq!(samples[1].rom_offset, 0x0410);
        assert_eq!(samples[1].sample_start_offset, 0x1100);
        assert_eq!(samples[1].num_blocks, 8);
        assert_eq!(samples[1].size, 128);
        assert_eq!(samples[1].sample_rate, 8_192);
        assert_eq!(samples[1].loop_start, Some(0));
    }

    #[test]
    fn exports_valid_wav16_and_soundfont() {
        let rom = build_synthetic_southpaw_rom();

        let wavs = export_southpaw_samples_wav16(&rom).expect("wav export should succeed");
        assert_eq!(wavs.len(), 6);
        for (idx, name, bytes) in &wavs {
            assert!(name.starts_with(&format!("southpaw_sample_{idx:03}_")));
            assert!(name.ends_with(".wav"));
            assert_eq!(&bytes[0..4], b"RIFF");
            assert_eq!(&bytes[8..12], b"WAVE");
        }
        assert_eq!(wavs[0].2.len(), 44 + 64 * 2);

        let sf2 = compile_southpaw_soundfont(&rom, Some("Synthetic Southpaw"))
            .expect("sf2 compile should succeed");
        let validation = validate_soundfont(&sf2.sf2_bytes).expect("sf2 should validate");
        assert_eq!(validation.sample_count, 6);
        assert_eq!(validation.preset_count, 6);
        assert_eq!(validation.instrument_count, 6);
        assert_eq!(validation.bank_name, "Synthetic Southpaw");
        assert_eq!(validation.sound_engine, "Southpaw GBA SFX Engine");

        let sf2_default = compile_southpaw_soundfont(&rom, None).expect("default title compile");
        let val_default = validate_soundfont(&sf2_default.sf2_bytes).expect("default sf2 valid");
        assert_eq!(val_default.bank_name, "SOUTHPAWTST (Southpaw)");
    }

    #[test]
    fn rejects_short_runs_and_non_southpaw_roms() {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"NONSPWROM\0\0\0");

        // Only 4 consecutive descriptors (< MIN_RUN_ENTRIES = 5) must be rejected
        for i in 0..4usize {
            write_southpaw_desc(
                &mut rom,
                0x0400 + i * SOUTHPAW_DESC_SIZE,
                0x1000 + i * 64,
                4,
                0x0080_FC15,
                0,
            );
        }

        assert!(!detect_southpaw_engine(&rom));
        assert!(scan_southpaw_soundpool(&rom).is_empty());
        assert!(export_southpaw_samples_wav16(&rom).is_err());
        assert!(compile_southpaw_soundfont(&rom, None).is_err());
    }
}
