//! Natsume Hybrid PSG + DirectSound PCM GBA audio engine detection,
//! sample/sequence pool scanning, WAV16 export, and SoundFont 2.04 compilation.
//!
//! Thin adapter delegating detection and zero-copy ROM parsing to [`natsume_audio`].

#![deny(unsafe_code)]

use std::collections::HashSet;

use formatkit_core::{Error, Result};
use natsume_audio::NatsumeVersion;
use serde::{Deserialize, Serialize};

use crate::encode_wav_i16_mono;
use crate::sf2::{
    compile_custom_samples_soundfont, extract_rom_title, RawSoundFontSample, SoundFontExport,
};

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

impl From<NatsumeVersion> for NatsumeRevision {
    fn from(v: NatsumeVersion) -> Self {
        match v {
            NatsumeVersion::Rev0 => Self::Rev0,
            NatsumeVersion::Rev1 => Self::Rev1,
            NatsumeVersion::Rev2 | _ => Self::Rev2,
        }
    }
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

/// Detect whether `rom` contains the Natsume Hybrid PSG+PCM DirectSound engine.
#[must_use]
pub fn detect_natsume_engine(rom: &[u8]) -> bool {
    natsume_audio::detect(rom)
}

/// Detect the Natsume engine revision if present (falls back to `None` if not detected).
#[must_use]
pub fn detect_natsume_revision(rom: &[u8]) -> Option<NatsumeRevision> {
    NatsumeVersion::detect_from_bytes(rom).map(NatsumeRevision::from)
}

/// Single-pass Natsume engine detection returning `Some(Some(rev))` when both anchors and
/// revision are resolved, `Some(None)` when anchors match without init calls, or `None`.
#[must_use]
pub fn detect_natsume_with_revision(rom: &[u8]) -> Option<Option<NatsumeRevision>> {
    match natsume_audio::parse(rom) {
        Ok(audio) => Some(Some(audio.revision.into())),
        Err(natsume_audio::Error::NotDetected) => None,
        Err(_) => Some(None),
    }
}

/// Scan a Natsume Hybrid PSG+PCM ROM for all PCM samples and BGM/SFX sequences.
pub fn scan_natsume_engine(rom: &[u8]) -> Option<NatsumeScanSummary> {
    let audio = natsume_audio::parse(rom).ok()?;

    let mut samples = Vec::new();
    let mut seen_offsets = HashSet::new();

    for (sample, kind) in audio
        .melodic_samples()
        .map(|s| (s, NatsumeSampleKind::Melodic))
        .chain(audio.drum_samples().map(|s| (s, NatsumeSampleKind::Drum)))
    {
        if !seen_offsets.insert(sample.rom_offset) {
            continue;
        }
        let loop_range = sample.is_looped().then_some((sample.loop_start, sample.length));
        let pcm_data: Vec<i8> = sample.pcm_data.iter().map(|&b| b as i8).collect();
        samples.push(NatsumeSampleEntry {
            index: samples.len(),
            kind,
            rom_offset: sample.rom_offset,
            sample_start_offset: sample.rom_offset.saturating_add(16),
            size: sample.pcm_data.len(),
            sample_rate: u32::from(sample.base_rate),
            fine_tune: sample.fine_tune,
            root_key: sample.root_note,
            loop_range,
            pcm_data,
        });
    }

    let mut songs = Vec::with_capacity(audio.bgm_table.count + audio.sfx_table.count);
    for (index, song) in audio.bgm_songs().enumerate() {
        songs.push(NatsumeSongEntry {
            index,
            is_sfx: false,
            rom_offset: song.rom_offset,
            channel_mask: song.channel_mask,
            tempo: song.tempo,
            track_count: song.track_count,
        });
    }
    for (index, song) in audio.sfx_songs().enumerate() {
        songs.push(NatsumeSongEntry {
            index,
            is_sfx: true,
            rom_offset: song.rom_offset,
            channel_mask: song.channel_mask,
            tempo: song.tempo,
            track_count: song.track_count,
        });
    }

    Some(NatsumeScanSummary {
        revision: audio.revision.into(),
        melodic_count: audio.melodic_table.count,
        drum_count: audio.drum_table.count,
        bgm_count: audio.bgm_table.count,
        sfx_count: audio.sfx_table.count,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sf2::validate_soundfont;
    use natsume_audio::detect::{DMA_FIFO_CTRL, MMIO6_TABLE, PITCH_AND_FINE_TABLE_PREFIX, ROM_BASE};

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
        assert_eq!(
            detect_natsume_with_revision(&rom_rev2),
            Some(Some(NatsumeRevision::Rev2))
        );

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
        assert_eq!(detect_natsume_with_revision(&[]), None);
        assert!(scan_natsume_soundpool(&[0u8; 512]).is_empty());
        assert!(export_natsume_samples_wav16(&[0u8; 512]).is_err());
    }
}
