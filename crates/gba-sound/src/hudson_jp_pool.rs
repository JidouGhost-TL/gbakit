//! Hudson / Red / Rocket Games JP DirectSound audio engine detection,
//! 7-section archive & standalone bank scanning, WAV16 export, and SoundFont 2.04 compilation.
//!
//! 6-channel hybrid 4x DMG PSG + 2x Hardware DirectSound A/B streaming signed
//! 8-bit PCM directly from ROM via `DMA1/TM0CNT_L` -> `FIFO_A` and `DMA2/TM1CNT_L`
//! -> `FIFO_B` (`DMAxCNT_H = 0xB640`, 16,780,000 Hz clock) after a manual 32-byte
//! byte-wise FIFO pre-fill.

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

const LITERAL_POOL_A: [u8; 28] = [
    0xa0, 0x00, 0x00, 0x04, 0xa1, 0x00, 0x00, 0x04, 0xa2, 0x00, 0x00, 0x04, 0xa3, 0x00, 0x00, 0x04,
    0xbc, 0x00, 0x00, 0x04, 0xc0, 0x00, 0x00, 0x04, 0x40, 0xb6, 0x00, 0x00,
];

const LITERAL_POOL_B: [u8; 32] = [
    0xa4, 0x00, 0x00, 0x04, 0xa5, 0x00, 0x00, 0x04, 0xa6, 0x00, 0x00, 0x04, 0xa7, 0x00, 0x00, 0x04,
    0xc8, 0x00, 0x00, 0x04, 0xcc, 0x00, 0x00, 0x04, 0x40, 0xb6, 0x00, 0x00, 0xe0, 0x0a, 0x00, 0x01,
];

const THUMB_PROLOGUE: [u8; 16] = [
    0xf0, 0xb5, 0x47, 0x46, 0x80, 0xb4, 0x03, 0x1c, 0x00, 0x29, 0x02, 0xd0, 0x01, 0x29, 0x41, 0xd0,
];

const POOL_B_DELTA: usize = 0xa0;
const PROLOGUE_TO_POOL_A: usize = 0x78;

const MAX_ARCHIVES: usize = 8;
const MAX_STANDALONE_BANKS: usize = 8;
const MAX_SAMPLES_PER_BANK: usize = 128;
const TRACK_COUNT: usize = 6;
const SONG_HEADER_SIZE: usize = 14;
const PCM_HEADER_SIZE: usize = 6;
const MIN_PCM_RECORD_SIZE: usize = 70;
const MAX_STANDALONE_PCM_RECORD_SIZE: usize = 0x40000;
const MAX_ARCHIVE_PCM_RECORD_SIZE: usize = 0x400000;
const MIN_SAMPLE_RATE_HZ: u16 = 2_000;
const MAX_ARCHIVE_SAMPLE_RATE_HZ: u16 = 48_000;
const MAX_STANDALONE_SAMPLE_RATE_HZ: u16 = 24_000;

/// Storage layout discovered in a Hudson / Red / Rocket Games JP ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HudsonJpLayout {
    /// Standard 7-section chunked audio archive(s).
    Chunked7Section,
    /// Standalone PCM and song/SFX linked-chain banks.
    StandaloneBanks,
}

/// Extracted Hudson / Red / Rocket Games JP signed 8-bit PCM sample entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HudsonJpSampleEntry {
    /// Sequential sample index across all banks.
    pub index: usize,
    /// Archive index (or `0` for standalone banks).
    pub archive_index: usize,
    /// Bank index (`0` = BGM PCM, `1` = SFX PCM, or standalone bank index).
    pub bank_index: usize,
    /// Absolute ROM file offset of the 6-byte record header.
    pub rom_offset: usize,
    /// Absolute ROM file offset of the signed 8-bit PCM payload (`rom_offset + 6`).
    pub sample_start_offset: usize,
    /// Sample payload length in bytes.
    pub size: usize,
    /// Native playback sample rate in Hz.
    pub sample_rate: u32,
    /// MIDI root key (`60` = Middle C).
    pub root_key: u8,
    /// Extracted signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Extracted 6-track BGM or SFX sequence descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HudsonJpSongEntry {
    /// Sequence index within its bank.
    pub index: usize,
    /// Archive or standalone bank index.
    pub archive_index: usize,
    /// `true` for SFX, `false` for BGM.
    pub is_sfx: bool,
    /// Absolute ROM file offset of the song header.
    pub rom_offset: usize,
    /// Record size in bytes.
    pub record_size: usize,
}

/// Complete scan summary for a Hudson / Red / Rocket Games JP ROM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HudsonJpScanSummary {
    /// ROM file offset of the Thumb DirectSound FIFO pre-fill routine.
    pub driver_offset: usize,
    /// Discovered storage layout.
    pub layout: HudsonJpLayout,
    /// Number of discovered archives or standalone PCM banks.
    pub archive_count: usize,
    /// All extracted PCM samples.
    pub samples: Vec<HudsonJpSampleEntry>,
    /// All extracted BGM and SFX sequence headers.
    pub songs: Vec<HudsonJpSongEntry>,
}

/// Locate the ROM offset of the Hudson / Red / Rocket Games JP DirectSound FIFO pre-fill routine.
#[must_use]
pub fn find_hudson_jp_driver_offset(rom: &[u8]) -> Option<usize> {
    let min_len = PROLOGUE_TO_POOL_A + POOL_B_DELTA + LITERAL_POOL_B.len();
    if rom.len() < min_len {
        return None;
    }

    let max_pool_a = rom.len() - (POOL_B_DELTA + LITERAL_POOL_B.len());
    let mut off = PROLOGUE_TO_POOL_A;
    if !off.is_multiple_of(4) {
        off += 4 - (off % 4);
    }

    while off <= max_pool_a {
        if let Some(window_a) = rom.get(off..off + LITERAL_POOL_A.len()) {
            if window_a == LITERAL_POOL_A {
                let pool_b_off = off + POOL_B_DELTA;
                let prologue_off = off - PROLOGUE_TO_POOL_A;
                let pool_b_match = rom
                    .get(pool_b_off..pool_b_off + LITERAL_POOL_B.len())
                    .is_some_and(|w| w == LITERAL_POOL_B);
                let prologue_match = rom
                    .get(prologue_off..prologue_off + THUMB_PROLOGUE.len())
                    .is_some_and(|w| w == THUMB_PROLOGUE);
                if pool_b_match && prologue_match {
                    return Some(prologue_off);
                }
            }
        }
        off += 4;
    }

    None
}

/// Detect whether `rom` uses the Hudson / Red / Rocket Games JP DirectSound audio engine.
#[must_use]
pub fn detect_hudson_jp_engine(rom: &[u8]) -> bool {
    find_hudson_jp_driver_offset(rom).is_some()
}

#[derive(Debug, Clone, Copy, Default)]
struct SectionSpan {
    offset: usize,
    size: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct ArchiveDesc {
    bgm_section: SectionSpan,
    sfx_section: SectionSpan,
    pcm_bank0: SectionSpan,
    pcm_bank1: SectionSpan,
}

#[derive(Debug, Clone, Copy, Default)]
struct StandaloneDesc {
    offset: usize,
    size: usize,
}

/// Scan a Hudson / Red / Rocket Games JP ROM for all PCM samples and sequences.
pub fn scan_hudson_jp_engine(rom: &[u8]) -> Option<HudsonJpScanSummary> {
    let driver_offset = find_hudson_jp_driver_offset(rom)?;

    let archives = scan_chunked_archives(rom);
    if !archives.is_empty() {
        let mut samples = Vec::new();
        let mut songs = Vec::new();
        for (arch_idx, arch) in archives.iter().enumerate() {
            collect_pcm_bank(rom, arch_idx, 0, arch.pcm_bank0, &mut samples);
            collect_pcm_bank(rom, arch_idx, 1, arch.pcm_bank1, &mut samples);
            collect_song_bank(rom, arch_idx, false, arch.bgm_section, &mut songs);
            collect_song_bank(rom, arch_idx, true, arch.sfx_section, &mut songs);
        }
        return Some(HudsonJpScanSummary {
            driver_offset,
            layout: HudsonJpLayout::Chunked7Section,
            archive_count: archives.len(),
            samples,
            songs,
        });
    }

    let pcm_banks = scan_standalone_pcm_banks(rom);
    let song_banks = scan_standalone_song_banks(rom);
    if pcm_banks.is_empty() && song_banks.is_empty() {
        return None;
    }

    let mut samples = Vec::new();
    for (bank_idx, b) in pcm_banks.iter().enumerate() {
        collect_pcm_bank(
            rom,
            0,
            bank_idx,
            SectionSpan {
                offset: b.offset,
                size: b.size,
            },
            &mut samples,
        );
    }

    let mut songs = Vec::new();
    for (bank_idx, b) in song_banks.iter().enumerate() {
        collect_song_bank(
            rom,
            bank_idx,
            bank_idx != 0,
            SectionSpan {
                offset: b.offset,
                size: b.size,
            },
            &mut songs,
        );
    }

    Some(HudsonJpScanSummary {
        driver_offset,
        layout: HudsonJpLayout::StandaloneBanks,
        archive_count: pcm_banks.len(),
        samples,
        songs,
    })
}

/// Scan a GBA ROM for Hudson / Red / Rocket Games JP signed 8-bit PCM samples.
pub fn scan_hudson_jp_soundpool(rom: &[u8]) -> Vec<HudsonJpSampleEntry> {
    scan_hudson_jp_engine(rom)
        .map(|s| s.samples)
        .unwrap_or_default()
}

/// Extract all Hudson / Red / Rocket Games JP samples as 16-bit mono RIFF WAV files.
pub fn export_hudson_jp_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_hudson_jp_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_hudson_jp_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!("hudson_jp_sample_{:03}_{:06X}.wav", s.index, s.rom_offset);
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Hudson / Red / Rocket Games JP samples into a SoundFont 2.04 (`.sf2`).
pub fn compile_hudson_jp_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_hudson_jp_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_hudson_jp_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Hudson JP DirectSound)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            RawSoundFontSample {
                name: format!("HUD_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range: None,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(
        &bank_title,
        "Hudson / Red / Rocket Games JP DirectSound Engine",
        &raw_samples,
    )
}

fn collect_pcm_bank(
    rom: &[u8],
    archive_index: usize,
    bank_index: usize,
    span: SectionSpan,
    out: &mut Vec<HudsonJpSampleEntry>,
) {
    let mut pos = span.offset;
    let end = span.offset.saturating_add(span.size).min(rom.len());
    let mut bank_count = 0usize;
    while bank_count < MAX_SAMPLES_PER_BANK && pos.saturating_add(4) <= end {
        let Some(record_size) = read_u32_le(rom, pos) else {
            break;
        };
        if record_size == 0 {
            break;
        }
        let Ok(rec_len) = usize::try_from(record_size) else {
            break;
        };
        if rec_len < PCM_HEADER_SIZE || pos.checked_add(rec_len).is_none_or(|e| e > end) {
            break;
        }
        let Some(rate) = read_u16_le(rom, pos + 4) else {
            break;
        };
        let Some(raw) = rom.get(pos + PCM_HEADER_SIZE..pos + rec_len) else {
            break;
        };
        let pcm_data: Vec<i8> = raw.iter().map(|&b| b as i8).collect();
        let idx = out.len();
        out.push(HudsonJpSampleEntry {
            index: idx,
            archive_index,
            bank_index,
            rom_offset: pos,
            sample_start_offset: pos + PCM_HEADER_SIZE,
            size: pcm_data.len(),
            sample_rate: u32::from(rate),
            root_key: 60,
            pcm_data,
        });
        pos += rec_len;
        bank_count += 1;
    }
}

fn collect_song_bank(
    rom: &[u8],
    archive_index: usize,
    is_sfx: bool,
    span: SectionSpan,
    out: &mut Vec<HudsonJpSongEntry>,
) {
    let mut pos = span.offset;
    let end = span.offset.saturating_add(span.size).min(rom.len());
    let mut seq_idx = 0usize;
    while pos.saturating_add(2) <= end {
        let Some(record_size) = read_u16_le(rom, pos) else {
            break;
        };
        if record_size == 0 {
            break;
        }
        let rec_len = usize::from(record_size);
        let step = rec_len & !1usize;
        if rec_len < SONG_HEADER_SIZE || step == 0 || pos.checked_add(step).is_none_or(|e| e > end)
        {
            break;
        }
        out.push(HudsonJpSongEntry {
            index: seq_idx,
            archive_index,
            is_sfx,
            rom_offset: pos,
            record_size: rec_len,
        });
        pos += step;
        seq_idx += 1;
    }
}

fn read_u16_le(buf: &[u8], off: usize) -> Option<u16> {
    let bytes = buf.get(off..off.checked_add(2)?)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32_le(buf: &[u8], off: usize) -> Option<u32> {
    let bytes = buf.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn validate_song_section(rom: &[u8], off: usize, size: usize) -> Option<usize> {
    let end = off.checked_add(size)?;
    if end > rom.len() {
        return None;
    }
    let mut pos = off;
    let mut count = 0usize;
    while pos + 2 <= end {
        let rsz = read_u16_le(rom, pos)?;
        if rsz == 0 {
            break;
        }
        let rec_len = usize::from(rsz);
        let step = rec_len & !1usize;
        if rec_len < SONG_HEADER_SIZE || step == 0 || pos.checked_add(step)? > end {
            return None;
        }
        for i in 0..TRACK_COUNT {
            let trk = usize::from(read_u16_le(rom, pos + 2 + i * 2)?);
            if trk < SONG_HEADER_SIZE || trk >= rec_len {
                return None;
            }
        }
        pos += step;
        count += 1;
    }
    Some(count)
}

fn validate_pcm_section(rom: &[u8], off: usize, size: usize) -> Option<usize> {
    let end = off.checked_add(size)?;
    if end > rom.len() {
        return None;
    }
    let mut pos = off;
    let mut count = 0usize;
    while pos + 4 <= end && count < MAX_SAMPLES_PER_BANK {
        let rsz = read_u32_le(rom, pos)?;
        if rsz == 0 {
            break;
        }
        let rec_len = usize::try_from(rsz).ok()?;
        if !(MIN_PCM_RECORD_SIZE..=MAX_ARCHIVE_PCM_RECORD_SIZE).contains(&rec_len)
            || pos.checked_add(rec_len)? > end
        {
            return None;
        }
        let rate = read_u16_le(rom, pos + 4)?;
        if !(MIN_SAMPLE_RATE_HZ..=MAX_ARCHIVE_SAMPLE_RATE_HZ).contains(&rate) {
            return None;
        }
        pos += rec_len;
        count += 1;
    }
    Some(count)
}

fn scan_chunked_archives(rom: &[u8]) -> Vec<ArchiveDesc> {
    let n = rom.len();
    let mut out = Vec::new();
    let mut off = 0usize;

    while off.saturating_add(32) <= n && out.len() < MAX_ARCHIVES {
        if let Some(sz0_raw) = read_u32_le(rom, off) {
            let sz0_aligned = (sz0_raw & !3u32) as usize;
            if sz0_raw <= 0x200000
                && (16..=0x200000).contains(&sz0_aligned)
                && off.saturating_add(4 + sz0_aligned + 24) <= n
            {
                if let Some(first_rsz) = read_u16_le(rom, off + 4) {
                    let first_len = usize::from(first_rsz);
                    if first_len >= SONG_HEADER_SIZE && first_len <= sz0_aligned {
                        let mut quick_ok = true;
                        for i in 0..TRACK_COUNT {
                            let trk = read_u16_le(rom, off + 6 + i * 2)
                                .map(usize::from)
                                .unwrap_or(0);
                            if trk < SONG_HEADER_SIZE || trk >= first_len {
                                quick_ok = false;
                                break;
                            }
                        }

                        if quick_ok {
                            if let Some((desc, next_off)) = try_parse_7section_at(rom, off) {
                                out.push(desc);
                                off = next_off;
                                continue;
                            }
                        }
                    }
                }
            }
        }
        off += 4;
    }

    out
}

fn try_parse_7section_at(rom: &[u8], start: usize) -> Option<(ArchiveDesc, usize)> {
    let n = rom.len();
    let mut pos = start;
    let mut spans = [SectionSpan::default(); 7];

    for span in &mut spans {
        if pos.checked_add(4)? > n {
            return None;
        }
        let raw_sz = read_u32_le(rom, pos)?;
        if raw_sz > 0x800000 {
            return None;
        }
        let aligned_sz = (raw_sz & !3u32) as usize;
        let payload_start = pos.checked_add(4)?;
        let payload_end = payload_start.checked_add(aligned_sz)?;
        if payload_end > n {
            return None;
        }
        *span = SectionSpan {
            offset: payload_start,
            size: aligned_sz,
        };
        pos = payload_end;
    }

    let bgm_count = validate_song_section(rom, spans[0].offset, spans[0].size)?;
    let sfx_count = validate_song_section(rom, spans[1].offset, spans[1].size)?;
    let pcm0_count = validate_pcm_section(rom, spans[5].offset, spans[5].size)?;
    let pcm1_count = validate_pcm_section(rom, spans[6].offset, spans[6].size)?;

    if bgm_count + sfx_count == 0 || pcm0_count + pcm1_count == 0 {
        return None;
    }

    Some((
        ArchiveDesc {
            bgm_section: spans[0],
            sfx_section: spans[1],
            pcm_bank0: spans[5],
            pcm_bank1: spans[6],
        },
        pos,
    ))
}

fn is_plausible_standalone_rate(rate: u16) -> bool {
    (MIN_SAMPLE_RATE_HZ..=MAX_STANDALONE_SAMPLE_RATE_HZ).contains(&rate)
        && (rate.is_multiple_of(25)
            || matches!(
                rate,
                5_512
                    | 5_564
                    | 5_723
                    | 7_418
                    | 8_192
                    | 10_512
                    | 11_025
                    | 13_379
                    | 15_768
                    | 16_384
                    | 21_024
                    | 22_050
            ))
}

fn scan_standalone_pcm_banks(rom: &[u8]) -> Vec<StandaloneDesc> {
    let n = rom.len();
    let mut out = Vec::new();
    let mut off = 0usize;

    while off.saturating_add(16) <= n && out.len() < MAX_STANDALONE_BANKS {
        if let (Some(rsz), Some(rate)) = (read_u32_le(rom, off), read_u16_le(rom, off + 4)) {
            let rec_len = rsz as usize;
            if (MIN_PCM_RECORD_SIZE..=MAX_STANDALONE_PCM_RECORD_SIZE).contains(&rec_len)
                && is_plausible_standalone_rate(rate)
                && off.saturating_add(rec_len + 4) <= n
            {
                let mut pos = off;
                let mut rec_cnt = 0usize;
                let mut valid = true;

                while pos.saturating_add(4) <= n && rec_cnt < MAX_SAMPLES_PER_BANK {
                    let Some(s) = read_u32_le(rom, pos).map(|v| v as usize) else {
                        valid = false;
                        break;
                    };
                    if s == 0 {
                        break;
                    }
                    if !(MIN_PCM_RECORD_SIZE..=MAX_STANDALONE_PCM_RECORD_SIZE).contains(&s)
                        || pos.saturating_add(s) > n
                    {
                        valid = false;
                        break;
                    }
                    let Some(r) = read_u16_le(rom, pos + 4) else {
                        valid = false;
                        break;
                    };
                    if !(MIN_SAMPLE_RATE_HZ..=MAX_STANDALONE_SAMPLE_RATE_HZ).contains(&r) {
                        valid = false;
                        break;
                    }
                    pos += s;
                    rec_cnt += 1;
                }

                let terminated = read_u32_le(rom, pos) == Some(0);
                if valid && terminated && rec_cnt >= 4 {
                    let bank_end = pos + 4;
                    out.push(StandaloneDesc {
                        offset: off,
                        size: bank_end - off,
                    });
                    off = (bank_end + 3) & !3usize;
                    continue;
                }
            }
        }
        off += 4;
    }

    out
}

fn scan_standalone_song_banks(rom: &[u8]) -> Vec<StandaloneDesc> {
    let n = rom.len();
    let mut out = Vec::new();
    let mut off = 0usize;

    while off.saturating_add(20) <= n && out.len() < MAX_STANDALONE_BANKS {
        if let Some(rsz) = read_u16_le(rom, off) {
            let rec_len = usize::from(rsz);
            if (SONG_HEADER_SIZE..=0x8000).contains(&rec_len)
                && (rec_len & 1) == 0
                && off.saturating_add(rec_len + 2) <= n
            {
                let mut first_offs = [0u16; TRACK_COUNT];
                let mut ok_first = true;
                for (i, slot) in first_offs.iter_mut().enumerate() {
                    match read_u16_le(rom, off + 2 + i * 2) {
                        Some(v)
                            if usize::from(v) >= SONG_HEADER_SIZE && usize::from(v) < rec_len =>
                        {
                            *slot = v;
                        }
                        _ => {
                            ok_first = false;
                            break;
                        }
                    }
                }
                let monotonic = first_offs[0] == SONG_HEADER_SIZE as u16
                    && first_offs.windows(2).all(|w| w[0] <= w[1]);

                if ok_first && monotonic {
                    let mut pos = off;
                    let mut rec_cnt = 0usize;
                    let mut valid = true;

                    while pos.saturating_add(2) <= n && rec_cnt < 256 {
                        let Some(s) = read_u16_le(rom, pos).map(usize::from) else {
                            valid = false;
                            break;
                        };
                        if s == 0 {
                            break;
                        }
                        let step = s & !1usize;
                        if !(SONG_HEADER_SIZE..=0x8000).contains(&s)
                            || step == 0
                            || pos.saturating_add(step) > n
                        {
                            valid = false;
                            break;
                        }
                        for i in 0..TRACK_COUNT {
                            let trk = read_u16_le(rom, pos + 2 + i * 2)
                                .map(usize::from)
                                .unwrap_or(0);
                            if trk < SONG_HEADER_SIZE || trk >= s {
                                valid = false;
                                break;
                            }
                        }
                        if !valid {
                            break;
                        }
                        pos += step;
                        rec_cnt += 1;
                    }

                    let terminated = read_u16_le(rom, pos) == Some(0);
                    if valid && terminated && rec_cnt >= 8 {
                        let bank_end = pos + 2;
                        out.push(StandaloneDesc {
                            offset: off,
                            size: bank_end - off,
                        });
                        off = (bank_end + 3) & !3usize;
                        continue;
                    }
                }
            }
        }
        off += 4;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;

    fn build_synthetic_rom_with_archive() -> Vec<u8> {
        let mut rom = vec![0u8; 0x600];
        rom[0xA0..0xAC].copy_from_slice(b"HUDSONJPTST\0");

        let drv_off = 0x100usize;
        rom[drv_off..drv_off + THUMB_PROLOGUE.len()].copy_from_slice(&THUMB_PROLOGUE);
        rom[drv_off + PROLOGUE_TO_POOL_A..drv_off + PROLOGUE_TO_POOL_A + LITERAL_POOL_A.len()]
            .copy_from_slice(&LITERAL_POOL_A);
        let pool_b_off = drv_off + PROLOGUE_TO_POOL_A + POOL_B_DELTA;
        rom[pool_b_off..pool_b_off + LITERAL_POOL_B.len()].copy_from_slice(&LITERAL_POOL_B);

        let mut pos = 0x300usize;
        // Section 0: 1 BGM
        rom[pos..pos + 4].copy_from_slice(&28u32.to_le_bytes());
        pos += 4;
        rom[pos..pos + 2].copy_from_slice(&20u16.to_le_bytes());
        for ch in 0..6u16 {
            let o = 14 + ch;
            rom[pos + 2 + usize::from(ch) * 2..pos + 4 + usize::from(ch) * 2]
                .copy_from_slice(&o.to_le_bytes());
        }
        pos += 28;

        // Section 1: 1 SFX
        rom[pos..pos + 4].copy_from_slice(&28u32.to_le_bytes());
        pos += 4;
        rom[pos..pos + 2].copy_from_slice(&20u16.to_le_bytes());
        for ch in 0..6u16 {
            let o = 14 + ch;
            rom[pos + 2 + usize::from(ch) * 2..pos + 4 + usize::from(ch) * 2]
                .copy_from_slice(&o.to_le_bytes());
        }
        pos += 28;

        // Sections 2..4 placeholders
        for _ in 0..3 {
            rom[pos..pos + 4].copy_from_slice(&4u32.to_le_bytes());
            pos += 8;
        }

        // Section 5: PCM Bank 0 (1 sample of 70 bytes + 4-byte zero terminator = 76)
        rom[pos..pos + 4].copy_from_slice(&76u32.to_le_bytes());
        pos += 4;
        rom[pos..pos + 4].copy_from_slice(&70u32.to_le_bytes());
        rom[pos + 4..pos + 6].copy_from_slice(&11025u16.to_le_bytes());
        for i in 0..64usize {
            rom[pos + 6 + i] = (i as u8).wrapping_mul(4);
        }
        pos += 76;

        // Section 6: empty PCM Bank 1
        rom[pos..pos + 4].copy_from_slice(&4u32.to_le_bytes());
        rom
    }

    #[test]
    fn detects_and_exports_hudson_jp_archive() {
        let rom = build_synthetic_rom_with_archive();
        assert!(detect_hudson_jp_engine(&rom));
        assert_eq!(find_hudson_jp_driver_offset(&rom), Some(0x100));

        let summary = scan_hudson_jp_engine(&rom).expect("valid summary");
        assert_eq!(summary.layout, HudsonJpLayout::Chunked7Section);
        assert_eq!(summary.samples.len(), 1);
        assert_eq!(summary.songs.len(), 2);
        assert_eq!(summary.samples[0].sample_rate, 11_025);
        assert_eq!(summary.samples[0].size, 64);

        let wavs = export_hudson_jp_samples_wav16(&rom).expect("wav export");
        assert_eq!(wavs.len(), 1);
        assert_eq!(&wavs[0].2[0..4], b"RIFF");

        let sf2 = compile_hudson_jp_soundfont(&rom, Some("Synthetic Hudson JP")).expect("sf2");
        let val = validate_soundfont(&sf2.sf2_bytes).expect("valid sf2");
        assert_eq!(val.sample_count, 1);
    }

    #[test]
    fn rejects_empty_and_malformed() {
        assert!(!detect_hudson_jp_engine(&[]));
        assert!(scan_hudson_jp_soundpool(&[0u8; 512]).is_empty());
    }
}
