//! Nintendo R&D1 "Kaede" Standard MIDI (`MThd`) Engine scanner, verbatim
//! Standard MIDI (`.mid`) extractor, 24-byte `Rd1SampleDesc` signed 8-bit PCM
//! pool extractor, WAV16 exporter, and SoundFont 2.04 soundbank compiler.
//!
//! Covers Nintendo R&D1 GBA titles built on the Kaede DirectSound MIDI engine
//! (*WarioWare, Inc.*, *Made in Wario*, *Rhythm Tengoku*) as well as third-party
//! GBA titles embedding verbatim Standard MIDI (`MThd` / `MTrk`) sequences.
//!
//! Each embedded sequence is a verbatim Standard MIDI Format 0/1/2 file
//! (`MThd` header + `num_tracks` contiguous `MTrk` chunks) that can be exported
//! directly as `.mid` without conversion. Melodic instruments, drum kits, and
//! voice/SFX clips are stored as raw signed 8-bit PCM referenced by clustered
//! arrays of 24-byte `Rd1SampleDesc` descriptors.

#![deny(unsafe_code)]

use std::collections::HashSet;

use formatkit_core::{Error, Result};
use rd1_kaede::{
    KaedeMidiHeader, KaedeSampleDesc, KaedeSampleView, KAEDE_SAMPLE_DESC_SIZE, MTHD_HEADER_SIZE,
    MTHD_MAGIC,
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

/// Minimum and maximum accepted native playback sample rates in Hz.
const MIN_SAMPLE_RATE_HZ: u32 = 4_000;
const MAX_SAMPLE_RATE_HZ: u32 = 48_000;

/// Minimum number of valid embedded Standard MIDI files required to detect the engine.
const MIN_DETECT_MIDIS: usize = 2;

/// Extracted Nintendo R&D1 Kaede signed 8-bit PCM sample entry from a GBA ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rd1SampleEntry {
    /// Sample index within the detected pool.
    pub index: usize,
    /// Absolute ROM file offset where the 24-byte `Rd1SampleDesc` descriptor begins.
    pub rom_offset: usize,
    /// Absolute ROM file offset where the signed 8-bit PCM payload begins.
    pub sample_start_offset: usize,
    /// Sample payload length in bytes (8-bit mono samples).
    pub size: usize,
    /// Native playback sample rate in Hz (`4000..=48000`).
    pub sample_rate: u32,
    /// MIDI root note (`0..=127`; `60` = Middle C / C4).
    pub root_key: u8,
    /// Loop range `(loop_start, loop_end)` in samples, if looped (`loop_start < loop_end <= size`).
    pub loop_range: Option<(u32, u32)>,
    /// Extracted raw signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Detect whether a GBA ROM uses the Nintendo R&D1 "Kaede" Standard MIDI (`MThd`) engine.
///
/// Confirms at least two structurally valid Standard MIDI (`MThd` + `MTrk`) sequences
/// embedded in the ROM via [`KaedeMidiHeader::parse_and_validate`].
pub fn detect_rd1_engine(rom: &[u8]) -> bool {
    if rom.len() < MTHD_HEADER_SIZE {
        return false;
    }

    let mut valid_count = 0usize;
    let mut pos = 0usize;
    while let Some(idx) = find_subslice(rom, MTHD_MAGIC, pos) {
        if let Ok(hdr) = KaedeMidiHeader::parse_and_validate(&rom[idx..]) {
            valid_count += 1;
            if valid_count >= MIN_DETECT_MIDIS {
                return true;
            }
            pos = idx + hdr.total_bytes;
        } else {
            pos = idx + MTHD_MAGIC.len();
        }
    }

    false
}

/// Scan a GBA ROM for all structurally valid verbatim Standard MIDI (`.mid`) files.
///
/// Returns a list of `(midi_index, recommended_filename, midi_bytes)` where each
/// byte vector is an exact, playable Standard MIDI Format 0/1/2 file slice.
pub fn scan_rd1_midis(rom: &[u8]) -> Vec<(usize, String, Vec<u8>)> {
    if rom.len() < MTHD_HEADER_SIZE {
        return Vec::new();
    }

    let mut midis = Vec::new();
    let mut pos = 0usize;
    while let Some(idx) = find_subslice(rom, MTHD_MAGIC, pos) {
        if let Ok(hdr) = KaedeMidiHeader::parse_and_validate(&rom[idx..]) {
            let end = idx + hdr.total_bytes;
            let index = midis.len();
            let name = format!("rd1_midi_{index:04}_{idx:06X}.mid");
            midis.push((index, name, rom[idx..end].to_vec()));
            pos = end;
        } else {
            pos = idx + MTHD_MAGIC.len();
        }
    }

    midis
}

/// Scan a GBA ROM for clustered 24-byte (`0x18`) Nintendo R&D1 `KaedeSampleDesc`
/// descriptors and extract their signed 8-bit PCM payloads, deduplicated by PCM
/// start offset.
pub fn scan_rd1_soundpool(rom: &[u8]) -> Vec<Rd1SampleEntry> {
    let rlen = rom.len();
    if rlen < KAEDE_SAMPLE_DESC_SIZE {
        return Vec::new();
    }

    let mut raw_hits: Vec<(usize, KaedeSampleDesc)> = Vec::new();
    let mut hit_offs = HashSet::new();

    let mut off = 0usize;
    while off + KAEDE_SAMPLE_DESC_SIZE <= rlen {
        if let Ok(desc) = KaedeSampleDesc::parse(&rom[off..off + KAEDE_SAMPLE_DESC_SIZE]) {
            let size = desc.sample_len as usize;
            if (MIN_SAMPLE_BYTES..=MAX_SAMPLE_BYTES).contains(&size)
                && (MIN_SAMPLE_RATE_HZ..=MAX_SAMPLE_RATE_HZ).contains(&desc.sample_rate_hz)
                && KaedeSampleView::from_desc_and_rom(0, &desc, rom).is_ok()
            {
                hit_offs.insert(off);
                raw_hits.push((off, desc));
            }
        }

        off += 4;
    }

    let mut seen_data = HashSet::new();
    let mut samples = Vec::new();

    for (hit_off, desc) in raw_hits {
        let has_prev = hit_off
            .checked_sub(KAEDE_SAMPLE_DESC_SIZE)
            .is_some_and(|prev| hit_offs.contains(&prev));
        let has_next = hit_offs.contains(&(hit_off + KAEDE_SAMPLE_DESC_SIZE));
        if !has_prev && !has_next {
            continue;
        }
        let Some(data_off) = desc.pcm_rom_offset() else {
            continue;
        };
        if !seen_data.insert(data_off) {
            continue;
        }

        let index = samples.len();
        let Ok(view) = KaedeSampleView::from_desc_and_rom(index as u32, &desc, rom) else {
            continue;
        };

        let pcm_data: Vec<i8> = view.pcm_bytes.iter().map(|&b| b as i8).collect();
        samples.push(Rd1SampleEntry {
            index,
            rom_offset: hit_off,
            sample_start_offset: data_off,
            size: view.len(),
            sample_rate: view.sample_rate_hz,
            root_key: view.root_note,
            loop_range: view.loop_range,
            pcm_data,
        });
    }

    samples
}

/// Extract all Nintendo R&D1 Kaede samples from a ROM and encode them as standard
/// 16-bit uncompressed mono RIFF WAV files.
///
/// Returns a list of `(sample_index, recommended_filename, wav_bytes)`.
pub fn export_rd1_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_rd1_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_rd1_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!(
            "rd1_sample_{:03}_{:06X}.wav",
            s.index, s.sample_start_offset
        );
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Nintendo R&D1 Kaede samples into a standard SoundFont 2.04
/// (`.sf2`) soundbank.
pub fn compile_rd1_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_rd1_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_rd1_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Nintendo R&D1)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            RawSoundFontSample {
                name: format!("RD1_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range: s.loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(
        &bank_title,
        "Nintendo R&D1 Kaede MIDI Engine",
        &raw_samples,
    )
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
    use rd1_kaede::{GBA_ROM_BUS_BASE, MTRK_MAGIC};

    fn build_minimal_midi(fmt: u16, tracks: &[&[u8]], division: u16) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MTHD_MAGIC);
        out.extend_from_slice(&fmt.to_be_bytes());
        out.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
        out.extend_from_slice(&division.to_be_bytes());
        for trk in tracks {
            out.extend_from_slice(MTRK_MAGIC);
            out.extend_from_slice(&(trk.len() as u32).to_be_bytes());
            out.extend_from_slice(trk);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn write_rd1_desc(
        rom: &mut [u8],
        off: usize,
        sample_start_off: usize,
        sample_len: usize,
        sample_rate_hz: u32,
        root_note: u32,
        loop_start: u32,
        loop_end: u32,
    ) {
        let ptr = GBA_ROM_BUS_BASE + (sample_start_off as u32);
        let words: [u32; 6] = [
            ptr,
            sample_len as u32,
            sample_rate_hz,
            root_note,
            loop_start,
            loop_end,
        ];
        for (i, w) in words.iter().enumerate() {
            rom[off + i * 4..off + (i + 1) * 4].copy_from_slice(&w.to_le_bytes());
        }
    }

    fn build_synthetic_rd1_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x3000];
        rom[0xA0..0xAC].copy_from_slice(b"RD1TESTROM\0\0");

        // Embed 2 valid Standard MIDI Format 1 sequences
        let eot: &[u8] = &[0x00, 0xFF, 0x2F, 0x00];
        let note_trk: &[u8] = &[0x00, 0x90, 0x3C, 0x64, 0x18, 0x80, 0x3C, 0x00, 0x00, 0xFF, 0x2F, 0x00];
        let midi0 = build_minimal_midi(1, &[eot, note_trk], 24);
        let midi1 = build_minimal_midi(0, &[note_trk], 48);

        rom[0x0200..0x0200 + midi0.len()].copy_from_slice(&midi0);
        rom[0x0300..0x0300 + midi1.len()].copy_from_slice(&midi1);

        // Also embed a malformed MThd header (missing MTrk) that must be ignored
        rom[0x0380..0x0388].copy_from_slice(MTHD_MAGIC);
        rom[0x0388..0x038A].copy_from_slice(&1u16.to_be_bytes());
        rom[0x038A..0x038C].copy_from_slice(&2u16.to_be_bytes());
        rom[0x038C..0x038E].copy_from_slice(&24u16.to_be_bytes());

        // PCM sample payloads
        let pcm0_off = 0x1000usize;
        let pcm0_len = 64usize;
        for k in 0..pcm0_len {
            let v = (((k as i32) % 16) - 8) * 9;
            rom[pcm0_off + k] = (v as i8) as u8;
        }

        let pcm1_off = 0x1100usize;
        let pcm1_len = 128usize;
        for k in 0..pcm1_len {
            let v = (((k as i32) % 32) - 16) * 5;
            rom[pcm1_off + k] = (v as i8) as u8;
        }

        let pcm2_off = 0x1200usize;
        let pcm2_len = 96usize;
        for k in 0..pcm2_len {
            let v = (((k as i32) % 12) - 6) * 11;
            rom[pcm2_off + k] = (v as i8) as u8;
        }

        // Clustered 24-byte Rd1SampleDesc table at 0x0800 (4 consecutive entries, 1 duplicate)
        write_rd1_desc(&mut rom, 0x0800, pcm0_off, pcm0_len, 13_379, 60, 0, 0);
        write_rd1_desc(&mut rom, 0x0818, pcm1_off, pcm1_len, 16_000, 69, 32, 120);
        // Duplicate pointer to pcm1_off (should be deduplicated)
        write_rd1_desc(&mut rom, 0x0830, pcm1_off, pcm1_len, 16_000, 69, 32, 120);
        write_rd1_desc(&mut rom, 0x0848, pcm2_off, pcm2_len, 10_512, 48, 16, 96);

        // Isolated non-clustered descriptor at 0x0A00 (must be filtered out)
        write_rd1_desc(&mut rom, 0x0A00, pcm0_off + 4, 32, 13_379, 60, 0, 0);

        rom
    }

    #[test]
    fn detects_rd1_engine_and_extracts_verbatim_midis() {
        let rom = build_synthetic_rd1_rom();
        assert!(detect_rd1_engine(&rom));

        let midis = scan_rd1_midis(&rom);
        assert_eq!(midis.len(), 2);

        assert_eq!(midis[0].0, 0);
        assert_eq!(midis[0].1, "rd1_midi_0000_000200.mid");
        assert_eq!(&midis[0].2[0..8], MTHD_MAGIC);
        assert_eq!(u16::from_be_bytes([midis[0].2[8], midis[0].2[9]]), 1);
        assert_eq!(u16::from_be_bytes([midis[0].2[10], midis[0].2[11]]), 2);

        assert_eq!(midis[1].0, 1);
        assert_eq!(midis[1].1, "rd1_midi_0001_000300.mid");
        assert_eq!(&midis[1].2[0..8], MTHD_MAGIC);
        assert_eq!(u16::from_be_bytes([midis[1].2[8], midis[1].2[9]]), 0);
        assert_eq!(u16::from_be_bytes([midis[1].2[10], midis[1].2[11]]), 1);
    }

    #[test]
    fn scans_clustered_rd1_samples_with_deduplication_and_loop_bounds() {
        let rom = build_synthetic_rd1_rom();
        let samples = scan_rd1_soundpool(&rom);

        assert_eq!(samples.len(), 3);

        // Sample 0: one-shot
        assert_eq!(samples[0].index, 0);
        assert_eq!(samples[0].rom_offset, 0x0800);
        assert_eq!(samples[0].sample_start_offset, 0x1000);
        assert_eq!(samples[0].size, 64);
        assert_eq!(samples[0].sample_rate, 13_379);
        assert_eq!(samples[0].root_key, 60);
        assert_eq!(samples[0].loop_range, None);
        assert_eq!(samples[0].pcm_data[0], -72);

        // Sample 1: looped with explicit loop_end < size
        assert_eq!(samples[1].index, 1);
        assert_eq!(samples[1].rom_offset, 0x0818);
        assert_eq!(samples[1].sample_start_offset, 0x1100);
        assert_eq!(samples[1].size, 128);
        assert_eq!(samples[1].sample_rate, 16_000);
        assert_eq!(samples[1].root_key, 69);
        assert_eq!(samples[1].loop_range, Some((32, 120)));
        assert_eq!(samples[1].pcm_data[0], -80);

        // Sample 2: looped to end
        assert_eq!(samples[2].index, 2);
        assert_eq!(samples[2].rom_offset, 0x0848);
        assert_eq!(samples[2].sample_start_offset, 0x1200);
        assert_eq!(samples[2].size, 96);
        assert_eq!(samples[2].sample_rate, 10_512);
        assert_eq!(samples[2].root_key, 48);
        assert_eq!(samples[2].loop_range, Some((16, 96)));
        assert_eq!(samples[2].pcm_data[0], -66);
    }

    #[test]
    fn exports_valid_wav16_and_soundfont() {
        let rom = build_synthetic_rd1_rom();

        let wavs = export_rd1_samples_wav16(&rom).expect("wav export should succeed");
        assert_eq!(wavs.len(), 3);
        for (idx, name, bytes) in &wavs {
            assert!(name.starts_with(&format!("rd1_sample_{idx:03}_")));
            assert!(name.ends_with(".wav"));
            assert_eq!(&bytes[0..4], b"RIFF");
            assert_eq!(&bytes[8..12], b"WAVE");
        }
        assert_eq!(wavs[0].2.len(), 44 + 64 * 2);

        let sf2 = compile_rd1_soundfont(&rom, Some("Synthetic RD1"))
            .expect("sf2 compile should succeed");
        let validation = validate_soundfont(&sf2.sf2_bytes).expect("sf2 should validate");
        assert_eq!(validation.sample_count, 3);
        assert_eq!(validation.preset_count, 3);
        assert_eq!(validation.instrument_count, 3);
        assert_eq!(validation.bank_name, "Synthetic RD1");
        assert_eq!(validation.sound_engine, "Nintendo R&D1 Kaede MIDI Engine");

        let sf2_default = compile_rd1_soundfont(&rom, None).expect("default title compile");
        let val_default = validate_soundfont(&sf2_default.sf2_bytes).expect("default sf2 valid");
        assert_eq!(val_default.bank_name, "RD1TESTROM (Nintendo R&D1)");
    }

    #[test]
    fn rejects_non_rd1_rom() {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"NONRD1ROM\0\0\0");

        // Only 1 valid MIDI should not trigger detect_rd1_engine (requires >= 2)
        let eot: &[u8] = &[0x00, 0xFF, 0x2F, 0x00];
        let midi0 = build_minimal_midi(0, &[eot], 24);
        rom[0x0200..0x0200 + midi0.len()].copy_from_slice(&midi0);

        assert!(!detect_rd1_engine(&rom));
        assert_eq!(scan_rd1_midis(&rom).len(), 1);
        assert!(scan_rd1_soundpool(&rom).is_empty());
        assert!(export_rd1_samples_wav16(&rom).is_err());
        assert!(compile_rd1_soundfont(&rom, None).is_err());
    }
}
