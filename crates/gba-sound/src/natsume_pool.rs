//! Natsume Hybrid PSG + DirectSound PCM GBA audio engine detection,
//! sample/sequence pool scanning, WAV16 export, and SoundFont 2.04 compilation.
//!
//! Hardware-pitched 6-voice hybrid 4x DMG PSG + 2x DirectSound A/B sequencer
//! (`DMA1CNT`/`DMA2CNT = 0xF640_0004`, 12-TET Q4.12 pitch table + 128-entry
//! fine-tune table, 16-byte sample headers followed by signed 8-bit linear PCM).

#![deny(unsafe_code)]

use std::collections::HashSet;

use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// GBA ROM virtual base address.
const ROM_BASE: u32 = 0x0800_0000;

/// 12-TET Q4.12 semitone pitch multiplier table (12 `u16` entries, 24 bytes)
/// immediately followed by the first 8 entries (16 bytes) of the 128-entry
/// Q4.12 fine-tune table (`0x1000..0x100C`).
const PITCH_AND_FINE_TABLE_PREFIX: [u8; 40] = [
    0x00, 0x10, 0xF3, 0x10, 0xF5, 0x11, 0x06, 0x13, 0x28, 0x14, 0x5B, 0x15, 0xA0, 0x16, 0xF9, 0x17,
    0x65, 0x19, 0xE8, 0x1A, 0x82, 0x1C, 0x34, 0x1E, 0x00, 0x10, 0x01, 0x10, 0x03, 0x10, 0x05, 0x10,
    0x07, 0x10, 0x09, 0x10, 0x0B, 0x10, 0x0C, 0x10,
];

/// Six-word DirectSound hardware MMIO pointer table (`FIFO_A`, `FIFO_B`,
/// `TM0CNT_L`, `TM1CNT_L`, `DMA1CNT_H`, `DMA2CNT_H`).
const MMIO6_TABLE: [u8; 24] = [
    0xA0, 0x00, 0x00, 0x04, // FIFO_A
    0xA4, 0x00, 0x00, 0x04, // FIFO_B
    0x00, 0x01, 0x00, 0x04, // TM0CNT_L
    0x04, 0x01, 0x00, 0x04, // TM1CNT_L
    0xC6, 0x00, 0x00, 0x04, // DMA1CNT_H
    0xD2, 0x00, 0x00, 0x04, // DMA2CNT_H
];

/// DirectSound FIFO DMA control word (`Enable | IRQ | Special | Repeat | 32-bit | FixedDest | count=4`).
const DMA_FIFO_CTRL: u32 = 0xF640_0004;

/// Maximum number of entries scanned in any pointer table.
const MAX_TABLE_ENTRIES: usize = 512;

/// Maximum valid PCM sample payload size (2 MiB).
const MAX_PCM_LENGTH: usize = 0x20_0000;

/// Maximum ROM offset scanned when locating Thumb `BL` init calls.
const MAX_CODE_SCAN_BYTES: usize = 0x20_0000;

/// Natsume hybrid PSG+PCM driver revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NatsumeRevision {
    /// Early revision: melodic/drum setters at `dma_setup + 0x19C / +0x1B0`, with `SetDrumInfo`.
    Rev0,
    /// Mid revision: melodic/drum setters at `dma_setup + 0x1A4 / +0x1B8`, without `SetDrumInfo`.
    Rev1,
    /// Standard revision: melodic/drum setters at `dma_setup + 0x1A4 / +0x1B8`, with `SetDrumInfo`.
    Rev2,
}

/// Whether a Natsume PCM sample belongs to the melodic or percussion table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NatsumeSampleKind {
    /// Melodic DirectSound instrument sample.
    Melodic,
    /// Percussion / drum-kit DirectSound sample.
    Drum,
}

/// Extracted Natsume Hybrid PSG+PCM signed 8-bit sample entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NatsumeSampleEntry {
    /// Sequential sample index within the deduplicated scan output.
    pub index: usize,
    /// Melodic vs percussion sample table origin.
    pub kind: NatsumeSampleKind,
    /// Absolute ROM file offset of the 16-byte sample header.
    pub rom_offset: usize,
    /// Absolute ROM file offset where the signed 8-bit PCM payload begins.
    pub sample_start_offset: usize,
    /// Payload length in bytes (equals sample count for 8-bit PCM).
    pub size: usize,
    /// Native playback rate in Hz at `root_key`.
    pub sample_rate: u32,
    /// Signed fine-tune index into the Q4.12 fine-tune table.
    pub fine_tune: i8,
    /// MIDI root key (`0..=127`).
    pub root_key: u8,
    /// Optional loop range `(loop_start, loop_end)` in samples.
    pub loop_range: Option<(u32, u32)>,
    /// Extracted raw signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Extracted Natsume BGM or SFX sequence header descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NatsumeSongEntry {
    /// Song index within its table.
    pub index: usize,
    /// `true` for SFX table, `false` for BGM table.
    pub is_sfx: bool,
    /// Absolute ROM file offset of the song header.
    pub rom_offset: usize,
    /// 12-bit active channel bitmask.
    pub channel_mask: u16,
    /// Song tempo parameter.
    pub tempo: u16,
    /// Number of active tracks (`channel_mask.count_ones()`).
    pub track_count: u8,
}

/// Full parsed summary of a Natsume Hybrid PSG+PCM ROM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsumeScanSummary {
    /// Detected driver revision.
    pub revision: NatsumeRevision,
    /// Number of melodic sample table entries.
    pub melodic_count: usize,
    /// Number of percussion sample table entries.
    pub drum_count: usize,
    /// Number of BGM sequence entries.
    pub bgm_count: usize,
    /// Number of SFX sequence entries.
    pub sfx_count: usize,
    /// Deduplicated PCM samples (melodic + drum).
    pub samples: Vec<NatsumeSampleEntry>,
    /// All BGM and SFX sequence headers.
    pub songs: Vec<NatsumeSongEntry>,
}

#[derive(Debug, Clone, Copy)]
struct DetectionAnchors {
    dma_setup_offset: usize,
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let first = needle[0];
    let last_start = haystack.len() - needle.len();
    let mut i = 0;
    while i <= last_start {
        if haystack[i] == first && haystack[i..i + needle.len()] == *needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn find_anchors(rom: &[u8]) -> Option<DetectionAnchors> {
    if rom.len() < 0x200 {
        return None;
    }
    let _pitch_table_offset = find_subslice(rom, &PITCH_AND_FINE_TABLE_PREFIX)?;
    let mmio6_table_offset = find_subslice(rom, &MMIO6_TABLE)?;

    let mmio6_addr = ROM_BASE.checked_add(u32::try_from(mmio6_table_offset).ok()?)?;
    let mut pool_pattern = [0u8; 8];
    pool_pattern[0..4].copy_from_slice(&mmio6_addr.to_le_bytes());
    pool_pattern[4..8].copy_from_slice(&DMA_FIFO_CTRL.to_le_bytes());

    let pool_offset = find_subslice(rom, &pool_pattern)?;
    let dma_setup_offset = pool_offset.checked_sub(0x24)?;

    Some(DetectionAnchors { dma_setup_offset })
}

/// Detect whether `rom` contains the Natsume Hybrid PSG+PCM DirectSound engine.
#[must_use]
pub fn detect_natsume_engine(rom: &[u8]) -> bool {
    find_anchors(rom).is_some()
}

/// Detect the Natsume engine revision if present (falls back to `None` if not detected).
#[must_use]
pub fn detect_natsume_revision(rom: &[u8]) -> Option<NatsumeRevision> {
    let anchors = find_anchors(rom)?;
    let (caller_off, is_rev0, _mel_addr, _drm_addr) =
        find_sample_setter_call(rom, anchors.dma_setup_offset)?;
    let v_m20 = caller_off
        .checked_sub(20)
        .and_then(|o| read_ldr_r0_literal(rom, o));
    match v_m20 {
        Some(addr) if (addr >> 24) == 0x03 => Some(NatsumeRevision::Rev1),
        _ if is_rev0 => Some(NatsumeRevision::Rev0),
        _ => Some(NatsumeRevision::Rev2),
    }
}

/// Scan a Natsume Hybrid PSG+PCM ROM for all PCM samples and BGM/SFX sequences.
pub fn scan_natsume_engine(rom: &[u8]) -> Option<NatsumeScanSummary> {
    let anchors = find_anchors(rom)?;
    let (caller_off, is_rev0, mel_addr, drm_addr) =
        find_sample_setter_call(rom, anchors.dma_setup_offset)?;

    let v_m20 = caller_off
        .checked_sub(20)
        .and_then(|o| read_ldr_r0_literal(rom, o));
    let v_m14 = caller_off
        .checked_sub(14)
        .and_then(|o| read_ldr_r0_literal(rom, o));
    let v_m8 = caller_off
        .checked_sub(8)
        .and_then(|o| read_ldr_r0_literal(rom, o));

    let (revision, bgm_addr, sfx_addr, drm_info_addr) = match v_m20 {
        Some(addr) if (addr >> 24) == 0x03 => (NatsumeRevision::Rev1, v_m14, v_m8, None),
        _ => {
            let rev = if is_rev0 {
                NatsumeRevision::Rev0
            } else {
                NatsumeRevision::Rev2
            };
            (rev, v_m20, v_m14, v_m8)
        }
    };

    let mel_off = rom_addr_to_offset(mel_addr)?;
    let drm_off = rom_addr_to_offset(drm_addr)?;

    let drum_info_meta = drm_info_addr
        .and_then(rom_addr_to_offset)
        .and_then(|off| parse_drum_info_meta(rom, off));

    let mel_max = if mel_off < drm_off {
        ((drm_off - mel_off) / 4).min(MAX_TABLE_ENTRIES)
    } else {
        MAX_TABLE_ENTRIES
    };
    let melodic_count = count_valid_pcm_table(rom, mel_off, mel_max);

    let mut drm_max = MAX_TABLE_ENTRIES;
    if let Some((di_off, di_count)) = drum_info_meta {
        if di_count > 0 {
            drm_max = drm_max.min(usize::from(di_count));
        }
        if drm_off < di_off {
            drm_max = drm_max.min((di_off - drm_off) / 4);
        }
    }
    let drum_count = count_valid_pcm_table(rom, drm_off, drm_max);

    if melodic_count == 0 && drum_count == 0 {
        return None;
    }

    let mut samples = Vec::new();
    let mut seen_offsets = HashSet::new();

    for (table_off, count, kind) in [
        (mel_off, melodic_count, NatsumeSampleKind::Melodic),
        (drm_off, drum_count, NatsumeSampleKind::Drum),
    ] {
        for idx in 0..count {
            let Some(ptr) = read_u32_le(rom, table_off + idx * 4) else {
                break;
            };
            let Some(off) = rom_addr_to_offset(ptr) else {
                continue;
            };
            if !seen_offsets.insert(off) {
                continue;
            }
            if let Some(mut entry) = parse_pcm_sample_entry(rom, off, kind) {
                entry.index = samples.len();
                samples.push(entry);
            }
        }
    }

    let mut songs = Vec::new();
    let mut bgm_count = 0usize;
    if let Some(bgm_off) = bgm_addr.and_then(rom_addr_to_offset) {
        bgm_count = collect_song_table(rom, bgm_off, false, &mut songs);
    }
    let mut sfx_count = 0usize;
    if let Some(sfx_off) = sfx_addr.and_then(rom_addr_to_offset) {
        sfx_count = collect_song_table(rom, sfx_off, true, &mut songs);
    }

    Some(NatsumeScanSummary {
        revision,
        melodic_count,
        drum_count,
        bgm_count,
        sfx_count,
        samples,
        songs,
    })
}

/// Scan a GBA ROM for Natsume Hybrid PSG+PCM signed 8-bit PCM samples.
pub fn scan_natsume_soundpool(rom: &[u8]) -> Vec<NatsumeSampleEntry> {
    scan_natsume_engine(rom).map(|s| s.samples).unwrap_or_default()
}

/// Extract all Natsume samples from a ROM and encode them as standard 16-bit mono RIFF WAV files.
pub fn export_natsume_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_natsume_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_natsume_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let tag = match s.kind {
            NatsumeSampleKind::Melodic => "mel",
            NatsumeSampleKind::Drum => "drm",
        };
        let name = format!("natsume_{tag}_{:03}_{:06X}.wav", s.index, s.rom_offset);
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Natsume Hybrid PSG+PCM samples into a SoundFont 2.04 (`.sf2`) soundbank.
pub fn compile_natsume_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_natsume_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_natsume_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Natsume PSG+PCM)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            RawSoundFontSample {
                name: format!("NAT_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range: s.loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(
        &bank_title,
        "Natsume Hybrid PSG+PCM DirectSound Engine",
        &raw_samples,
    )
}

fn find_sample_setter_call(rom: &[u8], dma_setup_off: usize) -> Option<(usize, bool, u32, u32)> {
    let limit = rom.len().saturating_sub(4).min(MAX_CODE_SCAN_BYTES);
    for &(delta, is_rev0) in &[(0x1A4usize, false), (0x19Cusize, true)] {
        let mel_fn = dma_setup_off.checked_add(delta)?;
        let drm_fn = mel_fn.checked_add(0x14)?;
        let mut off = 0usize;
        while off < limit {
            if decode_thumb_bl(rom, off) == Some(mel_fn) {
                if let (Some(mel_addr), Some(drm_addr)) = (
                    off.checked_sub(2).and_then(|o| read_ldr_r0_literal(rom, o)),
                    off.checked_add(4).and_then(|o| read_ldr_r0_literal(rom, o)),
                ) {
                    if decode_thumb_bl(rom, off.checked_add(6)?) == Some(drm_fn)
                        && is_rom_addr(rom, mel_addr)
                        && is_rom_addr(rom, drm_addr)
                    {
                        return Some((off, is_rev0, mel_addr, drm_addr));
                    }
                }
            }
            off += 2;
        }
    }
    None
}

fn decode_thumb_bl(rom: &[u8], off: usize) -> Option<usize> {
    let h1 = read_u16_le(rom, off)?;
    let h2 = read_u16_le(rom, off.checked_add(2)?)?;
    if (h1 & 0xF800) != 0xF000 || (h2 & 0xF800) != 0xF800 {
        return None;
    }
    let raw = (u32::from(h1 & 0x07FF) << 11) | u32::from(h2 & 0x07FF);
    let signed = if (raw & (1 << 21)) != 0 {
        (raw | !0x003F_FFFF) as i32
    } else {
        raw as i32
    };
    let delta = signed.checked_shl(1)?;
    let base = i32::try_from(off.checked_add(4)?).ok()?;
    let dest = base.checked_add(delta)?;
    usize::try_from(dest).ok()
}

fn read_ldr_r0_literal(rom: &[u8], off: usize) -> Option<u32> {
    let hw = read_u16_le(rom, off)?;
    if (hw & 0xFF00) != 0x4800 {
        return None;
    }
    let imm = usize::from(hw & 0x00FF) * 4;
    let pc_aligned = (off.checked_add(4)?) & !3;
    let pool_off = pc_aligned.checked_add(imm)?;
    read_u32_le(rom, pool_off)
}

fn parse_drum_info_meta(rom: &[u8], off: usize) -> Option<(usize, u16)> {
    let count = read_u16_le(rom, off)?;
    let base_note = read_u16_le(rom, off.checked_add(2)?)?;
    let pan_ptr = read_u32_le(rom, off.checked_add(4)?)?;
    if count == 0 || count > 128 || base_note > 127 {
        return None;
    }
    let pan_off = rom_addr_to_offset(pan_ptr)?;
    let pan_end = pan_off.checked_add(usize::from(count))?;
    if pan_end > rom.len() {
        return None;
    }
    Some((off, count))
}

fn parse_pcm_sample_entry(
    rom: &[u8],
    off: usize,
    kind: NatsumeSampleKind,
) -> Option<NatsumeSampleEntry> {
    let length = read_u32_le(rom, off)?;
    let base_rate = read_u16_le(rom, off.checked_add(4)?)?;
    let fine_tune = *rom.get(off.checked_add(6)?)? as i8;
    let root_note = *rom.get(off.checked_add(7)?)?;
    let flags = read_u32_le(rom, off.checked_add(8)?)?;
    let loop_start = read_u32_le(rom, off.checked_add(12)?)?;

    let len_usize = usize::try_from(length).ok()?;
    if len_usize == 0
        || len_usize > MAX_PCM_LENGTH
        || !(1000..=65535).contains(&base_rate)
        || root_note > 127
        || (flags & !1) != 0
        || (flags != 0 && loop_start > length)
    {
        return None;
    }

    let data_start = off.checked_add(16)?;
    let data_end = data_start.checked_add(len_usize)?;
    let raw = rom.get(data_start..data_end)?;
    let pcm_data: Vec<i8> = raw.iter().map(|&b| b as i8).collect();
    let loop_range = if (flags & 1) != 0 && loop_start < length {
        Some((loop_start, length))
    } else {
        None
    };

    Some(NatsumeSampleEntry {
        index: 0,
        kind,
        rom_offset: off,
        sample_start_offset: data_start,
        size: len_usize,
        sample_rate: u32::from(base_rate),
        fine_tune,
        root_key: root_note,
        loop_range,
        pcm_data,
    })
}

fn parse_valid_pcm_header(rom: &[u8], off: usize) -> bool {
    let Some(length) = read_u32_le(rom, off) else {
        return false;
    };
    let Some(base_rate) = off.checked_add(4).and_then(|o| read_u16_le(rom, o)) else {
        return false;
    };
    let Some(&root_note) = off.checked_add(7).and_then(|o| rom.get(o)) else {
        return false;
    };
    let Some(flags) = off.checked_add(8).and_then(|o| read_u32_le(rom, o)) else {
        return false;
    };
    let Some(loop_start) = off.checked_add(12).and_then(|o| read_u32_le(rom, o)) else {
        return false;
    };
    let Ok(len_usize) = usize::try_from(length) else {
        return false;
    };
    if len_usize > MAX_PCM_LENGTH
        || !(1000..=65535).contains(&base_rate)
        || root_note > 127
        || (flags & !1) != 0
        || (flags != 0 && loop_start > length)
    {
        return false;
    }
    off.checked_add(16)
        .and_then(|s| s.checked_add(len_usize))
        .is_some_and(|end| end <= rom.len())
}

fn parse_song_entry(rom: &[u8], off: usize, index: usize, is_sfx: bool) -> Option<NatsumeSongEntry> {
    let channel_mask = read_u16_le(rom, off)?;
    let tempo = read_u16_le(rom, off.checked_add(2)?)?;
    if channel_mask == 0 || (channel_mask & 0xF000) != 0 {
        return None;
    }
    let track_count = channel_mask.count_ones() as usize;
    let ptrs_start = off.checked_add(4)?;
    let ptrs_end = ptrs_start.checked_add(track_count.checked_mul(4)?)?;
    let raw_track_ptrs = rom.get(ptrs_start..ptrs_end)?;

    for i in 0..track_count {
        let ptr = read_u32_le(raw_track_ptrs, i * 4)?;
        if !is_rom_addr(rom, ptr) {
            return None;
        }
    }

    Some(NatsumeSongEntry {
        index,
        is_sfx,
        rom_offset: off,
        channel_mask,
        tempo,
        track_count: track_count as u8,
    })
}

fn count_valid_pcm_table(rom: &[u8], table_off: usize, max_entries: usize) -> usize {
    let mut n = 0usize;
    while n < max_entries {
        let Some(entry_off) = table_off.checked_add(n * 4) else {
            break;
        };
        let Some(ptr) = read_u32_le(rom, entry_off) else {
            break;
        };
        let Some(sample_off) = rom_addr_to_offset(ptr) else {
            break;
        };
        if !parse_valid_pcm_header(rom, sample_off) {
            break;
        }
        n += 1;
    }
    n
}

fn collect_song_table(
    rom: &[u8],
    table_off: usize,
    is_sfx: bool,
    out: &mut Vec<NatsumeSongEntry>,
) -> usize {
    let mut n = 0usize;
    while n < MAX_TABLE_ENTRIES {
        let Some(entry_off) = table_off.checked_add(n * 4) else {
            break;
        };
        let Some(ptr) = read_u32_le(rom, entry_off) else {
            break;
        };
        let Some(song_off) = rom_addr_to_offset(ptr) else {
            break;
        };
        let Some(song) = parse_song_entry(rom, song_off, n, is_sfx) else {
            break;
        };
        out.push(song);
        n += 1;
    }
    n
}

#[inline]
fn is_rom_addr(rom: &[u8], addr: u32) -> bool {
    rom_addr_to_offset(addr).is_some_and(|off| off < rom.len())
}

#[inline]
fn rom_addr_to_offset(addr: u32) -> Option<usize> {
    addr.checked_sub(ROM_BASE)
        .and_then(|off| usize::try_from(off).ok())
}

#[inline]
fn read_u16_le(buf: &[u8], off: usize) -> Option<u16> {
    let bytes = buf.get(off..off.checked_add(2)?)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

#[inline]
fn read_u32_le(buf: &[u8], off: usize) -> Option<u32> {
    let bytes = buf.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;

    fn encode_thumb_bl(rom: &mut [u8], off: usize, target: usize) {
        let delta = (target as isize) - ((off + 4) as isize);
        let imm22 = ((delta >> 1) as u32) & 0x003F_FFFF;
        let h1 = 0xF000u16 | (((imm22 >> 11) & 0x07FF) as u16);
        let h2 = 0xF800u16 | ((imm22 & 0x07FF) as u16);
        rom[off..off + 2].copy_from_slice(&h1.to_le_bytes());
        rom[off + 2..off + 4].copy_from_slice(&h2.to_le_bytes());
    }

    fn build_synthetic_rom(rev1: bool) -> Vec<u8> {
        let mut rom = vec![0u8; 0x1000];
        rom[0xA0..0xAC].copy_from_slice(b"NATSUMETEST\0");

        rom[0x0800..0x0800 + PITCH_AND_FINE_TABLE_PREFIX.len()]
            .copy_from_slice(&PITCH_AND_FINE_TABLE_PREFIX);
        rom[0x0900..0x0900 + MMIO6_TABLE.len()].copy_from_slice(&MMIO6_TABLE);

        let mmio6_addr = ROM_BASE + 0x0900;
        rom[0x0424..0x0428].copy_from_slice(&mmio6_addr.to_le_bytes());
        rom[0x0428..0x042C].copy_from_slice(&DMA_FIFO_CTRL.to_le_bytes());

        let mel_fn = 0x05A4usize;
        let drm_fn = 0x05B8usize;

        rom[0x0A00..0x0A04].copy_from_slice(&(ROM_BASE + 0x0A20).to_le_bytes());
        rom[0x0A20..0x0A22].copy_from_slice(&0x0800u16.to_le_bytes());
        rom[0x0A22..0x0A24].copy_from_slice(&192u16.to_le_bytes());
        rom[0x0A24..0x0A28].copy_from_slice(&(ROM_BASE + 0x0AC0).to_le_bytes());

        rom[0x0A40..0x0A44].copy_from_slice(&(ROM_BASE + 0x0A60).to_le_bytes());
        rom[0x0A60..0x0A62].copy_from_slice(&0x0080u16.to_le_bytes());
        rom[0x0A62..0x0A64].copy_from_slice(&240u16.to_le_bytes());
        rom[0x0A64..0x0A68].copy_from_slice(&(ROM_BASE + 0x0AC0).to_le_bytes());

        rom[0x0A80..0x0A82].copy_from_slice(&1u16.to_le_bytes());
        rom[0x0A82..0x0A84].copy_from_slice(&35u16.to_le_bytes());
        rom[0x0A84..0x0A88].copy_from_slice(&(ROM_BASE + 0x0A90).to_le_bytes());
        rom[0x0A90] = 3;

        rom[0x0B00..0x0B04].copy_from_slice(&(ROM_BASE + 0x0B40).to_le_bytes());
        rom[0x0B04..0x0B08].copy_from_slice(&(ROM_BASE + 0x0B80).to_le_bytes());

        rom[0x0B40..0x0B44].copy_from_slice(&32u32.to_le_bytes());
        rom[0x0B44..0x0B46].copy_from_slice(&16000u16.to_le_bytes());
        rom[0x0B46] = 0;
        rom[0x0B47] = 60;
        rom[0x0B48..0x0B4C].copy_from_slice(&1u32.to_le_bytes());
        rom[0x0B4C..0x0B50].copy_from_slice(&8u32.to_le_bytes());
        for i in 0..32 {
            rom[0x0B50 + i] = (i as u8).wrapping_mul(8);
        }

        rom[0x0B80..0x0B84].copy_from_slice(&16u32.to_le_bytes());
        rom[0x0B84..0x0B86].copy_from_slice(&22050u16.to_le_bytes());
        rom[0x0B86] = (-2i8) as u8;
        rom[0x0B87] = 35;
        rom[0x0B88..0x0B8C].copy_from_slice(&0u32.to_le_bytes());
        rom[0x0B8C..0x0B90].copy_from_slice(&0u32.to_le_bytes());

        let pool_base = 0x0140usize;
        let args: [u32; 5] = if rev1 {
            [
                0x0300_0510,
                ROM_BASE + 0x0A00,
                ROM_BASE + 0x0A40,
                ROM_BASE + 0x0B00,
                ROM_BASE + 0x0B04,
            ]
        } else {
            [
                ROM_BASE + 0x0A00,
                ROM_BASE + 0x0A40,
                ROM_BASE + 0x0A80,
                ROM_BASE + 0x0B00,
                ROM_BASE + 0x0B04,
            ]
        };
        for (i, &val) in args.iter().enumerate() {
            let p = pool_base + i * 4;
            rom[p..p + 4].copy_from_slice(&val.to_le_bytes());
            let ldr_off = 0x0100 + i * 6;
            let pc_aligned = (ldr_off + 4) & !3;
            let imm = ((p - pc_aligned) / 4) as u16;
            let ldr = 0x4800u16 | (imm & 0xFF);
            rom[ldr_off..ldr_off + 2].copy_from_slice(&ldr.to_le_bytes());
        }

        encode_thumb_bl(&mut rom, 0x0114, mel_fn);
        encode_thumb_bl(&mut rom, 0x011A, drm_fn);
        rom
    }

    #[test]
    fn detects_and_exports_natsume_rev2_and_rev1() {
        let rom_rev2 = build_synthetic_rom(false);
        assert!(detect_natsume_engine(&rom_rev2));
        assert_eq!(detect_natsume_revision(&rom_rev2), Some(NatsumeRevision::Rev2));

        let summary = scan_natsume_engine(&rom_rev2).expect("valid Rev2 summary");
        assert_eq!(summary.revision, NatsumeRevision::Rev2);
        assert_eq!(summary.samples.len(), 2);
        assert_eq!(summary.bgm_count, 1);
        assert_eq!(summary.sfx_count, 1);
        assert_eq!(summary.samples[0].loop_range, Some((8, 32)));
        assert_eq!(summary.samples[1].loop_range, None);

        let wavs = export_natsume_samples_wav16(&rom_rev2).expect("wav export");
        assert_eq!(wavs.len(), 2);
        assert_eq!(&wavs[0].2[0..4], b"RIFF");

        let sf2 = compile_natsume_soundfont(&rom_rev2, Some("Synthetic Natsume"))
            .expect("sf2 compile");
        let val = validate_soundfont(&sf2.sf2_bytes).expect("valid sf2");
        assert_eq!(val.sample_count, 2);

        let rom_rev1 = build_synthetic_rom(true);
        assert_eq!(detect_natsume_revision(&rom_rev1), Some(NatsumeRevision::Rev1));
    }

    #[test]
    fn rejects_empty_and_truncated() {
        assert!(!detect_natsume_engine(&[]));
        assert!(scan_natsume_soundpool(&[0u8; 512]).is_empty());
        assert!(export_natsume_samples_wav16(&[0u8; 512]).is_err());
    }
}
