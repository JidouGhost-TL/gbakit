//! Paragon 5 GBA Tracker Engine sample pool scanning, signed 8-bit PCM extraction,
//! WAV16 export, and SoundFont 2.04 soundbank generation.
//!
//! Covers GBA titles built on the Paragon 5 custom DirectSound A+B tracker driver,
//! whose 64-byte master header references a contiguous array of 24-byte
//! `Paragon5SampleDesc` descriptors using 16.16 fixed-point lengths and loop points
//! targeting raw signed 8-bit linear PCM (`i8`).

#![deny(unsafe_code)]

use std::collections::HashSet;

use formatkit_core::{Error, Result};
use paragon5::{
    Paragon5SampleDesc, Paragon5Version, PARAGON5_PITCH_SCALE_ONE, PARAGON5_SAMPLE_DESC_SIZE,
};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// 8-byte descriptor interior signature at `+0x04..+0x0C` (`pitch_scale_fp16 == 1.0`, `reserved_zero == 0`).
const PARAGON5_DESC_SIG: [u8; 8] = [0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00];

/// Minimum accepted sample length in bytes.
const MIN_SAMPLE_BYTES: usize = 16;

/// Maximum accepted sample length in bytes (`256 KiB`).
const MAX_SAMPLE_BYTES: usize = 0x0004_0000;

/// Minimum number of contiguous 24-byte descriptors required to confirm a Paragon 5 sample table.
const MIN_CONTIGUOUS_ENTRIES: usize = 3;

/// Extracted Paragon 5 Tracker Engine signed 8-bit PCM sample entry from a GBA ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paragon5SampleEntry {
    /// Sample index within the detected sample pool.
    pub index: usize,
    /// Absolute ROM file offset where the 24-byte `Paragon5SampleDesc` descriptor begins.
    pub rom_offset: usize,
    /// Absolute ROM file offset where the signed 8-bit PCM payload begins.
    pub sample_start_offset: usize,
    /// Sample payload length in bytes (signed 8-bit mono PCM).
    pub size: usize,
    /// Native playback sample rate in Hz (default `13_379 Hz`).
    pub sample_rate: u32,
    /// MIDI root key (`0..=127`; `60` = Middle C / C4).
    pub root_key: u8,
    /// Loop start offset in samples, if looped.
    pub loop_start: Option<u32>,
    /// Loop end offset in samples, if looped.
    pub loop_end: Option<u32>,
    /// Extracted raw signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Detect whether a GBA ROM uses the Paragon 5 / GBASS Tracker Engine via either
/// a contiguous sample descriptor table (`>= 3` entries) or its ARM7TDMI mixer fingerprint.
pub fn detect_paragon5_engine(rom: &[u8]) -> bool {
    !find_clustered_descriptor_offsets(rom).is_empty()
        || Paragon5Version::detect_from_bytes(rom).is_some()
}

/// Scan a GBA ROM for contiguous 24-byte (`0x18`) Paragon 5 `Paragon5SampleDesc`
/// descriptors and extract their signed 8-bit PCM payloads, deduplicated by PCM start offset.
pub fn scan_paragon5_soundpool(rom: &[u8]) -> Vec<Paragon5SampleEntry> {
    let clustered = find_clustered_descriptor_offsets(rom);
    if clustered.is_empty() {
        return Vec::new();
    }

    let mut seen_data = HashSet::new();
    let mut results = Vec::new();

    for off in clustered {
        let Ok(desc) = Paragon5SampleDesc::parse(&rom[off..off + PARAGON5_SAMPLE_DESC_SIZE]) else {
            continue;
        };
        let sample_start_offset = desc.sample_rom_offset();
        if !seen_data.insert(sample_start_offset) {
            continue;
        }
        let Ok(view) = desc.resolve_view(rom) else {
            continue;
        };

        let pcm_data: Vec<i8> = view.pcm_bytes.iter().map(|&b| b as i8).collect();
        let index = results.len();
        results.push(Paragon5SampleEntry {
            index,
            rom_offset: off,
            sample_start_offset,
            size: view.len(),
            sample_rate: view.sample_rate_hz,
            root_key: view.root_key,
            loop_start: view.loop_start,
            loop_end: view.loop_end,
            pcm_data,
        });
    }

    results
}

/// Extract all Paragon 5 Tracker Engine samples from a ROM and encode them as standard
/// 16-bit uncompressed mono RIFF WAV files.
///
/// Returns a list of `(sample_index, recommended_filename, wav_bytes)`.
pub fn export_paragon5_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_paragon5_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_paragon5_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!(
            "paragon5_sample_{:03}_{:06X}.wav",
            s.index, s.sample_start_offset
        );
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Paragon 5 Tracker Engine samples into a standard
/// SoundFont 2.04 (`.sf2`) soundbank.
pub fn compile_paragon5_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_paragon5_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_paragon5_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Paragon 5)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            let loop_range = s.loop_start.zip(s.loop_end);
            RawSoundFontSample {
                name: format!("P5_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(&bank_title, "Paragon 5 Tracker Engine", &raw_samples)
}

/// Locate all 4-byte-aligned `Paragon5SampleDesc` offsets that belong to a contiguous
/// 24-byte-strided run of at least [`MIN_CONTIGUOUS_ENTRIES`] valid descriptors.
fn find_clustered_descriptor_offsets(rom: &[u8]) -> Vec<usize> {
    let rlen = rom.len();
    if rlen < PARAGON5_SAMPLE_DESC_SIZE * MIN_CONTIGUOUS_ENTRIES {
        return Vec::new();
    }

    let mut cand_offs = HashSet::new();
    let mut pos = 0usize;

    while let Some(idx) = find_subslice(rom, &PARAGON5_DESC_SIG, pos) {
        pos = idx + 4;
        if idx < 4 {
            continue;
        }

        let off = idx - 4;
        if !off.is_multiple_of(4) || off + PARAGON5_SAMPLE_DESC_SIZE > rlen {
            continue;
        }

        let Ok(desc) = Paragon5SampleDesc::parse(&rom[off..off + PARAGON5_SAMPLE_DESC_SIZE]) else {
            continue;
        };
        if desc.pitch_scale_fp16 != PARAGON5_PITCH_SCALE_ONE {
            continue;
        }
        if (desc.length_fp16 & 0xFFFF) != 0
            || (desc.loop_start_fp16 & 0xFFFF) != 0
            || (desc.loop_end_fp16 & 0xFFFF) != 0
        {
            continue;
        }

        let slen = desc.length();
        let raw_lend = (desc.loop_end_fp16 >> 16) as usize;
        if !(MIN_SAMPLE_BYTES..=MAX_SAMPLE_BYTES).contains(&slen) {
            continue;
        }
        if raw_lend < slen || raw_lend > slen + 2 {
            continue;
        }
        if desc.resolve_view(rom).is_err() {
            continue;
        }

        cand_offs.insert(off);
    }

    let mut clustered: Vec<usize> = cand_offs
        .iter()
        .copied()
        .filter(|&o| {
            let prev1 = o
                .checked_sub(PARAGON5_SAMPLE_DESC_SIZE)
                .is_some_and(|p| cand_offs.contains(&p));
            let prev2 = o
                .checked_sub(PARAGON5_SAMPLE_DESC_SIZE * 2)
                .is_some_and(|p| cand_offs.contains(&p));
            let next1 = o
                .checked_add(PARAGON5_SAMPLE_DESC_SIZE)
                .is_some_and(|n| cand_offs.contains(&n));
            let next2 = o
                .checked_add(PARAGON5_SAMPLE_DESC_SIZE * 2)
                .is_some_and(|n| cand_offs.contains(&n));

            (prev1 && prev2) || (prev1 && next1) || (next1 && next2)
        })
        .collect();

    if clustered.len() < MIN_CONTIGUOUS_ENTRIES {
        return Vec::new();
    }

    clustered.sort_unstable();
    clustered
}

/// Find the next occurrence of `needle` in `haystack` starting at `start`.
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
    use paragon5::GBA_ROM_BUS_BASE;

    fn write_paragon5_desc(
        rom: &mut [u8],
        off: usize,
        sample_off: usize,
        length: u32,
        loop_start: u32,
        loop_end: u32,
    ) {
        let ptr = GBA_ROM_BUS_BASE + (sample_off as u32);
        let words: [u32; 6] = [
            ptr,
            PARAGON5_PITCH_SCALE_ONE,
            0,
            length << 16,
            loop_start << 16,
            loop_end << 16,
        ];
        for (i, w) in words.iter().enumerate() {
            rom[off + i * 4..off + (i + 1) * 4].copy_from_slice(&w.to_le_bytes());
        }
    }

    fn build_synthetic_paragon5_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"PARAGON5TST\0");

        // Packed PCM8 samples starting at 0x0800
        let specs: [(usize, u32, u32, u32); 4] = [
            (0x0800, 64, 0, 64),    // one-shot
            (0x0840, 128, 32, 130), // looped (loop_end clamped to length=128)
            (0x08C2, 96, 16, 96),   // looped
            (0x0840, 128, 32, 130), // duplicate pointer to sample 1 (deduplicated)
        ];

        for &(sample_off, len, _, _) in &specs[..3] {
            for k in 0..(len as usize) {
                let v = (((k as i32) % 16) - 8) * 11;
                rom[sample_off + k] = (v as i8) as u8;
            }
        }

        // Write 4 contiguous 24-byte descriptors at 0x0400
        let table_off = 0x0400usize;
        for (i, &(sample_off, len, lstart, lend)) in specs.iter().enumerate() {
            write_paragon5_desc(
                &mut rom,
                table_off + i * PARAGON5_SAMPLE_DESC_SIZE,
                sample_off,
                len,
                lstart,
                lend,
            );
        }

        rom
    }

    #[test]
    fn detects_and_scans_paragon5_table_with_dedup_and_loop_clamping() {
        let rom = build_synthetic_paragon5_rom();
        assert!(detect_paragon5_engine(&rom));

        let samples = scan_paragon5_soundpool(&rom);
        assert_eq!(samples.len(), 3);

        // Sample 0: one-shot
        assert_eq!(samples[0].index, 0);
        assert_eq!(samples[0].rom_offset, 0x0400);
        assert_eq!(samples[0].sample_start_offset, 0x0800);
        assert_eq!(samples[0].size, 64);
        assert_eq!(samples[0].sample_rate, 13_379);
        assert_eq!(samples[0].root_key, 60);
        assert_eq!(samples[0].loop_start, None);
        assert_eq!(samples[0].loop_end, None);
        assert_eq!(samples[0].pcm_data[0], -88);

        // Sample 1: looped, loop_end=130 clamped to size=128
        assert_eq!(samples[1].index, 1);
        assert_eq!(samples[1].sample_start_offset, 0x0840);
        assert_eq!(samples[1].size, 128);
        assert_eq!(samples[1].loop_start, Some(32));
        assert_eq!(samples[1].loop_end, Some(128));

        // Sample 2: looped
        assert_eq!(samples[2].index, 2);
        assert_eq!(samples[2].sample_start_offset, 0x08C2);
        assert_eq!(samples[2].size, 96);
        assert_eq!(samples[2].loop_start, Some(16));
        assert_eq!(samples[2].loop_end, Some(96));
    }

    #[test]
    fn rejects_isolated_or_non_integer_fp16_descriptors() {
        let mut rom = vec![0u8; 0x2000];
        // Only 2 contiguous descriptors (< MIN_CONTIGUOUS_ENTRIES)
        write_paragon5_desc(&mut rom, 0x0200, 0x0800, 64, 0, 64);
        write_paragon5_desc(&mut rom, 0x0218, 0x0840, 64, 0, 64);

        // 3 descriptors, but middle one has non-zero fractional bits in length_fp16
        write_paragon5_desc(&mut rom, 0x0400, 0x0900, 64, 0, 64);
        write_paragon5_desc(&mut rom, 0x0418, 0x0940, 64, 0, 64);
        rom[0x0418 + 12] = 0x01; // fractional bit set
        write_paragon5_desc(&mut rom, 0x0430, 0x0980, 64, 0, 64);

        assert!(!detect_paragon5_engine(&rom));
        assert!(scan_paragon5_soundpool(&rom).is_empty());
    }

    #[test]
    fn exports_valid_wav16_and_soundfont() {
        let rom = build_synthetic_paragon5_rom();

        let wavs = export_paragon5_samples_wav16(&rom).expect("wav export should succeed");
        assert_eq!(wavs.len(), 3);
        for (idx, name, bytes) in &wavs {
            assert!(name.starts_with(&format!("paragon5_sample_{idx:03}_")));
            assert!(name.ends_with(".wav"));
            assert_eq!(&bytes[0..4], b"RIFF");
            assert_eq!(&bytes[8..12], b"WAVE");
        }
        assert_eq!(wavs[0].2.len(), 44 + 64 * 2);

        let sf2 = compile_paragon5_soundfont(&rom, Some("Synthetic Paragon 5"))
            .expect("sf2 compile should succeed");
        let validation = validate_soundfont(&sf2.sf2_bytes).expect("sf2 should validate");
        assert_eq!(validation.sample_count, 3);
        assert_eq!(validation.preset_count, 3);
        assert_eq!(validation.instrument_count, 3);
        assert_eq!(validation.bank_name, "Synthetic Paragon 5");
        assert_eq!(validation.sound_engine, "Paragon 5 Tracker Engine");

        let sf2_default = compile_paragon5_soundfont(&rom, None).expect("default title compile");
        let val_default = validate_soundfont(&sf2_default.sf2_bytes).expect("default sf2 valid");
        assert_eq!(val_default.bank_name, "PARAGON5TST (Paragon 5)");
    }

    #[test]
    fn rejects_non_paragon5_rom() {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"NONPARAGON5\0");
        assert!(!detect_paragon5_engine(&rom));
        assert!(scan_paragon5_soundpool(&rom).is_empty());
        assert!(export_paragon5_samples_wav16(&rom).is_err());
        assert!(compile_paragon5_soundfont(&rom, None).is_err());
    }
}
