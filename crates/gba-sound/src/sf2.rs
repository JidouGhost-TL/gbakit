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

/// Target Sound Engine string embedded into SF2 INFO chunk.
pub const SF2_SOUND_ENGINE: &str = "GBAKit DirectSound";

/// Maximum aggregate decoded PCM samples permitted across a single compiled M4A SoundFont
/// (32M samples = 64 MiB PCM16) to prevent unbounded memory allocation.
pub const MAX_SF2_BANK_SAMPLES: usize = 32 * 1024 * 1024;

pub use formatkit_codec::sf2::{
    GEN_ATTACK_VOL_ENV, GEN_COARSE_TUNE, GEN_DECAY_VOL_ENV, GEN_FINE_TUNE, GEN_HOLD_VOL_ENV,
    GEN_INITIAL_ATTENUATION, GEN_INSTRUMENT, GEN_KEY_RANGE, GEN_OVERRIDING_ROOT_KEY, GEN_PAN,
    GEN_RELEASE_VOL_ENV, GEN_SAMPLE_ID, GEN_SAMPLE_MODES, GEN_SCALE_TUNING, GEN_SUSTAIN_VOL_ENV,
    GEN_VEL_RANGE, IBAG_RECORD_SIZE, IGEN_RECORD_SIZE, IMOD_RECORD_SIZE, INST_RECORD_SIZE,
    PBAG_RECORD_SIZE, PGEN_RECORD_SIZE, PHDR_RECORD_SIZE, PMOD_RECORD_SIZE,
    SAMPLE_TERMINAL_PADDING, SF2_VERSION_MAJOR, SF2_VERSION_MINOR, SHDR_RECORD_SIZE,
};

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

/// Compile a raw SoundFont 2 binary stream from samples, instruments, and presets.
pub fn assemble_sf2_binary(
    bank_title: &str,
    sound_engine: &str,
    smpl_pcm_bytes: &[u8],
    samples: &[SampleRecord],
    instruments: &[InstrumentBuilder],
    presets: &[PresetBuilder],
) -> Result<Vec<u8>> {
    use formatkit_codec::sf2::{
        write_sf2, Sf2Font, Sf2Generator, Sf2Info, Sf2Instrument, Sf2Preset, Sf2Sample,
        Sf2SampleType, Sf2Zone,
    };

    let mut info = Sf2Info::new(bank_title);
    info.sound_engine = sound_engine.to_string();
    info.engineer = Some("GBAKit".to_string());
    info.software = Some("GBAKit SoundFont Engine".to_string());

    let mut sf2_samples = Vec::with_capacity(samples.len());
    for s in samples {
        let start_byte = (s.dw_start as usize)
            .checked_mul(2)
            .ok_or(Error::InvalidField {
                what: "sf2_sample_start",
                value: u64::from(s.dw_start),
            })?;
        let end_byte = (s.dw_end as usize)
            .checked_mul(2)
            .ok_or(Error::InvalidField {
                what: "sf2_sample_end",
                value: u64::from(s.dw_end),
            })?;
        if start_byte > end_byte || end_byte > smpl_pcm_bytes.len() {
            return Err(Error::Truncated {
                offset: start_byte,
                needed: end_byte.saturating_sub(start_byte),
                available: smpl_pcm_bytes.len().saturating_sub(start_byte),
            });
        }
        let pcm: Vec<i16> = smpl_pcm_bytes[start_byte..end_byte]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&b| i16::from_le_bytes(b))
            .collect();
        let name = String::from_utf8_lossy(&s.name)
            .trim_end_matches('\0')
            .to_string();
        sf2_samples.push(Sf2Sample {
            name,
            pcm,
            sample_rate: s.dw_sample_rate,
            original_pitch: s.by_original_pitch,
            pitch_correction: s.ch_pitch_correction,
            loop_start: s.dw_startloop.saturating_sub(s.dw_start),
            loop_end: s.dw_endloop.saturating_sub(s.dw_start),
            sample_link: s.w_sample_link,
            sample_type: Sf2SampleType::from_raw(s.sf_sample_type).unwrap_or(Sf2SampleType::Mono),
        });
    }

    let sf2_instruments: Vec<Sf2Instrument> = instruments
        .iter()
        .map(|inst| Sf2Instrument {
            name: inst.name.clone(),
            zones: inst
                .zones
                .iter()
                .map(|z| {
                    let mut generators = Vec::with_capacity(13);
                    generators.push(Sf2Generator::key_range(z.key_range.0, z.key_range.1));
                    generators.push(Sf2Generator::vel_range(z.vel_range.0, z.vel_range.1));
                    generators.push(Sf2Generator::overriding_root_key(z.root_key));
                    generators.push(Sf2Generator::sample_modes(z.sample_modes));
                    if let Some(pan_val) = z.pan {
                        generators.push(Sf2Generator::from_i16(GEN_PAN, pan_val));
                    }
                    generators.push(Sf2Generator::from_u16(
                        GEN_INITIAL_ATTENUATION,
                        z.initial_attenuation,
                    ));
                    generators.push(Sf2Generator::from_i16(GEN_ATTACK_VOL_ENV, z.attack_tc));
                    generators.push(Sf2Generator::from_i16(GEN_HOLD_VOL_ENV, z.hold_tc));
                    generators.push(Sf2Generator::from_i16(GEN_DECAY_VOL_ENV, z.decay_tc));
                    generators.push(Sf2Generator::from_u16(GEN_SUSTAIN_VOL_ENV, z.sustain_cb));
                    generators.push(Sf2Generator::from_i16(GEN_RELEASE_VOL_ENV, z.release_tc));
                    if let Some(st) = z.scale_tuning {
                        generators.push(Sf2Generator::from_u16(GEN_SCALE_TUNING, st));
                    }
                    // sampleID MUST BE LAST per SF2.04
                    generators.push(Sf2Generator::sample_id(z.sample_id));
                    Sf2Zone { generators }
                })
                .collect(),
        })
        .collect();

    let sf2_presets: Vec<Sf2Preset> = presets
        .iter()
        .map(|p| Sf2Preset {
            name: p.name.clone(),
            preset: p.preset,
            bank: p.bank,
            zones: p
                .zones
                .iter()
                .map(|z| Sf2Zone {
                    generators: vec![
                        Sf2Generator::key_range(z.key_range.0, z.key_range.1),
                        Sf2Generator::vel_range(z.vel_range.0, z.vel_range.1),
                        Sf2Generator::instrument(z.instrument_id),
                    ],
                })
                .collect(),
        })
        .collect();

    write_sf2(&Sf2Font {
        info,
        samples: sf2_samples,
        instruments: sf2_instruments,
        presets: sf2_presets,
    })
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
        let dw_start = u32::try_from(current_sample_idx).map_err(|_| Error::ResourceLimit {
            resource: "sf2_sample_offset",
            requested: current_sample_idx as u64,
            limit: u32::MAX as u64,
        })?;
        let end_idx = current_sample_idx
            .checked_add(s.pcm16.len())
            .ok_or(Error::ResourceLimit {
                resource: "sf2_sample_offset",
                requested: u64::MAX,
                limit: u32::MAX as u64,
            })?;
        let dw_end = u32::try_from(end_idx).map_err(|_| Error::ResourceLimit {
            resource: "sf2_sample_offset",
            requested: end_idx as u64,
            limit: u32::MAX as u64,
        })?;
        let (dw_startloop, dw_endloop) = if let Some((start_l, end_l)) = s.loop_range {
            let loop_s = dw_start.saturating_add(start_l).min(dw_end.saturating_sub(1));
            let loop_e = dw_start.saturating_add(end_l).min(dw_end);
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
        current_sample_idx = end_idx
            .checked_add(SAMPLE_TERMINAL_PADDING)
            .ok_or(Error::ResourceLimit {
                resource: "sf2_sample_offset",
                requested: u64::MAX,
                limit: u32::MAX as u64,
            })?;

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
        let inst_id = u16::try_from(instruments.len()).map_err(|_| Error::ResourceLimit {
            resource: "sf2_instrument_count",
            requested: instruments.len() as u64,
            limit: u16::MAX as u64,
        })?;
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

        let bank = u16::try_from(i / 128).map_err(|_| Error::ResourceLimit {
            resource: "sf2_preset_bank",
            requested: (i / 128) as u64,
            limit: u16::MAX as u64,
        })?;
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
        DetectedSoundEngine::EadSma(_) => {
            crate::ead_sma_pool::compile_ead_sma_soundfont(rom, title_override)
        }
        DetectedSoundEngine::EngineSw(_) => {
            crate::engine_sw_pool::compile_engine_sw_soundfont(rom, title_override)
        }
        DetectedSoundEngine::CosmigoSonix(_) => {
            crate::sonix_pool::compile_cosmigo_sonix_soundfont(rom, title_override)
        }
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
    let mut total_bank_samples: usize = 0;

    for &vg_ptr in &scan.voicegroups {
        let ptrs = collect_voicegroup_sample_pointers(rom, vg_ptr);
        for ptr in ptrs {
            if let std::collections::hash_map::Entry::Vacant(e) = sample_index_map.entry(ptr) {
                if let Some(off) = rom_ptr_to_offset(ptr, rom.len()) {
                    let remaining = MAX_SF2_BANK_SAMPLES.saturating_sub(total_bank_samples);
                    match extract_pcm_i8(&rom[off..], remaining.min(10_000_000)) {
                        Ok((hdr, pcm)) => {
                            let next_total = total_bank_samples.saturating_add(pcm.len());
                            if next_total > MAX_SF2_BANK_SAMPLES {
                                return Err(Error::ResourceLimit {
                                    resource: "sf2_bank_samples",
                                    requested: next_total as u64,
                                    limit: MAX_SF2_BANK_SAMPLES as u64,
                                });
                            }
                            total_bank_samples = next_total;
                            let idx = unique_samples.len();
                            e.insert(idx);
                            unique_samples.push((ptr, hdr, pcm));
                        }
                        Err(Error::ResourceLimit { requested, .. }) => {
                            let req_total = (total_bank_samples as u64).saturating_add(requested);
                            if req_total > MAX_SF2_BANK_SAMPLES as u64 {
                                return Err(Error::ResourceLimit {
                                    resource: "sf2_bank_samples",
                                    requested: req_total,
                                    limit: MAX_SF2_BANK_SAMPLES as u64,
                                });
                            }
                        }
                        Err(_) => {}
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
        let dw_start = u32::try_from(current_sample_idx).map_err(|_| Error::ResourceLimit {
            resource: "sf2_sample_offset",
            requested: current_sample_idx as u64,
            limit: u32::MAX as u64,
        })?;
        let end_idx = current_sample_idx
            .checked_add(pcm.len())
            .ok_or(Error::ResourceLimit {
                resource: "sf2_sample_offset",
                requested: u64::MAX,
                limit: u32::MAX as u64,
            })?;
        let dw_end = u32::try_from(end_idx).map_err(|_| Error::ResourceLimit {
            resource: "sf2_sample_offset",
            requested: end_idx as u64,
            limit: u32::MAX as u64,
        })?;
        let (dw_startloop, dw_endloop) = if hdr.is_looped() {
            let loop_s = dw_start
                .saturating_add(hdr.loop_start)
                .min(dw_end.saturating_sub(1));
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
        current_sample_idx = end_idx
            .checked_add(SAMPLE_TERMINAL_PADDING)
            .ok_or(Error::ResourceLimit {
                resource: "sf2_sample_offset",
                requested: u64::MAX,
                limit: u32::MAX as u64,
            })?;

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

                            let new_id = u16::try_from(instruments.len()).map_err(|_| {
                                Error::ResourceLimit {
                                    resource: "sf2_instrument_count",
                                    requested: instruments.len() as u64,
                                    limit: u16::MAX as u64,
                                }
                            })?;
                            let sample_id = u16::try_from(s_id).map_err(|_| Error::ResourceLimit {
                                resource: "sf2_sample_count",
                                requested: s_id as u64,
                                limit: u16::MAX as u64,
                            })?;
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
                                    sample_id,
                                }],
                            });
                            instrument_cache.insert(inst_key, new_id);
                            new_id
                        }
                    };

                    // Assign preset slot (Bank = vg_idx min 127, Preset = p)
                    let bank = vg_idx.min(127) as u16;
                    let pr = p.min(127) as u16;
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

                                        let sample_id = u16::try_from(s_id).map_err(|_| {
                                            Error::ResourceLimit {
                                                resource: "sf2_sample_count",
                                                requested: s_id as u64,
                                                limit: u16::MAX as u64,
                                            }
                                        })?;
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
                                            sample_id,
                                        });
                                    }
                                }
                            }

                            if !inst_zones.is_empty() {
                                let new_id = u16::try_from(instruments.len()).map_err(|_| {
                                    Error::ResourceLimit {
                                        resource: "sf2_instrument_count",
                                        requested: instruments.len() as u64,
                                        limit: u16::MAX as u64,
                                    }
                                })?;
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
                    let bank = vg_idx.min(127) as u16;
                    let pr = p.min(127) as u16;
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

                                let sample_id = u16::try_from(s_id).map_err(|_| {
                                    Error::ResourceLimit {
                                        resource: "sf2_sample_count",
                                        requested: s_id as u64,
                                        limit: u16::MAX as u64,
                                    }
                                })?;
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
                                    sample_id,
                                });
                            }
                        }

                        if !drum_zones.is_empty() {
                            let new_id = u16::try_from(instruments.len()).map_err(|_| {
                                Error::ResourceLimit {
                                    resource: "sf2_instrument_count",
                                    requested: instruments.len() as u64,
                                    limit: u16::MAX as u64,
                                }
                            })?;
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
                    let mut pr = vg_idx.min(127) as u16;
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
    let s = formatkit_codec::sf2::validate_sf2(sf2_bytes)?;
    Ok(SoundFontValidation {
        major_version: s.major_version,
        minor_version: s.minor_version,
        sound_engine: s.sound_engine,
        bank_name: s.bank_name,
        sample_count: s.sample_count,
        smpl_byte_len: s.smpl_byte_len,
        preset_count: s.preset_count,
        instrument_count: s.instrument_count,
        pbag_count: s.pbag_count,
        ibag_count: s.ibag_count,
        pgen_count: s.pgen_count,
        igen_count: s.igen_count,
    })
}
