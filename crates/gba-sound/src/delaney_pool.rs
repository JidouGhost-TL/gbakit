//! Michael Delaney / Allister Brimble GBA Stereo DirectSound + PSG (`0xB6600004`)
//! audio engine detection, sample/song pool scanning, WAV16 export, and SoundFont 2.04 compilation.
//!
//! 12-channel hybrid 4x DMG PSG + 8x DirectSound signed 8-bit PCM (4 SWAR-mixed
//! into FIFO A, 4 into FIFO B) using 5-slot IWRAM ring buffers (`0xE0` or `0xB0`
//! stride) driven by `DMA1CNT`/`DMA2CNT = 0xB660_0004` at 10,512 Hz or 13,379 Hz.

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

const GBA_ROM_BASE: u32 = 0x0800_0000;
const GBA_IWRAM_BASE: u32 = 0x0300_0000;
const GBA_IWRAM_END: u32 = 0x0300_8000;
const GBA_CPU_CLOCK_HZ: u32 = 16_777_216;

const MMIO_DMA1SAD: u32 = 0x0400_00BC;
const DMA_CTRL_FIFO_STEREO: u32 = 0xB660_0004;
const MMIO_FIFO_A: u32 = 0x0400_00A0;
const MMIO_FIFO_B: u32 = 0x0400_00A4;
const MMIO_SOUNDCNT_L: u32 = 0x0400_0080;
const MMIO_SOUND1CNT_L: u32 = 0x0400_0060;
const PSG_MASK_A: u32 = 0x29FE_07FF;
const PSG_MASK_B: u32 = 0x28FF_07FF;

const DRIVER_WINDOW_BYTES: usize = 0x2000;
const DEFAULT_SAMPLE_RATE_HZ: u32 = 10_512;
const MAX_SAMPLE_ENTRIES: usize = 256;
const MAX_SONG_ENTRIES: usize = 256;
const SONG_ENTRY_SIZE: usize = 0x3C;
const SAMPLE_ENTRY_SIZE: usize = 0x10;

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

#[inline]
fn read_u16_le(data: &[u8], off: usize) -> Option<u16> {
    let bytes = data.get(off..off.checked_add(2)?)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

#[inline]
fn read_u32_le(data: &[u8], off: usize) -> Option<u32> {
    let bytes = data.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[inline]
fn is_iwram_ptr(addr: u32) -> bool {
    (GBA_IWRAM_BASE..GBA_IWRAM_END).contains(&addr)
}

#[inline]
fn rom_ptr_to_offset(ptr: u32, rom_len: usize) -> Option<usize> {
    if ptr < GBA_ROM_BASE.saturating_add(0xC0) {
        return None;
    }
    let off = (ptr - GBA_ROM_BASE) as usize;
    if off < rom_len { Some(off) } else { None }
}

fn verify_driver_window(rom: &[u8], anchor_off: usize) -> bool {
    let win_end = anchor_off.saturating_add(DRIVER_WINDOW_BYTES).min(rom.len()) & !3;
    let mut has_fifo_a = false;
    let mut has_fifo_b = false;
    let mut has_song_mmio = false;
    let mut has_psg_mask = false;

    let mut cur = anchor_off.saturating_add(8);
    while cur.saturating_add(4) <= win_end {
        if let Some(w) = read_u32_le(rom, cur) {
            if w == MMIO_FIFO_A {
                has_fifo_a = true;
            } else if w == MMIO_FIFO_B {
                has_fifo_b = true;
            } else if w == MMIO_SOUNDCNT_L
                && cur.saturating_add(8) <= win_end
                && read_u32_le(rom, cur + 4) == Some(MMIO_SOUND1CNT_L)
            {
                has_song_mmio = true;
            } else if w == PSG_MASK_A || w == PSG_MASK_B {
                has_psg_mask = true;
            }
        }
        cur += 4;
    }

    has_fifo_a && has_fifo_b && has_song_mmio && has_psg_mask
}

/// Locate the ROM file offset of the verified `0x0400_00BC` / `0xB660_0004` DMA literal pool pair.
#[must_use]
pub fn find_delaney_driver_anchor(rom: &[u8]) -> Option<usize> {
    if rom.len() < 32 {
        return None;
    }
    let aligned_end = rom.len() & !3;
    let mut off = 0usize;
    while off.saturating_add(20) <= aligned_end {
        if read_u32_le(rom, off) == Some(MMIO_DMA1SAD)
            && read_u32_le(rom, off + 4) == Some(DMA_CTRL_FIFO_STEREO)
        {
            let idx_a = read_u32_le(rom, off + 8).unwrap_or(0);
            let idx_b = read_u32_le(rom, off + 12).unwrap_or(0);
            let buf_0 = read_u32_le(rom, off + 16).unwrap_or(0);

            if is_iwram_ptr(idx_a)
                && is_iwram_ptr(idx_b)
                && idx_b == idx_a.wrapping_add(2)
                && is_iwram_ptr(buf_0)
                && verify_driver_window(rom, off)
            {
                return Some(off);
            }
        }
        off += 4;
    }
    None
}

/// Detect whether `rom` contains the Michael Delaney / Allister Brimble Stereo DirectSound + PSG engine.
#[must_use]
pub fn detect_delaney_engine(rom: &[u8]) -> bool {
    find_delaney_driver_anchor(rom).is_some()
}

fn sample_rate_from_timer_word(timer_word: u32) -> u32 {
    let reload = timer_word & 0xFFFF;
    let ticks = 65_536u32.saturating_sub(reload);
    if ticks == 0 {
        return DEFAULT_SAMPLE_RATE_HZ;
    }
    let rate = (GBA_CPU_CLOCK_HZ + (ticks / 2)) / ticks;
    if (4_000..=65_536).contains(&rate) {
        rate
    } else {
        DEFAULT_SAMPLE_RATE_HZ
    }
}

fn find_timer_word(rom: &[u8], anchor_off: usize) -> u32 {
    let win_end = anchor_off.saturating_add(DRIVER_WINDOW_BYTES).min(rom.len()) & !3;
    let mut cur = anchor_off.saturating_add(8);
    while cur.saturating_add(4) <= win_end {
        if read_u32_le(rom, cur) == Some(MMIO_FIFO_A) {
            if let Some(prev) = cur.checked_sub(4).and_then(|o| read_u32_le(rom, o)) {
                if (prev >> 16) == 0x0080 {
                    return prev;
                }
            }
        }
        cur += 4;
    }
    0x0080_F9C4
}

fn find_song_table_addr(rom: &[u8], anchor_off: usize) -> Option<u32> {
    let win_end = anchor_off.saturating_add(DRIVER_WINDOW_BYTES).min(rom.len()) & !3;
    let mut cur = anchor_off.saturating_add(8);
    while cur.saturating_add(8) <= win_end {
        if read_u32_le(rom, cur) == Some(MMIO_SOUNDCNT_L)
            && read_u32_le(rom, cur + 4) == Some(MMIO_SOUND1CNT_L)
        {
            let candidate = cur.checked_sub(4).and_then(|o| read_u32_le(rom, o))?;
            if rom_ptr_to_offset(candidate, rom.len()).is_some() {
                return Some(candidate);
            }
        }
        cur += 4;
    }
    None
}

fn is_valid_sample_entry(rom: &[u8], off: usize) -> Option<(u32, u32, u32)> {
    let start = read_u32_le(rom, off)?;
    let end = read_u32_le(rom, off.checked_add(4)?)?;
    let loop_ptr = read_u32_le(rom, off.checked_add(8)?)?;
    let reserved = read_u32_le(rom, off.checked_add(12)?)?;
    if reserved != 0 {
        return None;
    }
    let rom_limit = GBA_ROM_BASE.saturating_add(u32::try_from(rom.len()).unwrap_or(u32::MAX));
    if start < GBA_ROM_BASE.saturating_add(0xC0) || end <= start || end > rom_limit {
        return None;
    }
    if loop_ptr != 0 && (loop_ptr < start || loop_ptr >= end) {
        return None;
    }
    Some((start, end, loop_ptr))
}

fn is_null_sample_entry(rom: &[u8], off: usize) -> bool {
    read_u32_le(rom, off) == Some(0)
        && read_u32_le(rom, off.saturating_add(4)) == Some(0)
        && read_u32_le(rom, off.saturating_add(8)) == Some(0)
        && read_u32_le(rom, off.saturating_add(12)) == Some(0)
}

fn looks_like_sample_table(rom: &[u8], table_ptr: u32) -> bool {
    let Some(base) = rom_ptr_to_offset(table_ptr, rom.len()) else {
        return false;
    };
    if !is_null_sample_entry(rom, base) {
        return false;
    }
    let mut valid_count = 0usize;
    let mut k = 1usize;
    while k < 32 {
        let Some(off) = base.checked_add(k * SAMPLE_ENTRY_SIZE) else {
            break;
        };
        if off.saturating_add(SAMPLE_ENTRY_SIZE) > rom.len() {
            break;
        }
        if is_valid_sample_entry(rom, off).is_some() {
            valid_count += 1;
        }
        k += 1;
    }
    valid_count >= 1
}

fn find_sample_and_pattern_tables(rom: &[u8], anchor_off: usize) -> (Option<u32>, Option<u32>) {
    let win_end = anchor_off.saturating_add(0x1800).min(rom.len()) & !3;
    let mut slots: [(u32, u16); 64] = [(0, 0); 64];
    let mut used = 0usize;

    let mut cur = anchor_off.saturating_add(8);
    while cur.saturating_add(4) <= win_end {
        if let Some(w) = read_u32_le(rom, cur) {
            if (w & 3) == 0 && rom_ptr_to_offset(w, rom.len()).is_some() {
                let mut found = false;
                for slot in slots.iter_mut().take(used) {
                    if slot.0 == w {
                        slot.1 = slot.1.saturating_add(1);
                        found = true;
                        break;
                    }
                }
                if !found && used < slots.len() {
                    slots[used] = (w, 1);
                    used += 1;
                }
            }
        }
        cur += 4;
    }

    let mut best_sample: Option<(u32, u16)> = None;
    for &(ptr, cnt) in slots.iter().take(used) {
        if cnt >= 4
            && looks_like_sample_table(rom, ptr)
            && best_sample.is_none_or(|(_, bc)| cnt > bc)
        {
            best_sample = Some((ptr, cnt));
        }
    }

    let sample_ptr = best_sample.map(|(p, _)| p);
    let mut best_pattern: Option<(u32, u16)> = None;
    for &(ptr, cnt) in slots.iter().take(used) {
        if Some(ptr) == sample_ptr || cnt < 6 {
            continue;
        }
        if let Some(off) = rom_ptr_to_offset(ptr, rom.len()) {
            let p0 = read_u32_le(rom, off).unwrap_or(0);
            let p1 = read_u32_le(rom, off.saturating_add(4)).unwrap_or(0);
            if rom_ptr_to_offset(p0, rom.len()).is_some()
                && rom_ptr_to_offset(p1, rom.len()).is_some()
                && best_pattern.is_none_or(|(_, bc)| cnt > bc)
            {
                best_pattern = Some((ptr, cnt));
            }
        }
    }

    (sample_ptr, best_pattern.map(|(p, _)| p))
}

fn parse_driver_info(rom: &[u8]) -> Option<DelaneyDriverInfo> {
    if rom.len() < 0xC0 {
        return None;
    }
    let anchor_offset = find_delaney_driver_anchor(rom)?;
    let timer_word = find_timer_word(rom, anchor_offset);
    let sample_rate_hz = sample_rate_from_timer_word(timer_word);

    let buf_0 = read_u32_le(rom, anchor_offset.saturating_add(16)).unwrap_or(0);
    let buf_1 = read_u32_le(rom, anchor_offset.saturating_add(20)).unwrap_or(0);
    let ring_slot_stride = if is_iwram_ptr(buf_0) && is_iwram_ptr(buf_1) && buf_1 > buf_0 {
        buf_1 - buf_0
    } else {
        0xE0
    };

    let (sample_table_addr, pattern_table_addr) =
        find_sample_and_pattern_tables(rom, anchor_offset);
    let sample_table_addr = sample_table_addr?;
    let song_table_addr = find_song_table_addr(rom, anchor_offset);

    Some(DelaneyDriverInfo {
        anchor_offset,
        timer_word,
        sample_rate_hz,
        ring_slot_stride,
        sample_table_addr,
        song_table_addr,
        pattern_table_addr,
    })
}

fn parse_song_entry(rom: &[u8], song_table_off: usize, index: usize) -> Option<DelaneySongEntry> {
    let off = song_table_off.checked_add(index.checked_mul(SONG_ENTRY_SIZE)?)?;
    if off.checked_add(SONG_ENTRY_SIZE)? > rom.len() {
        return None;
    }
    let tempo_ticks = read_u16_le(rom, off)?;
    let channel_mask = read_u16_le(rom, off + 2)?;
    if tempo_ticks == 0 || tempo_ticks > 64 || channel_mask == 0 || (channel_mask & !0x0FFF) != 0 {
        return None;
    }
    for ch in 0..12 {
        let ptr = read_u32_le(rom, off + 4 + ch * 4)?;
        if ((channel_mask >> ch) & 1) != 0 && rom_ptr_to_offset(ptr, rom.len()).is_none() {
            return None;
        }
    }
    let wave_ram_ptr = read_u32_le(rom, off + 0x38)?;
    if ((channel_mask >> 2) & 1) != 0 && rom_ptr_to_offset(wave_ram_ptr, rom.len()).is_none() {
        return None;
    }
    Some(DelaneySongEntry {
        index,
        rom_offset: off,
        tempo_ticks,
        channel_mask,
        active_channels: (channel_mask & 0x0FFF).count_ones(),
    })
}

/// Scan a Michael Delaney / Allister Brimble Stereo ROM for driver info, samples, and songs.
pub fn scan_delaney_engine(rom: &[u8]) -> Option<DelaneyScanSummary> {
    let driver = parse_driver_info(rom)?;
    let sample_table_off = rom_ptr_to_offset(driver.sample_table_addr, rom.len())?;

    let mut samples = Vec::new();
    for k in 0..MAX_SAMPLE_ENTRIES {
        let Some(off) = sample_table_off.checked_add(k * SAMPLE_ENTRY_SIZE) else {
            break;
        };
        if off.saturating_add(SAMPLE_ENTRY_SIZE) > rom.len() {
            break;
        }
        if is_null_sample_entry(rom, off) {
            continue;
        }
        if let Some((start_addr, end_addr, loop_raw)) = is_valid_sample_entry(rom, off) {
            let start = match rom_ptr_to_offset(start_addr, rom.len()) {
                Some(s) => s,
                None => continue,
            };
            let len = end_addr.saturating_sub(start_addr) as usize;
            let Some(raw) = start.checked_add(len).and_then(|end| rom.get(start..end)) else {
                continue;
            };
            let loop_range = if loop_raw >= start_addr && loop_raw < end_addr {
                Some((loop_raw - start_addr, len as u32))
            } else {
                None
            };
            let pcm_data: Vec<i8> = raw.iter().map(|&b| b as i8).collect();
            let index = samples.len();
            samples.push(DelaneySampleEntry {
                index,
                table_index: k as u8,
                rom_offset: start,
                size: len,
                sample_rate: driver.sample_rate_hz,
                root_key: 60,
                loop_range,
                pcm_data,
            });
        } else {
            let start = read_u32_le(rom, off).unwrap_or(0);
            let reserved = read_u32_le(rom, off + 12).unwrap_or(1);
            if reserved == 0 && rom_ptr_to_offset(start, rom.len()).is_some() {
                continue;
            }
            break;
        }
    }

    let mut songs = Vec::new();
    if let Some(song_addr) = driver.song_table_addr {
        if let Some(song_off) = rom_ptr_to_offset(song_addr, rom.len()) {
            for idx in 0..MAX_SONG_ENTRIES {
                match parse_song_entry(rom, song_off, idx) {
                    Some(song) => songs.push(song),
                    None => break,
                }
            }
        }
    }

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
