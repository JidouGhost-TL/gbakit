//! Krawall / ProTracker MOD / Scream Tracker 3 sample pool scanning,
//! 8-bit PCM extraction, WAV16 export, and SoundFont 2.04 soundbank generation.
//!
//! Supports three tracker formats found in GBA ROMs:
//! 1. **Krawall Audio Engine** (`samples[]` ROM pointer tables to 18-byte `Sample`
//!    descriptors with inline unsigned 8-bit PCM and 64-byte unrolled loop/silence tails).
//! 2. **ProTracker MOD** (1084-byte `M.K.` / `M!K!` / `4CHN` / `6CHN` / `8CHN` / `FLT4` / `FLT8`
//!    modules with 31 sample descriptors and inline signed 8-bit PCM).
//! 3. **Scream Tracker 3 (S3M)** (`SCRM` / `SCRS` modules with parapointer-indexed
//!    8-bit PCM instruments).

#![deny(unsafe_code)]

use std::collections::HashSet;

use formatkit_core::{Error, Result};
pub use krawall::{
    KrawallLoopMode, KrawallSampleHeader, KrawallSampleView, KrawallVersion, KRAWALL_MIX_RATE,
};
use krawall::{
    GBA_ROM_BUS_BASE as ROM_BUS_START, KRAWALL_DEFAULT_C2_FREQ, KRAWALL_SAMPLE_HEADER_SIZE,
    KRAWALL_SILENCE_BYTE, KRAWALL_TAIL_SIZE,
};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// Standard ProTracker / S3M C2 playback rate in Hz.
pub const KRAWALL_DEFAULT_C2_RATE: u32 = KRAWALL_DEFAULT_C2_FREQ;

/// Maximum individual sample length in bytes accepted by the scanner.
const MAX_SAMPLE_BYTES: usize = 512 * 1024;

/// Source tracker format of a detected sample entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KrawallSampleFormat {
    /// Native Krawall `Sample` table entry (18-byte header + unsigned 8-bit PCM).
    KrawallPcm8,
    /// ProTracker MOD (`M.K.`, `4CHN`, `6CHN`, `8CHN`, `FLT4`, `FLT8`, `M!K!`) signed 8-bit PCM.
    ProTrackerMod,
    /// Scream Tracker 3 (`SCRM` / `SCRS`) 8-bit PCM sample.
    ScreamTracker3,
}

/// Extracted Krawall / ProTracker MOD / S3M PCM sample entry from a GBA ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KrawallSampleEntry {
    /// Sample index within the detected pool.
    pub index: usize,
    /// Absolute ROM file offset where the sample PCM payload begins.
    pub rom_offset: usize,
    /// 32-bit GBA bus address of the sample descriptor (Krawall) or PCM payload (MOD/S3M).
    pub bus_address: u32,
    /// Sample payload length in bytes (8-bit mono samples).
    pub size: usize,
    /// Playback sample rate in Hz (C2SPD / `c2Freq`).
    pub sample_rate: u32,
    /// Loop start offset in samples, if looped.
    pub loop_start: Option<u32>,
    /// Loop end offset in samples, if looped.
    pub loop_end: Option<u32>,
    /// Default tracker volume (`0..=64`).
    pub default_volume: u8,
    /// Detected tracker / engine format.
    pub format: KrawallSampleFormat,
    /// Optional sample name (populated for ProTracker MOD and S3M modules).
    pub name: String,
    /// Extracted 8-bit signed linear mono PCM samples (normalized to signed `i8`).
    pub pcm_data: Vec<i8>,
}

/// Fallback Krawall detection when `KrawallVersion::detect_from_bytes` returned `None`.
pub(crate) fn detect_krawall_fallback(rom: &[u8]) -> bool {
    if rom.len() < 32 {
        return false;
    }

    const ENGINE_SIGNATURES: &[&[u8]] = &[
        b"$Id: Krawall",
        b"Krawall Advance",
        b"Krawall GBA",
        b"Sebastian Kienzl",
        b"SEBASTIAN KIENZL",
    ];

    if ENGINE_SIGNATURES
        .iter()
        .any(|sig| find_subslice(rom, sig, 0).is_some())
    {
        return true;
    }

    !scan_krawall_soundpool(rom).is_empty()
}

/// Detect whether a GBA ROM uses the Krawall engine or embeds valid ProTracker MOD / S3M modules.
pub fn detect_krawall_engine(rom: &[u8]) -> bool {
    if rom.len() < 32 {
        return false;
    }

    KrawallVersion::detect_from_bytes(rom).is_some() || detect_krawall_fallback(rom)
}

/// Scan a GBA ROM for Krawall native sample tables, embedded ProTracker MOD modules,
/// and embedded Scream Tracker 3 (S3M) modules.
pub fn scan_krawall_soundpool(rom: &[u8]) -> Vec<KrawallSampleEntry> {
    if rom.len() < 32 {
        return Vec::new();
    }

    let mut samples = Vec::new();
    let mut seen_pcm_offsets = HashSet::new();

    scan_krawall_native_tables(rom, &mut samples, &mut seen_pcm_offsets);
    scan_protracker_mod_modules(rom, &mut samples, &mut seen_pcm_offsets);
    scan_s3m_modules(rom, &mut samples, &mut seen_pcm_offsets);
    if samples.is_empty() {
        scan_krawall_relocatable_samples(rom, &mut samples, &mut seen_pcm_offsets);
    }

    samples
}

/// Extract all Krawall / MOD / S3M samples from a ROM and encode them as standard
/// 16-bit uncompressed mono RIFF WAV files.
///
/// Returns a list of `(sample_index, recommended_filename, wav_bytes)`.
pub fn export_krawall_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_krawall_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_krawall_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!("krawall_sample_{:03}_{:06X}.wav", s.index, s.rom_offset);
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Krawall / ProTracker MOD / S3M samples into a standard
/// SoundFont 2.04 (`.sf2`) soundbank.
pub fn compile_krawall_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_krawall_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_krawall_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Krawall)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            let loop_range = s.loop_start.zip(s.loop_end);
            let label = if s.name.is_empty() {
                format!("KRW_{:03}", s.index)
            } else {
                format!("KRW_{:03}_{}", s.index, s.name)
            };
            RawSoundFontSample {
                name: label,
                sample_rate: s.sample_rate,
                root_key: 60,
                loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(&bank_title, "Krawall / ProTracker MOD", &raw_samples)
}

/// Validate a candidate 18-byte native Krawall `Sample` descriptor at `off`
/// using [`krawall::KrawallSampleView::parse_from_rom`].
fn parse_krawall_sample_view<'a>(rom: &'a [u8], off: usize) -> Option<KrawallSampleView<'a>> {
    let view = KrawallSampleView::parse_from_rom(0, rom, off).ok()?;

    if !(1_000..=65_535).contains(&view.c2_freq)
        || !(16..=MAX_SAMPLE_BYTES).contains(&view.len())
        || view.flags > 1
    {
        return None;
    }

    // Verify Krawall's unrolled loop or 0x80 silence buffer immediately following `end_ptr`.
    if view.tail_u8.len() >= 8 && !view.has_valid_tail() {
        return None;
    }

    Some(view)
}

/// Scan 4-byte-aligned ROM pointer tables (`const Sample * const samples[]`) for Krawall samples.
fn scan_krawall_native_tables(
    rom: &[u8],
    out: &mut Vec<KrawallSampleEntry>,
    seen_pcm_offsets: &mut HashSet<usize>,
) {
    let max_bus_ptr = ROM_BUS_START.saturating_add(rom.len() as u32);
    let mut i = 0usize;

    while i + 16 <= rom.len() {
        let p0 = read_u32_le(rom, i);
        if p0 >= ROM_BUS_START
            && p0 < max_bus_ptr
            && parse_krawall_sample_view(rom, (p0 - ROM_BUS_START) as usize).is_some()
        {
            let mut j = i;
            let mut valid_ptrs: Vec<u32> = Vec::new();
            let mut null_run = 0usize;

            while j + 4 <= rom.len() {
                let pj = read_u32_le(rom, j);
                if pj == 0 {
                    null_run += 1;
                    if null_run > 8 {
                        break;
                    }
                    j += 4;
                    continue;
                }
                if pj >= ROM_BUS_START
                    && pj < max_bus_ptr
                    && parse_krawall_sample_view(rom, (pj - ROM_BUS_START) as usize).is_some()
                {
                    null_run = 0;
                    valid_ptrs.push(pj);
                    j += 4;
                } else {
                    break;
                }
            }

            // Require at least 4 valid Krawall Sample pointers in a table to reject coincidental hits.
            if valid_ptrs.len() >= 4 {
                for ptr in valid_ptrs {
                    let hdr_off = (ptr - ROM_BUS_START) as usize;
                    if let Some(view) = parse_krawall_sample_view(rom, hdr_off) {
                        let pcm_offset = hdr_off + KRAWALL_SAMPLE_HEADER_SIZE;
                        if seen_pcm_offsets.insert(pcm_offset) {
                            let pcm_data: Vec<i8> =
                                (0..view.len()).map(|k| view.sample_i8(k)).collect();
                            let loop_start = view.loop_start().map(|v| v as u32);
                            let loop_end = view.loop_end().map(|v| v as u32);

                            let idx = out.len();
                            out.push(KrawallSampleEntry {
                                index: idx,
                                rom_offset: pcm_offset,
                                bus_address: ptr,
                                size: view.len(),
                                sample_rate: view.c2_freq,
                                loop_start,
                                loop_end,
                                default_volume: view.vol_default,
                                format: KrawallSampleFormat::KrawallPcm8,
                                name: String::new(),
                                pcm_data,
                            });
                        }
                    }
                }
                i = j;
                continue;
            }
        }
        i += 4;
    }
}

/// Scan a GBA ROM for embedded standard 31-sample ProTracker MOD modules.
fn scan_protracker_mod_modules(
    rom: &[u8],
    out: &mut Vec<KrawallSampleEntry>,
    seen_pcm_offsets: &mut HashSet<usize>,
) {
    const MOD_TAGS: &[(&[u8; 4], usize)] = &[
        (b"M.K.", 4),
        (b"M!K!", 4),
        (b"4CHN", 4),
        (b"6CHN", 6),
        (b"8CHN", 8),
        (b"FLT4", 4),
        (b"FLT8", 8),
    ];

    for &(tag, channels) in MOD_TAGS {
        let mut search_from = 1080usize;
        while let Some(tag_off) = find_subslice(rom, tag, search_from) {
            search_from = tag_off + 4;
            let base = tag_off - 1080;

            let song_len = rom[base + 950] as usize;
            if song_len == 0 || song_len > 128 {
                continue;
            }

            let orders = &rom[base + 952..base + 1080];
            if orders[..song_len].iter().any(|&o| o > 127) {
                continue;
            }

            let mut headers = Vec::with_capacity(31);
            let mut valid = true;
            let mut total_sample_bytes = 0usize;

            for s in 0..31 {
                let sh = base + 20 + s * 30;
                let len_bytes = (u16::from_be_bytes([rom[sh + 22], rom[sh + 23]]) as usize) * 2;
                let finetune = rom[sh + 24];
                let volume = rom[sh + 25];
                let rep_start = (u16::from_be_bytes([rom[sh + 26], rom[sh + 27]]) as usize) * 2;
                let rep_len = (u16::from_be_bytes([rom[sh + 28], rom[sh + 29]]) as usize) * 2;

                if finetune > 15 || volume > 64 {
                    valid = false;
                    break;
                }
                if len_bytes > 0 && rep_start > len_bytes {
                    valid = false;
                    break;
                }

                total_sample_bytes = total_sample_bytes.saturating_add(len_bytes);
                let name = clean_ascii_name(&rom[sh..sh + 22]);
                headers.push((name, len_bytes, finetune, volume, rep_start, rep_len));
            }

            if !valid || total_sample_bytes == 0 {
                continue;
            }

            let max_pattern = orders[..song_len].iter().copied().max().unwrap_or(0) as usize;
            let num_patterns = max_pattern + 1;
            let pattern_bytes = num_patterns * 64 * channels * 4;
            let Some(sample_data_start) = (base + 1084).checked_add(pattern_bytes) else {
                continue;
            };
            let Some(sample_data_end) = sample_data_start.checked_add(total_sample_bytes) else {
                continue;
            };
            if sample_data_end > rom.len() {
                continue;
            }

            let mut cursor = sample_data_start;
            for (name, len_bytes, finetune, volume, rep_start, rep_len) in headers {
                if len_bytes == 0 {
                    continue;
                }
                let pcm_off = cursor;
                cursor += len_bytes;

                if seen_pcm_offsets.insert(pcm_off) {
                    let pcm_data: Vec<i8> = rom[pcm_off..pcm_off + len_bytes]
                        .iter()
                        .map(|&b| b as i8)
                        .collect();
                    let (loop_start, loop_end) = if rep_len > 2 && rep_start + rep_len <= len_bytes
                    {
                        (Some(rep_start as u32), Some((rep_start + rep_len) as u32))
                    } else {
                        (None, None)
                    };

                    let idx = out.len();
                    out.push(KrawallSampleEntry {
                        index: idx,
                        rom_offset: pcm_off,
                        bus_address: ROM_BUS_START.saturating_add(pcm_off as u32),
                        size: len_bytes,
                        sample_rate: mod_finetune_rate(finetune),
                        loop_start,
                        loop_end,
                        default_volume: volume,
                        format: KrawallSampleFormat::ProTrackerMod,
                        name,
                        pcm_data,
                    });
                }
            }
        }
    }
}

/// Scan a GBA ROM for embedded Scream Tracker 3 (`SCRM` / `SCRS`) modules.
fn scan_s3m_modules(
    rom: &[u8],
    out: &mut Vec<KrawallSampleEntry>,
    seen_pcm_offsets: &mut HashSet<usize>,
) {
    let mut search_from = 44usize;
    while let Some(sig_off) = find_subslice(rom, b"SCRM", search_from) {
        search_from = sig_off + 4;
        let base = sig_off - 44;

        if base + 96 > rom.len() || rom[base + 28] != 0x1A || rom[base + 29] != 0x10 {
            continue;
        }

        let ord_num = u16::from_le_bytes([rom[base + 32], rom[base + 33]]) as usize;
        let ins_num = u16::from_le_bytes([rom[base + 34], rom[base + 35]]) as usize;
        let ffi = u16::from_le_bytes([rom[base + 42], rom[base + 43]]);

        if ins_num == 0 || ins_num > 255 || ord_num > 256 || (ffi != 1 && ffi != 2) {
            continue;
        }

        let ins_ptr_off = base + 96 + ord_num;
        if ins_ptr_off + ins_num * 2 > rom.len() {
            continue;
        }

        for i in 0..ins_num {
            let para =
                u16::from_le_bytes([rom[ins_ptr_off + i * 2], rom[ins_ptr_off + i * 2 + 1]])
                    as usize;
            let ioff = base + para * 16;
            if ioff + 80 > rom.len() {
                continue;
            }
            if rom[ioff] != 1 || &rom[ioff + 76..ioff + 80] != b"SCRS" {
                continue;
            }
            let pack = rom[ioff + 30];
            if pack != 0 {
                continue;
            }

            let memseg_hi = rom[ioff + 13] as usize;
            let memseg_lo = u16::from_le_bytes([rom[ioff + 14], rom[ioff + 15]]) as usize;
            let memseg = (memseg_hi << 16) | memseg_lo;
            let length = read_u32_le(rom, ioff + 16) as usize;
            let loop_begin = read_u32_le(rom, ioff + 20) as usize;
            let loop_end_raw = read_u32_le(rom, ioff + 24) as usize;
            let volume = rom[ioff + 28].min(64);
            let sflags = rom[ioff + 31];
            let c2spd = read_u32_le(rom, ioff + 32);

            if length == 0 || length > MAX_SAMPLE_BYTES {
                continue;
            }
            // Skip stereo or 16-bit S3M samples for 8-bit mono PCM extractor.
            if (sflags & 0x06) != 0 {
                continue;
            }

            let Some(pcm_off) = base.checked_add(memseg * 16) else {
                continue;
            };
            if pcm_off + length > rom.len() {
                continue;
            }

            if seen_pcm_offsets.insert(pcm_off) {
                let raw = &rom[pcm_off..pcm_off + length];
                let pcm_data: Vec<i8> = if ffi == 2 {
                    raw.iter().map(|&b| (b ^ KRAWALL_SILENCE_BYTE) as i8).collect()
                } else {
                    raw.iter().map(|&b| b as i8).collect()
                };

                let (loop_start, loop_end) =
                    if (sflags & 1) != 0 && loop_begin < loop_end_raw && loop_end_raw <= length {
                        (Some(loop_begin as u32), Some(loop_end_raw as u32))
                    } else {
                        (None, None)
                    };

                let sample_rate = if (1_000..=65_535).contains(&c2spd) {
                    c2spd
                } else {
                    KRAWALL_DEFAULT_C2_RATE
                };

                let idx = out.len();
                out.push(KrawallSampleEntry {
                    index: idx,
                    rom_offset: pcm_off,
                    bus_address: ROM_BUS_START.saturating_add(pcm_off as u32),
                    size: length,
                    sample_rate,
                    loop_start,
                    loop_end,
                    default_volume: volume,
                    format: KrawallSampleFormat::ScreamTracker3,
                    name: clean_ascii_name(&rom[ioff + 48..ioff + 76]),
                    pcm_data,
                });
            }
        }
    }
}

/// Fallback scanner for relocatable Krawall sample banks where the 18-byte
/// `KrawallSampleHeader::end_ptr` stores a relative PCM byte length (`< 0x0800_0000`)
/// instead of an absolute GBA ROM bus pointer, followed immediately by inline
/// unsigned 8-bit PCM and Krawall's 64-byte unrolled loop or `0x80` silence tail.
fn scan_krawall_relocatable_samples(
    rom: &[u8],
    out: &mut Vec<KrawallSampleEntry>,
    seen_pcm_offsets: &mut HashSet<usize>,
) {
    if rom.len() < KRAWALL_SAMPLE_HEADER_SIZE + 16 + KRAWALL_TAIL_SIZE {
        return;
    }

    let mut found: Vec<(usize, KrawallSampleHeader, usize)> = Vec::new();
    let mut off = 0usize;

    while off + KRAWALL_SAMPLE_HEADER_SIZE + 16 + KRAWALL_TAIL_SIZE <= rom.len() {
        let Ok(hdr) = KrawallSampleHeader::parse(&rom[off..off + KRAWALL_SAMPLE_HEADER_SIZE])
        else {
            off += 4;
            continue;
        };

        let length = hdr.end_ptr as usize;
        let loop_mode_matches = (hdr.loop_mode == KrawallLoopMode::None) == (hdr.loop_length == 0);
        if hdr.end_ptr >= ROM_BUS_START
            || !(16..=MAX_SAMPLE_BYTES).contains(&length)
            || !(1_000..=65_535).contains(&hdr.c2_freq)
            || !(-64..=64).contains(&hdr.pan_default)
            || hdr.flags > 1
            || !loop_mode_matches
            || (hdr.loop_length as usize) > length
        {
            off += 4;
            continue;
        }

        let pcm_start = off + KRAWALL_SAMPLE_HEADER_SIZE;
        let Some(pcm_end) = pcm_start.checked_add(length) else {
            off += 4;
            continue;
        };
        let Some(tail_end) = pcm_end.checked_add(KRAWALL_TAIL_SIZE) else {
            off += 4;
            continue;
        };
        if tail_end > rom.len() {
            off += 4;
            continue;
        }

        let pcm_u8 = &rom[pcm_start..pcm_end];
        let tail_u8 = &rom[pcm_end..tail_end];
        let is_silence_tail = tail_u8.iter().all(|&b| b == KRAWALL_SILENCE_BYTE);
        let valid_tail = if hdr.loop_length == 0 {
            is_silence_tail
        } else if is_silence_tail {
            true
        } else {
            let loop_len = hdr.loop_length as usize;
            let loop_start = length - loop_len;
            tail_u8
                .iter()
                .enumerate()
                .all(|(k, &b)| b == pcm_u8[loop_start + (k % loop_len)])
        };

        if valid_tail {
            found.push((off, hdr, length));
            // Advance past this sample + tail, keeping 4-byte alignment.
            let next = tail_end & !3;
            off = next.max(off + 4);
        } else {
            off += 4;
        }
    }

    // Require at least 4 valid relocatable Krawall samples to reject coincidental hits.
    if found.len() < 4 {
        return;
    }

    for (hdr_off, hdr, length) in found {
        let pcm_offset = hdr_off + KRAWALL_SAMPLE_HEADER_SIZE;
        if !seen_pcm_offsets.insert(pcm_offset) {
            continue;
        }
        let pcm_data: Vec<i8> = rom[pcm_offset..pcm_offset + length]
            .iter()
            .map(|&b| (b ^ KRAWALL_SILENCE_BYTE) as i8)
            .collect();
        let (loop_start, loop_end) = if hdr.loop_length > 0 {
            (
                Some((length - hdr.loop_length as usize) as u32),
                Some(length as u32),
            )
        } else {
            (None, None)
        };

        let idx = out.len();
        out.push(KrawallSampleEntry {
            index: idx,
            rom_offset: pcm_offset,
            bus_address: ROM_BUS_START.saturating_add(hdr_off as u32),
            size: length,
            sample_rate: hdr.c2_freq,
            loop_start,
            loop_end,
            default_volume: hdr.vol_default,
            format: KrawallSampleFormat::KrawallPcm8,
            name: String::new(),
            pcm_data,
        });
    }
}

/// Convert a ProTracker 4-bit signed finetune value (`0..=15`) to a C2 sample rate in Hz.
fn mod_finetune_rate(finetune: u8) -> u32 {
    let signed_ft: i32 = if finetune >= 8 {
        (finetune as i32) - 16
    } else {
        finetune as i32
    };
    if signed_ft == 0 {
        return KRAWALL_DEFAULT_C2_RATE;
    }
    let scale = 2.0_f64.powf((signed_ft as f64) / 96.0);
    ((KRAWALL_DEFAULT_C2_RATE as f64) * scale).round() as u32
}

/// Extract a printable, null-terminated ASCII identifier from a fixed-width tracker field.
fn clean_ascii_name(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    bytes[..end]
        .iter()
        .filter(|&&b| (0x20..=0x7E).contains(&b))
        .map(|&b| b as char)
        .collect::<String>()
        .trim()
        .to_string()
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

    /// Build a synthetic GBA ROM containing a native Krawall `samples[]` pointer table
    /// with 4 samples (including one looped sample and a NULL gap slot).
    fn build_synthetic_krawall_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x4000];
        // GBA ROM title at 0xA0..0xAC
        rom[0xA0..0xAC].copy_from_slice(b"KRAWALLTEST\0");

        let mut ptrs = Vec::new();
        let mut cursor = 0x0400usize;

        for idx in 0..4usize {
            let pcm_len = 64 + idx * 32;
            let looped = idx == 2;
            let loop_len = if looped { 32usize } else { 0usize };
            let c2_freq: u32 = if idx % 2 == 0 { 16_000 } else { 8_363 };
            let vol: u8 = 48 + (idx as u8) * 4;

            let hdr_off = cursor;
            let pcm_start = hdr_off + KRAWALL_SAMPLE_HEADER_SIZE;
            let pcm_end = pcm_start + pcm_len;
            let end_ptr = ROM_BUS_START + (pcm_end as u32);

            rom[hdr_off..hdr_off + 4].copy_from_slice(&(loop_len as u32).to_le_bytes());
            rom[hdr_off + 4..hdr_off + 8].copy_from_slice(&end_ptr.to_le_bytes());
            rom[hdr_off + 8..hdr_off + 12].copy_from_slice(&c2_freq.to_le_bytes());
            rom[hdr_off + 12..hdr_off + 14].copy_from_slice(&0u16.to_le_bytes());
            rom[hdr_off + 14] = vol;
            rom[hdr_off + 15] = 0;
            rom[hdr_off + 16] = u8::from(looped);
            rom[hdr_off + 17] = 0;

            // Write unsigned 8-bit PCM ramp centered at 0x80
            for k in 0..pcm_len {
                let val = 128i32 + ((k as i32 % 16) - 8) * 6;
                rom[pcm_start + k] = val as u8;
            }

            // Write Krawall 64-byte unrolled loop or 0x80 silence tail
            if looped {
                let loop_src = pcm_end - loop_len;
                for k in 0..64 {
                    rom[pcm_end + k] = rom[loop_src + (k % loop_len)];
                }
            } else {
                rom[pcm_end..pcm_end + 64].fill(0x80);
            }

            ptrs.push(ROM_BUS_START + (hdr_off as u32));
            if idx == 1 {
                ptrs.push(0); // Null slot between valid entries
            }

            cursor = (pcm_end + 64 + 3) & !3;
        }

        // Write pointer table at 0x2000
        let table_off = 0x2000usize;
        for (i, ptr) in ptrs.iter().enumerate() {
            rom[table_off + i * 4..table_off + i * 4 + 4].copy_from_slice(&ptr.to_le_bytes());
        }

        // Embed Krawall RCS version tag right after pointer table
        let id_off = table_off + ptrs.len() * 4;
        let tag = b"$Id: Krawall $Date: 2003/09/01 06:51:01 $\0";
        rom[id_off..id_off + tag.len()].copy_from_slice(tag);

        rom
    }

    /// Build a synthetic GBA ROM containing a valid 4-channel ProTracker `M.K.` module.
    fn build_synthetic_mod_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x3000];
        rom[0xA0..0xAC].copy_from_slice(b"MODTESTROM\0\0");

        let base = 0x0200usize;
        rom[base..base + 9].copy_from_slice(b"TEST SONG");

        // Sample 0: 64 bytes, unlooped, volume 64, finetune 0
        let sh0 = base + 20;
        rom[sh0..sh0 + 4].copy_from_slice(b"KICK");
        rom[sh0 + 22..sh0 + 24].copy_from_slice(&32u16.to_be_bytes()); // 32 words = 64 bytes
        rom[sh0 + 24] = 0;
        rom[sh0 + 25] = 64;

        // Sample 1: 128 bytes, looped (start=16, len=64), volume 48, finetune 0
        let sh1 = base + 20 + 30;
        rom[sh1..sh1 + 4].copy_from_slice(b"LEAD");
        rom[sh1 + 22..sh1 + 24].copy_from_slice(&64u16.to_be_bytes()); // 64 words = 128 bytes
        rom[sh1 + 24] = 0;
        rom[sh1 + 25] = 48;
        rom[sh1 + 26..sh1 + 28].copy_from_slice(&8u16.to_be_bytes()); // 16 bytes
        rom[sh1 + 28..sh1 + 30].copy_from_slice(&32u16.to_be_bytes()); // 64 bytes

        rom[base + 950] = 1; // 1 order
        rom[base + 951] = 127;
        rom[base + 952] = 0; // pattern 0
        rom[base + 1080..base + 1084].copy_from_slice(b"M.K.");

        // Pattern 0 is 64 * 4 * 4 = 1024 bytes; samples follow immediately
        let pcm_start = base + 1084 + 1024;
        for k in 0..64 {
            rom[pcm_start + k] = ((k as i8).wrapping_mul(3)) as u8;
        }
        for k in 0..128 {
            rom[pcm_start + 64 + k] = (((k as i32 % 32) - 16) * 4) as i8 as u8;
        }

        rom
    }

    /// Build a synthetic GBA ROM containing a valid Scream Tracker 3 (`SCRM`/`SCRS`) module.
    fn build_synthetic_s3m_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"S3MTESTROM\0\0");

        let base = 0x0200usize;
        rom[base..base + 4].copy_from_slice(b"ARYX");
        rom[base + 28] = 0x1A;
        rom[base + 29] = 0x10;
        rom[base + 32..base + 34].copy_from_slice(&2u16.to_le_bytes()); // ord_num
        rom[base + 34..base + 36].copy_from_slice(&1u16.to_le_bytes()); // ins_num
        rom[base + 36..base + 38].copy_from_slice(&1u16.to_le_bytes()); // pat_num
        rom[base + 42..base + 44].copy_from_slice(&2u16.to_le_bytes()); // ffi = 2 (unsigned PCM)
        rom[base + 44..base + 48].copy_from_slice(b"SCRM");

        // Instrument parapointer at base + 96 + 2 = base + 98 -> paragraph 0x10 (offset 0x100)
        rom[base + 98..base + 100].copy_from_slice(&0x0010u16.to_le_bytes());

        let ioff = base + 0x100;
        rom[ioff] = 1; // PCM sample
        rom[ioff + 13] = 0; // memseg hi
        rom[ioff + 14..ioff + 16].copy_from_slice(&0x0020u16.to_le_bytes()); // memseg lo -> +0x200
        rom[ioff + 16..ioff + 20].copy_from_slice(&96u32.to_le_bytes()); // length
        rom[ioff + 20..ioff + 24].copy_from_slice(&16u32.to_le_bytes()); // loop_begin
        rom[ioff + 24..ioff + 28].copy_from_slice(&96u32.to_le_bytes()); // loop_end
        rom[ioff + 28] = 60; // vol
        rom[ioff + 30] = 0; // unpacked
        rom[ioff + 31] = 1; // looped
        rom[ioff + 32..ioff + 36].copy_from_slice(&22_050u32.to_le_bytes()); // c2spd
        rom[ioff + 48..ioff + 56].copy_from_slice(b"SYNTH_01");
        rom[ioff + 76..ioff + 80].copy_from_slice(b"SCRS");

        let pcm_off = base + 0x200;
        for k in 0..96 {
            rom[pcm_off + k] = (128i32 + ((k as i32 % 16) - 8) * 5) as u8;
        }

        rom
    }

    #[test]
    fn scans_native_krawall_soundpool_and_converts_unsigned_pcm() {
        let rom = build_synthetic_krawall_rom();
        assert!(detect_krawall_engine(&rom));

        let samples = scan_krawall_soundpool(&rom);
        assert_eq!(samples.len(), 4);

        assert_eq!(samples[0].format, KrawallSampleFormat::KrawallPcm8);
        assert_eq!(samples[0].size, 64);
        assert_eq!(samples[0].sample_rate, 16_000);
        assert_eq!(samples[0].loop_start, None);
        assert_eq!(samples[0].loop_end, None);
        assert_eq!(samples[0].default_volume, 48);
        // First unsigned sample was 128 - 48 = 80 -> signed -48
        assert_eq!(samples[0].pcm_data[0], -48);

        assert_eq!(samples[2].size, 128);
        assert_eq!(samples[2].loop_start, Some(96));
        assert_eq!(samples[2].loop_end, Some(128));
    }

    #[test]
    fn scans_protracker_mod_module() {
        let rom = build_synthetic_mod_rom();
        assert!(detect_krawall_engine(&rom));

        let samples = scan_krawall_soundpool(&rom);
        assert_eq!(samples.len(), 2);

        assert_eq!(samples[0].format, KrawallSampleFormat::ProTrackerMod);
        assert_eq!(samples[0].name, "KICK");
        assert_eq!(samples[0].size, 64);
        assert_eq!(samples[0].sample_rate, KRAWALL_DEFAULT_C2_RATE);
        assert_eq!(samples[0].loop_start, None);

        assert_eq!(samples[1].name, "LEAD");
        assert_eq!(samples[1].size, 128);
        assert_eq!(samples[1].loop_start, Some(16));
        assert_eq!(samples[1].loop_end, Some(80));
        assert_eq!(samples[1].default_volume, 48);
    }

    #[test]
    fn scans_s3m_module() {
        let rom = build_synthetic_s3m_rom();
        assert!(detect_krawall_engine(&rom));

        let samples = scan_krawall_soundpool(&rom);
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].format, KrawallSampleFormat::ScreamTracker3);
        assert_eq!(samples[0].name, "SYNTH_01");
        assert_eq!(samples[0].size, 96);
        assert_eq!(samples[0].sample_rate, 22_050);
        assert_eq!(samples[0].loop_start, Some(16));
        assert_eq!(samples[0].loop_end, Some(96));
        assert_eq!(samples[0].pcm_data[0], -40);
    }

    #[test]
    fn exports_valid_wav16_and_soundfont() {
        let rom = build_synthetic_krawall_rom();
        let wavs = export_krawall_samples_wav16(&rom).expect("wav export should succeed");
        assert_eq!(wavs.len(), 4);
        for (idx, name, bytes) in &wavs {
            assert!(name.starts_with(&format!("krawall_sample_{idx:03}_")));
            assert!(name.ends_with(".wav"));
            assert_eq!(&bytes[0..4], b"RIFF");
            assert_eq!(&bytes[8..12], b"WAVE");
        }

        let sf2 = compile_krawall_soundfont(&rom, Some("Synthetic Krawall"))
            .expect("sf2 compile should succeed");
        let validation = validate_soundfont(&sf2.sf2_bytes).expect("sf2 should validate");
        assert_eq!(validation.sample_count, 4);
        assert_eq!(validation.preset_count, 4);
        assert_eq!(validation.bank_name, "Synthetic Krawall");
    }

    #[test]
    fn rejects_coincidental_german_krawall_ascii() {
        let mut rom = vec![0u8; 0x2000];
        let text = b"KRAWALLHANDGEMENGE\0";
        rom[0x0500..0x0500 + text.len()].copy_from_slice(text);

        assert!(!detect_krawall_engine(&rom));
        assert!(scan_krawall_soundpool(&rom).is_empty());
        assert!(export_krawall_samples_wav16(&rom).is_err());
        assert!(compile_krawall_soundfont(&rom, None).is_err());
    }

    #[test]
    fn scans_relocatable_krawall_samples_and_hq_flags() {
        let mut rom = vec![0u8; 0x4000];
        rom[0xA0..0xAC].copy_from_slice(b"KRAWALLRELO\0");

        let mut cursor = 0x0400usize;
        for idx in 0..4usize {
            let pcm_len = 64 + idx * 32;
            let looped = idx % 2 == 1;
            let loop_len = if looped { 32usize } else { 0usize };
            let c2_freq: u32 = 11_023;
            let flags: u8 = u8::from(idx == 3); // exercise HQ interpolation flag == 1

            let hdr_off = cursor;
            let pcm_start = hdr_off + KRAWALL_SAMPLE_HEADER_SIZE;
            let pcm_end = pcm_start + pcm_len;

            rom[hdr_off..hdr_off + 4].copy_from_slice(&(loop_len as u32).to_le_bytes());
            // Relative PCM length instead of 0x08xxxxxx absolute bus pointer
            rom[hdr_off + 4..hdr_off + 8].copy_from_slice(&(pcm_len as u32).to_le_bytes());
            rom[hdr_off + 8..hdr_off + 12].copy_from_slice(&c2_freq.to_le_bytes());
            rom[hdr_off + 12..hdr_off + 14].copy_from_slice(&0u16.to_le_bytes());
            rom[hdr_off + 14] = 64;
            rom[hdr_off + 15] = 0;
            rom[hdr_off + 16] = u8::from(looped);
            rom[hdr_off + 17] = flags;

            for k in 0..pcm_len {
                let val = 128i32 + ((k as i32 % 16) - 8) * 5;
                rom[pcm_start + k] = val as u8;
            }
            if looped {
                let loop_src = pcm_end - loop_len;
                for k in 0..KRAWALL_TAIL_SIZE {
                    rom[pcm_end + k] = rom[loop_src + (k % loop_len)];
                }
            } else {
                rom[pcm_end..pcm_end + KRAWALL_TAIL_SIZE].fill(KRAWALL_SILENCE_BYTE);
            }

            cursor = (pcm_end + KRAWALL_TAIL_SIZE + 15) & !15;
        }

        let samples = scan_krawall_soundpool(&rom);
        assert_eq!(samples.len(), 4);
        assert_eq!(samples[0].size, 64);
        assert_eq!(samples[0].loop_start, None);
        assert_eq!(samples[1].size, 96);
        assert_eq!(samples[1].loop_start, Some(64));
        assert_eq!(samples[1].loop_end, Some(96));
        assert_eq!(samples[3].size, 160);

        let sf2 = compile_krawall_soundfont(&rom, Some("Relocatable Krawall"))
            .expect("relocatable krawall sf2 should compile");
        let validation = validate_soundfont(&sf2.sf2_bytes).expect("sf2 should validate");
        assert_eq!(validation.sample_count, 4);
    }
}
