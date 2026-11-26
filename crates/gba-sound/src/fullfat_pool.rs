//! Full Fat GBA Mono DirectSound audio engine detection, soundbank/sample/sequence
//! scanning, WAV16 export, and SoundFont 2.04 compilation.
//!
//! Mono DirectSound A software mixer (`SOUNDCNT_H = 0x0B04`, `DMA1CNT = 0xB600_0000`,
//! `FIFO_A = 0x0400_00A0`), dynamic Timer 0 sample rate with Timer 1 cascade IRQ,
//! ARM IWRAM 4.28 fixed-point pitch accumulator into a signed 16-bit mix buffer
//! finalized via `asr #4` and clamped to signed 8-bit PCM. Base rate `11,025 Hz`
//! at MIDI root 60 driven by a 128-entry 16.16 fixed-point MIDI frequency table.

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

const GBA_ROM_BASE: u32 = 0x0800_0000;

/// Base sample rate in Hz (`0x2B11`) used by the Full Fat pitch-table generator.
pub const FULLFAT_BASE_SAMPLE_RATE_HZ: u32 = 11_025;

/// MIDI root key corresponding to the engine's 261 Hz pitch reference (Middle C).
pub const FULLFAT_ROOT_KEY: u8 = 60;

const MAX_BANKS: usize = 16;
const MAX_SONGS_PER_BANK: u32 = 256;
const MAX_SAMPLES_PER_BANK: u32 = 2048;
const MAX_SAMPLE_LEN: u32 = 0x0040_0000;

/// 32-byte ARM IWRAM `MixChannel` inner mixing loop.
const MIX_LOOP_SIG: [u8; 32] = [
    0xD0, 0x00, 0xD8, 0xE1, // ldrsb r0, [r8]
    0xF0, 0x10, 0xD2, 0xE1, // ldrsh r1, [r2]
    0x04, 0x32, 0x93, 0xE0, // adds  r3, r3, r4, lsl #4
    0x24, 0x8E, 0xA8, 0xE0, // adc   r8, r8, r4, lsr #28
    0x90, 0x19, 0x21, 0xE0, // mla   r1, r0, sb, r1
    0xB2, 0x10, 0xC2, 0xE0, // strh  r1, [r2], #2
    0x01, 0x60, 0x56, 0xE2, // subs  r6, r6, #1
    0xF7, 0xFF, 0xFF, 0x5A, // bpl   #-36
];

/// 32-byte prefix (notes 0..7) of the 128-entry 16.16 fixed-point MIDI frequency table.
const FREQ_TABLE_PREFIX: [u8; 32] = [
    0x01, 0x2D, 0x08, 0x00, 0x76, 0xA9, 0x08, 0x00, 0x51, 0x2D, 0x09, 0x00, 0x04, 0xB9, 0x09, 0x00,
    0x05, 0x4D, 0x0A, 0x00, 0xD3, 0xE9, 0x0A, 0x00, 0xF4, 0x8F, 0x0B, 0x00, 0xF6, 0x3F, 0x0C, 0x00,
];

/// 36-byte ARM `FinalizeBuffer` clamp loop (Rev A: `bpl`).
const FINALIZE_LOOP_REVA: [u8; 36] = [
    0xF2, 0x00, 0xD2, 0xE0, 0x40, 0x02, 0xA0, 0xE1, 0x80, 0x00, 0x70, 0xE3, 0x7F, 0x00, 0xE0, 0xB3,
    0x7F, 0x00, 0x50, 0xE3, 0x7F, 0x00, 0xA0, 0xC3, 0x01, 0x00, 0xCE, 0xE4, 0x01, 0xC0, 0x5C, 0xE2,
    0xF6, 0xFF, 0xFF, 0x5A,
];

/// 36-byte ARM `FinalizeBuffer` clamp loop (Rev B: `bgt`).
const FINALIZE_LOOP_REVB: [u8; 36] = [
    0xF2, 0x00, 0xD2, 0xE0, 0x40, 0x02, 0xA0, 0xE1, 0x80, 0x00, 0x70, 0xE3, 0x7F, 0x00, 0xE0, 0xB3,
    0x7F, 0x00, 0x50, 0xE3, 0x7F, 0x00, 0xA0, 0xC3, 0x01, 0x00, 0xCE, 0xE4, 0x01, 0xC0, 0x5C, 0xE2,
    0xF6, 0xFF, 0xFF, 0xCA,
];

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

/// Extracted Full Fat signed 8-bit PCM sample entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FullFatSampleEntry {
    /// Sequential sample index across all banks.
    pub index: usize,
    /// Sound bank index.
    pub bank_index: usize,
    /// Absolute ROM file offset of the 16-byte sample header.
    pub rom_offset: usize,
    /// Absolute ROM file offset of the PCM payload.
    pub sample_start_offset: usize,
    /// Sample payload length in bytes.
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

#[derive(Debug, Clone, Copy)]
struct FullFatDetection {
    revision: FullFatRevision,
    mixer_offset: usize,
    freq_table_offset: usize,
}

fn find_aligned4(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() < 4 || haystack.len() < needle.len() {
        return None;
    }
    let last = haystack.len() - needle.len();
    let n0 = needle[0];
    let n1 = needle[1];
    let n2 = needle[2];
    let n3 = needle[3];
    let mut off = 0usize;
    while off <= last {
        if haystack[off] == n0
            && haystack[off + 1] == n1
            && haystack[off + 2] == n2
            && haystack[off + 3] == n3
            && &haystack[off..off + needle.len()] == needle
        {
            return Some(off);
        }
        off += 4;
    }
    None
}

fn detect_details(rom: &[u8]) -> Option<FullFatDetection> {
    if rom.len() < 512 {
        return None;
    }

    let mixer_offset = find_aligned4(rom, &MIX_LOOP_SIG)?;
    let freq_table_offset = find_aligned4(rom, &FREQ_TABLE_PREFIX)?;

    let ft_end = freq_table_offset.checked_add(512)?;
    if ft_end > rom.len() {
        return None;
    }
    let a4_off = freq_table_offset.checked_add(69 * 4)?;
    let a4_val = read_u32_le(rom, a4_off)?;
    if a4_val != 28_835_840 {
        return None;
    }

    let search_end = mixer_offset.saturating_add(0x400).min(rom.len());
    let window = rom.get(mixer_offset..search_end).unwrap_or(&[]);
    let revision = if find_aligned4(window, &FINALIZE_LOOP_REVB).is_some() {
        FullFatRevision::RevB
    } else if find_aligned4(window, &FINALIZE_LOOP_REVA).is_some() {
        FullFatRevision::RevA
    } else {
        FullFatRevision::Unknown
    };

    Some(FullFatDetection {
        revision,
        mixer_offset,
        freq_table_offset,
    })
}

/// Detect whether `rom` contains the Full Fat Mono DirectSound audio engine.
#[must_use]
pub fn detect_fullfat_engine(rom: &[u8]) -> Option<FullFatRevision> {
    detect_details(rom).map(|d| d.revision)
}

#[derive(Debug, Clone, Copy)]
struct BankSummary {
    song_count: u32,
    sample_count: u32,
    song_table_offset: usize,
    sample_table_offset: usize,
}

fn try_parse_bank(rom: &[u8], off: usize) -> Option<BankSummary> {
    let song_count = read_u32_le(rom, off)?;
    let sample_count = read_u32_le(rom, off + 4)?;
    let song_table_ptr = read_u32_le(rom, off + 8)?;
    let sample_table_ptr = read_u32_le(rom, off + 12)?;

    if song_count > MAX_SONGS_PER_BANK || sample_count > MAX_SAMPLES_PER_BANK {
        return None;
    }
    if song_count == 0 && sample_count == 0 {
        return None;
    }

    let bank_addr = GBA_ROM_BASE.checked_add(u32::try_from(off).ok()?)?;
    let expected_st = bank_addr.checked_add(16)?;
    if song_table_ptr != expected_st {
        return None;
    }
    let expected_nt = expected_st.checked_add(song_count.checked_mul(8)?)?;
    if sample_table_ptr != expected_nt {
        return None;
    }

    let st_off = (song_table_ptr - GBA_ROM_BASE) as usize;
    let nt_off = (sample_table_ptr - GBA_ROM_BASE) as usize;

    let st_bytes = (song_count as usize).checked_mul(8)?;
    if st_off.checked_add(st_bytes)? > rom.len() {
        return None;
    }
    for i in 0..(song_count as usize) {
        let e_off = st_off + i * 8;
        let start_ptr = read_u32_le(rom, e_off)?;
        let loop_ptr = read_u32_le(rom, e_off + 4)?;
        rom_ptr_to_offset(start_ptr, rom.len())?;
        if loop_ptr != 0 {
            rom_ptr_to_offset(loop_ptr, rom.len())?;
        }
    }

    let nt_bytes = (sample_count as usize).checked_mul(4)?;
    if nt_off.checked_add(nt_bytes)? > rom.len() {
        return None;
    }
    for i in 0..(sample_count as usize) {
        let samp_ptr = read_u32_le(rom, nt_off + i * 4)?;
        if (samp_ptr & 3) != 0 {
            return None;
        }
        let samp_off = rom_ptr_to_offset(samp_ptr, rom.len())?;
        validate_sample_at(rom, samp_off)?;
    }

    Some(BankSummary {
        song_count,
        sample_count,
        song_table_offset: st_off,
        sample_table_offset: nt_off,
    })
}

fn validate_sample_at(rom: &[u8], off: usize) -> Option<(u32, u32, u32)> {
    if off.checked_add(16)? > rom.len() {
        return None;
    }
    let fmt_raw = read_u32_le(rom, off)?;
    let length = read_u32_le(rom, off + 4)?;
    let loop_start = read_u32_le(rom, off + 8)?;
    let reserved = read_u32_le(rom, off + 12)?;

    if fmt_raw > 1 || reserved != 0 || length == 0 || length > MAX_SAMPLE_LEN || loop_start >= length
    {
        return None;
    }
    let pcm_end = off.checked_add(16)?.checked_add(length as usize)?;
    if pcm_end > rom.len() {
        return None;
    }
    Some((fmt_raw, length, loop_start))
}

/// Scan a Full Fat Mono DirectSound ROM for all sound banks, PCM samples, and songs.
pub fn scan_fullfat_engine(rom: &[u8]) -> Option<FullFatScanSummary> {
    if rom.len() < 512 || rom.len() > 0x0200_0000 {
        return None;
    }
    let det = detect_details(rom)?;

    let mut banks = Vec::new();
    let mut off = 0usize;
    let limit = rom.len().saturating_sub(16);
    while off <= limit && banks.len() < MAX_BANKS {
        if let Some(summary) = try_parse_bank(rom, off) {
            let skip = 16usize
                .saturating_add((summary.song_count as usize).saturating_mul(8))
                .saturating_add((summary.sample_count as usize).saturating_mul(4));
            banks.push(summary);
            off = off.saturating_add(skip.max(4));
            continue;
        }
        off += 4;
    }

    let mut samples = Vec::new();
    let mut songs = Vec::new();

    for (bank_idx, bank) in banks.iter().enumerate() {
        for s_idx in 0..(bank.song_count as usize) {
            let e_off = bank.song_table_offset + s_idx * 8;
            if let (Some(start_ptr), Some(loop_ptr)) =
                (read_u32_le(rom, e_off), read_u32_le(rom, e_off + 4))
            {
                if let Some(start_offset) = rom_ptr_to_offset(start_ptr, rom.len()) {
                    let loop_offset = if loop_ptr == 0 {
                        0
                    } else {
                        rom_ptr_to_offset(loop_ptr, rom.len()).unwrap_or(0)
                    };
                    songs.push(FullFatSongEntry {
                        index: s_idx,
                        bank_index: bank_idx,
                        start_offset,
                        loop_offset,
                    });
                }
            }
        }

        for n_idx in 0..(bank.sample_count as usize) {
            let Some(samp_ptr) = read_u32_le(rom, bank.sample_table_offset + n_idx * 4) else {
                continue;
            };
            let Some(samp_off) = rom_ptr_to_offset(samp_ptr, rom.len()) else {
                continue;
            };
            let Some((_fmt, length, loop_start)) = validate_sample_at(rom, samp_off) else {
                continue;
            };
            let pcm_start = samp_off + 16;
            let len_usize = length as usize;
            let Some(raw) = rom.get(pcm_start..pcm_start + len_usize) else {
                continue;
            };
            let pcm_data: Vec<i8> = raw.iter().map(|&b| b as i8).collect();
            let loop_range = if loop_start != 0 && loop_start < length {
                Some((loop_start, length))
            } else {
                None
            };
            let index = samples.len();
            samples.push(FullFatSampleEntry {
                index,
                bank_index: bank_idx,
                rom_offset: samp_off,
                sample_start_offset: pcm_start,
                size: len_usize,
                sample_rate: FULLFAT_BASE_SAMPLE_RATE_HZ,
                root_key: FULLFAT_ROOT_KEY,
                loop_range,
                pcm_data,
            });
        }
    }

    Some(FullFatScanSummary {
        revision: det.revision,
        mixer_offset: det.mixer_offset,
        freq_table_offset: det.freq_table_offset,
        bank_count: banks.len(),
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

#[inline]
fn read_u32_le(data: &[u8], off: usize) -> Option<u32> {
    let b = data.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

#[inline]
fn rom_ptr_to_offset(ptr: u32, rom_len: usize) -> Option<usize> {
    if ptr < GBA_ROM_BASE {
        return None;
    }
    let off = (ptr - GBA_ROM_BASE) as usize;
    if off < rom_len { Some(off) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;

    fn build_synthetic_rom(rev_b: bool) -> Vec<u8> {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"FULLFATTEST\0");

        // Place MixChannel + FinalizeBuffer
        rom[0x200..0x200 + MIX_LOOP_SIG.len()].copy_from_slice(&MIX_LOOP_SIG);
        let fin = if rev_b {
            &FINALIZE_LOOP_REVB
        } else {
            &FINALIZE_LOOP_REVA
        };
        rom[0x240..0x240 + fin.len()].copy_from_slice(fin);

        // Place 128-entry frequency table @ 0x400 with A4 (note 69) = 28_835_840
        rom[0x400..0x400 + FREQ_TABLE_PREFIX.len()].copy_from_slice(&FREQ_TABLE_PREFIX);
        rom[0x400 + 69 * 4..0x400 + 69 * 4 + 4].copy_from_slice(&28_835_840u32.to_le_bytes());

        // Place 1 bank @ 0x800: 1 song, 2 samples
        let bank_off = 0x800usize;
        let bank_addr = GBA_ROM_BASE + bank_off as u32;
        rom[bank_off..bank_off + 4].copy_from_slice(&1u32.to_le_bytes());
        rom[bank_off + 4..bank_off + 8].copy_from_slice(&2u32.to_le_bytes());
        rom[bank_off + 8..bank_off + 12].copy_from_slice(&(bank_addr + 16).to_le_bytes());
        rom[bank_off + 12..bank_off + 16].copy_from_slice(&(bank_addr + 24).to_le_bytes());
        // Song 0: start @ 0x900, loop = 0
        rom[bank_off + 16..bank_off + 20].copy_from_slice(&(GBA_ROM_BASE + 0x900).to_le_bytes());
        // Sample ptrs: 0xA00, 0xA60
        rom[bank_off + 24..bank_off + 28].copy_from_slice(&(GBA_ROM_BASE + 0xA00).to_le_bytes());
        rom[bank_off + 28..bank_off + 32].copy_from_slice(&(GBA_ROM_BASE + 0xA60).to_le_bytes());

        // Sample 0 @ 0xA00: PCM8, len=64, loop=0
        rom[0xA00..0xA04].copy_from_slice(&1u32.to_le_bytes());
        rom[0xA04..0xA08].copy_from_slice(&64u32.to_le_bytes());
        // Sample 1 @ 0xA60: PCM8, len=64, loop=16
        rom[0xA60..0xA64].copy_from_slice(&1u32.to_le_bytes());
        rom[0xA64..0xA68].copy_from_slice(&64u32.to_le_bytes());
        rom[0xA68..0xA6C].copy_from_slice(&16u32.to_le_bytes());

        rom
    }

    #[test]
    fn detects_and_exports_fullfat_reva_and_revb() {
        let rom_a = build_synthetic_rom(false);
        assert_eq!(detect_fullfat_engine(&rom_a), Some(FullFatRevision::RevA));

        let rom_b = build_synthetic_rom(true);
        assert_eq!(detect_fullfat_engine(&rom_b), Some(FullFatRevision::RevB));

        let summary = scan_fullfat_engine(&rom_b).expect("valid summary");
        assert_eq!(summary.bank_count, 1);
        assert_eq!(summary.songs.len(), 1);
        assert_eq!(summary.samples.len(), 2);
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
