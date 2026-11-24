//! Rareware GBA Sound Engine sample pool scanning, signed 8-bit PCM extraction,
//! WAV16 export, and SoundFont 2.04 soundbank generation.
//!
//! Covers GBA titles built on Rareware's custom ARM7TDMI DirectSound A mixer
//! (mono 13,379 Hz master mix rate, 23-bit fractional linear interpolation,
//! 68-byte `RareSampleDesc` descriptors referencing raw signed 8-bit PCM).

#![deny(unsafe_code)]

use std::collections::HashSet;

use formatkit_core::{Error, Result};
use rare_audio::{
    RareAudioVersion, RareSampleDesc, RareSampleView, RARE_SAMPLE_DESC_SIZE,
};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// Minimum accepted sample length in bytes.
const MIN_SAMPLE_BYTES: usize = 16;

/// Maximum accepted sample length in bytes (`1 MiB`).
const MAX_SAMPLE_BYTES: usize = 0x10_0000;

/// Extracted Rareware GBA Sound Engine PCM sample entry from a ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RareSampleEntry {
    /// Sample index within the detected pool.
    pub index: usize,
    /// Absolute ROM file offset where the 68-byte `RareSampleDesc` descriptor begins.
    pub rom_offset: usize,
    /// Absolute ROM file offset where the signed 8-bit PCM payload begins.
    pub sample_start_offset: usize,
    /// Sample payload length in bytes (8-bit mono samples).
    pub size: usize,
    /// Native playback sample rate in Hz (`4000..=48000`).
    pub sample_rate: u32,
    /// MIDI root note (`0..=127`; `60` = Middle C / C4).
    pub root_key: u8,
    /// Loop start offset in samples, if looped (loop end is `size as u32`).
    pub loop_start: Option<u32>,
    /// Playback mode (`1` = MIDI one-shot, `2` = MIDI looped, `3` = SFX one-shot, `4` = SFX looped).
    pub playback_mode: u32,
    /// Extracted raw signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Detect whether a GBA ROM uses the Rareware GBA Sound Engine.
///
/// Matches the Rareware audio RAM allocation debug string, the DirectSound A + DMA1
/// literal-pool triplet, or the Rareware ARM7TDMI mixer signature via [`rare_audio`].
pub fn detect_rare_engine(rom: &[u8]) -> bool {
    RareAudioVersion::has_rare_signature(rom)
        || RareAudioVersion::detect_from_bytes(rom).is_some()
}

/// Scan a GBA ROM for 68-byte (`0x44`) Rareware `RareSampleDesc` sample descriptors
/// and extract their signed 8-bit PCM payloads, deduplicated by PCM start offset.
pub fn scan_rare_soundpool(rom: &[u8]) -> Vec<RareSampleEntry> {
    if rom.len() < RARE_SAMPLE_DESC_SIZE {
        return Vec::new();
    }

    let mut samples = Vec::new();
    let mut seen_sample_starts = HashSet::new();

    let mut off = 0usize;
    while off + RARE_SAMPLE_DESC_SIZE <= rom.len() {
        let Ok(desc) = RareSampleDesc::parse(&rom[off..off + RARE_SAMPLE_DESC_SIZE]) else {
            off += 4;
            continue;
        };

        let size = desc.sample_len();
        if !(MIN_SAMPLE_BYTES..=MAX_SAMPLE_BYTES).contains(&size) {
            off += 4;
            continue;
        }

        let index = samples.len();
        let Ok(view) = RareSampleView::from_rom_descriptor(index, &desc, rom) else {
            off += 4;
            continue;
        };

        let sample_start_offset = desc.rom_pcm_offset();
        if !seen_sample_starts.insert(sample_start_offset) {
            off += 4;
            continue;
        }

        let pcm_data: Vec<i8> = view.pcm[..view.len()].iter().map(|&b| b as i8).collect();

        samples.push(RareSampleEntry {
            index,
            rom_offset: off,
            sample_start_offset,
            size,
            sample_rate: desc.sample_rate_hz,
            root_key: desc.root_note,
            loop_start: desc.loop_start().map(|s| s as u32),
            playback_mode: desc.playback_mode as u32,
            pcm_data,
        });

        off += 4;
    }

    samples
}

/// Extract all Rareware GBA samples from a ROM and encode them as standard
/// 16-bit uncompressed mono RIFF WAV files.
///
/// Returns a list of `(sample_index, recommended_filename, wav_bytes)`.
pub fn export_rare_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_rare_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_rare_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!(
            "rare_sample_{:03}_{:06X}.wav",
            s.index, s.sample_start_offset
        );
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Rareware GBA samples into a standard SoundFont 2.04 (`.sf2`) soundbank.
pub fn compile_rare_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_rare_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_rare_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Rareware)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            let loop_range = s.loop_start.map(|start| (start, s.size as u32));
            RawSoundFontSample {
                name: format!("RARE_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(&bank_title, "Rareware GBA Sound Engine", &raw_samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;
    use rare_audio::{
        GBA_ROM_BUS_BASE, RARE_DEBUG_SIGNATURE, RARE_DESC_MAGIC_SIZE, RARE_FIFO_DMA_SIGNATURE,
    };

    #[allow(clippy::too_many_arguments)]
    fn write_rare_desc(
        rom: &mut [u8],
        off: usize,
        playback_mode: u32,
        sample_rate: u32,
        root_note: u32,
        sample_start_off: usize,
        sample_len: usize,
        loop_length: u32,
        adsr: [u32; 4],
    ) {
        let start_ptr = GBA_ROM_BUS_BASE + (sample_start_off as u32);
        let end_ptr = start_ptr + (sample_len as u32);
        let words: [u32; 17] = [
            RARE_DESC_MAGIC_SIZE,
            playback_mode,
            sample_rate,
            root_note,
            start_ptr,
            loop_length,
            end_ptr,
            0,       // reserved_0
            0,       // reserved_1
            adsr[0], // adsr_attack
            adsr[1], // adsr_decay
            adsr[2], // adsr_sustain
            adsr[3], // adsr_release
            0,       // fine_tune_cents
            2,       // pitch_bend_range
            0,       // vibrato_rate
            0,       // vibrato_depth
        ];
        for (i, w) in words.iter().enumerate() {
            rom[off + i * 4..off + (i + 1) * 4].copy_from_slice(&w.to_le_bytes());
        }
    }

    fn build_synthetic_rare_rom(use_debug_sig: bool) -> Vec<u8> {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"RARETESTROM\0");

        if use_debug_sig {
            let sig_off = 0x0200;
            rom[sig_off..sig_off + RARE_DEBUG_SIGNATURE.len()]
                .copy_from_slice(RARE_DEBUG_SIGNATURE);
        } else {
            let sig_off = 0x0200;
            rom[sig_off..sig_off + RARE_FIFO_DMA_SIGNATURE.len()]
                .copy_from_slice(&RARE_FIFO_DMA_SIGNATURE);
        }

        // PCM sample 0: 64 bytes signed 8-bit ramp at 0x0800
        let pcm0_off = 0x0800usize;
        let pcm0_len = 64usize;
        for k in 0..pcm0_len {
            let v = (((k as i32) % 16) - 8) * 10;
            rom[pcm0_off + k] = (v as i8) as u8;
        }

        // PCM sample 1: 128 bytes signed 8-bit ramp at 0x0900
        let pcm1_off = 0x0900usize;
        let pcm1_len = 128usize;
        for k in 0..pcm1_len {
            let v = (((k as i32) % 32) - 16) * 6;
            rom[pcm1_off + k] = (v as i8) as u8;
        }

        // PCM sample 2: 96 bytes signed 8-bit ramp at 0x0A00
        let pcm2_off = 0x0A00usize;
        let pcm2_len = 96usize;
        for k in 0..pcm2_len {
            let v = (((k as i32) % 12) - 6) * 12;
            rom[pcm2_off + k] = (v as i8) as u8;
        }

        // Descriptor 0: MIDI one-shot (mode 1), 11025 Hz, root C4 (60), loop_length=0
        write_rare_desc(
            &mut rom,
            0x0400,
            1,
            11_025,
            60,
            pcm0_off,
            pcm0_len,
            0,
            [99, 50, 80, 30],
        );

        // Descriptor 1: MIDI looped (mode 2), 13379 Hz, root A4 (69), loop_length=48 -> loop_start=80
        write_rare_desc(
            &mut rom,
            0x0480,
            2,
            13_379,
            69,
            pcm1_off,
            pcm1_len,
            48,
            [99, 20, 90, 15],
        );

        // Descriptor 2: Duplicate pointer to pcm1_off (should be deduplicated)
        write_rare_desc(
            &mut rom,
            0x0500,
            2,
            13_379,
            69,
            pcm1_off,
            pcm1_len,
            48,
            [99, 20, 90, 15],
        );

        // Descriptor 3: SFX looped (mode 4), 22050 Hz, root E3 (52), loop_length=32 -> loop_start=64
        write_rare_desc(
            &mut rom,
            0x0580,
            4,
            22_050,
            52,
            pcm2_off,
            pcm2_len,
            32,
            [99, 0, 99, 10],
        );

        rom
    }

    #[test]
    fn detects_rare_engine_via_debug_string_and_fifo_dma_triplet() {
        let rom_dbg = build_synthetic_rare_rom(true);
        assert!(detect_rare_engine(&rom_dbg));

        let rom_dma = build_synthetic_rare_rom(false);
        assert!(detect_rare_engine(&rom_dma));
    }

    #[test]
    fn parses_looped_and_oneshot_descriptors_with_deduplication() {
        let rom = build_synthetic_rare_rom(true);
        let samples = scan_rare_soundpool(&rom);

        assert_eq!(samples.len(), 3);

        // Sample 0: MIDI one-shot
        assert_eq!(samples[0].index, 0);
        assert_eq!(samples[0].rom_offset, 0x0400);
        assert_eq!(samples[0].sample_start_offset, 0x0800);
        assert_eq!(samples[0].size, 64);
        assert_eq!(samples[0].sample_rate, 11_025);
        assert_eq!(samples[0].root_key, 60);
        assert_eq!(samples[0].loop_start, None);
        assert_eq!(samples[0].playback_mode, 1);
        assert_eq!(samples[0].pcm_data.len(), 64);
        assert_eq!(samples[0].pcm_data[0], -80);

        // Sample 1: MIDI looped (size=128, loop_length=48 -> loop_start=80)
        assert_eq!(samples[1].index, 1);
        assert_eq!(samples[1].rom_offset, 0x0480);
        assert_eq!(samples[1].sample_start_offset, 0x0900);
        assert_eq!(samples[1].size, 128);
        assert_eq!(samples[1].sample_rate, 13_379);
        assert_eq!(samples[1].root_key, 69);
        assert_eq!(samples[1].loop_start, Some(80));
        assert_eq!(samples[1].playback_mode, 2);
        assert_eq!(samples[1].pcm_data[0], -96);

        // Sample 2: SFX looped (size=96, loop_length=32 -> loop_start=64)
        assert_eq!(samples[2].index, 2);
        assert_eq!(samples[2].rom_offset, 0x0580);
        assert_eq!(samples[2].sample_start_offset, 0x0A00);
        assert_eq!(samples[2].size, 96);
        assert_eq!(samples[2].sample_rate, 22_050);
        assert_eq!(samples[2].root_key, 52);
        assert_eq!(samples[2].loop_start, Some(64));
        assert_eq!(samples[2].playback_mode, 4);
        assert_eq!(samples[2].pcm_data[0], -72);
    }

    #[test]
    fn rejects_invalid_descriptors() {
        let mut rom = vec![0u8; 0x1000];
        // Invalid playback_mode (5)
        write_rare_desc(&mut rom, 0x0100, 5, 11_025, 60, 0x0800, 64, 0, [99, 0, 99, 0]);
        // Invalid sample_rate (2000 Hz < 4000)
        write_rare_desc(&mut rom, 0x0180, 1, 2_000, 60, 0x0840, 64, 0, [99, 0, 99, 0]);
        // Invalid root_note (128 > 127)
        write_rare_desc(&mut rom, 0x0200, 1, 11_025, 128, 0x0880, 64, 0, [99, 0, 99, 0]);
        // Invalid ADSR (> 99)
        write_rare_desc(&mut rom, 0x0280, 1, 11_025, 60, 0x08C0, 64, 0, [100, 0, 99, 0]);
        // Invalid loop_length (> size)
        write_rare_desc(&mut rom, 0x0300, 2, 11_025, 60, 0x0900, 64, 65, [99, 0, 99, 0]);
        // Invalid reserved_0 != 0
        write_rare_desc(&mut rom, 0x0380, 1, 11_025, 60, 0x0940, 64, 0, [99, 0, 99, 0]);
        rom[0x0380 + 0x1C..0x0380 + 0x20].copy_from_slice(&1u32.to_le_bytes());

        assert!(scan_rare_soundpool(&rom).is_empty());
    }

    #[test]
    fn exports_valid_wav16_and_soundfont() {
        let rom = build_synthetic_rare_rom(true);

        let wavs = export_rare_samples_wav16(&rom).expect("wav export should succeed");
        assert_eq!(wavs.len(), 3);
        for (idx, name, bytes) in &wavs {
            assert!(name.starts_with(&format!("rare_sample_{idx:03}_")));
            assert!(name.ends_with(".wav"));
            assert_eq!(&bytes[0..4], b"RIFF");
            assert_eq!(&bytes[8..12], b"WAVE");
        }
        // Verify sample 0 PCM16 length: 44-byte WAV header + 64 * 2 bytes = 172 bytes
        assert_eq!(wavs[0].2.len(), 44 + 64 * 2);

        let sf2 = compile_rare_soundfont(&rom, Some("Synthetic Rareware"))
            .expect("sf2 compile should succeed");
        let validation = validate_soundfont(&sf2.sf2_bytes).expect("sf2 should validate");
        assert_eq!(validation.sample_count, 3);
        assert_eq!(validation.preset_count, 3);
        assert_eq!(validation.instrument_count, 3);
        assert_eq!(validation.bank_name, "Synthetic Rareware");
        assert_eq!(validation.sound_engine, "Rareware GBA Sound Engine");

        // Verify default title derivation from ROM header
        let sf2_default = compile_rare_soundfont(&rom, None).expect("default title compile");
        let val_default = validate_soundfont(&sf2_default.sf2_bytes).expect("default sf2 valid");
        assert_eq!(val_default.bank_name, "RARETESTROM (Rareware)");
    }

    #[test]
    fn rejects_non_rare_rom() {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"NONRAREROM\0\0");
        rom[0x0300..0x0310].copy_from_slice(b"AUDIO TEST DATA\0");

        assert!(!detect_rare_engine(&rom));
        assert!(scan_rare_soundpool(&rom).is_empty());
        assert!(export_rare_samples_wav16(&rom).is_err());
        assert!(compile_rare_soundfont(&rom, None).is_err());
    }
}
