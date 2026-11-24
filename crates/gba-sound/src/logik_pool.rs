//! Logik State GBAModPlay sample pool scanning, signed 8-bit PCM and 4-bit IMA ADPCM
//! extraction, WAV16 export, and SoundFont 2.04 soundbank generation.
//!
//! Covers GBA titles built on Logik State's GBAModPlay tracker driver across both
//! sample-bank revisions:
//! 1. **v3.0b / v3.1 (`LogikSampleDescV3`, 24-byte descriptors)** with explicit per-sample
//!    native playback rates in Hz.
//! 2. **v2.x / early v3.0 (`LogikSampleDescV2`, 20-byte descriptors)** using the song module's
//!    `GBAMOD30` / `GBAMOD2.` mix rate (fallback 10,512 Hz).
//!
//! Supports both raw signed 8-bit linear PCM (`i8`) and low-nibble-first 4-bit IMA ADPCM
//! (automatically detected when the standard 89-entry 32-bit IMA step table is present in ROM
//! and a descriptor has `loop_start == 0 && loop_len == 1`).

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use logik_state::{
    LogikModHeader, LogikSampleDescV2, LogikSampleDescV3, LogikSampleView, LogikVersion,
    GBAMOD1_MAGIC, GBAMOD2_MAGIC, GBAMOD30_MAGIC, LOGIK_DEFAULT_MIX_RATE_HZ, LOGIK_DESC_V2_SIZE,
    LOGIK_DESC_V3_SIZE,
};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// Minimum accepted encoded sample length in bytes.
const MIN_SAMPLE_BYTES: usize = 16;

/// Maximum accepted encoded sample length in bytes (`512 KiB`).
const MAX_SAMPLE_BYTES: usize = 0x0008_0000;

/// Minimum number of samples required for a valid contiguous Logik State sample bank.
const MIN_BANK_SAMPLES: usize = 3;

/// Maximum number of samples accepted in a single Logik State sample bank.
const MAX_BANK_SAMPLES: usize = 1024;

/// Minimum and maximum plausible native playback sample rates in Hz.
const MIN_SAMPLE_RATE_HZ: u32 = 4_000;
const MAX_SAMPLE_RATE_HZ: u32 = 48_000;

/// ASCII driver credit string embedded in Logik State GBAModPlay builds.
const LOGIK_CREDIT_SIG: &[u8] = b"GBAModPlay";

/// First 8 entries of the 32-bit little-endian IMA ADPCM step table (`7..=14`).
const IMA_STEP_SIG_U32: [u8; 32] = [
    7, 0, 0, 0, 8, 0, 0, 0, 9, 0, 0, 0, 10, 0, 0, 0, 11, 0, 0, 0, 12, 0, 0, 0, 13, 0, 0, 0, 14, 0,
    0, 0,
];

/// Logik State GBAModPlay sample-bank revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogikEngineVariant {
    /// v2.x / early v3.0 (20-byte `LogikSampleDescV2` descriptors).
    V2,
    /// v3.0b / v3.1 (24-byte `LogikSampleDescV3` descriptors with explicit Hz).
    V3,
}

/// Sample encoding format used by a Logik State GBAModPlay sample entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogikSampleCodec {
    /// Raw signed 8-bit linear PCM (`i8`).
    Pcm8,
    /// Low-nibble-first 4-bit IMA ADPCM decoded to signed 8-bit PCM.
    ImaAdpcm4,
}

/// Extracted Logik State GBAModPlay sample entry from a GBA ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogikSampleEntry {
    /// Sample index within the detected sample bank.
    pub index: usize,
    /// Detected GBAModPlay sample-bank revision (`V2` or `V3`).
    pub variant: LogikEngineVariant,
    /// Source sample encoding codec (`Pcm8` or `ImaAdpcm4`).
    pub codec: LogikSampleCodec,
    /// Absolute ROM file offset where the sample descriptor begins.
    pub rom_offset: usize,
    /// Absolute ROM file offset where the encoded sample payload begins.
    pub data_offset: usize,
    /// Decoded sample length in 8-bit mono PCM samples.
    pub size: usize,
    /// Native playback sample rate in Hz (`4000..=48000`).
    pub sample_rate: u32,
    /// MIDI root key (`0..=127`; `60` = Middle C / C4).
    pub root_key: u8,
    /// Default sample volume extracted from bits `16..=23` of `vol_flags` (typically `64`).
    pub default_volume: u8,
    /// Loop start offset in samples, if looped.
    pub loop_start: Option<u32>,
    /// Loop end offset in samples, if looped.
    pub loop_end: Option<u32>,
    /// Decoded signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Decode low-nibble-first 4-bit IMA ADPCM bytes into signed 8-bit linear PCM (`i8`).
///
/// Delegates to [`logik_state::decode_ima_adpcm4_standard_to_pcm8`].
pub fn decode_ima_adpcm4_to_pcm8(raw: &[u8]) -> Vec<i8> {
    let mut out = vec![0i8; raw.len().saturating_mul(2)];
    let n = logik_state::decode_ima_adpcm4_standard_to_pcm8(raw, &mut out);
    out.truncate(n);
    out
}

/// Decode 4-bit IMA ADPCM using the exact Logik State `V3EarlyVol64Adpcm` ARM7TDMI
/// high-nibble-first algorithm (*Super Puzzle Fighter II*, `AZ8E`/`AZ8P`).
///
/// Delegates to [`logik_state::decode_ima_adpcm4_arm_to_pcm8`] at unity 8-bit scale (`volume = 256`).
pub fn decode_ima_adpcm4_arm_to_pcm8(raw: &[u8]) -> Vec<i8> {
    let mut out = vec![0i8; raw.len().saturating_mul(2)];
    let n = logik_state::decode_ima_adpcm4_arm_to_pcm8(raw, 256, &mut out);
    out.truncate(n);
    out
}

/// Detect whether a GBA ROM uses the Logik State GBAModPlay audio engine.
///
/// Matches the `"GBAModPlay"` ASCII driver credit, the 8-byte `"GBAMOD30"` /
/// `"GBAMOD2."` / `"GBAMOD1."` song module magic headers, or any verified
/// Logik State ARM7TDMI mixer revision via [`logik_state::LogikVersion`].
pub fn detect_logik_state(rom: &[u8]) -> bool {
    if rom.len() < GBAMOD30_MAGIC.len() {
        return false;
    }

    find_subslice(rom, LOGIK_CREDIT_SIG, 0).is_some()
        || find_subslice(rom, GBAMOD30_MAGIC, 0).is_some()
        || find_subslice(rom, GBAMOD2_MAGIC, 0).is_some()
        || find_subslice(rom, GBAMOD1_MAGIC, 0).is_some()
        || LogikVersion::detect_from_bytes(rom).is_some()
}

/// Scan a GBA ROM for a Logik State GBAModPlay sample bank and extract all
/// PCM8 / 4-bit IMA ADPCM samples.
///
/// Checks 24-byte `LogikSampleDescV3` banks before falling back to 20-byte
/// `LogikSampleDescV2` banks. Returns an empty vector if no Logik State signature
/// or valid contiguous sample bank is found.
pub fn scan_logik_soundpool(rom: &[u8]) -> Vec<LogikSampleEntry> {
    if !detect_logik_state(rom) {
        return Vec::new();
    }

    let default_rate = infer_default_rate(rom);
    let version = LogikVersion::detect_from_bytes(rom);
    let use_arm_adpcm = version.is_some_and(LogikVersion::has_ima_adpcm_mixer);
    let has_ima_table = use_arm_adpcm || find_subslice(rom, &IMA_STEP_SIG_U32, 0).is_some();

    let v3 = scan_v3_bank(rom, default_rate, has_ima_table, use_arm_adpcm);
    if !v3.is_empty() {
        return v3;
    }

    scan_v2_bank(rom, default_rate, has_ima_table, use_arm_adpcm)
}

/// Extract all Logik State GBAModPlay samples from a ROM and encode them as standard
/// 16-bit uncompressed mono RIFF WAV files.
///
/// Returns a list of `(sample_index, recommended_filename, wav_bytes)`.
pub fn export_logik_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_logik_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_logik_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let codec_tag = match s.codec {
            LogikSampleCodec::Pcm8 => "pcm8",
            LogikSampleCodec::ImaAdpcm4 => "ima4",
        };
        let name = format!(
            "logik_sample_{:03}_{:06X}_{codec_tag}.wav",
            s.index, s.data_offset
        );
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Logik State GBAModPlay samples into a standard
/// SoundFont 2.04 (`.sf2`) soundbank.
pub fn compile_logik_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_logik_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_logik_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Logik State GBAModPlay)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            let loop_range = s.loop_start.zip(s.loop_end);
            RawSoundFontSample {
                name: format!("LOGIK_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(&bank_title, "Logik State GBAModPlay", &raw_samples)
}

/// Infer the default mix rate in Hz from the first `GBAMOD30`, `GBAMOD2.`, or `GBAMOD1.`
/// song module header via [`LogikModHeader::parse`], falling back to `10_512 Hz`.
fn infer_default_rate(rom: &[u8]) -> u32 {
    for magic in [GBAMOD30_MAGIC, GBAMOD2_MAGIC, GBAMOD1_MAGIC] {
        if let Some(idx) = find_subslice(rom, magic, 0) {
            if let Ok(hdr) = LogikModHeader::parse(&rom[idx..]) {
                if (MIN_SAMPLE_RATE_HZ..=MAX_SAMPLE_RATE_HZ).contains(&hdr.mix_rate_hz) {
                    return hdr.mix_rate_hz;
                }
            }
        }
    }

    LOGIK_DEFAULT_MIX_RATE_HZ
}

/// Pass 1: scan for a 24-byte `LogikSampleDescV3` contiguous sample bank.
fn scan_v3_bank(
    rom: &[u8],
    default_rate: u32,
    has_ima_table: bool,
    use_arm_adpcm: bool,
) -> Vec<LogikSampleEntry> {
    let rlen = rom.len();
    let min_bank_span = 4 + LOGIK_DESC_V3_SIZE * MIN_BANK_SAMPLES;
    if rlen < min_bank_span {
        return Vec::new();
    }

    let mut off = 0usize;
    while off + min_bank_span <= rlen {
        let n = read_u32_le(rom, off) as usize;
        if !(MIN_BANK_SAMPLES..=MAX_BANK_SAMPLES).contains(&n) {
            off += 4;
            continue;
        }

        let Some(dbase) = off
            .checked_add(4)
            .and_then(|v| v.checked_add(n.saturating_mul(LOGIK_DESC_V3_SIZE)))
        else {
            off += 4;
            continue;
        };
        if dbase > rlen {
            off += 4;
            continue;
        }

        let Ok(d0) = LogikSampleDescV3::parse(&rom[off + 4..]) else {
            off += 4;
            continue;
        };
        let Ok(d1) = LogikSampleDescV3::parse(&rom[off + 4 + LOGIK_DESC_V3_SIZE..]) else {
            off += 4;
            continue;
        };
        let Ok(d2) = LogikSampleDescV3::parse(&rom[off + 4 + 2 * LOGIK_DESC_V3_SIZE..]) else {
            off += 4;
            continue;
        };

        let (o0, l0) = (d0.data_offset as usize, d0.length as usize);
        let (o1, l1) = (d1.data_offset as usize, d1.length as usize);
        let (o2, l2) = (d2.data_offset as usize, d2.length as usize);

        if o0 != 0
            || l0 > MAX_SAMPLE_BYTES
            || o1 != l0
            || l1 > MAX_SAMPLE_BYTES
            || !(MIN_SAMPLE_RATE_HZ..=MAX_SAMPLE_RATE_HZ).contains(&d1.sample_rate_hz)
            || o2 != o1.saturating_add(l1)
            || l2 > MAX_SAMPLE_BYTES
            || !(MIN_SAMPLE_RATE_HZ..=MAX_SAMPLE_RATE_HZ).contains(&d2.sample_rate_hz)
            || l0.saturating_add(l1).saturating_add(l2) < 64
        {
            off += 4;
            continue;
        }

        let last_off = off + 4 + (n - 1) * LOGIK_DESC_V3_SIZE;
        let Ok(dlast) = LogikSampleDescV3::parse(&rom[last_off..]) else {
            off += 4;
            continue;
        };
        let last_end = dbase
            .checked_add(dlast.data_offset as usize)
            .and_then(|v| v.checked_add(dlast.length as usize));
        if !last_end.is_some_and(|end| end <= rlen) {
            off += 4;
            continue;
        }

        let payload = &rom[dbase..];
        let mut results = Vec::new();
        for i in 0..n {
            let eoff = off + 4 + i * LOGIK_DESC_V3_SIZE;
            let Ok(desc) = LogikSampleDescV3::parse(&rom[eoff..]) else {
                continue;
            };
            if (desc.length as usize) < MIN_SAMPLE_BYTES {
                continue;
            }
            let Ok(view) = LogikSampleView::from_v3_desc(&desc, payload) else {
                continue;
            };
            let Some(data_offset) = dbase.checked_add(desc.data_offset as usize) else {
                continue;
            };
            let sample_rate = if (MIN_SAMPLE_RATE_HZ..=MAX_SAMPLE_RATE_HZ).contains(&desc.sample_rate_hz) {
                desc.sample_rate_hz
            } else {
                default_rate
            };

            results.push(build_sample_entry(
                results.len(),
                LogikEngineVariant::V3,
                eoff,
                data_offset,
                view.pcm_bytes,
                desc.default_volume(),
                desc.loop_start,
                desc.loop_len,
                sample_rate,
                has_ima_table,
                use_arm_adpcm,
            ));
        }

        if !results.is_empty() {
            return results;
        }

        off += 4;
    }

    Vec::new()
}

/// Pass 2: scan for a 20-byte `LogikSampleDescV2` contiguous sample bank.
fn scan_v2_bank(
    rom: &[u8],
    default_rate: u32,
    has_ima_table: bool,
    use_arm_adpcm: bool,
) -> Vec<LogikSampleEntry> {
    let rlen = rom.len();
    let min_bank_span = 4 + LOGIK_DESC_V2_SIZE * MIN_BANK_SAMPLES;
    if rlen < min_bank_span {
        return Vec::new();
    }

    let mut off = 0usize;
    while off + min_bank_span <= rlen {
        let n = read_u32_le(rom, off) as usize;
        if !(MIN_BANK_SAMPLES..=MAX_BANK_SAMPLES).contains(&n) {
            off += 4;
            continue;
        }

        let Some(dbase) = off
            .checked_add(4)
            .and_then(|v| v.checked_add(n.saturating_mul(LOGIK_DESC_V2_SIZE)))
        else {
            off += 4;
            continue;
        };
        if dbase > rlen {
            off += 4;
            continue;
        }

        let Ok(d0) = LogikSampleDescV2::parse(&rom[off + 4..]) else {
            off += 4;
            continue;
        };
        let Ok(d1) = LogikSampleDescV2::parse(&rom[off + 4 + LOGIK_DESC_V2_SIZE..]) else {
            off += 4;
            continue;
        };
        let Ok(d2) = LogikSampleDescV2::parse(&rom[off + 4 + 2 * LOGIK_DESC_V2_SIZE..]) else {
            off += 4;
            continue;
        };

        let (o0, l0) = (d0.data_offset as usize, d0.length as usize);
        let (o1, l1) = (d1.data_offset as usize, d1.length as usize);
        let (o2, l2) = (d2.data_offset as usize, d2.length as usize);

        if o0 != 0
            || !(MIN_SAMPLE_BYTES..=MAX_SAMPLE_BYTES).contains(&l0)
            || (d0.vol_flags & 0xFF00_FFFF) != 0
            || o1 != l0
            || !(MIN_SAMPLE_BYTES..=MAX_SAMPLE_BYTES).contains(&l1)
            || o2 != o1.saturating_add(l1)
            || !(MIN_SAMPLE_BYTES..=MAX_SAMPLE_BYTES).contains(&l2)
        {
            off += 4;
            continue;
        }

        let last_off = off + 4 + (n - 1) * LOGIK_DESC_V2_SIZE;
        let Ok(dlast) = LogikSampleDescV2::parse(&rom[last_off..]) else {
            off += 4;
            continue;
        };
        let last_end = dbase
            .checked_add(dlast.data_offset as usize)
            .and_then(|v| v.checked_add(dlast.length as usize));
        if !last_end.is_some_and(|end| end <= rlen) {
            off += 4;
            continue;
        }

        let payload = &rom[dbase..];
        let mut results = Vec::new();
        for i in 0..n {
            let eoff = off + 4 + i * LOGIK_DESC_V2_SIZE;
            let Ok(desc) = LogikSampleDescV2::parse(&rom[eoff..]) else {
                continue;
            };
            if (desc.length as usize) < MIN_SAMPLE_BYTES {
                continue;
            }
            let Ok(view) = LogikSampleView::from_v2_desc(&desc, payload, default_rate) else {
                continue;
            };
            let Some(data_offset) = dbase.checked_add(desc.data_offset as usize) else {
                continue;
            };

            results.push(build_sample_entry(
                results.len(),
                LogikEngineVariant::V2,
                eoff,
                data_offset,
                view.pcm_bytes,
                desc.default_volume(),
                desc.loop_start,
                desc.loop_len,
                view.sample_rate_hz,
                has_ima_table,
                use_arm_adpcm,
            ));
        }

        if !results.is_empty() {
            return results;
        }

        off += 4;
    }

    Vec::new()
}

/// Construct a decoded [`LogikSampleEntry`] from raw encoded bytes and descriptor metadata.
#[allow(clippy::too_many_arguments)]
fn build_sample_entry(
    index: usize,
    variant: LogikEngineVariant,
    rom_offset: usize,
    data_offset: usize,
    raw: &[u8],
    default_volume: u8,
    lstart: u32,
    llen: u32,
    sample_rate: u32,
    has_ima_table: bool,
    use_arm_adpcm: bool,
) -> LogikSampleEntry {
    let is_ima = has_ima_table && llen == 1 && lstart == 0;
    let (codec, pcm_data) = if is_ima {
        let decoded = if use_arm_adpcm {
            decode_ima_adpcm4_arm_to_pcm8(raw)
        } else {
            decode_ima_adpcm4_to_pcm8(raw)
        };
        (LogikSampleCodec::ImaAdpcm4, decoded)
    } else {
        (
            LogikSampleCodec::Pcm8,
            raw.iter().map(|&b| b as i8).collect(),
        )
    };

    let decoded_len = pcm_data.len();
    let loop_end_opt = lstart.checked_add(llen);
    let looped = !is_ima
        && llen > 2
        && loop_end_opt.is_some_and(|end| (end as usize) <= decoded_len);

    let (loop_start, loop_end) = if looped {
        (Some(lstart), loop_end_opt)
    } else {
        (None, None)
    };

    LogikSampleEntry {
        index,
        variant,
        codec,
        rom_offset,
        data_offset,
        size: decoded_len,
        sample_rate,
        root_key: 60,
        default_volume,
        loop_start,
        loop_end,
        pcm_data,
    }
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

    fn write_u32_le(buf: &mut [u8], off: usize, val: u32) {
        buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }

    /// Build a synthetic Logik State GBAModPlay v3.0+ ROM (24-byte descriptors, explicit Hz,
    /// including a 1-byte dummy slot 0 and a looped PCM8 sample).
    fn build_synthetic_logik_v3_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"LOGIKV3TEST\0");

        // Driver credit and GBAMOD30 header with mix_rate_hz = 13379 at +0x34
        let credit = b"GBAModPlay (C) Logik State 2003";
        rom[0x0100..0x0100 + credit.len()].copy_from_slice(credit);
        rom[0x0200..0x0208].copy_from_slice(GBAMOD30_MAGIC);
        write_u32_le(&mut rom, 0x0234, 13_379);

        // 4-sample V3 bank at 0x0400:
        // slot 0: 1-byte dummy (skipped because slen < 16)
        // slot 1: 64-byte PCM8 one-shot at 10512 Hz
        // slot 2: 96-byte PCM8 looped (start=16, len=64 -> end=80) at 15768 Hz
        // slot 3: 80-byte PCM8 one-shot with invalid rate 0 (falls back to 13379 Hz)
        let bank_off = 0x0400usize;
        let num_samples = 4usize;
        write_u32_le(&mut rom, bank_off, num_samples as u32);

        let specs: [(u32, u32, u32, u32, u32); 4] = [
            (1, 0x0040_0000, 0, 10_512, 0),
            (64, 0x0040_0000, 0, 10_512, 0),
            (96, 0x0038_0000, 16, 15_768, 64),
            (80, 0x0040_0000, 0, 0, 0),
        ];

        let dbase = bank_off + 4 + num_samples * LOGIK_DESC_V3_SIZE;
        let mut rel_off = 0u32;
        for (i, &(slen, vf, lstart, rate, llen)) in specs.iter().enumerate() {
            let eoff = bank_off + 4 + i * LOGIK_DESC_V3_SIZE;
            write_u32_le(&mut rom, eoff, rel_off);
            write_u32_le(&mut rom, eoff + 4, slen);
            write_u32_le(&mut rom, eoff + 8, vf);
            write_u32_le(&mut rom, eoff + 12, lstart);
            write_u32_le(&mut rom, eoff + 16, rate);
            write_u32_le(&mut rom, eoff + 20, llen);

            for k in 0..(slen as usize) {
                let s = (((k as i32 % 16) - 8) * 9) as i8;
                rom[dbase + (rel_off as usize) + k] = s as u8;
            }
            rel_off += slen;
        }

        rom
    }

    /// Build a synthetic Logik State GBAModPlay v2.x ROM (20-byte descriptors)
    /// optionally enabling 4-bit IMA ADPCM mode.
    fn build_synthetic_logik_v2_rom(enable_ima_adpcm: bool) -> Vec<u8> {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"LOGIKV2TEST\0");

        // GBAMOD2. header with mix_rate_hz = 21024 at +0x14
        rom[0x0180..0x0188].copy_from_slice(GBAMOD2_MAGIC);
        write_u32_le(&mut rom, 0x0194, 21_024);

        if enable_ima_adpcm {
            rom[0x0200..0x0200 + IMA_STEP_SIG_U32.len()].copy_from_slice(&IMA_STEP_SIG_U32);
        }

        // 3-sample V2 bank at 0x0400
        let bank_off = 0x0400usize;
        let num_samples = 3usize;
        write_u32_le(&mut rom, bank_off, num_samples as u32);

        let specs: [(u32, u32, u32, u32); 3] = if enable_ima_adpcm {
            [
                (32, 0x0040_0000, 0, 1), // IMA ADPCM4 -> 64 decoded samples
                (48, 0x0040_0000, 0, 1), // IMA ADPCM4 -> 96 decoded samples
                (64, 0x0040_0000, 8, 40), // PCM8 looped (start=8, end=48)
            ]
        } else {
            [
                (64, 0x0040_0000, 0, 0),  // PCM8 one-shot
                (96, 0x0040_0000, 16, 48), // PCM8 looped (start=16, end=64)
                (48, 0x0030_0000, 0, 0),  // PCM8 one-shot
            ]
        };

        let dbase = bank_off + 4 + num_samples * LOGIK_DESC_V2_SIZE;
        let mut rel_off = 0u32;
        for (i, &(slen, vf, lstart, llen)) in specs.iter().enumerate() {
            let eoff = bank_off + 4 + i * LOGIK_DESC_V2_SIZE;
            write_u32_le(&mut rom, eoff, rel_off);
            write_u32_le(&mut rom, eoff + 4, slen);
            write_u32_le(&mut rom, eoff + 8, vf);
            write_u32_le(&mut rom, eoff + 12, lstart);
            write_u32_le(&mut rom, eoff + 16, llen);

            for k in 0..(slen as usize) {
                rom[dbase + (rel_off as usize) + k] = if enable_ima_adpcm && llen == 1 {
                    // Alternating positive/negative IMA nibbles (0x7 low, 0xF high)
                    if k % 2 == 0 { 0x67 } else { 0xEF }
                } else {
                    ((((k as i32) % 16) - 8) * 8) as i8 as u8
                };
            }
            rel_off += slen;
        }

        rom
    }

    #[test]
    fn detects_and_scans_v3_pcm8_bank() {
        let rom = build_synthetic_logik_v3_rom();
        assert!(detect_logik_state(&rom));

        let samples = scan_logik_soundpool(&rom);
        // Dummy 1-byte slot 0 is skipped -> 3 extracted samples
        assert_eq!(samples.len(), 3);

        assert_eq!(samples[0].variant, LogikEngineVariant::V3);
        assert_eq!(samples[0].codec, LogikSampleCodec::Pcm8);
        assert_eq!(samples[0].size, 64);
        assert_eq!(samples[0].sample_rate, 10_512);
        assert_eq!(samples[0].default_volume, 64);
        assert_eq!(samples[0].loop_start, None);
        assert_eq!(samples[0].loop_end, None);

        assert_eq!(samples[1].size, 96);
        assert_eq!(samples[1].sample_rate, 15_768);
        assert_eq!(samples[1].default_volume, 0x38);
        assert_eq!(samples[1].loop_start, Some(16));
        assert_eq!(samples[1].loop_end, Some(80));

        // Out-of-range descriptor rate falls back to GBAMOD30 mix rate (13379 Hz)
        assert_eq!(samples[2].size, 80);
        assert_eq!(samples[2].sample_rate, 13_379);
    }

    #[test]
    fn detects_and_scans_v2_pcm8_and_ima_adpcm4_banks() {
        let rom_pcm = build_synthetic_logik_v2_rom(false);
        assert!(detect_logik_state(&rom_pcm));
        let pcm_samples = scan_logik_soundpool(&rom_pcm);
        assert_eq!(pcm_samples.len(), 3);
        assert_eq!(pcm_samples[0].variant, LogikEngineVariant::V2);
        assert_eq!(pcm_samples[0].codec, LogikSampleCodec::Pcm8);
        assert_eq!(pcm_samples[0].sample_rate, 21_024);
        assert_eq!(pcm_samples[1].loop_start, Some(16));
        assert_eq!(pcm_samples[1].loop_end, Some(64));

        let rom_ima = build_synthetic_logik_v2_rom(true);
        let ima_samples = scan_logik_soundpool(&rom_ima);
        assert_eq!(ima_samples.len(), 3);
        assert_eq!(ima_samples[0].codec, LogikSampleCodec::ImaAdpcm4);
        assert_eq!(ima_samples[0].size, 64);
        assert_eq!(ima_samples[0].pcm_data.len(), 64);
        assert_eq!(ima_samples[0].loop_start, None);

        assert_eq!(ima_samples[1].codec, LogikSampleCodec::ImaAdpcm4);
        assert_eq!(ima_samples[1].size, 96);

        assert_eq!(ima_samples[2].codec, LogikSampleCodec::Pcm8);
        assert_eq!(ima_samples[2].loop_start, Some(8));
        assert_eq!(ima_samples[2].loop_end, Some(48));
    }

    #[test]
    fn decodes_ima_adpcm4_low_nibble_first_deterministically() {
        // Nibble 0 = 0x7 (step=7, diff=0+1+3+7=11 -> pred=11, idx=8, out=0)
        // Nibble 1 = 0x7 (step=16, diff=2+4+8+16=30 -> pred=41, idx=16, out=0)
        // Nibble 2 = 0x7 (step=34, diff=4+8+17+34=63 -> pred=104, idx=24, out=0)
        // Nibble 3 = 0x7 (step=73, diff=9+18+36+73=136 -> pred=240, idx=32, out=0)
        // Nibble 4 = 0x7 (step=157, diff=19+39+78+157=293 -> pred=533, idx=40, out=2)
        // Nibble 5 = 0xF (step=337, diff=42+84+168+337=631 -> pred=-98, idx=48, out=-1)
        let decoded = decode_ima_adpcm4_to_pcm8(&[0x77, 0x77, 0xF7]);
        assert_eq!(decoded, vec![0, 0, 0, 0, 2, -1]);
    }

    #[test]
    fn exports_valid_wav16_and_soundfont() {
        let rom = build_synthetic_logik_v3_rom();

        let wavs = export_logik_samples_wav16(&rom).expect("wav export should succeed");
        assert_eq!(wavs.len(), 3);
        for (idx, name, bytes) in &wavs {
            assert!(name.starts_with(&format!("logik_sample_{idx:03}_")));
            assert!(name.ends_with(".wav"));
            assert_eq!(&bytes[0..4], b"RIFF");
            assert_eq!(&bytes[8..12], b"WAVE");
        }
        assert_eq!(wavs[0].2.len(), 44 + 64 * 2);

        let sf2 = compile_logik_soundfont(&rom, Some("Synthetic Logik"))
            .expect("sf2 compile should succeed");
        let validation = validate_soundfont(&sf2.sf2_bytes).expect("sf2 should validate");
        assert_eq!(validation.sample_count, 3);
        assert_eq!(validation.preset_count, 3);
        assert_eq!(validation.instrument_count, 3);
        assert_eq!(validation.bank_name, "Synthetic Logik");
        assert_eq!(validation.sound_engine, "Logik State GBAModPlay");

        let sf2_default = compile_logik_soundfont(&rom, None).expect("default title compile");
        let val_default = validate_soundfont(&sf2_default.sf2_bytes).expect("default sf2 valid");
        assert_eq!(val_default.bank_name, "LOGIKV3TEST (Logik State GBAModPlay)");
    }

    #[test]
    fn rejects_non_logik_rom() {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"NONLOGIKROM\0");
        assert!(!detect_logik_state(&rom));
        assert!(scan_logik_soundpool(&rom).is_empty());
        assert!(export_logik_samples_wav16(&rom).is_err());
        assert!(compile_logik_soundfont(&rom, None).is_err());
    }
}
