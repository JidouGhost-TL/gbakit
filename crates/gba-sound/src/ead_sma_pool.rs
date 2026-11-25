//! Nintendo EAD / SRD (*Super Mario Advance 2–4* & *The Legend of Zelda:
//! A Link to the Past & Four Swords*) DirectSound sample pool scanning,
//! signed 8-bit PCM extraction, WAV16 export, and SoundFont 2.04 soundbank
//! compilation.
//!
//! Retail EAD/SRD GBA ROMs pack signed 8-bit PCM instruments into contiguous
//! banks of 16-byte [`ead_sma::EadSampleDesc`] headers (`total_len`,
//! `rate_word`, `loop_start`, `loop_end`) immediately followed by `total_len`
//! bytes of inline signed 8-bit PCM padded to 4- or 16-byte alignment.

#![deny(unsafe_code)]

use std::collections::{HashMap, HashSet};

use ead_sma::{EadSampleDesc, EadSampleView, EadSmaVersion, EAD_SAMPLE_DESC_SIZE};
use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// Minimum accepted EAD/SRD sample length in bytes.
const MIN_SAMPLE_BYTES: usize = 32;

/// Maximum accepted EAD/SRD sample length in bytes (`256 KiB`).
const MAX_SAMPLE_BYTES: usize = 256 * 1024;

/// Minimum and maximum accepted native playback sample rates in Hz.
const MIN_SAMPLE_RATE_HZ: u32 = 4_000;
const MAX_SAMPLE_RATE_HZ: u32 = 48_000;

/// Minimum number of chained descriptors required to accept a sample bank.
const MIN_CHAIN_LEN: usize = 3;

/// Maximum 4-byte padding words tolerated between chained EAD/SRD samples
/// (covers both 4-byte and 16-byte alignment padding).
const MAX_INTER_SAMPLE_PAD_WORDS: usize = 4;

/// Extracted Nintendo EAD/SRD signed 8-bit PCM sample entry from a GBA ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EadSmaSampleEntry {
    /// Sample index within the detected pool.
    pub index: usize,
    /// Absolute ROM file offset where the 16-byte `EadSampleDesc` begins.
    pub rom_offset: usize,
    /// Absolute ROM file offset where the inline signed 8-bit PCM payload begins.
    pub sample_start_offset: usize,
    /// Sample payload length in bytes (`total_len`).
    pub size: usize,
    /// Native playback sample rate in Hz (`rate_word`).
    pub sample_rate: u32,
    /// MIDI root note (`60` = Middle C / C4).
    pub root_key: u8,
    /// Optional loop range `(loop_start, loop_end)` in samples.
    pub loop_range: Option<(u32, u32)>,
    /// Extracted raw signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Detect whether a GBA ROM uses the Nintendo EAD/SRD SMA2–4 sound engine.
pub fn detect_ead_sma_engine(rom: &[u8]) -> bool {
    EadSmaVersion::detect_from_bytes(rom).is_some()
}

/// Validate a candidate 16-byte [`EadSampleDesc`] at `off` within `rom`.
fn parse_valid_ead_desc(rom: &[u8], off: usize) -> Option<EadSampleDesc> {
    let slice = rom.get(off..off.checked_add(EAD_SAMPLE_DESC_SIZE)?)?;
    let desc = EadSampleDesc::parse(slice).ok()?;

    let total_len = desc.total_len as usize;
    if !(MIN_SAMPLE_BYTES..=MAX_SAMPLE_BYTES).contains(&total_len) {
        return None;
    }
    if !(MIN_SAMPLE_RATE_HZ..=MAX_SAMPLE_RATE_HZ).contains(&desc.rate_word) {
        return None;
    }

    let valid_loop = if desc.loop_end == 0 {
        desc.loop_start == 0
    } else {
        desc.loop_start < desc.loop_end && desc.loop_end <= desc.total_len
    };
    if !valid_loop {
        return None;
    }

    let pcm_start = off.checked_add(EAD_SAMPLE_DESC_SIZE)?;
    let pcm_end = pcm_start.checked_add(total_len)?;
    if pcm_end > rom.len() {
        return None;
    }

    Some(desc)
}

/// Returns `true` if the signed 8-bit PCM slice contains non-silent waveform energy.
fn has_nonzero_pcm_energy(pcm: &[u8]) -> bool {
    let mut non_zero = 0usize;
    let check_len = pcm.len().min(512);
    for &b in &pcm[..check_len] {
        if b != 0 && b != 0xFF {
            non_zero += 1;
            if non_zero >= 8 {
                return true;
            }
        }
    }
    // Check across full slice if the attack begins with leading silence.
    if pcm.len() > check_len {
        for &b in &pcm[check_len..] {
            if b != 0 && b != 0xFF {
                non_zero += 1;
                if non_zero >= 8 {
                    return true;
                }
            }
        }
    }
    false
}

/// Scan a GBA ROM for chained 16-byte Nintendo EAD/SRD [`EadSampleDesc`] banks
/// and extract their signed 8-bit PCM payloads.
pub fn scan_ead_sma_soundpool(rom: &[u8]) -> Vec<EadSmaSampleEntry> {
    let rlen = rom.len();
    if rlen < EAD_SAMPLE_DESC_SIZE + MIN_SAMPLE_BYTES {
        return Vec::new();
    }

    let mut candidates: HashMap<usize, EadSampleDesc> = HashMap::new();
    let mut candidate_offsets: Vec<usize> = Vec::new();

    let mut off = 0usize;
    while off + EAD_SAMPLE_DESC_SIZE + MIN_SAMPLE_BYTES <= rlen {
        if let Some(desc) = parse_valid_ead_desc(rom, off) {
            candidates.insert(off, desc);
            candidate_offsets.push(off);
        }
        off += 4;
    }

    let mut visited = HashSet::new();
    let mut seen_pcm_offsets = HashSet::new();
    let mut samples = Vec::new();

    for start_off in candidate_offsets {
        if visited.contains(&start_off) {
            continue;
        }

        let mut chain = vec![start_off];
        let mut cur = start_off;
        while let Some(&desc) = candidates.get(&cur) {
            let aligned_end = (cur + EAD_SAMPLE_DESC_SIZE + desc.total_len as usize + 3) & !3;
            let mut next_hit = None;
            for pad in 0..=MAX_INTER_SAMPLE_PAD_WORDS {
                let cand_next = aligned_end + pad * 4;
                if cand_next > cur && candidates.contains_key(&cand_next) {
                    next_hit = Some(cand_next);
                    break;
                }
            }
            if let Some(nxt) = next_hit {
                if visited.contains(&nxt) {
                    break;
                }
                chain.push(nxt);
                cur = nxt;
            } else {
                break;
            }
        }

        if chain.len() < MIN_CHAIN_LEN {
            continue;
        }

        // Reject chains consisting entirely of silent/zero-filled buffers.
        let mut active_entries = Vec::with_capacity(chain.len());
        for &entry_off in &chain {
            visited.insert(entry_off);
            let desc = candidates[&entry_off];
            let pcm_start = entry_off + EAD_SAMPLE_DESC_SIZE;
            let pcm_end = pcm_start + desc.total_len as usize;
            let pcm_slice = &rom[pcm_start..pcm_end];
            if has_nonzero_pcm_energy(pcm_slice) {
                active_entries.push((entry_off, desc, pcm_start, pcm_end));
            }
        }

        if active_entries.len() < MIN_CHAIN_LEN {
            continue;
        }

        for (entry_off, desc, pcm_start, pcm_end) in active_entries {
            if !seen_pcm_offsets.insert(pcm_start) {
                continue;
            }
            let view = EadSampleView::from_parts(desc, &rom[pcm_start..pcm_end]);
            let pcm_data: Vec<i8> = view.pcm_bytes.iter().map(|&b| b as i8).collect();
            let loop_range = if desc.is_looped() {
                Some((desc.loop_start, desc.loop_end))
            } else {
                None
            };

            let index = samples.len();
            samples.push(EadSmaSampleEntry {
                index,
                rom_offset: entry_off,
                sample_start_offset: pcm_start,
                size: view.len(),
                sample_rate: desc.rate_word,
                root_key: 60,
                loop_range,
                pcm_data,
            });
        }
    }

    samples
}

/// Extract all Nintendo EAD/SRD samples from a ROM and encode them as standard
/// 16-bit uncompressed mono RIFF WAV files.
///
/// Returns a list of `(sample_index, recommended_filename, wav_bytes)`.
pub fn export_ead_sma_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_ead_sma_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_ead_sma_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!(
            "ead_sma_sample_{:03}_{:06X}.wav",
            s.index, s.sample_start_offset
        );
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Nintendo EAD/SRD samples into a standard SoundFont 2.04
/// (`.sf2`) soundbank.
pub fn compile_ead_sma_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_ead_sma_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_ead_sma_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Nintendo EAD SMA)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            RawSoundFontSample {
                name: format!("EAD_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range: s.loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(
        &bank_title,
        "Nintendo EAD/SRD SMA DirectSound Engine",
        &raw_samples,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;

    fn write_ead_sample(
        rom: &mut [u8],
        off: usize,
        total_len: usize,
        rate_word: u32,
        loop_start: u32,
        loop_end: u32,
    ) -> usize {
        rom[off..off + 4].copy_from_slice(&(total_len as u32).to_le_bytes());
        rom[off + 4..off + 8].copy_from_slice(&rate_word.to_le_bytes());
        rom[off + 8..off + 12].copy_from_slice(&loop_start.to_le_bytes());
        rom[off + 12..off + 16].copy_from_slice(&loop_end.to_le_bytes());
        for k in 0..total_len {
            let s = (((k as i32) % 16) - 8) * 9;
            rom[off + EAD_SAMPLE_DESC_SIZE + k] = (s as i8) as u8;
        }
        (off + EAD_SAMPLE_DESC_SIZE + total_len + 15) & !15
    }

    fn build_synthetic_ead_sma_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"EADSMA4TEST\0");

        let mut cur = 0x0400usize;
        cur = write_ead_sample(&mut rom, cur, 64, 10_512, 0, 0);
        cur = write_ead_sample(&mut rom, cur, 96, 32_000, 16, 92);
        cur = write_ead_sample(&mut rom, cur, 80, 48_000, 0, 0);
        let _ = write_ead_sample(&mut rom, cur, 128, 10_512, 32, 128);

        rom
    }

    #[test]
    fn scans_chained_ead_sma_samples_with_loop_metadata() {
        let rom = build_synthetic_ead_sma_rom();
        let samples = scan_ead_sma_soundpool(&rom);
        assert_eq!(samples.len(), 4);

        assert_eq!(samples[0].rom_offset, 0x0400);
        assert_eq!(samples[0].sample_start_offset, 0x0410);
        assert_eq!(samples[0].size, 64);
        assert_eq!(samples[0].sample_rate, 10_512);
        assert_eq!(samples[0].loop_range, None);
        assert_eq!(samples[0].pcm_data[0], -72);

        assert_eq!(samples[1].size, 96);
        assert_eq!(samples[1].sample_rate, 32_000);
        assert_eq!(samples[1].loop_range, Some((16, 92)));

        assert_eq!(samples[2].size, 80);
        assert_eq!(samples[2].sample_rate, 48_000);
        assert_eq!(samples[2].loop_range, None);

        assert_eq!(samples[3].size, 128);
        assert_eq!(samples[3].loop_range, Some((32, 128)));
    }

    #[test]
    fn exports_valid_ead_sma_wav16_and_soundfont() {
        let rom = build_synthetic_ead_sma_rom();

        let wavs = export_ead_sma_samples_wav16(&rom).expect("wav export should succeed");
        assert_eq!(wavs.len(), 4);
        assert_eq!(wavs[0].2.len(), 44 + 64 * 2);
        assert_eq!(&wavs[0].2[0..4], b"RIFF");
        assert_eq!(&wavs[0].2[8..12], b"WAVE");

        let sf2 = compile_ead_sma_soundfont(&rom, Some("Synthetic EAD SMA"))
            .expect("sf2 compile should succeed");
        let validation = validate_soundfont(&sf2.sf2_bytes).expect("sf2 should validate");
        assert_eq!(validation.sample_count, 4);
        assert_eq!(validation.preset_count, 4);
        assert_eq!(validation.instrument_count, 4);
        assert_eq!(validation.bank_name, "Synthetic EAD SMA");
        assert_eq!(
            validation.sound_engine,
            "Nintendo EAD/SRD SMA DirectSound Engine"
        );
    }

    #[test]
    fn rejects_short_chains_and_silent_ead_sma_buffers() {
        let mut rom = vec![0u8; 0x1000];
        let mut cur = 0x0200usize;
        cur = write_ead_sample(&mut rom, cur, 64, 10_512, 0, 0);
        let _ = write_ead_sample(&mut rom, cur, 64, 10_512, 0, 0);

        assert!(scan_ead_sma_soundpool(&rom).is_empty());
        assert!(export_ead_sma_samples_wav16(&rom).is_err());
        assert!(compile_ead_sma_soundfont(&rom, None).is_err());
    }
}
