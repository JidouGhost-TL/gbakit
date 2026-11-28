//! Engine Software / Bob Koon EIRE v2.0 DirectSound sample pool scanning,
//! signed 8-bit PCM extraction, WAV16 export, and SoundFont 2.04 soundbank
//! compilation.
//!
//! Retail EIRE v2.0 GBA ROMs pack instruments into contiguous 4-byte-aligned
//! banks of 124-byte (`0x7C`) [`engine_sw_audio::EngineSwSampleHeader`]
//! descriptors followed immediately at `+0x7C` by inline signed 8-bit PCM.

#![deny(unsafe_code)]

use std::collections::HashSet;

use engine_sw_audio::{
    EngineSwSampleHeader, EngineSwVersion, ENGINE_SW_HEADER_SIZE, ENGINE_SW_MIX_RATE,
};
use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

/// Minimum accepted EIRE instrument PCM length in bytes.
const MIN_SAMPLE_BYTES: usize = 32;

/// Maximum accepted EIRE instrument PCM length in bytes (`256 KiB`).
const MAX_SAMPLE_BYTES: usize = 256 * 1024;

/// Minimum number of chained 124-byte EIRE instruments required to accept a bank.
const MIN_CHAIN_LEN: usize = 3;

/// Minimum number of zero bytes expected in the 100-byte envelope tail (`+24..+124`).
const MIN_ENVELOPE_TAIL_ZEROS: usize = 50;

/// Extracted Engine Software EIRE v2.0 signed 8-bit PCM sample entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineSwSampleEntry {
    /// Sample index within the detected pool.
    pub index: usize,
    /// Absolute ROM file offset where the 124-byte `EngineSwSampleHeader` begins.
    pub rom_offset: usize,
    /// Absolute ROM file offset where the inline signed 8-bit PCM payload begins.
    pub sample_start_offset: usize,
    /// Sample payload length in bytes.
    pub size: usize,
    /// Playback sample rate in Hz (`ENGINE_SW_MIX_RATE` = 21,024 Hz).
    pub sample_rate: u32,
    /// MIDI root note (`60` = Middle C / C4).
    pub root_key: u8,
    /// Default volume byte (`0..=64`).
    pub vol_default: u8,
    /// Default pan (`-64..=63`).
    pub pan_default: i8,
    /// Signed pitch offset byte from header `+14`.
    pub pitch_offset: i8,
    /// Optional loop range `(loop_start, loop_end)` in samples.
    pub loop_range: Option<(u32, u32)>,
    /// Extracted raw signed 8-bit linear mono PCM samples.
    pub pcm_data: Vec<i8>,
}

/// Exact 16-byte middle anchor of the Engine Software EIRE v1.x ARM mixer
/// prologue (`ldmia r1, {r2, r5}; add r3, r0, r5; str r3, [r1, #4]; ldr r6, [r1, #0x10]`).
const EIRE_V1_MIXER_MID_SIG: [u8; 16] = [
    0x24, 0x00, 0x91, 0xe8, 0x05, 0x30, 0x80, 0xe0, 0x04, 0x30, 0x81, 0xe5, 0x10, 0x60, 0x91, 0xe5,
];

/// Detect the Engine Software EIRE v1.x ARM7TDMI software mixer prologue
/// (`stmfd sp!, {r0-r2, r4-r11}; ldr r1, [pc, #imm]; ldmia r1, {r2, r5}; ...`).
///
/// Captures early EIRE v1.x builds (e.g., *Mission Impossible: Operation Surma*,
/// *James Pond: Codename Robocod*, *Animaniacs*, *Spirits & Spells*, *xXx*,
/// *Max Payne*, *Bionicle*) that share the same 124-byte `EngineSwSampleHeader`
/// format as EIRE v2.0.
pub fn has_engine_sw_v1_mixer(rom: &[u8]) -> bool {
    if rom.len() < 32 {
        return false;
    }
    let mut start = 8usize;
    while start + 24 <= rom.len() {
        let Some(rel) = rom[start..]
            .windows(EIRE_V1_MIXER_MID_SIG.len())
            .position(|w| w == EIRE_V1_MIXER_MID_SIG)
        else {
            break;
        };
        let mid = start + rel;
        if mid >= 8 && mid + 24 <= rom.len() {
            let w0 = u32::from_le_bytes([rom[mid - 8], rom[mid - 7], rom[mid - 6], rom[mid - 5]]);
            let w1 = u32::from_le_bytes([rom[mid - 4], rom[mid - 3], rom[mid - 2], rom[mid - 1]]);
            let w6 =
                u32::from_le_bytes([rom[mid + 16], rom[mid + 17], rom[mid + 18], rom[mid + 19]]);
            let w7 =
                u32::from_le_bytes([rom[mid + 20], rom[mid + 21], rom[mid + 22], rom[mid + 23]]);
            if matches!(w0, 0xe92d_0ff6 | 0xe92d_0ff7)
                && (w1 & 0xFFFF_F000) == 0xe59f_1000
                && (w6 & 0xFFFF_F000) == 0xe59f_c000
                && w7 == 0xe1a0_400c
            {
                return true;
            }
        }
        start = mid + 4;
    }
    false
}

/// Detect early EIRE v1.x ROMs that carry an explicit Engine Software driver credit
/// (`"Sound Driver by Engine Software"` or `"ENGINE SOFTWARE BV"`) corroborated by a
/// valid chained 124-byte EIRE instrument bank.
pub fn has_engine_sw_v1_tag_with_bank(rom: &[u8]) -> bool {
    const TAG_DRV: &[u8] = b"Sound Driver by Engine Software";
    const TAG_BV: &[u8] = b"ENGINE SOFTWARE BV";
    let has_tag = (rom.len() >= TAG_DRV.len() && rom.windows(TAG_DRV.len()).any(|w| w == TAG_DRV))
        || (rom.len() >= TAG_BV.len() && rom.windows(TAG_BV.len()).any(|w| w == TAG_BV));
    has_tag && !scan_engine_sw_soundpool(rom).is_empty()
}

/// Detect whether a GBA ROM uses the Engine Software / Bob Koon EIRE engine (v1.x or v2.0).
pub fn detect_engine_sw_engine(rom: &[u8]) -> bool {
    EngineSwVersion::detect_from_bytes(rom).is_some()
        || has_engine_sw_v1_mixer(rom)
        || has_engine_sw_v1_tag_with_bank(rom)
}

/// Validate a candidate 124-byte (`0x7C`) EIRE instrument header at `off` within `rom`.
fn parse_valid_engine_sw_header(rom: &[u8], off: usize) -> Option<EngineSwSampleHeader> {
    let hdr_slice = rom.get(off..off.checked_add(ENGINE_SW_HEADER_SIZE)?)?;
    let hdr = EngineSwSampleHeader::parse(hdr_slice).ok()?;

    let length = hdr.length as usize;
    if !(MIN_SAMPLE_BYTES..=MAX_SAMPLE_BYTES).contains(&length) {
        return None;
    }
    if hdr.loop_start > hdr.length || hdr.loop_length > hdr.length - hdr.loop_start {
        return None;
    }
    if hdr.vol_default > 64 || !(-48..=48).contains(&hdr.pitch_offset) {
        return None;
    }

    let pcm_start = off.checked_add(ENGINE_SW_HEADER_SIZE)?;
    let pcm_end = pcm_start.checked_add(length)?;
    if pcm_end > rom.len() {
        return None;
    }

    // Retail EIRE 124-byte instrument headers carry a 0xFFFF envelope terminator
    // at bytes +22..+24 and a predominantly zero-padded envelope table in +24..+124.
    if hdr_slice[22] != 0xFF || hdr_slice[23] != 0xFF {
        return None;
    }
    let tail_zeros = hdr_slice[24..ENGINE_SW_HEADER_SIZE]
        .iter()
        .filter(|&&b| b == 0)
        .count();
    if tail_zeros < MIN_ENVELOPE_TAIL_ZEROS {
        return None;
    }

    Some(hdr)
}

/// Scan a GBA ROM for Engine Software EIRE instrument banks (both Mono DirectSound A
/// `0x21 0x01` music/SFX banks and Stereo v2.0 chained 124-byte banks) and extract their
/// signed 8-bit PCM payloads.
pub fn scan_engine_sw_soundpool(rom: &[u8]) -> Vec<EngineSwSampleEntry> {
    if let Ok(eire_rom) = engine_sw_audio::parse_mono_rom(rom) {
        let mut mono_samples = Vec::new();
        let mut seen = HashSet::new();

        for bank_idx in 0..eire_rom.music_bank_count() {
            let Some(iter) = eire_rom.music_samples(bank_idx) else {
                continue;
            };
            for s in iter {
                let pcm_start = s.pcm_offset;
                if s.pcm.is_empty() || !seen.insert(pcm_start) {
                    continue;
                }
                let loop_range = if s.header.loop_len > 0 {
                    Some((
                        s.header.loop_start,
                        s.header.loop_start.saturating_add(s.header.loop_len),
                    ))
                } else {
                    None
                };
                let index = mono_samples.len();
                mono_samples.push(EngineSwSampleEntry {
                    index,
                    rom_offset: s.header_offset,
                    sample_start_offset: pcm_start,
                    size: s.pcm.len(),
                    sample_rate: engine_sw_audio::BASE_SAMPLE_RATE_HZ,
                    root_key: engine_sw_audio::ROOT_KEY,
                    vol_default: s.header.default_volume,
                    pan_default: s.header.decoded_pan(),
                    pitch_offset: s.header.finetune,
                    loop_range,
                    pcm_data: s.pcm.iter().map(|&b| b as i8).collect(),
                });
            }
        }

        for bank_idx in 0..eire_rom.sfx_bank_count() {
            let Some(bank) = eire_rom.sfx_bank(bank_idx) else {
                continue;
            };
            for sfx_idx in 0..bank.sample_count {
                let Some(s) = eire_rom.sfx_sample(bank_idx, sfx_idx) else {
                    continue;
                };
                let pcm_start = s.pcm_offset;
                if s.pcm.is_empty() || !seen.insert(pcm_start) {
                    continue;
                }
                let index = mono_samples.len();
                mono_samples.push(EngineSwSampleEntry {
                    index,
                    rom_offset: pcm_start,
                    sample_start_offset: pcm_start,
                    size: s.pcm.len(),
                    sample_rate: s.entry.sample_rate,
                    root_key: engine_sw_audio::ROOT_KEY,
                    vol_default: 64,
                    pan_default: 0,
                    pitch_offset: 0,
                    loop_range: None,
                    pcm_data: s.pcm.iter().map(|&b| b as i8).collect(),
                });
            }
        }

        if !mono_samples.is_empty() {
            return mono_samples;
        }
    }

    let rlen = rom.len();
    if rlen < ENGINE_SW_HEADER_SIZE + MIN_SAMPLE_BYTES {
        return Vec::new();
    }

    let mut samples = Vec::new();
    let mut seen_pcm_offsets = HashSet::new();
    let mut off = 0usize;

    while off + ENGINE_SW_HEADER_SIZE + MIN_SAMPLE_BYTES <= rlen {
        if parse_valid_engine_sw_header(rom, off).is_none() {
            off += 4;
            continue;
        }

        let mut chain: Vec<(usize, EngineSwSampleHeader)> = Vec::new();
        let mut cur = off;
        while let Some(hdr) = parse_valid_engine_sw_header(rom, cur) {
            chain.push((cur, hdr));
            let next = (cur + ENGINE_SW_HEADER_SIZE + hdr.length as usize + 3) & !3;
            if next <= cur || next + ENGINE_SW_HEADER_SIZE > rlen {
                break;
            }
            cur = next;
        }

        if chain.len() >= MIN_CHAIN_LEN {
            let end_cursor = cur;
            for (hdr_off, hdr) in chain {
                let pcm_start = hdr_off + ENGINE_SW_HEADER_SIZE;
                let pcm_len = hdr.length as usize;
                if !seen_pcm_offsets.insert(pcm_start) {
                    continue;
                }
                let pcm_data: Vec<i8> = rom[pcm_start..pcm_start + pcm_len]
                    .iter()
                    .map(|&b| b as i8)
                    .collect();
                let loop_range = if hdr.loop_length > 0 {
                    Some((hdr.loop_start, hdr.loop_start + hdr.loop_length))
                } else {
                    None
                };

                let index = samples.len();
                samples.push(EngineSwSampleEntry {
                    index,
                    rom_offset: hdr_off,
                    sample_start_offset: pcm_start,
                    size: pcm_len,
                    sample_rate: ENGINE_SW_MIX_RATE,
                    root_key: 60,
                    vol_default: hdr.vol_default,
                    pan_default: hdr.pan_default,
                    pitch_offset: hdr.pitch_offset,
                    loop_range,
                    pcm_data,
                });
            }
            off = end_cursor.max(off + 4);
        } else {
            off += 4;
        }
    }

    samples
}

/// Extract all Engine Software EIRE v2.0 samples from a ROM and encode them as
/// standard 16-bit uncompressed mono RIFF WAV files.
///
/// Returns a list of `(sample_index, recommended_filename, wav_bytes)`.
pub fn export_engine_sw_samples_wav16(rom: &[u8]) -> Result<Vec<(usize, String, Vec<u8>)>> {
    let samples = scan_engine_sw_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_engine_sw_samples_found",
            value: 0,
        });
    }

    let mut out = Vec::with_capacity(samples.len());
    for s in &samples {
        let wav = encode_wav_i16_mono(s.sample_rate, &s.pcm_data)?;
        let name = format!(
            "engine_sw_sample_{:03}_{:06X}.wav",
            s.index, s.sample_start_offset
        );
        out.push((s.index, name, wav));
    }

    Ok(out)
}

/// Compile all extracted Engine Software EIRE v2.0 samples into a standard
/// SoundFont 2.04 (`.sf2`) soundbank.
pub fn compile_engine_sw_soundfont(
    rom: &[u8],
    title_override: Option<&str>,
) -> Result<SoundFontExport> {
    let samples = scan_engine_sw_soundpool(rom);
    if samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_engine_sw_samples_found",
            value: 0,
        });
    }

    let bank_title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| {
            let t = extract_rom_title(rom);
            format!("{t} (Engine Software EIRE)")
        });

    let raw_samples: Vec<RawSoundFontSample> = samples
        .iter()
        .map(|s| {
            let pcm16: Vec<i16> = s.pcm_data.iter().map(|&b| (b as i16) << 8).collect();
            RawSoundFontSample {
                name: format!("EIRE_{:03}", s.index),
                sample_rate: s.sample_rate,
                root_key: s.root_key,
                loop_range: s.loop_range,
                pcm16,
            }
        })
        .collect();

    compile_custom_samples_soundfont(
        &bank_title,
        "Engine Software EIRE v2.0 DirectSound Engine",
        &raw_samples,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;

    #[allow(clippy::too_many_arguments)]
    fn write_eire_instrument(
        rom: &mut [u8],
        off: usize,
        length: usize,
        loop_start: u32,
        loop_length: u32,
        vol: u8,
        pan_raw: u8,
        pitch_offset: i8,
    ) -> usize {
        rom[off..off + 4].copy_from_slice(&(length as u32).to_le_bytes());
        rom[off + 4..off + 8].copy_from_slice(&loop_start.to_le_bytes());
        rom[off + 8..off + 12].copy_from_slice(&loop_length.to_le_bytes());
        rom[off + 12] = vol;
        rom[off + 13] = pan_raw;
        rom[off + 14] = pitch_offset as u8;
        rom[off + 15] = 5;
        rom[off + 16..off + 24].copy_from_slice(&[0x00, 0x04, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF]);
        for k in 0..length {
            let v = (((k as i32) % 16) - 8) * 7;
            rom[off + ENGINE_SW_HEADER_SIZE + k] = (v as i8) as u8;
        }
        (off + ENGINE_SW_HEADER_SIZE + length + 3) & !3
    }

    fn build_synthetic_engine_sw_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x2000];
        rom[0xA0..0xAC].copy_from_slice(b"EIREV2TEST\0\0");

        let mut cur = 0x0400usize;
        cur = write_eire_instrument(&mut rom, cur, 64, 0, 0, 64, 128, 0);
        cur = write_eire_instrument(&mut rom, cur, 128, 16, 96, 60, 128, -12);
        let _ = write_eire_instrument(&mut rom, cur, 96, 0, 0, 48, 64, 12);

        rom
    }

    #[test]
    fn scans_chained_eire_instruments_with_metadata() {
        let rom = build_synthetic_engine_sw_rom();
        let samples = scan_engine_sw_soundpool(&rom);
        assert_eq!(samples.len(), 3);

        assert_eq!(samples[0].rom_offset, 0x0400);
        assert_eq!(samples[0].sample_start_offset, 0x0400 + ENGINE_SW_HEADER_SIZE);
        assert_eq!(samples[0].size, 64);
        assert_eq!(samples[0].sample_rate, ENGINE_SW_MIX_RATE);
        assert_eq!(samples[0].vol_default, 64);
        assert_eq!(samples[0].pan_default, 0);
        assert_eq!(samples[0].pitch_offset, 0);
        assert_eq!(samples[0].loop_range, None);
        assert_eq!(samples[0].pcm_data[0], -56);

        assert_eq!(samples[1].size, 128);
        assert_eq!(samples[1].loop_range, Some((16, 112)));
        assert_eq!(samples[1].pitch_offset, -12);

        assert_eq!(samples[2].size, 96);
        assert_eq!(samples[2].vol_default, 48);
        assert_eq!(samples[2].pan_default, -32);
    }

    #[test]
    fn exports_valid_engine_sw_wav16_and_soundfont() {
        let rom = build_synthetic_engine_sw_rom();

        let wavs = export_engine_sw_samples_wav16(&rom).expect("wav export should succeed");
        assert_eq!(wavs.len(), 3);
        assert_eq!(wavs[0].2.len(), 44 + 64 * 2);
        assert_eq!(&wavs[0].2[0..4], b"RIFF");
        assert_eq!(&wavs[0].2[8..12], b"WAVE");

        let sf2 = compile_engine_sw_soundfont(&rom, Some("Synthetic EIRE"))
            .expect("sf2 compile should succeed");
        let validation = validate_soundfont(&sf2.sf2_bytes).expect("sf2 should validate");
        assert_eq!(validation.sample_count, 3);
        assert_eq!(validation.preset_count, 3);
        assert_eq!(validation.instrument_count, 3);
        assert_eq!(validation.bank_name, "Synthetic EIRE");
        assert_eq!(
            validation.sound_engine,
            "Engine Software EIRE v2.0 DirectSound Engine"
        );
    }

    #[test]
    fn rejects_short_chains_and_missing_envelope_markers() {
        let mut rom = vec![0u8; 0x1000];
        let mut cur = 0x0200usize;
        cur = write_eire_instrument(&mut rom, cur, 64, 0, 0, 64, 128, 0);
        let _ = write_eire_instrument(&mut rom, cur, 64, 0, 0, 64, 128, 0);

        assert!(scan_engine_sw_soundpool(&rom).is_empty());
        assert!(export_engine_sw_samples_wav16(&rom).is_err());
        assert!(compile_engine_sw_soundfont(&rom, None).is_err());
    }

    #[test]
    fn detects_eire_v1_arm_mixer_signature() {
        let mut rom = vec![0u8; 0x200];
        let words: [u32; 8] = [
            0xe92d_0ff7,
            0xe59f_1044,
            0xe891_0024,
            0xe080_3005,
            0xe581_3004,
            0xe591_6010,
            0xe59f_c02c,
            0xe1a0_400c,
        ];
        for (i, word) in words.iter().enumerate() {
            rom[0x80 + i * 4..0x84 + i * 4].copy_from_slice(&word.to_le_bytes());
        }
        assert!(has_engine_sw_v1_mixer(&rom));
        assert!(detect_engine_sw_engine(&rom));
        assert_eq!(
            crate::detect_sound_engine(&rom),
            crate::DetectedSoundEngine::EngineSw(None)
        );

        // Earlier v1.x builds omitting r0 (`0xe92d_0ff6`) must also match
        rom[0x80..0x84].copy_from_slice(&0xe92d_0ff6u32.to_le_bytes());
        assert!(has_engine_sw_v1_mixer(&rom));
        assert!(detect_engine_sw_engine(&rom));

        // Corrupting w0 to another opcode must reject
        rom[0x80] ^= 0x04;
        assert!(!has_engine_sw_v1_mixer(&rom));
        assert!(!detect_engine_sw_engine(&rom));
    }

    #[test]
    fn detects_eire_v1_tag_with_valid_bank() {
        let mut rom = build_synthetic_engine_sw_rom();
        assert!(!has_engine_sw_v1_tag_with_bank(&rom));
        rom[0x100..0x112].copy_from_slice(b"ENGINE SOFTWARE BV");
        assert!(has_engine_sw_v1_tag_with_bank(&rom));
        assert!(detect_engine_sw_engine(&rom));
    }
}
