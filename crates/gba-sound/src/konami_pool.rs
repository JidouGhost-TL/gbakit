//! Konami KCE (KCEH & KCEJ) GBA Sound Engine sample pool scanning,
//! 8-bit PCM extraction, WAV16 export, and SoundFont 2.04 soundbank generation.
//!
//! Supports the two dominant Konami Computer Entertainment custom DirectSound drivers:
//! 1. **KCEH** (Konami Computer Entertainment Hawaii): 32-byte relocatable soundbank
//!    container (`KcehBankHeader`) pointing to a relative-offset table of 16-byte
//!    inline sample descriptors (`KcehSampleHeader`) followed by raw signed 8-bit PCM.
//! 2. **KCEJ** (Konami Computer Entertainment Japan): 32-bit ROM pointer tables
//!    targeting 12-byte inline sample descriptors (`KcejSampleHeader`) followed by
//!    raw signed 8-bit PCM, with Timer0-derived native playback rates.
//!
//! Detection and scanning always dispatch **KCEH before KCEJ**, as Japanese KCEH
//! builds share the `0xF600_0004` DMA1 control word while storing samples in the
//! 32-byte KCEH soundbank container.

#![deny(unsafe_code)]

use std::collections::HashSet;

use formatkit_core::{Error, Result};
use konami_kce::{
    rate_from_timer_reload, KcehBankHeader, KcehSampleHeader, KcejSampleHeader, KonamiKceFamily,
    KonamiKceVersion, KonamiSampleView, GBA_ROM_BUS_BASE, KCEH_BANK_HEADER_SIZE,
    KCEH_SAMPLE_HEADER_SIZE, KCEJ_SAMPLE_HEADER_SIZE, KCEJ_TIMER0_RELOAD_TABLE,
};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// Default fallback KCEH sample rate in Hz when header rate is out of range.
const KCEH_FALLBACK_SAMPLE_RATE_HZ: u32 = 11_025;

/// Minimum and maximum sample payload sizes accepted by the scanner.
const MIN_SAMPLE_BYTES: usize = 16;
const MAX_SAMPLE_BYTES: usize = 0x0008_0000;

/// Konami KCE custom DirectSound engine sub-family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KonamiEngineVariant {
    /// Konami Computer Entertainment Hawaii / Cross-Studio 32-byte soundbank + 16-byte sample headers.
    Kceh,
    /// Konami Computer Entertainment Japan (ROM pointer tables + 12-byte sample headers).
    Kcej,
}

impl From<KonamiKceFamily> for KonamiEngineVariant {
    #[inline]
    fn from(family: KonamiKceFamily) -> Self {
        match family {
            KonamiKceFamily::Kceh => Self::Kceh,
            KonamiKceFamily::Kcej => Self::Kcej,
        }
    }
}

impl From<KonamiEngineVariant> for KonamiKceFamily {
    #[inline]
    fn from(variant: KonamiEngineVariant) -> Self {
        match variant {
            KonamiEngineVariant::Kceh => Self::Kceh,
            KonamiEngineVariant::Kcej => Self::Kcej,
        }
    }
}

impl From<KonamiKceVersion> for KonamiEngineVariant {
    #[inline]
    fn from(version: KonamiKceVersion) -> Self {
        Self::from(version.family())
    }
}

/// Extracted Konami KCE (KCEH or KCEJ) signed 8-bit PCM sample entry from a GBA ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KonamiSampleEntry {
    /// Sample index within the detected pool.
    pub index: usize,
    /// Detected Konami KCE driver sub-family.
    pub variant: KonamiEngineVariant,
    /// Absolute ROM file offset where the sample PCM payload begins.
    pub rom_offset: usize,
    /// Sample payload length in bytes (signed 8-bit mono PCM).
    pub size: usize,
    /// Native playback sample rate in Hz.
    pub sample_rate: u32,
    /// MIDI root key (`0..=127`, typically `60` = Middle C).
    pub root_key: u8,
    /// Loop start offset in samples, if looped (`loop_end == size`).
    pub loop_start: Option<u32>,
    /// Extracted signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Detect the exact Konami KCE ARM7TDMI mixer revision from a GBA ROM image,
/// delegating to [`KonamiKceVersion::detect_from_bytes`].
pub fn detect_konami_version(rom: &[u8]) -> Option<KonamiKceVersion> {
    KonamiKceVersion::detect_from_bytes(rom)
}

/// Detect Konami KCE engine variant and optional version in a single `detect_from_bytes` pass.
pub(crate) fn detect_konami_engine_with_version(
    rom: &[u8],
) -> Option<(KonamiEngineVariant, Option<KonamiKceVersion>)> {
    if rom.len() < 0x40 {
        return None;
    }

    let detected_ver = KonamiKceVersion::detect_from_bytes(rom);
    let detected_family = detected_ver.map(KonamiKceVersion::family);

    if (detected_family == Some(KonamiKceFamily::Kceh) || has_kceh_signature(rom))
        && !scan_kceh_banks(rom).is_empty()
    {
        return Some((KonamiEngineVariant::Kceh, detected_ver));
    }

    if (detected_family == Some(KonamiKceFamily::Kcej) || has_kcej_signature(rom))
        && !scan_kcej_tables(rom).is_empty()
    {
        return Some((KonamiEngineVariant::Kcej, detected_ver));
    }

    None
}

/// Detect whether a GBA ROM uses a supported Konami KCE custom DirectSound engine.
///
/// Always checks **KCEH before KCEJ** to correctly classify builds that share
/// the `0xF600_0004` DMA1 control constant while using KCEH soundbank containers.
pub fn detect_konami_engine(rom: &[u8]) -> Option<KonamiEngineVariant> {
    detect_konami_engine_with_version(rom).map(|(variant, _)| variant)
}

/// Scan a GBA ROM for Konami KCE (KCEH or KCEJ) signed 8-bit PCM samples.
///
/// Dispatches KCEH before KCEJ. Returns an empty vector if no valid Konami KCE
/// engine signature and sample pool are found.
pub fn scan_konami_soundpool(rom: &[u8]) -> Vec<KonamiSampleEntry> {
    if rom.len() < 0x40 {
        return Vec::new();
    }

    let detected_family = KonamiKceVersion::detect_from_bytes(rom).map(KonamiKceVersion::family);

    if detected_family == Some(KonamiKceFamily::Kceh) || has_kceh_signature(rom) {
        let kceh = scan_kceh_banks(rom);
        if !kceh.is_empty() {
            return kceh;
        }
    }

    if detected_family == Some(KonamiKceFamily::Kcej) || has_kcej_signature(rom) {
        let kcej = scan_kcej_tables(rom);
        if !kcej.is_empty() {
            return kcej;
        }
    }

    Vec::new()
}

/// Extract all Konami KCE samples from a ROM and encode them as standard
/// 16-bit uncompressed mono RIFF WAV files.
///
/// Returns a list of `(sample_index, recommended_filename, wav_bytes)`.
pub fn export_konami_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_konami_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_konami_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let tag = match s.variant {
            KonamiEngineVariant::Kceh => "kceh",
            KonamiEngineVariant::Kcej => "kcej",
        };
        let name = format!("konami_{tag}_sample_{:03}_{:06X}.wav", s.index, s.rom_offset);
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Konami KCE samples into a standard SoundFont 2.04 (`.sf2`) soundbank.
pub fn compile_konami_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_konami_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_konami_samples_found",
            value: 0,
        });
    }

    let (engine_label, sample_prefix) = match samples[0].variant {
        KonamiEngineVariant::Kceh => ("Konami KCEH", "KCEH"),
        KonamiEngineVariant::Kcej => ("Konami KCEJ", "KCEJ"),
    };

    let bank_title = title_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} ({engine_label})")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            let loop_range = s.loop_start.map(|start| (start, s.pcm_data.len() as u32));
            RawSoundFontSample {
                name: format!("{sample_prefix}_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(&bank_title, engine_label, &raw_samples)
}

/// Check for Konami KCEH driver binary signatures:
/// 1. `DMA1CNT`/`DMA2CNT` constant `0xB660_0004`.
/// 2. Consecutive DirectSound FIFO A + FIFO B + DMA1 literal pool
///    (`0x0400_00A0`, `0x0400_00A4`, `0x0400_00BC`).
fn has_kceh_signature(rom: &[u8]) -> bool {
    const KCEH_DMA_CNT_SIG: [u8; 4] = 0xB660_0004u32.to_le_bytes();
    const KCEH_FIFO_SIG: [u8; 12] = [
        0xA0, 0x00, 0x00, 0x04, // REG_FIFO_A (0x0400_00A0)
        0xA4, 0x00, 0x00, 0x04, // REG_FIFO_B (0x0400_00A4)
        0xBC, 0x00, 0x00, 0x04, // REG_DMA1SAD (0x0400_00BC)
    ];

    find_subslice(rom, &KCEH_DMA_CNT_SIG, 0).is_some()
        || find_subslice(rom, &KCEH_FIFO_SIG, 0).is_some()
}

/// Check for Konami KCEJ driver binary signatures:
/// 1. `DMA1CNT` constant `0xF600_0004` (IRQ-enabled DirectSound DMA).
/// 2. Thumb `REG_IE` mask setup `0x82 << 2 = 0x0208` (DMA1 + Timer0 IRQ).
fn has_kcej_signature(rom: &[u8]) -> bool {
    const KCEJ_DMA_CNT_SIG: [u8; 4] = 0xF600_0004u32.to_le_bytes();
    const KCEJ_IE_SIGS: &[[u8; 4]] = &[
        [0x82, 0x23, 0x9B, 0x00], // movs r3, #0x82; lsls r3, r3, #2
        [0x82, 0x21, 0x89, 0x00], // movs r1, #0x82; lsls r1, r1, #2
        [0x82, 0x20, 0x80, 0x00], // movs r0, #0x82; lsls r0, r0, #2
    ];

    if find_subslice(rom, &KCEJ_DMA_CNT_SIG, 0).is_none() {
        return false;
    }

    KCEJ_IE_SIGS
        .iter()
        .any(|sig| find_subslice(rom, sig, 0).is_some())
}

/// Detect the KCEJ Timer0 reload value near `REG_TM0CNT_L` (`0x0400_0100`) literal pools.
///
/// Falls back to the default KCEJ Timer0 mix rate (~18,157.2 Hz, reload `0xFC64`)
/// if no fixed reload constant is found adjacent to `0x0400_0100`.
fn detect_kcej_mix_rate(rom: &[u8]) -> f64 {
    const TIMER0_REG_BYTES: [u8; 4] = 0x0400_0100u32.to_le_bytes();
    const SEARCH_DELTAS: [isize; 4] = [-8, -4, 4, 8];

    let mut pos = 0usize;
    while let Some(idx) = find_subslice(rom, &TIMER0_REG_BYTES, pos) {
        pos = idx + 4;
        for delta in SEARCH_DELTAS {
            let Some(off) = idx.checked_add_signed(delta) else {
                continue;
            };
            if off + 4 <= rom.len() {
                let v = read_u32_le(rom, off);
                let reload = (v & 0xFFFF) as u16;
                if (v >> 16) == 0x0080 && (0xF000..=0xFFE0).contains(&reload) {
                    return rate_from_timer_reload(reload);
                }
            }
        }
    }

    rate_from_timer_reload(KCEJ_TIMER0_RELOAD_TABLE[1])
}

/// Scan a GBA ROM for Konami KCEH / Cross-Studio 32-byte soundbank containers and
/// extract their 16-byte inline signed 8-bit PCM samples via [`konami_kce`].
fn scan_kceh_banks(rom: &[u8]) -> Vec<KonamiSampleEntry> {
    let rlen = rom.len();
    if rlen < 0x40 {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut seen_hdrs = HashSet::new();

    let mut off = 0usize;
    while off + 0x40 <= rlen {
        let Ok(bank_hdr) = KcehBankHeader::parse(&rom[off..]) else {
            off += 4;
            continue;
        };

        let sfx_table_rel = bank_hdr.sfx_table_off as usize;
        let stbl_rel = bank_hdr.sample_table_off as usize;
        let remaining = rlen - off;

        if stbl_rel + 32 >= remaining
            || sfx_table_rel < KCEH_BANK_HEADER_SIZE
            || sfx_table_rel >= remaining
            || !sfx_table_rel.is_multiple_of(4)
        {
            off += 4;
            continue;
        }

        let stbl = off + stbl_rel;
        if stbl + 12 > rlen {
            off += 4;
            continue;
        }

        let s0 = read_u32_le(rom, stbl) as usize;
        let s1 = read_u32_le(rom, stbl + 4) as usize;
        let s2 = read_u32_le(rom, stbl + 8) as usize;

        if !(stbl_rel < s0 && s0 < s1 && s1 < s2 && s2 + KCEH_SAMPLE_HEADER_SIZE < remaining)
            || !s0.is_multiple_of(4)
            || !s1.is_multiple_of(4)
            || !s2.is_multiple_of(4)
        {
            off += 4;
            continue;
        }

        let l0 = read_u32_le(rom, off + s0) as usize;
        let l1 = read_u32_le(rom, off + s1) as usize;

        if !(MIN_SAMPLE_BYTES..=MAX_SAMPLE_BYTES).contains(&l0)
            || !(MIN_SAMPLE_BYTES..=MAX_SAMPLE_BYTES).contains(&l1)
        {
            off += 4;
            continue;
        }

        let min_s1 = s0.saturating_add(KCEH_SAMPLE_HEADER_SIZE).saturating_add(l0);
        let min_s2 = s1.saturating_add(KCEH_SAMPLE_HEADER_SIZE).saturating_add(l1);
        if !(min_s1 <= s1 && s1 <= min_s1.saturating_add(16))
            || !(min_s2 <= s2 && s2 <= min_s2.saturating_add(16))
        {
            off += 4;
            continue;
        }

        // Validated KCEH soundbank container: walk relative offset table up to off + s0.
        let table_end = off + s0;
        let mut cur = stbl;
        while cur + 4 <= table_end {
            let so = read_u32_le(rom, cur) as usize;
            cur += 4;

            if !(stbl_rel < so && so + KCEH_SAMPLE_HEADER_SIZE < remaining) {
                break;
            }

            let hdr_off = off + so;
            if seen_hdrs.contains(&hdr_off) {
                continue;
            }

            let Ok(hdr) = KcehSampleHeader::parse(&rom[hdr_off..]) else {
                continue;
            };
            let slen = hdr.sample_len as usize;
            if !(MIN_SAMPLE_BYTES..=MAX_SAMPLE_BYTES).contains(&slen) {
                continue;
            }

            let idx = results.len();
            let Ok(view) = KonamiSampleView::parse_kceh(idx as u32, &rom[hdr_off..]) else {
                continue;
            };

            seen_hdrs.insert(hdr_off);
            let data_off = hdr_off + KCEH_SAMPLE_HEADER_SIZE;
            let pcm_data: Vec<i8> = view.pcm_bytes.iter().map(|&b| b as i8).collect();
            let sample_rate = if (4_000..=48_000).contains(&view.sample_rate_hz) {
                view.sample_rate_hz
            } else {
                KCEH_FALLBACK_SAMPLE_RATE_HZ
            };
            let root_key = if hdr.root_note <= 127 {
                hdr.root_note
            } else {
                60
            };

            results.push(KonamiSampleEntry {
                index: idx,
                variant: KonamiEngineVariant::Kceh,
                rom_offset: data_off,
                size: view.len(),
                sample_rate,
                root_key,
                loop_start: view.loop_start,
                pcm_data,
            });
        }

        off += 4;
    }

    results
}

/// Validated 12-byte KCEJ inline sample header with its ROM offset.
struct ParsedKcejHeader {
    hdr_off: usize,
    sample_len: usize,
}

/// Validate a 12-byte KCEJ sample header referenced by a GBA ROM bus pointer `ptr`.
fn parse_kcej_header_at_ptr(rom: &[u8], ptr: u32, max_ptr: u32) -> Option<ParsedKcejHeader> {
    if ptr < GBA_ROM_BUS_BASE || ptr >= max_ptr || (ptr & 3) != 0 {
        return None;
    }

    let hdr_off = (ptr - GBA_ROM_BUS_BASE) as usize;
    let hdr = KcejSampleHeader::parse(rom.get(hdr_off..)?).ok()?;
    let sample_len = hdr.sample_len as usize;

    if !(0x80..=0x8000).contains(&hdr.pitch_base)
        || !(MIN_SAMPLE_BYTES..=MAX_SAMPLE_BYTES).contains(&sample_len)
    {
        return None;
    }

    let data_end = hdr_off
        .checked_add(KCEJ_SAMPLE_HEADER_SIZE)?
        .checked_add(sample_len)?;
    if data_end > rom.len() {
        return None;
    }

    if hdr.loop_offset != -1 && hdr.loop_offset < 0 {
        return None;
    }

    Some(ParsedKcejHeader {
        hdr_off,
        sample_len,
    })
}

/// Scan a GBA ROM for Konami KCEJ sample pointer tables targeting 12-byte inline
/// signed 8-bit PCM samples via [`konami_kce`].
fn scan_kcej_tables(rom: &[u8]) -> Vec<KonamiSampleEntry> {
    let rlen = rom.len();
    if rlen < 32 {
        return Vec::new();
    }

    let mix_rate = detect_kcej_mix_rate(rom);
    let max_ptr = GBA_ROM_BUS_BASE.saturating_add(rlen.saturating_sub(16) as u32);
    let mut results = Vec::new();
    let mut seen_hdrs = HashSet::new();

    let mut off = 0usize;
    while off + 32 <= rlen {
        let p0 = read_u32_le(rom, off);
        if parse_kcej_header_at_ptr(rom, p0, max_ptr).is_some() {
            let mut cur = off;
            let mut table_entries: Vec<ParsedKcejHeader> = Vec::new();
            let mut last_valid: Option<(u32, u32)> = None;
            let mut null_run = 0usize;

            while cur + 4 <= rlen {
                let p = read_u32_le(rom, cur);
                if p == 0 {
                    null_run += 1;
                    if null_run > 8 {
                        break;
                    }
                    cur += 4;
                    continue;
                }

                let Some(hdr) = parse_kcej_header_at_ptr(rom, p, max_ptr) else {
                    break;
                };

                if let Some((prev_p, prev_len)) = last_valid {
                    let expected_min = prev_p
                        .saturating_add(KCEJ_SAMPLE_HEADER_SIZE as u32)
                        .saturating_add(prev_len);
                    if p < expected_min || p > expected_min.saturating_add(64) {
                        break;
                    }
                }

                null_run = 0;
                last_valid = Some((p, hdr.sample_len as u32));
                table_entries.push(hdr);
                cur += 4;
            }

            if table_entries.len() >= 5 {
                for hdr in table_entries {
                    if !seen_hdrs.insert(hdr.hdr_off) {
                        continue;
                    }
                    let idx = results.len();
                    let Ok(view) =
                        KonamiSampleView::parse_kcej(idx as u32, &rom[hdr.hdr_off..], mix_rate)
                    else {
                        continue;
                    };
                    let data_off = hdr.hdr_off + KCEJ_SAMPLE_HEADER_SIZE;
                    let pcm_data: Vec<i8> = view.pcm_bytes.iter().map(|&b| b as i8).collect();
                    let sample_rate = view.sample_rate_hz.clamp(4_000, 48_000);

                    results.push(KonamiSampleEntry {
                        index: idx,
                        variant: KonamiEngineVariant::Kcej,
                        rom_offset: data_off,
                        size: view.len(),
                        sample_rate,
                        root_key: view.root_note,
                        loop_start: view.loop_start,
                        pcm_data,
                    });
                }
                off = cur;
                continue;
            }
        }
        off += 4;
    }

    results
}

/// Read an unaligned little-endian `u32` from `data[off..off + 4]`.
#[inline]
fn read_u32_le(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
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

    /// Build a synthetic GBA ROM containing a valid KCEH 32-byte soundbank container
    /// with 4 inline signed 8-bit PCM samples (including one looped sample).
    fn build_synthetic_kceh_rom(include_kcej_dma_sig: bool) -> Vec<u8> {
        let mut rom = vec![0u8; 0x4000];
        rom[0xA0..0xAC].copy_from_slice(b"KCEHTESTROM\0");

        // Embed KCEH DMA1CNT signature at 0x0200
        rom[0x0200..0x0204].copy_from_slice(&0xB660_0004u32.to_le_bytes());

        // Optionally also embed KCEJ signatures to verify KCEH-first dispatch priority
        if include_kcej_dma_sig {
            rom[0x0210..0x0214].copy_from_slice(&0xF600_0004u32.to_le_bytes());
            rom[0x0214..0x0218].copy_from_slice(&[0x82, 0x23, 0x9B, 0x00]);
        }

        // Place KCEH soundbank container at 0x0800
        let bank_base = 0x0800usize;
        let sfx_table_rel: u32 = 0x0040;
        let sample_table_rel: u32 = 0x0080;

        // 32-byte KcehBankHeader
        rom[bank_base..bank_base + 4].copy_from_slice(&0x20u32.to_le_bytes()); // inst_table_off
        rom[bank_base + 4..bank_base + 8].copy_from_slice(&0x30u32.to_le_bytes()); // seq_table_off
        rom[bank_base + 8..bank_base + 12].copy_from_slice(&0x38u32.to_le_bytes()); // sub_table_off
        rom[bank_base + 12..bank_base + 16].copy_from_slice(&sfx_table_rel.to_le_bytes());
        rom[bank_base + 16..bank_base + 20].copy_from_slice(&0u32.to_le_bytes()); // reserved_0
        rom[bank_base + 20..bank_base + 24].copy_from_slice(&0u32.to_le_bytes()); // reserved_1
        rom[bank_base + 24..bank_base + 28].copy_from_slice(&0x60u32.to_le_bytes()); // extra_table_off
        rom[bank_base + 28..bank_base + 32].copy_from_slice(&sample_table_rel.to_le_bytes());

        // 4 samples starting right after the 4-entry (16-byte) relative offset table
        let sample_count = 4usize;
        let mut cursor_rel = (sample_table_rel as usize) + sample_count * 4;
        let specs: [(usize, i32, u16, u8); 4] = [
            (64, -1, 11_025, 60),
            (96, 32, 11_397, 64),
            (80, -1, 8_192, 57),
            (128, 16, 16_000, 72),
        ];

        for (i, &(slen, loop_start, rate_hz, root_note)) in specs.iter().enumerate() {
            let tbl_entry = bank_base + (sample_table_rel as usize) + i * 4;
            rom[tbl_entry..tbl_entry + 4].copy_from_slice(&(cursor_rel as u32).to_le_bytes());

            let hdr_off = bank_base + cursor_rel;
            rom[hdr_off..hdr_off + 4].copy_from_slice(&(slen as u32).to_le_bytes());
            rom[hdr_off + 4..hdr_off + 8].copy_from_slice(&loop_start.to_le_bytes());
            rom[hdr_off + 8..hdr_off + 10].copy_from_slice(&rate_hz.to_le_bytes());
            rom[hdr_off + 10] = root_note;
            rom[hdr_off + 11] = 0;
            rom[hdr_off + 12..hdr_off + 16].copy_from_slice(&0u32.to_le_bytes());

            let pcm_start = hdr_off + KCEH_SAMPLE_HEADER_SIZE;
            for k in 0..slen {
                let sample = (((k as i32 % 16) - 8) * 7) as i8;
                rom[pcm_start + k] = sample as u8;
            }

            cursor_rel = (cursor_rel + KCEH_SAMPLE_HEADER_SIZE + slen + 3) & !3;
        }

        rom
    }

    /// Build a synthetic GBA ROM containing a valid KCEJ pointer table with 6 samples
    /// (including a NULL gap slot and looped samples) plus Timer0 mix-rate literal pool.
    fn build_synthetic_kcej_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x4000];
        rom[0xA0..0xAC].copy_from_slice(b"KCEJTESTROM\0");

        // Embed KCEJ DMA1CNT + Thumb REG_IE signature
        rom[0x0200..0x0204].copy_from_slice(&0xF600_0004u32.to_le_bytes());
        rom[0x0204..0x0208].copy_from_slice(&[0x82, 0x23, 0x9B, 0x00]);

        // Embed Timer0 literal pool: 0x0400_0100 followed by 0x0080_FCB9 (~19,996.7 Hz)
        rom[0x0220..0x0224].copy_from_slice(&0x0400_0100u32.to_le_bytes());
        rom[0x0224..0x0228].copy_from_slice(&0x0080_FCB9u32.to_le_bytes());

        // Write 6 contiguous KCEJ samples starting at 0x0800
        let mut ptrs = Vec::new();
        let mut cursor = 0x0800usize;
        let specs: [(u32, usize, i32); 6] = [
            (0x0800, 64, -1),   // 0.5x mix rate -> ~9,998 Hz
            (0x1000, 96, 24),   // 1.0x mix rate -> ~19,997 Hz, looped
            (0x0C00, 80, -1),   // 0.75x mix rate -> ~14,998 Hz
            (0x0800, 112, 48),  // looped
            (0x1000, 64, -1),
            (0x0900, 128, -1),
        ];

        for (i, &(pitch_base, slen, loop_off)) in specs.iter().enumerate() {
            let hdr_off = cursor;
            rom[hdr_off..hdr_off + 4].copy_from_slice(&pitch_base.to_le_bytes());
            rom[hdr_off + 4..hdr_off + 8].copy_from_slice(&(slen as u32).to_le_bytes());
            rom[hdr_off + 8..hdr_off + 12].copy_from_slice(&loop_off.to_le_bytes());

            let pcm_start = hdr_off + KCEJ_SAMPLE_HEADER_SIZE;
            for k in 0..slen {
                let sample = (((k as i32 % 16) - 8) * 6) as i8;
                rom[pcm_start + k] = sample as u8;
            }

            ptrs.push(GBA_ROM_BUS_BASE + (hdr_off as u32));
            if i == 2 {
                ptrs.push(0); // Allowed NULL slot
            }

            cursor = (pcm_start + slen + 3) & !3;
        }

        // Write pointer table at 0x2000
        let table_off = 0x2000usize;
        for (i, ptr) in ptrs.iter().enumerate() {
            rom[table_off + i * 4..table_off + i * 4 + 4].copy_from_slice(&ptr.to_le_bytes());
        }

        rom
    }

    #[test]
    fn detects_and_scans_kceh_soundbank() {
        let rom = build_synthetic_kceh_rom(false);
        assert_eq!(detect_konami_engine(&rom), Some(KonamiEngineVariant::Kceh));

        let samples = scan_konami_soundpool(&rom);
        assert_eq!(samples.len(), 4);

        assert_eq!(samples[0].variant, KonamiEngineVariant::Kceh);
        assert_eq!(samples[0].size, 64);
        assert_eq!(samples[0].sample_rate, 11_025);
        assert_eq!(samples[0].root_key, 60);
        assert_eq!(samples[0].loop_start, None);
        assert_eq!(samples[0].pcm_data[0], -56);

        assert_eq!(samples[1].size, 96);
        assert_eq!(samples[1].sample_rate, 11_397);
        assert_eq!(samples[1].root_key, 64);
        assert_eq!(samples[1].loop_start, Some(32));

        assert_eq!(samples[3].size, 128);
        assert_eq!(samples[3].sample_rate, 16_000);
        assert_eq!(samples[3].root_key, 72);
        assert_eq!(samples[3].loop_start, Some(16));
    }

    #[test]
    fn dispatches_kceh_before_kcej_when_both_signatures_present() {
        let rom = build_synthetic_kceh_rom(true);
        assert_eq!(detect_konami_engine(&rom), Some(KonamiEngineVariant::Kceh));

        let samples = scan_konami_soundpool(&rom);
        assert_eq!(samples.len(), 4);
        assert!(samples.iter().all(|s| s.variant == KonamiEngineVariant::Kceh));
    }

    #[test]
    fn detects_and_scans_kcej_pointer_table() {
        let rom = build_synthetic_kcej_rom();
        assert_eq!(detect_konami_engine(&rom), Some(KonamiEngineVariant::Kcej));

        let samples = scan_konami_soundpool(&rom);
        assert_eq!(samples.len(), 6);

        assert_eq!(samples[0].variant, KonamiEngineVariant::Kcej);
        assert_eq!(samples[0].size, 64);
        assert_eq!(samples[0].sample_rate, 9_998);
        assert_eq!(samples[0].root_key, 60);
        assert_eq!(samples[0].loop_start, None);
        assert_eq!(samples[0].pcm_data[0], -48);

        assert_eq!(samples[1].size, 96);
        assert_eq!(samples[1].sample_rate, 19_997);
        assert_eq!(samples[1].loop_start, Some(24));

        assert_eq!(samples[3].size, 112);
        assert_eq!(samples[3].loop_start, Some(48));
    }

    #[test]
    fn exports_valid_wav16_and_soundfont_for_kceh_and_kcej() {
        for (rom, expected_count, prefix, title) in [
            (build_synthetic_kceh_rom(false), 4, "konami_kceh_sample_", "Synthetic KCEH"),
            (build_synthetic_kcej_rom(), 6, "konami_kcej_sample_", "Synthetic KCEJ"),
        ] {
            let wavs = export_konami_samples_wav16(&rom).expect("wav export should succeed");
            assert_eq!(wavs.len(), expected_count);
            for (idx, name, bytes) in &wavs {
                assert!(name.starts_with(&format!("{prefix}{idx:03}_")));
                assert!(name.ends_with(".wav"));
                assert_eq!(&bytes[0..4], b"RIFF");
                assert_eq!(&bytes[8..12], b"WAVE");
            }

            let sf2 = compile_konami_soundfont(&rom, Some(title))
                .expect("sf2 compilation should succeed");
            let validation = validate_soundfont(&sf2.sf2_bytes).expect("sf2 should validate");
            assert_eq!(validation.sample_count, expected_count);
            assert_eq!(validation.preset_count, expected_count);
            assert_eq!(validation.bank_name, title);
        }
    }

    #[test]
    fn rejects_non_konami_roms() {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"NONKONAMI00\0");
        // Even with a coincidental FIFO+DMA literal pool, no valid soundbank exists.
        rom[0x0300..0x030C].copy_from_slice(&[
            0xA0, 0x00, 0x00, 0x04, 0xA4, 0x00, 0x00, 0x04, 0xBC, 0x00, 0x00, 0x04,
        ]);

        assert_eq!(detect_konami_engine(&rom), None);
        assert!(scan_konami_soundpool(&rom).is_empty());
        assert!(export_konami_samples_wav16(&rom).is_err());
        assert!(compile_konami_soundfont(&rom, None).is_err());
    }

    #[test]
    fn detects_arm_mixer_revisions_via_konami_kce_crate() {
        // KCEH RelocBank32 ARM mixer signature without DMA1CNT literal (cross-studio builds)
        let mut kceh_rom = build_synthetic_kceh_rom(false);
        kceh_rom[0x0200..0x0204].fill(0); // strip DMA literal
        kceh_rom[0x0240..0x0250].copy_from_slice(&[
            0x18, 0x40, 0xD0, 0xE5, 0x19, 0x50, 0xD0, 0xE5, 0x00, 0x00, 0x54, 0xE3, 0x01, 0x40,
            0x84, 0x12,
        ]);
        assert_eq!(
            detect_konami_version(&kceh_rom),
            Some(KonamiKceVersion::KcehRelocBank32)
        );
        assert_eq!(
            detect_konami_engine(&kceh_rom),
            Some(KonamiEngineVariant::Kceh)
        );
        assert_eq!(scan_konami_soundpool(&kceh_rom).len(), 4);

        // KCEJ v2 Stereo LUT ARM mixer signature without DMA/IE literals
        let mut kcej_rom = build_synthetic_kcej_rom();
        kcej_rom[0x0200..0x0208].fill(0); // strip DMA + IE literals
        kcej_rom[0x0240..0x0244].copy_from_slice(&0xE199_B0D1u32.to_le_bytes());
        kcej_rom[0x0248..0x0250]
            .copy_from_slice(&[0x02, 0x30, 0x83, 0xE0, 0x23, 0x06, 0xB0, 0xE1]);
        assert_eq!(
            detect_konami_version(&kcej_rom),
            Some(KonamiKceVersion::KcejV2StereoLut)
        );
        assert_eq!(
            detect_konami_engine(&kcej_rom),
            Some(KonamiEngineVariant::Kcej)
        );
        assert_eq!(scan_konami_soundpool(&kcej_rom).len(), 6);

        // Family/variant conversions
        assert_eq!(
            KonamiEngineVariant::from(KonamiKceVersion::KcehRelocBank32),
            KonamiEngineVariant::Kceh
        );
        assert_eq!(
            KonamiEngineVariant::from(KonamiKceVersion::KcejV5StereoAsr7Bias),
            KonamiEngineVariant::Kcej
        );
        assert_eq!(
            KonamiKceFamily::from(KonamiEngineVariant::Kceh),
            KonamiKceFamily::Kceh
        );
        assert_eq!(
            KonamiKceFamily::from(KonamiEngineVariant::Kcej),
            KonamiKceFamily::Kcej
        );
    }
}
