//! SoundFont 2.04 (`.sf2`) binary compiler and validator for GBA DirectSound audio.
//!
//! Converts Nintendo M4A / Sappy VoiceGroups, ADSR envelopes, key splits,
//! and 8-bit DirectSound PCM wave samples into standard SoundFont 2.04 soundbanks
//! playable directly in FluidSynth, VLC, FL Studio, Ableton, or any MIDI synthesizer.

#![deny(unsafe_code)]

use std::collections::{HashMap, HashSet};
use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::{
    collect_voicegroup_sample_pointers, detect_sound_engine, extract_pcm_i8,
    parse_voice_entry, rom_ptr_to_offset, scan_m4a_engine, DetectedSoundEngine,
    VoiceEntry, VoiceKind, ROM_BUS_START, VOICE_ENTRY_SIZE,
};

/// SoundFont 2.04 Specification Major Version.
pub const SF2_VERSION_MAJOR: u16 = 2;

/// SoundFont 2.04 Specification Minor Version.
pub const SF2_VERSION_MINOR: u16 = 4;

/// Target Sound Engine string embedded into SF2 INFO chunk.
pub const SF2_SOUND_ENGINE: &str = "GBAKit DirectSound";

/// Record size of `phdr` (Preset Header) in bytes.
pub const PHDR_RECORD_SIZE: usize = 38;

/// Record size of `pbag` (Preset Bag / Zone) in bytes.
pub const PBAG_RECORD_SIZE: usize = 4;

/// Record size of `pmod` (Preset Modulator) in bytes.
pub const PMOD_RECORD_SIZE: usize = 10;

/// Record size of `pgen` (Preset Generator) in bytes.
pub const PGEN_RECORD_SIZE: usize = 4;

/// Record size of `inst` (Instrument Header) in bytes.
pub const INST_RECORD_SIZE: usize = 22;

/// Record size of `ibag` (Instrument Bag / Zone) in bytes.
pub const IBAG_RECORD_SIZE: usize = 4;

/// Record size of `imod` (Instrument Modulator) in bytes.
pub const IMOD_RECORD_SIZE: usize = 10;

/// Record size of `igen` (Instrument Generator) in bytes.
pub const IGEN_RECORD_SIZE: usize = 4;

/// Record size of `shdr` (Sample Header) in bytes.
pub const SHDR_RECORD_SIZE: usize = 46;

/// Number of null zero-sample points (16-bit) terminating each sample per SF2 standard.
pub const SAMPLE_TERMINAL_PADDING: usize = 46;

// SoundFont 2.04 Generator IDs (sfGenOper)
pub const GEN_PAN: u16 = 17;
pub const GEN_ATTACK_VOL_ENV: u16 = 34;
pub const GEN_HOLD_VOL_ENV: u16 = 35;
pub const GEN_DECAY_VOL_ENV: u16 = 36;
pub const GEN_SUSTAIN_VOL_ENV: u16 = 37;
pub const GEN_RELEASE_VOL_ENV: u16 = 38;
pub const GEN_INSTRUMENT: u16 = 41;
pub const GEN_KEY_RANGE: u16 = 43;
pub const GEN_VEL_RANGE: u16 = 44;
pub const GEN_INITIAL_ATTENUATION: u16 = 48;
pub const GEN_COARSE_TUNE: u16 = 51;
pub const GEN_FINE_TUNE: u16 = 52;
pub const GEN_SAMPLE_ID: u16 = 53;
pub const GEN_SAMPLE_MODES: u16 = 54;
pub const GEN_SCALE_TUNING: u16 = 56;
pub const GEN_OVERRIDING_ROOT_KEY: u16 = 58;

/// Convert GBA M4A attack byte (0..=255) to SF2 timecents (-12000..=8000).
/// In M4A, 255 is instantaneous (fastest), lower values are slower.
pub fn m4a_attack_to_timecents(attack: u8) -> i16 {
    if attack >= 250 {
        -12000 // ~1 ms
    } else {
        let t = 0.001 + (255.0 - attack as f64) / 255.0 * 5.0;
        let tc = (1200.0 * t.log2()).round() as i32;
        tc.clamp(-12000, 8000) as i16
    }
}

/// Convert GBA M4A decay byte (0..=255) to SF2 timecents (-12000..=8000).
/// In M4A, 255 is fastest decay, lower values are slower decay.
pub fn m4a_decay_to_timecents(decay: u8) -> i16 {
    if decay == 0 {
        8000 // sustained / very long decay
    } else {
        let t = 0.01 + (256.0 - decay as f64) / 256.0 * 8.0;
        let tc = (1200.0 * t.log2()).round() as i32;
        tc.clamp(-12000, 8000) as i16
    }
}

/// Convert GBA M4A sustain byte (0..=255) to SF2 centibels (0..=1000).
/// In M4A, 255 is 100% volume (0 cB attenuation), 0 is silence (1000 cB attenuation).
pub fn m4a_sustain_to_centibels(sustain: u8) -> u16 {
    if sustain == 255 {
        0
    } else if sustain == 0 {
        1000
    } else {
        let ratio = sustain as f64 / 255.0;
        let db_down = -200.0 * ratio.log10();
        db_down.round().clamp(0.0, 1000.0) as u16
    }
}

/// Convert GBA M4A release byte (0..=255) to SF2 timecents (-12000..=8000).
/// In M4A, 255 is fastest release, lower values are slower release.
pub fn m4a_release_to_timecents(release: u8) -> i16 {
    if release >= 250 {
        -12000 // ~1 ms
    } else if release == 0 {
        -3000 // default ~175 ms
    } else {
        let t = 0.005 + (256.0 - release as f64) / 256.0 * 6.0;
        let tc = (1200.0 * t.log2()).round() as i32;
        tc.clamp(-12000, 8000) as i16
    }
}

/// Pad or truncate an ASCII string into a 20-byte null-padded SF2 record buffer.
pub fn encode_fixed_string_20(s: &str) -> [u8; 20] {
    let mut buf = [0u8; 20];
    let bytes = s.as_bytes();
    let len = bytes.len().min(19); // preserve trailing null
    buf[..len].copy_from_slice(&bytes[..len]);
    buf
}

/// Extract clean ASCII game title from GBA ROM header at `0xA0..0xAC`.
pub fn extract_rom_title(rom: &[u8]) -> String {
    if rom.len() >= 0xAC {
        let title_bytes = &rom[0xA0..0xAC];
        let mut s = String::new();
        for &b in title_bytes {
            if b == 0 {
                break;
            }
            if b.is_ascii_graphic() || b == b' ' {
                s.push(b as char);
            }
        }
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "GBA Soundbank".to_string()
}

/// Zone generator definition in an SF2 zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneGenerator {
    pub oper: u16,
    pub amount: [u8; 2],
}

impl ZoneGenerator {
    pub fn new_u16(oper: u16, val: u16) -> Self {
        Self {
            oper,
            amount: val.to_le_bytes(),
        }
    }

    pub fn new_i16(oper: u16, val: i16) -> Self {
        Self {
            oper,
            amount: val.to_le_bytes(),
        }
    }

    pub fn new_range(oper: u16, lo: u8, hi: u8) -> Self {
        Self {
            oper,
            amount: [lo, hi],
        }
    }
}

/// Internal instrument zone builder.
#[derive(Debug, Clone)]
pub struct InstrumentZoneBuilder {
    pub key_range: (u8, u8),
    pub vel_range: (u8, u8),
    pub root_key: u8,
    pub sample_modes: u16,
    pub pan: Option<i16>,
    pub initial_attenuation: u16,
    pub attack_tc: i16,
    pub hold_tc: i16,
    pub decay_tc: i16,
    pub sustain_cb: u16,
    pub release_tc: i16,
    pub scale_tuning: Option<u16>,
    pub sample_id: u16,
}

/// Internal instrument builder.
#[derive(Debug, Clone)]
pub struct InstrumentBuilder {
    pub name: String,
    pub zones: Vec<InstrumentZoneBuilder>,
}

/// Internal preset zone builder.
#[derive(Debug, Clone)]
pub struct PresetZoneBuilder {
    pub key_range: (u8, u8),
    pub vel_range: (u8, u8),
    pub instrument_id: u16,
}

/// Internal preset builder.
#[derive(Debug, Clone)]
pub struct PresetBuilder {
    pub bank: u16,
    pub preset: u16,
    pub name: String,
    pub zones: Vec<PresetZoneBuilder>,
}

/// Sample metadata record.
#[derive(Debug, Clone)]
pub struct SampleRecord {
    pub name: [u8; 20],
    pub dw_start: u32,
    pub dw_end: u32,
    pub dw_startloop: u32,
    pub dw_endloop: u32,
    pub dw_sample_rate: u32,
    pub by_original_pitch: u8,
    pub ch_pitch_correction: i8,
    pub w_sample_link: u16,
    pub sf_sample_type: u16,
}

/// Summary report of an exported SoundFont 2 soundbank.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoundFontReport {
    pub title: String,
    pub engine: String,
    pub version: String,
    pub total_presets: usize,
    pub total_instruments: usize,
    pub total_samples: usize,
    pub sample_data_bytes: usize,
    pub file_size_bytes: usize,
    pub voicegroups_mapped: usize,
    pub presets: Vec<PresetSummary>,
}

/// Summary of an individual preset inside an exported SoundFont.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresetSummary {
    pub bank: u16,
    pub preset: u16,
    pub name: String,
    pub zones: usize,
}

/// Result of SoundFont 2 compilation.
pub struct SoundFontExport {
    pub sf2_bytes: Vec<u8>,
    pub report: SoundFontReport,
}

/// Parsed / validated metadata from an SF2 binary stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoundFontValidation {
    pub major_version: u16,
    pub minor_version: u16,
    pub sound_engine: String,
    pub bank_name: String,
    pub sample_count: usize,
    pub smpl_byte_len: usize,
    pub preset_count: usize,
    pub instrument_count: usize,
    pub pbag_count: usize,
    pub ibag_count: usize,
    pub pgen_count: usize,
    pub igen_count: usize,
}

/// Write an INFO subchunk with null termination and RIFF even-byte alignment.
fn write_info_string_subchunk(out: &mut Vec<u8>, fourcc: &[u8; 4], text: &str) {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0); // null terminator
    if !bytes.len().is_multiple_of(2) {
        bytes.push(0); // pad to even length
    }
    out.extend_from_slice(fourcc);
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&bytes);
}

/// Compile a raw SoundFont 2 binary stream from samples, instruments, and presets.
pub fn assemble_sf2_binary(
    bank_title: &str,
    sound_engine: &str,
    smpl_pcm_bytes: &[u8],
    samples: &[SampleRecord],
    instruments: &[InstrumentBuilder],
    presets: &[PresetBuilder],
) -> Result<Vec<u8>> {
    // 1. Construct LIST 'INFO'
    let mut info_body = Vec::new();
    info_body.extend_from_slice(b"INFO");

    // ifil subchunk (mandatory 4 bytes)
    info_body.extend_from_slice(b"ifil");
    info_body.extend_from_slice(&4u32.to_le_bytes());
    info_body.extend_from_slice(&SF2_VERSION_MAJOR.to_le_bytes());
    info_body.extend_from_slice(&SF2_VERSION_MINOR.to_le_bytes());

    // isng subchunk (mandatory)
    write_info_string_subchunk(&mut info_body, b"isng", sound_engine);

    // INAM subchunk (mandatory)
    write_info_string_subchunk(&mut info_body, b"INAM", bank_title);

    // IENG and ISFT subchunks
    write_info_string_subchunk(&mut info_body, b"IENG", "GBAKit");
    write_info_string_subchunk(&mut info_body, b"ISFT", "GBAKit SoundFont Engine");

    let mut info_chunk = Vec::new();
    info_chunk.extend_from_slice(b"LIST");
    info_chunk.extend_from_slice(&(info_body.len() as u32).to_le_bytes());
    info_chunk.extend_from_slice(&info_body);

    // 2. Construct LIST 'sdta'
    let mut sdta_body = Vec::new();
    sdta_body.extend_from_slice(b"sdta");
    sdta_body.extend_from_slice(b"smpl");
    sdta_body.extend_from_slice(&(smpl_pcm_bytes.len() as u32).to_le_bytes());
    sdta_body.extend_from_slice(smpl_pcm_bytes);

    let mut sdta_chunk = Vec::new();
    sdta_chunk.extend_from_slice(b"LIST");
    sdta_chunk.extend_from_slice(&(sdta_body.len() as u32).to_le_bytes());
    sdta_chunk.extend_from_slice(&sdta_body);

    // 3. Construct LIST 'pdta'
    // Presets MUST be sorted by (wBank, wPreset) ascending per SF2.04 Section 7.2.
    let mut sorted_presets = presets.to_vec();
    sorted_presets.sort_by_key(|p| (p.bank, p.preset));

    let mut phdr_bytes = Vec::with_capacity((sorted_presets.len() + 1) * PHDR_RECORD_SIZE);
    let mut pbag_bytes = Vec::new();
    let mut pgen_bytes = Vec::new();
    let mut inst_bytes = Vec::with_capacity((instruments.len() + 1) * INST_RECORD_SIZE);
    let mut ibag_bytes = Vec::new();
    let mut igen_bytes = Vec::new();

    let mut current_pbag_idx = 0u16;
    let mut current_pgen_idx = 0u16;

    for preset in &sorted_presets {
        let name_buf = encode_fixed_string_20(&preset.name);
        phdr_bytes.extend_from_slice(&name_buf);
        phdr_bytes.extend_from_slice(&preset.preset.to_le_bytes());
        phdr_bytes.extend_from_slice(&preset.bank.to_le_bytes());
        phdr_bytes.extend_from_slice(&current_pbag_idx.to_le_bytes());
        phdr_bytes.extend_from_slice(&0u32.to_le_bytes()); // dwLibrary
        phdr_bytes.extend_from_slice(&0u32.to_le_bytes()); // dwGenre
        phdr_bytes.extend_from_slice(&0u32.to_le_bytes()); // dwMorphology

        for zone in &preset.zones {
            pbag_bytes.extend_from_slice(&current_pgen_idx.to_le_bytes());
            pbag_bytes.extend_from_slice(&0u16.to_le_bytes()); // wModNdx = 0
            current_pbag_idx += 1;

            // Generators: keyRange, velRange, and instrument (LAST)
            pgen_bytes.extend_from_slice(&GEN_KEY_RANGE.to_le_bytes());
            pgen_bytes.extend_from_slice(&[zone.key_range.0, zone.key_range.1]);
            current_pgen_idx += 1;

            pgen_bytes.extend_from_slice(&GEN_VEL_RANGE.to_le_bytes());
            pgen_bytes.extend_from_slice(&[zone.vel_range.0, zone.vel_range.1]);
            current_pgen_idx += 1;

            pgen_bytes.extend_from_slice(&GEN_INSTRUMENT.to_le_bytes());
            pgen_bytes.extend_from_slice(&zone.instrument_id.to_le_bytes());
            current_pgen_idx += 1;
        }
    }

    // Terminal record for phdr: "EOP"
    phdr_bytes.extend_from_slice(&encode_fixed_string_20("EOP"));
    phdr_bytes.extend_from_slice(&0u16.to_le_bytes());
    phdr_bytes.extend_from_slice(&0u16.to_le_bytes());
    phdr_bytes.extend_from_slice(&current_pbag_idx.to_le_bytes());
    phdr_bytes.extend_from_slice(&0u32.to_le_bytes());
    phdr_bytes.extend_from_slice(&0u32.to_le_bytes());
    phdr_bytes.extend_from_slice(&0u32.to_le_bytes());

    // Terminal record for pbag
    pbag_bytes.extend_from_slice(&current_pgen_idx.to_le_bytes());
    pbag_bytes.extend_from_slice(&0u16.to_le_bytes());

    // Terminal record for pgen
    pgen_bytes.extend_from_slice(&0u16.to_le_bytes());
    pgen_bytes.extend_from_slice(&0u16.to_le_bytes());

    // Preset modulators: single 10-byte zero terminal record
    let pmod_bytes = vec![0u8; PMOD_RECORD_SIZE];

    // Instruments, ibag, igen
    let mut current_ibag_idx = 0u16;
    let mut current_igen_idx = 0u16;

    for inst in instruments {
        let name_buf = encode_fixed_string_20(&inst.name);
        inst_bytes.extend_from_slice(&name_buf);
        inst_bytes.extend_from_slice(&current_ibag_idx.to_le_bytes());

        for zone in &inst.zones {
            ibag_bytes.extend_from_slice(&current_igen_idx.to_le_bytes());
            ibag_bytes.extend_from_slice(&0u16.to_le_bytes()); // wInstModNdx = 0
            current_ibag_idx += 1;

            // 1. keyRange
            igen_bytes.extend_from_slice(&GEN_KEY_RANGE.to_le_bytes());
            igen_bytes.extend_from_slice(&[zone.key_range.0, zone.key_range.1]);
            current_igen_idx += 1;

            // 2. velRange
            igen_bytes.extend_from_slice(&GEN_VEL_RANGE.to_le_bytes());
            igen_bytes.extend_from_slice(&[zone.vel_range.0, zone.vel_range.1]);
            current_igen_idx += 1;

            // 3. overridingRootKey
            igen_bytes.extend_from_slice(&GEN_OVERRIDING_ROOT_KEY.to_le_bytes());
            igen_bytes.extend_from_slice(&(zone.root_key as u16).to_le_bytes());
            current_igen_idx += 1;

            // 4. sampleModes
            igen_bytes.extend_from_slice(&GEN_SAMPLE_MODES.to_le_bytes());
            igen_bytes.extend_from_slice(&zone.sample_modes.to_le_bytes());
            current_igen_idx += 1;

            // 5. pan
            if let Some(pan_val) = zone.pan {
                igen_bytes.extend_from_slice(&GEN_PAN.to_le_bytes());
                igen_bytes.extend_from_slice(&pan_val.to_le_bytes());
                current_igen_idx += 1;
            }

            // 6. initialAttenuation
            igen_bytes.extend_from_slice(&GEN_INITIAL_ATTENUATION.to_le_bytes());
            igen_bytes.extend_from_slice(&zone.initial_attenuation.to_le_bytes());
            current_igen_idx += 1;

            // 7. attackVolEnv
            igen_bytes.extend_from_slice(&GEN_ATTACK_VOL_ENV.to_le_bytes());
            igen_bytes.extend_from_slice(&zone.attack_tc.to_le_bytes());
            current_igen_idx += 1;

            // 8. holdVolEnv
            igen_bytes.extend_from_slice(&GEN_HOLD_VOL_ENV.to_le_bytes());
            igen_bytes.extend_from_slice(&zone.hold_tc.to_le_bytes());
            current_igen_idx += 1;

            // 9. decayVolEnv
            igen_bytes.extend_from_slice(&GEN_DECAY_VOL_ENV.to_le_bytes());
            igen_bytes.extend_from_slice(&zone.decay_tc.to_le_bytes());
            current_igen_idx += 1;

            // 10. sustainVolEnv
            igen_bytes.extend_from_slice(&GEN_SUSTAIN_VOL_ENV.to_le_bytes());
            igen_bytes.extend_from_slice(&zone.sustain_cb.to_le_bytes());
            current_igen_idx += 1;

            // 11. releaseVolEnv
            igen_bytes.extend_from_slice(&GEN_RELEASE_VOL_ENV.to_le_bytes());
            igen_bytes.extend_from_slice(&zone.release_tc.to_le_bytes());
            current_igen_idx += 1;

            // 12. scaleTuning
            if let Some(st) = zone.scale_tuning {
                igen_bytes.extend_from_slice(&GEN_SCALE_TUNING.to_le_bytes());
                igen_bytes.extend_from_slice(&st.to_le_bytes());
                current_igen_idx += 1;
            }

            // 13. sampleID (MUST BE LAST!)
            igen_bytes.extend_from_slice(&GEN_SAMPLE_ID.to_le_bytes());
            igen_bytes.extend_from_slice(&zone.sample_id.to_le_bytes());
            current_igen_idx += 1;
        }
    }

    // Terminal record for inst: "EOI"
    inst_bytes.extend_from_slice(&encode_fixed_string_20("EOI"));
    inst_bytes.extend_from_slice(&current_ibag_idx.to_le_bytes());

    // Terminal record for ibag
    ibag_bytes.extend_from_slice(&current_igen_idx.to_le_bytes());
    ibag_bytes.extend_from_slice(&0u16.to_le_bytes());

    // Terminal record for igen
    igen_bytes.extend_from_slice(&0u16.to_le_bytes());
    igen_bytes.extend_from_slice(&0u16.to_le_bytes());

    // Instrument modulators: single 10-byte zero terminal record
    let imod_bytes = vec![0u8; IMOD_RECORD_SIZE];

    // Sample headers (shdr)
    let mut shdr_bytes = Vec::with_capacity((samples.len() + 1) * SHDR_RECORD_SIZE);
    for s in samples {
        shdr_bytes.extend_from_slice(&s.name);
        shdr_bytes.extend_from_slice(&s.dw_start.to_le_bytes());
        shdr_bytes.extend_from_slice(&s.dw_end.to_le_bytes());
        shdr_bytes.extend_from_slice(&s.dw_startloop.to_le_bytes());
        shdr_bytes.extend_from_slice(&s.dw_endloop.to_le_bytes());
        shdr_bytes.extend_from_slice(&s.dw_sample_rate.to_le_bytes());
        shdr_bytes.push(s.by_original_pitch);
        shdr_bytes.push(s.ch_pitch_correction as u8);
        shdr_bytes.extend_from_slice(&s.w_sample_link.to_le_bytes());
        shdr_bytes.extend_from_slice(&s.sf_sample_type.to_le_bytes());
    }

    // Terminal record for shdr: "EOS"
    shdr_bytes.extend_from_slice(&encode_fixed_string_20("EOS"));
    shdr_bytes.extend_from_slice(&0u32.to_le_bytes());
    shdr_bytes.extend_from_slice(&0u32.to_le_bytes());
    shdr_bytes.extend_from_slice(&0u32.to_le_bytes());
    shdr_bytes.extend_from_slice(&0u32.to_le_bytes());
    shdr_bytes.extend_from_slice(&0u32.to_le_bytes());
    shdr_bytes.push(0);
    shdr_bytes.push(0);
    shdr_bytes.extend_from_slice(&0u16.to_le_bytes());
    shdr_bytes.extend_from_slice(&0u16.to_le_bytes());

    // Assemble LIST 'pdta' body
    let mut pdta_body = Vec::new();
    pdta_body.extend_from_slice(b"pdta");

    pdta_body.extend_from_slice(b"phdr");
    pdta_body.extend_from_slice(&(phdr_bytes.len() as u32).to_le_bytes());
    pdta_body.extend_from_slice(&phdr_bytes);

    pdta_body.extend_from_slice(b"pbag");
    pdta_body.extend_from_slice(&(pbag_bytes.len() as u32).to_le_bytes());
    pdta_body.extend_from_slice(&pbag_bytes);

    pdta_body.extend_from_slice(b"pmod");
    pdta_body.extend_from_slice(&(pmod_bytes.len() as u32).to_le_bytes());
    pdta_body.extend_from_slice(&pmod_bytes);

    pdta_body.extend_from_slice(b"pgen");
    pdta_body.extend_from_slice(&(pgen_bytes.len() as u32).to_le_bytes());
    pdta_body.extend_from_slice(&pgen_bytes);

    pdta_body.extend_from_slice(b"inst");
    pdta_body.extend_from_slice(&(inst_bytes.len() as u32).to_le_bytes());
    pdta_body.extend_from_slice(&inst_bytes);

    pdta_body.extend_from_slice(b"ibag");
    pdta_body.extend_from_slice(&(ibag_bytes.len() as u32).to_le_bytes());
    pdta_body.extend_from_slice(&ibag_bytes);

    pdta_body.extend_from_slice(b"imod");
    pdta_body.extend_from_slice(&(imod_bytes.len() as u32).to_le_bytes());
    pdta_body.extend_from_slice(&imod_bytes);

    pdta_body.extend_from_slice(b"igen");
    pdta_body.extend_from_slice(&(igen_bytes.len() as u32).to_le_bytes());
    pdta_body.extend_from_slice(&igen_bytes);

    pdta_body.extend_from_slice(b"shdr");
    pdta_body.extend_from_slice(&(shdr_bytes.len() as u32).to_le_bytes());
    pdta_body.extend_from_slice(&shdr_bytes);

    let mut pdta_chunk = Vec::new();
    pdta_chunk.extend_from_slice(b"LIST");
    pdta_chunk.extend_from_slice(&(pdta_body.len() as u32).to_le_bytes());
    pdta_chunk.extend_from_slice(&pdta_body);

    // 4. Assemble Top-Level RIFF Container
    let total_riff_size = 4 + info_chunk.len() + sdta_chunk.len() + pdta_chunk.len();
    let mut riff = Vec::with_capacity(8 + total_riff_size);
    riff.extend_from_slice(b"RIFF");
    riff.extend_from_slice(&(total_riff_size as u32).to_le_bytes());
    riff.extend_from_slice(b"sfbk");
    riff.extend_from_slice(&info_chunk);
    riff.extend_from_slice(&sdta_chunk);
    riff.extend_from_slice(&pdta_chunk);

    Ok(riff)
}

/// Helper to group key table indices into contiguous ranges `(key_min, key_max, child_idx)`.
fn group_key_ranges(table: &[u8], num_children: usize) -> Vec<(u8, u8, usize)> {
    if table.is_empty() || num_children == 0 {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let mut current_idx = table[0] as usize;
    let mut start_k = 0u8;

    for (k, &child_idx) in table.iter().enumerate().take(table.len().min(128)).skip(1) {
        let idx = child_idx as usize;
        if idx != current_idx {
            if current_idx < num_children {
                ranges.push((start_k, (k - 1) as u8, current_idx));
            }
            start_k = k as u8;
            current_idx = idx;
        }
    }
    if current_idx < num_children && start_k < 128 {
        let end_k = (table.len().min(128) - 1) as u8;
        ranges.push((start_k, end_k, current_idx));
    }
    ranges
}

/// Parse child voices from a secondary VoiceGroup table at `sub_ptr`.
fn parse_child_voice_table(rom: &[u8], sub_ptr: u32, max_entries: usize) -> Vec<VoiceEntry> {
    let mut out = Vec::new();
    let off = match rom_ptr_to_offset(sub_ptr, rom.len()) {
        Some(o) => o,
        None => return out,
    };
    for i in 0..max_entries {
        let entry_off = off + i * VOICE_ENTRY_SIZE;
        if entry_off + VOICE_ENTRY_SIZE > rom.len() {
            break;
        }
        if let Ok(entry) = parse_voice_entry(rom, entry_off) {
            out.push(entry);
        } else {
            break;
        }
    }
    out
}

/// Raw sample descriptor used for SoundFont compilation from arbitrary engines (GAX, MusyX, etc.).
#[derive(Debug, Clone)]
pub struct RawSoundFontSample {
    pub name: String,
    pub sample_rate: u32,
    pub root_key: u8,
    pub loop_range: Option<(u32, u32)>,
    pub pcm16: Vec<i16>,
}

/// Compile a collection of arbitrary 16-bit linear PCM samples into a standard SoundFont 2.04 soundbank.
pub fn compile_custom_samples_soundfont(
    bank_title: &str,
    sound_engine: &str,
    samples_in: &[RawSoundFontSample],
) -> Result<SoundFontExport> {
    if samples_in.is_empty() {
        return Err(Error::InvalidField {
            what: "no_samples_provided_for_soundfont",
            value: 0,
        });
    }

    let mut smpl_pcm_bytes = Vec::new();
    let mut samples = Vec::with_capacity(samples_in.len());
    let mut instruments = Vec::with_capacity(samples_in.len());
    let mut presets = Vec::with_capacity(samples_in.len());
    let mut current_sample_idx = 0usize;

    for (i, s) in samples_in.iter().enumerate() {
        let dw_start = current_sample_idx as u32;
        let dw_end = (current_sample_idx + s.pcm16.len()) as u32;
        let (dw_startloop, dw_endloop) = if let Some((start_l, end_l)) = s.loop_range {
            let loop_s = (dw_start + start_l).min(dw_end.saturating_sub(1));
            let loop_e = (dw_start + end_l).min(dw_end);
            if loop_s < loop_e {
                (loop_s, loop_e)
            } else {
                (dw_start, dw_end)
            }
        } else {
            (dw_start, dw_end)
        };

        for &val in &s.pcm16 {
            smpl_pcm_bytes.extend_from_slice(&val.to_le_bytes());
        }
        for _ in 0..SAMPLE_TERMINAL_PADDING {
            smpl_pcm_bytes.extend_from_slice(&0i16.to_le_bytes());
        }
        current_sample_idx += s.pcm16.len() + SAMPLE_TERMINAL_PADDING;

        let sample_rate = if s.sample_rate == 0 { 18_158 } else { s.sample_rate };
        let sample_rec = SampleRecord {
            name: encode_fixed_string_20(&s.name),
            dw_start,
            dw_end,
            dw_startloop,
            dw_endloop,
            dw_sample_rate: sample_rate,
            by_original_pitch: s.root_key,
            ch_pitch_correction: 0,
            w_sample_link: 0,
            sf_sample_type: 1, // monoSample
        };
        samples.push(sample_rec);

        let is_loop = s.loop_range.is_some();
        let inst_id = instruments.len() as u16;
        instruments.push(InstrumentBuilder {
            name: format!("Inst_{:03}", i),
            zones: vec![InstrumentZoneBuilder {
                key_range: (0, 127),
                vel_range: (0, 127),
                root_key: s.root_key,
                sample_modes: if is_loop { 1 } else { 0 },
                pan: None,
                initial_attenuation: 0,
                attack_tc: -12000,
                hold_tc: -12000,
                decay_tc: 8000,
                sustain_cb: 0,
                release_tc: -3000,
                scale_tuning: None,
                sample_id: inst_id,
            }],
        });

        let bank = (i / 128) as u16;
        let preset = (i % 128) as u16;
        presets.push(PresetBuilder {
            bank,
            preset,
            name: format!("Prog_{:03}", i),
            zones: vec![PresetZoneBuilder {
                key_range: (0, 127),
                vel_range: (0, 127),
                instrument_id: inst_id,
            }],
        });
    }

    let sf2_bytes = assemble_sf2_binary(
        bank_title,
        sound_engine,
        &smpl_pcm_bytes,
        &samples,
        &instruments,
        &presets,
    )?;

    let preset_summaries: Vec<PresetSummary> = presets
        .iter()
        .map(|p| PresetSummary {
            bank: p.bank,
            preset: p.preset,
            name: p.name.clone(),
            zones: p.zones.len(),
        })
        .collect();

    let report = SoundFontReport {
        title: bank_title.to_string(),
        engine: sound_engine.to_string(),
        version: format!("{}.{:02}", SF2_VERSION_MAJOR, SF2_VERSION_MINOR),
        total_presets: presets.len(),
        total_instruments: instruments.len(),
        total_samples: samples.len(),
        sample_data_bytes: smpl_pcm_bytes.len(),
        file_size_bytes: sf2_bytes.len(),
        voicegroups_mapped: 0,
        presets: preset_summaries,
    };

    Ok(SoundFontExport { sf2_bytes, report })
}

/// Compile a SoundFont 2.04 soundbank from any supported GBA sound engine (M4A, GAX, MusyX).
pub fn compile_rom_soundfont(rom: &[u8], title_override: Option<&str>) -> Result<SoundFontExport> {
    match detect_sound_engine(rom) {
        DetectedSoundEngine::M4a(_) => compile_m4a_soundfont(rom, title_override),
        DetectedSoundEngine::Gax(_) => crate::gax_pool::compile_gax_soundfont(rom, title_override),
        DetectedSoundEngine::Musyx(_) => crate::musyx_pool::compile_musyx_soundfont(rom, title_override),
        DetectedSoundEngine::Krawall(_) => crate::krawall_pool::compile_krawall_soundfont(rom, title_override),
        DetectedSoundEngine::Rare(_) => crate::rare_pool::compile_rare_soundfont(rom, title_override),
        DetectedSoundEngine::Konami(_, _) => crate::konami_pool::compile_konami_soundfont(rom, title_override),
        DetectedSoundEngine::LogikState(_) => crate::logik_pool::compile_logik_soundfont(rom, title_override),
        DetectedSoundEngine::Paragon5(_) => crate::paragon5_pool::compile_paragon5_soundfont(rom, title_override),
        DetectedSoundEngine::Rd1(_) => crate::rd1_pool::compile_rd1_soundfont(rom, title_override),
        DetectedSoundEngine::Southpaw(_) => crate::southpaw_pool::compile_southpaw_soundfont(rom, title_override),
        DetectedSoundEngine::Custom => {
            // Check if GAX or MusyX sample tables exist in ROM as fallback
            let gax = crate::gax_pool::scan_gax_soundpool(rom);
            if !gax.is_empty() {
                return crate::gax_pool::compile_gax_soundfont(rom, title_override);
            }
            let msx = crate::musyx_pool::scan_musyx_soundpool(rom);
            if !msx.is_empty() {
                return crate::musyx_pool::compile_musyx_soundfont(rom, title_override);
            }
            Err(Error::InvalidField {
                what: "no_supported_sound_engine_or_samples_detected",
                value: 0,
            })
        }
    }
}

/// High-level compiler converting GBA M4A ROM audio into a standard SoundFont 2.04 soundbank.
pub fn compile_m4a_soundfont(rom: &[u8], title_override: Option<&str>) -> Result<SoundFontExport> {
    let scan = scan_m4a_engine(rom).ok_or(Error::InvalidField {
        what: "no_m4a_sound_engine_detected",
        value: 0,
    })?;

    let bank_title = title_override
        .map(|s| s.to_string())
        .unwrap_or_else(|| extract_rom_title(rom));

    // 1. Collect all unique DirectSound sample pointers across all voicegroups
    let mut unique_samples = Vec::new();
    let mut sample_index_map = HashMap::new();

    for &vg_ptr in &scan.voicegroups {
        let ptrs = collect_voicegroup_sample_pointers(rom, vg_ptr);
        for ptr in ptrs {
            if let std::collections::hash_map::Entry::Vacant(e) = sample_index_map.entry(ptr) {
                if let Some(off) = rom_ptr_to_offset(ptr, rom.len()) {
                    if let Ok((hdr, pcm)) = extract_pcm_i8(&rom[off..], 10_000_000) {
                        let idx = unique_samples.len();
                        e.insert(idx);
                        unique_samples.push((ptr, hdr, pcm));
                    }
                }
            }
        }
    }

    if unique_samples.is_empty() {
        return Err(Error::InvalidField {
            what: "no_directsound_samples_found",
            value: 0,
        });
    }

    // 2. Build `smpl` audio data and `shdr` records
    let mut smpl_pcm_bytes = Vec::new();
    let mut samples = Vec::with_capacity(unique_samples.len());
    let mut current_sample_idx = 0usize;

    for (ptr, hdr, pcm) in &unique_samples {
        let dw_start = current_sample_idx as u32;
        let dw_end = (current_sample_idx + pcm.len()) as u32;
        let (dw_startloop, dw_endloop) = if hdr.is_looped() {
            let loop_s = (dw_start + hdr.loop_start).min(dw_end.saturating_sub(1));
            let loop_e = dw_end;
            if loop_s < loop_e {
                (loop_s, loop_e)
            } else {
                (dw_start, dw_end)
            }
        } else {
            (dw_start, dw_end)
        };

        for &s in pcm {
            let s16 = (s as i16) << 8;
            smpl_pcm_bytes.extend_from_slice(&s16.to_le_bytes());
        }
        for _ in 0..SAMPLE_TERMINAL_PADDING {
            smpl_pcm_bytes.extend_from_slice(&0i16.to_le_bytes());
        }
        current_sample_idx += pcm.len() + SAMPLE_TERMINAL_PADDING;

        let rate = hdr.base_sample_rate_hz().max(1000);
        let sample_name = format!("SMP_{:06X}", (ptr - ROM_BUS_START) & 0xFFFFFF);

        samples.push(SampleRecord {
            name: encode_fixed_string_20(&sample_name),
            dw_start,
            dw_end,
            dw_startloop,
            dw_endloop,
            dw_sample_rate: rate,
            by_original_pitch: 60,
            ch_pitch_correction: 0,
            w_sample_link: 0,
            sf_sample_type: 1, // monoSample
        });
    }

    // 3. Compile Instruments and Presets with deduplication
    let mut instruments = Vec::new();
    let mut presets = Vec::new();
    let mut seen_preset_slots = HashSet::new();

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    enum InstrumentKey {
        DirectSound {
            sample_ptr: u32,
            base_key: u8,
            pan: u8,
            adsr: [u8; 4],
            is_fixed: bool,
        },
        Keysplit {
            sub_vg_ptr: u32,
            keysplit_table_ptr: u32,
        },
        Drumkit {
            drum_vg_ptr: u32,
        },
    }

    let mut instrument_cache: HashMap<InstrumentKey, u16> = HashMap::new();

    for (vg_idx, &vg_ptr) in scan.voicegroups.iter().enumerate() {
        let vg_off = match rom_ptr_to_offset(vg_ptr, rom.len()) {
            Some(o) => o,
            None => continue,
        };
        let available = (rom.len() - vg_off) / VOICE_ENTRY_SIZE;
        let count = 128.min(available);

        for p in 0..count {
            let entry_off = vg_off + p * VOICE_ENTRY_SIZE;
            let voice = match parse_voice_entry(rom, entry_off) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let is_directsound_or_dpcm = voice.kind.is_directsound()
                || matches!(voice.kind, VoiceKind::Unknown(raw) if (raw & 0x20) != 0 && (raw & 0x07) == 0);

            if is_directsound_or_dpcm {
                if let Some(&s_id) = sample_index_map.get(&voice.data_ptr) {
                    let adsr = [voice.attack, voice.decay, voice.sustain, voice.release];
                    let is_fixed = voice.kind == VoiceKind::DirectSoundFixed
                        || matches!(voice.kind, VoiceKind::Unknown(raw) if (raw & 0x08) != 0);
                    let inst_key = InstrumentKey::DirectSound {
                        sample_ptr: voice.data_ptr,
                        base_key: voice.base_key,
                        pan: voice.pan_or_sweep,
                        adsr,
                        is_fixed,
                    };

                    let inst_id = match instrument_cache.get(&inst_key) {
                        Some(&id) => id,
                        None => {
                            let root_k = if voice.base_key > 0 { voice.base_key } else { 60 };
                            let sample_info = &unique_samples[s_id];
                            let is_loop = sample_info.1.is_looped();
                            let pan_val = if voice.pan_or_sweep & 0x80 != 0 {
                                let midi_pan = voice.pan_or_sweep & 0x7F;
                                Some(((midi_pan as i32 - 64) * 500 / 64) as i16)
                            } else {
                                None
                            };
                            let scale_tuning = if is_fixed { Some(0) } else { None };

                            let new_id = instruments.len() as u16;
                            instruments.push(InstrumentBuilder {
                                name: format!("Inst_{:03}", new_id),
                                zones: vec![InstrumentZoneBuilder {
                                    key_range: (0, 127),
                                    vel_range: (0, 127),
                                    root_key: root_k,
                                    sample_modes: if is_loop { 1 } else { 0 },
                                    pan: pan_val,
                                    initial_attenuation: 0,
                                    attack_tc: m4a_attack_to_timecents(voice.attack),
                                    hold_tc: -12000,
                                    decay_tc: m4a_decay_to_timecents(voice.decay),
                                    sustain_cb: m4a_sustain_to_centibels(voice.sustain),
                                    release_tc: m4a_release_to_timecents(voice.release),
                                    scale_tuning,
                                    sample_id: s_id as u16,
                                }],
                            });
                            instrument_cache.insert(inst_key, new_id);
                            new_id
                        }
                    };

                    // Assign preset slot (Bank = vg_idx min 127, Preset = p)
                    let bank = (vg_idx as u16).min(127);
                    let pr = (p as u16).min(127);
                    if seen_preset_slots.insert((bank, pr)) {
                        presets.push(PresetBuilder {
                            bank,
                            preset: pr,
                            name: format!("Prog_{:03}", pr),
                            zones: vec![PresetZoneBuilder {
                                key_range: (0, 127),
                                vel_range: (0, 127),
                                instrument_id: inst_id,
                            }],
                        });
                    }
                }
            } else if voice.kind == VoiceKind::Keysplit {
                let p2 = u32::from_le_bytes([voice.attack, voice.decay, voice.sustain, voice.release]);
                let inst_key = InstrumentKey::Keysplit {
                    sub_vg_ptr: voice.data_ptr,
                    keysplit_table_ptr: p2,
                };

                let inst_id_opt = match instrument_cache.get(&inst_key) {
                    Some(&id) => Some(id),
                    None => {
                        let children = parse_child_voice_table(rom, voice.data_ptr, 32);
                        if !children.is_empty() {
                            let ranges = if let Some(table_off) = rom_ptr_to_offset(p2, rom.len()) {
                                if table_off + 128 <= rom.len() {
                                    group_key_ranges(&rom[table_off..table_off + 128], children.len())
                                } else {
                                    Vec::new()
                                }
                            } else {
                                Vec::new()
                            };

                            let effective_ranges = if !ranges.is_empty() {
                                ranges
                            } else {
                                (0..children.len())
                                    .map(|i| {
                                        let kmin = (i * 128 / children.len()) as u8;
                                        let kmax = (((i + 1) * 128 / children.len()) - 1) as u8;
                                        (kmin, kmax, i)
                                    })
                                    .collect()
                            };

                            let mut inst_zones = Vec::new();
                            for (kmin, kmax, c_idx) in effective_ranges {
                                if c_idx < children.len() {
                                    let cv = &children[c_idx];
                                    if let Some(&s_id) = sample_index_map.get(&cv.data_ptr) {
                                        let root_k = if cv.base_key > 0 { cv.base_key } else { 60 };
                                        let is_loop = unique_samples[s_id].1.is_looped();
                                        let pan_val = if cv.pan_or_sweep & 0x80 != 0 {
                                            let midi_pan = cv.pan_or_sweep & 0x7F;
                                            Some(((midi_pan as i32 - 64) * 500 / 64) as i16)
                                        } else {
                                            None
                                        };
                                        let scale_tuning = if cv.kind == VoiceKind::DirectSoundFixed {
                                            Some(0)
                                        } else {
                                            None
                                        };

                                        inst_zones.push(InstrumentZoneBuilder {
                                            key_range: (kmin, kmax),
                                            vel_range: (0, 127),
                                            root_key: root_k,
                                            sample_modes: if is_loop { 1 } else { 0 },
                                            pan: pan_val,
                                            initial_attenuation: 0,
                                            attack_tc: m4a_attack_to_timecents(cv.attack),
                                            hold_tc: -12000,
                                            decay_tc: m4a_decay_to_timecents(cv.decay),
                                            sustain_cb: m4a_sustain_to_centibels(cv.sustain),
                                            release_tc: m4a_release_to_timecents(cv.release),
                                            scale_tuning,
                                            sample_id: s_id as u16,
                                        });
                                    }
                                }
                            }

                            if !inst_zones.is_empty() {
                                let new_id = instruments.len() as u16;
                                instruments.push(InstrumentBuilder {
                                    name: format!("Split_{:03}", new_id),
                                    zones: inst_zones,
                                });
                                instrument_cache.insert(inst_key, new_id);
                                Some(new_id)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                };

                if let Some(inst_id) = inst_id_opt {
                    let bank = (vg_idx as u16).min(127);
                    let pr = (p as u16).min(127);
                    if seen_preset_slots.insert((bank, pr)) {
                        presets.push(PresetBuilder {
                            bank,
                            preset: pr,
                            name: format!("Split_{:03}", pr),
                            zones: vec![PresetZoneBuilder {
                                key_range: (0, 127),
                                vel_range: (0, 127),
                                instrument_id: inst_id,
                            }],
                        });
                    }
                }
            } else if voice.kind == VoiceKind::Drumkit {
                let inst_key = InstrumentKey::Drumkit {
                    drum_vg_ptr: voice.data_ptr,
                };

                let inst_id_opt = match instrument_cache.get(&inst_key) {
                    Some(&id) => Some(id),
                    None => {
                        let drum_voices = parse_child_voice_table(rom, voice.data_ptr, 128);
                        let mut drum_zones = Vec::new();

                        for (note, dv) in drum_voices.iter().enumerate() {
                            if let Some(&s_id) = sample_index_map.get(&dv.data_ptr) {
                                let root_k = if dv.base_key > 0 { dv.base_key } else { note as u8 };
                                let is_loop = unique_samples[s_id].1.is_looped();
                                let pan_val = if dv.pan_or_sweep & 0x80 != 0 {
                                    let midi_pan = dv.pan_or_sweep & 0x7F;
                                    Some(((midi_pan as i32 - 64) * 500 / 64) as i16)
                                } else {
                                    None
                                };

                                drum_zones.push(InstrumentZoneBuilder {
                                    key_range: (note as u8, note as u8),
                                    vel_range: (0, 127),
                                    root_key: root_k,
                                    sample_modes: if is_loop { 1 } else { 0 },
                                    pan: pan_val,
                                    initial_attenuation: 0,
                                    attack_tc: m4a_attack_to_timecents(dv.attack),
                                    hold_tc: -12000,
                                    decay_tc: m4a_decay_to_timecents(dv.decay),
                                    sustain_cb: m4a_sustain_to_centibels(dv.sustain),
                                    release_tc: m4a_release_to_timecents(dv.release),
                                    scale_tuning: Some(0),
                                    sample_id: s_id as u16,
                                });
                            }
                        }

                        if !drum_zones.is_empty() {
                            let new_id = instruments.len() as u16;
                            instruments.push(InstrumentBuilder {
                                name: format!("Drums_{:03}", new_id),
                                zones: drum_zones,
                            });
                            instrument_cache.insert(inst_key, new_id);
                            Some(new_id)
                        } else {
                            None
                        }
                    }
                };

                if let Some(inst_id) = inst_id_opt {
                    // Drum presets live in MIDI Bank 128
                    let bank = 128u16;
                    let mut pr = (vg_idx as u16).min(127);
                    while seen_preset_slots.contains(&(bank, pr)) && pr < 127 {
                        pr += 1;
                    }
                    if seen_preset_slots.insert((bank, pr)) {
                        presets.push(PresetBuilder {
                            bank,
                            preset: pr,
                            name: format!("Drums_{:03}", pr),
                            zones: vec![PresetZoneBuilder {
                                key_range: (0, 127),
                                vel_range: (0, 127),
                                instrument_id: inst_id,
                            }],
                        });
                    }
                }
            }
        }
    }

    if presets.is_empty() {
        return Err(Error::InvalidField {
            what: "no_soundfont_presets_generated",
            value: 0,
        });
    }

    let sf2_bytes = assemble_sf2_binary(
        &bank_title,
        SF2_SOUND_ENGINE,
        &smpl_pcm_bytes,
        &samples,
        &instruments,
        &presets,
    )?;

    let preset_summaries: Vec<PresetSummary> = presets
        .iter()
        .map(|p| PresetSummary {
            bank: p.bank,
            preset: p.preset,
            name: p.name.clone(),
            zones: p.zones.len(),
        })
        .collect();

    let report = SoundFontReport {
        title: bank_title,
        engine: SF2_SOUND_ENGINE.to_string(),
        version: format!("{}.{:02}", SF2_VERSION_MAJOR, SF2_VERSION_MINOR),
        total_presets: presets.len(),
        total_instruments: instruments.len(),
        total_samples: samples.len(),
        sample_data_bytes: smpl_pcm_bytes.len(),
        file_size_bytes: sf2_bytes.len(),
        voicegroups_mapped: scan.voicegroups.len(),
        presets: preset_summaries,
    };

    Ok(SoundFontExport { sf2_bytes, report })
}

/// Validate and inspect an SF2 binary stream against the SoundFont 2.04 specification.
pub fn validate_soundfont(sf2_bytes: &[u8]) -> Result<SoundFontValidation> {
    if sf2_bytes.len() < 12 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 12,
            available: sf2_bytes.len(),
        });
    }

    if &sf2_bytes[0..4] != b"RIFF" {
        return Err(Error::InvalidField {
            what: "sf2_riff_magic",
            value: 0,
        });
    }

    let riff_size = u32::from_le_bytes([
        sf2_bytes[4],
        sf2_bytes[5],
        sf2_bytes[6],
        sf2_bytes[7],
    ]) as usize;

    if 8 + riff_size != sf2_bytes.len() {
        return Err(Error::InvalidField {
            what: "sf2_riff_size_mismatch",
            value: riff_size as u64,
        });
    }

    if &sf2_bytes[8..12] != b"sfbk" {
        return Err(Error::InvalidField {
            what: "sf2_sfbk_magic",
            value: 0,
        });
    }

    let mut pos = 12;
    let mut major_version = 0u16;
    let mut minor_version = 0u16;
    let mut sound_engine = String::new();
    let mut bank_name = String::new();

    let mut smpl_byte_len = 0usize;
    let mut phdr_count = 0usize;
    let mut pbag_count = 0usize;
    let mut pgen_count = 0usize;
    let mut inst_count = 0usize;
    let mut ibag_count = 0usize;
    let mut igen_count = 0usize;
    let mut shdr_count = 0usize;

    while pos + 8 <= sf2_bytes.len() {
        let chunk_id = &sf2_bytes[pos..pos + 4];
        let chunk_len = u32::from_le_bytes([
            sf2_bytes[pos + 4],
            sf2_bytes[pos + 5],
            sf2_bytes[pos + 6],
            sf2_bytes[pos + 7],
        ]) as usize;
        pos += 8;

        if pos + chunk_len > sf2_bytes.len() {
            return Err(Error::Truncated {
                offset: pos,
                needed: chunk_len,
                available: sf2_bytes.len() - pos,
            });
        }

        if chunk_id == b"LIST" {
            let list_type = &sf2_bytes[pos..pos + 4];
            let list_end = pos + chunk_len;
            let mut sub_pos = pos + 4;

            if list_type == b"INFO" {
                while sub_pos + 8 <= list_end {
                    let sub_id = &sf2_bytes[sub_pos..sub_pos + 4];
                    let sub_len = u32::from_le_bytes([
                        sf2_bytes[sub_pos + 4],
                        sf2_bytes[sub_pos + 5],
                        sf2_bytes[sub_pos + 6],
                        sf2_bytes[sub_pos + 7],
                    ]) as usize;
                    sub_pos += 8;

                    if sub_pos + sub_len > list_end {
                        return Err(Error::Truncated {
                            offset: sub_pos,
                            needed: sub_len,
                            available: list_end - sub_pos,
                        });
                    }

                    if sub_id == b"ifil" && sub_len >= 4 {
                        major_version = u16::from_le_bytes([sf2_bytes[sub_pos], sf2_bytes[sub_pos + 1]]);
                        minor_version = u16::from_le_bytes([sf2_bytes[sub_pos + 2], sf2_bytes[sub_pos + 3]]);
                    } else if sub_id == b"isng" {
                        sound_engine = String::from_utf8_lossy(&sf2_bytes[sub_pos..sub_pos + sub_len])
                            .trim_matches('\0')
                            .to_string();
                    } else if sub_id == b"INAM" {
                        bank_name = String::from_utf8_lossy(&sf2_bytes[sub_pos..sub_pos + sub_len])
                            .trim_matches('\0')
                            .to_string();
                    }
                    sub_pos += sub_len;
                    if !sub_len.is_multiple_of(2) {
                        sub_pos += 1;
                    }
                }
            } else if list_type == b"sdta" {
                while sub_pos + 8 <= list_end {
                    let sub_id = &sf2_bytes[sub_pos..sub_pos + 4];
                    let sub_len = u32::from_le_bytes([
                        sf2_bytes[sub_pos + 4],
                        sf2_bytes[sub_pos + 5],
                        sf2_bytes[sub_pos + 6],
                        sf2_bytes[sub_pos + 7],
                    ]) as usize;
                    sub_pos += 8;

                    if sub_id == b"smpl" {
                        smpl_byte_len = sub_len;
                    }
                    sub_pos += sub_len;
                    if !sub_len.is_multiple_of(2) {
                        sub_pos += 1;
                    }
                }
            } else if list_type == b"pdta" {
                while sub_pos + 8 <= list_end {
                    let sub_id = &sf2_bytes[sub_pos..sub_pos + 4];
                    let sub_len = u32::from_le_bytes([
                        sf2_bytes[sub_pos + 4],
                        sf2_bytes[sub_pos + 5],
                        sf2_bytes[sub_pos + 6],
                        sf2_bytes[sub_pos + 7],
                    ]) as usize;
                    sub_pos += 8;

                    if sub_pos + sub_len > list_end {
                        return Err(Error::Truncated {
                            offset: sub_pos,
                            needed: sub_len,
                            available: list_end - sub_pos,
                        });
                    }

                    match sub_id {
                        b"phdr" => {
                            if !sub_len.is_multiple_of(PHDR_RECORD_SIZE) {
                                return Err(Error::InvalidField {
                                    what: "sf2_phdr_size_not_multiple",
                                    value: sub_len as u64,
                                });
                            }
                            phdr_count = sub_len / PHDR_RECORD_SIZE;
                            // Verify EOP terminal record
                            let last_off = sub_pos + sub_len - PHDR_RECORD_SIZE;
                            if &sf2_bytes[last_off..last_off + 3] != b"EOP" {
                                return Err(Error::InvalidField {
                                    what: "sf2_missing_eop",
                                    value: 0,
                                });
                            }
                            // Verify (bank, preset) sorting
                            let mut prev = (0u16, 0u16);
                            for i in 0..phdr_count - 1 {
                                let rec = sub_pos + i * PHDR_RECORD_SIZE;
                                let pr = u16::from_le_bytes([sf2_bytes[rec + 20], sf2_bytes[rec + 21]]);
                                let bk = u16::from_le_bytes([sf2_bytes[rec + 22], sf2_bytes[rec + 23]]);
                                if i > 0 && (bk, pr) <= prev {
                                    return Err(Error::InvalidField {
                                        what: "sf2_presets_not_sorted",
                                        value: ((bk as u64) << 16) | (pr as u64),
                                    });
                                }
                                prev = (bk, pr);
                            }
                        }
                        b"pbag" => {
                            if !sub_len.is_multiple_of(PBAG_RECORD_SIZE) {
                                return Err(Error::InvalidField {
                                    what: "sf2_pbag_size_not_multiple",
                                    value: sub_len as u64,
                                });
                            }
                            pbag_count = sub_len / PBAG_RECORD_SIZE;
                        }
                        b"pgen" => {
                            if !sub_len.is_multiple_of(PGEN_RECORD_SIZE) {
                                return Err(Error::InvalidField {
                                    what: "sf2_pgen_size_not_multiple",
                                    value: sub_len as u64,
                                });
                            }
                            pgen_count = sub_len / PGEN_RECORD_SIZE;
                        }
                        b"inst" => {
                            if !sub_len.is_multiple_of(INST_RECORD_SIZE) {
                                return Err(Error::InvalidField {
                                    what: "sf2_inst_size_not_multiple",
                                    value: sub_len as u64,
                                });
                            }
                            inst_count = sub_len / INST_RECORD_SIZE;
                            // Verify EOI terminal record
                            let last_off = sub_pos + sub_len - INST_RECORD_SIZE;
                            if &sf2_bytes[last_off..last_off + 3] != b"EOI" {
                                return Err(Error::InvalidField {
                                    what: "sf2_missing_eoi",
                                    value: 0,
                                });
                            }
                        }
                        b"ibag" => {
                            if !sub_len.is_multiple_of(IBAG_RECORD_SIZE) {
                                return Err(Error::InvalidField {
                                    what: "sf2_ibag_size_not_multiple",
                                    value: sub_len as u64,
                                });
                            }
                            ibag_count = sub_len / IBAG_RECORD_SIZE;
                        }
                        b"igen" => {
                            if !sub_len.is_multiple_of(IGEN_RECORD_SIZE) {
                                return Err(Error::InvalidField {
                                    what: "sf2_igen_size_not_multiple",
                                    value: sub_len as u64,
                                });
                            }
                            igen_count = sub_len / IGEN_RECORD_SIZE;
                        }
                        b"shdr" => {
                            if !sub_len.is_multiple_of(SHDR_RECORD_SIZE) {
                                return Err(Error::InvalidField {
                                    what: "sf2_shdr_size_not_multiple",
                                    value: sub_len as u64,
                                });
                            }
                            shdr_count = sub_len / SHDR_RECORD_SIZE;
                            // Verify EOS terminal record
                            let last_off = sub_pos + sub_len - SHDR_RECORD_SIZE;
                            if &sf2_bytes[last_off..last_off + 3] != b"EOS" {
                                return Err(Error::InvalidField {
                                    what: "sf2_missing_eos",
                                    value: 0,
                                });
                            }
                            // Validate sample offset bounds
                            let total_sample_points = (smpl_byte_len / 2) as u32;
                            for i in 0..shdr_count - 1 {
                                let rec = sub_pos + i * SHDR_RECORD_SIZE;
                                let dw_start = u32::from_le_bytes([sf2_bytes[rec + 20], sf2_bytes[rec + 21], sf2_bytes[rec + 22], sf2_bytes[rec + 23]]);
                                let dw_end = u32::from_le_bytes([sf2_bytes[rec + 24], sf2_bytes[rec + 25], sf2_bytes[rec + 26], sf2_bytes[rec + 27]]);
                                let dw_startloop = u32::from_le_bytes([sf2_bytes[rec + 28], sf2_bytes[rec + 29], sf2_bytes[rec + 30], sf2_bytes[rec + 31]]);
                                let dw_endloop = u32::from_le_bytes([sf2_bytes[rec + 32], sf2_bytes[rec + 33], sf2_bytes[rec + 34], sf2_bytes[rec + 35]]);

                                if dw_start > dw_startloop || dw_startloop > dw_endloop || dw_endloop > dw_end || dw_end > total_sample_points {
                                    return Err(Error::InvalidField {
                                        what: "sf2_shdr_sample_bounds_invalid",
                                        value: dw_end as u64,
                                    });
                                }
                            }
                        }
                        _ => {}
                    }
                    sub_pos += sub_len;
                    if !sub_len.is_multiple_of(2) {
                        sub_pos += 1;
                    }
                }
            }
        }
        pos += chunk_len;
        if !chunk_len.is_multiple_of(2) {
            pos += 1;
        }
    }

    if major_version != SF2_VERSION_MAJOR {
        return Err(Error::InvalidField {
            what: "sf2_major_version_mismatch",
            value: major_version as u64,
        });
    }

    Ok(SoundFontValidation {
        major_version,
        minor_version,
        sound_engine,
        bank_name,
        sample_count: shdr_count.saturating_sub(1),
        smpl_byte_len,
        preset_count: phdr_count.saturating_sub(1),
        instrument_count: inst_count.saturating_sub(1),
        pbag_count: pbag_count.saturating_sub(1),
        ibag_count: ibag_count.saturating_sub(1),
        pgen_count: pgen_count.saturating_sub(1),
        igen_count: igen_count.saturating_sub(1),
    })
}
