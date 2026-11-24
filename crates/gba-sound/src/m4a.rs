//! Standard GBA `m4a` (MusicPlayer2000 / "Sappy") sound engine parsing,
//! track bytecode decompilation, MIDI File Format 1 export, and DirectSound sample extraction.

use std::collections::HashSet;
use formatkit_core::{Error, Reader, Result};
use serde::{Deserialize, Serialize};

pub use ::m4a::{
    decode_dpcm_block, decode_dpcm_into, decode_dpcm_vec, detect_mixer_variant, dpcm_encoded_len,
    midi_key_to_freq, M4aMixerVariant, M4aSample, M4aSampleView, CLOCK_TABLE, DPCM_BLOCK_BYTES,
    DPCM_BLOCK_SAMPLES, DPCM_DELTA_TABLE, M4A_RATE_TABLE, VOICE_ENTRY_SIZE,
    WAVE_DATA_HEADER_SIZE,
};

use crate::{encode_wav_i16_mono, extract_pcm_i8, WaveDataHeader};

/// Size of a single GBA `m4a` song table entry in bytes.
pub const SONG_TABLE_ENTRY_SIZE: usize = 8;

/// Maximum number of tracks permitted in a GBA `m4a` song header.
pub const MAX_TRACKS: u8 = 16;

/// Start of the GBA Wait State 0 ROM bus window.
pub const ROM_BUS_START: u32 = 0x0800_0000;

/// Exclusive end of the GBA Wait State 0 ROM bus window (32 MiB).
pub const ROM_BUS_END: u32 = 0x0A00_0000;

/// Standard MIDI PPQN (pulses / ticks per quarter note) matching M4A timing (24 ticks = quarter note).
pub const PPQN: u16 = 24;

/// Human-readable label for a detected [`M4aMixerVariant`].
pub const fn m4a_mixer_variant_name(variant: M4aMixerVariant) -> &'static str {
    match variant {
        M4aMixerVariant::Standard => "Standard (SDK v2 Stereo)",
        M4aMixerVariant::PokemonGen3 => "Pokemon Gen 3 (SDK v2 Stereo + 4-bit DPCM)",
        M4aMixerVariant::Mono => "Mono (SDK v2 Mono)",
        M4aMixerVariant::EarlyV1Stereo => "Early SDK v1 Stereo",
        M4aMixerVariant::EarlyV1Mono => "Early SDK v1 Mono",
        M4aMixerVariant::MetroidRd1 => "Nintendo R&D1 32-bit Stereo (Metroid)",
    }
}

/// Returns true if `ptr` falls within the GBA ROM bus window (`0x0800_0000..0x0A00_0000`).
pub fn is_rom_bus_ptr(ptr: u32) -> bool {
    (ROM_BUS_START..ROM_BUS_END).contains(&ptr)
}

/// Convert a 32-bit GBA ROM bus address (`0x08xxxxxx`) into a file offset within `rom_len`.
pub fn rom_ptr_to_offset(ptr: u32, rom_len: usize) -> Option<usize> {
    if !is_rom_bus_ptr(ptr) {
        return None;
    }
    let off = (ptr - ROM_BUS_START) as usize;
    if off < rom_len {
        Some(off)
    } else {
        None
    }
}

/// Convert a file offset into a 32-bit GBA ROM bus address.
pub fn offset_to_rom_ptr(offset: usize) -> u32 {
    u32::try_from(offset).map_or(u32::MAX, |off| ROM_BUS_START.saturating_add(off))
}

/// Instrument / voice type in a GBA `m4a` voicegroup entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VoiceKind {
    DirectSound,
    DirectSoundFixed,
    DirectSoundAlt,
    Square1,
    Square2,
    ProgrammableWave,
    Noise,
    Keysplit,
    Drumkit,
    Unknown(u8),
}

impl VoiceKind {
    /// Decode a raw GBA `m4a` voice type byte.
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            0x00 => Self::DirectSound,
            0x08 => Self::DirectSoundFixed,
            0x10 => Self::DirectSoundAlt,
            0x01 | 0x09 => Self::Square1,
            0x02 | 0x0A => Self::Square2,
            0x03 | 0x0B => Self::ProgrammableWave,
            0x04 | 0x0C => Self::Noise,
            0x40 => Self::Keysplit,
            0x80 => Self::Drumkit,
            other => Self::Unknown(other),
        }
    }

    /// Convert back to canonical raw type byte.
    pub const fn to_raw(&self) -> u8 {
        match self {
            Self::DirectSound => 0x00,
            Self::DirectSoundFixed => 0x08,
            Self::DirectSoundAlt => 0x10,
            Self::Square1 => 0x01,
            Self::Square2 => 0x02,
            Self::ProgrammableWave => 0x03,
            Self::Noise => 0x04,
            Self::Keysplit => 0x40,
            Self::Drumkit => 0x80,
            Self::Unknown(raw) => *raw,
        }
    }

    /// True if this instrument uses 8-bit DirectSound PCM sample data.
    pub const fn is_directsound(&self) -> bool {
        matches!(self, Self::DirectSound | Self::DirectSoundFixed | Self::DirectSoundAlt)
    }
}

impl From<u8> for VoiceKind {
    fn from(raw: u8) -> Self {
        Self::from_raw(raw)
    }
}

/// A 12-byte GBA `m4a` instrument / voicegroup entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceEntry {
    pub kind: VoiceKind,
    pub base_key: u8,
    pub length: u8,
    pub pan_or_sweep: u8,
    pub data_ptr: u32,
    pub attack: u8,
    pub decay: u8,
    pub sustain: u8,
    pub release: u8,
}

/// Parse a single 12-byte GBA `m4a` voice entry at `offset` via [`::m4a::M4aVoice`].
pub fn parse_voice_entry(bytes: &[u8], offset: usize) -> Result<VoiceEntry> {
    let end = offset
        .checked_add(VOICE_ENTRY_SIZE)
        .ok_or(Error::InvalidField {
            what: "m4a_voice_entry_offset",
            value: offset as u64,
        })?;
    if end > bytes.len() {
        return Err(Error::Truncated {
            offset,
            needed: VOICE_ENTRY_SIZE,
            available: bytes.len().saturating_sub(offset),
        });
    }
    let v = ::m4a::M4aVoice::parse(&bytes[offset..end]).map_err(|_| Error::Truncated {
        offset,
        needed: VOICE_ENTRY_SIZE,
        available: bytes.len().saturating_sub(offset),
    })?;
    Ok(VoiceEntry {
        kind: VoiceKind::from_raw(v.kind.to_raw()),
        base_key: v.base_key,
        length: v.length,
        pan_or_sweep: v.pan_or_sweep,
        data_ptr: v.data_ptr,
        attack: v.attack,
        decay: v.decay,
        sustain: v.sustain,
        release: v.release,
    })
}

/// Parse `count` consecutive 12-byte GBA `m4a` voice entries starting at `offset`.
pub fn parse_voicegroup(bytes: &[u8], offset: usize, count: usize) -> Result<Vec<VoiceEntry>> {
    let total = count
        .checked_mul(VOICE_ENTRY_SIZE)
        .ok_or(Error::InvalidField {
            what: "m4a_voicegroup_count",
            value: count as u64,
        })?;
    let end = offset.checked_add(total).ok_or(Error::InvalidField {
        what: "m4a_voicegroup_offset",
        value: offset as u64,
    })?;
    if end > bytes.len() {
        return Err(Error::Truncated {
            offset,
            needed: total,
            available: bytes.len().saturating_sub(offset),
        });
    }
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        entries.push(parse_voice_entry(bytes, offset + i * VOICE_ENTRY_SIZE)?);
    }
    Ok(entries)
}

/// Parsed GBA `m4a` song header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SongHeader {
    pub track_count: u8,
    pub block_count: u8,
    pub priority: u8,
    pub reverb: u8,
    pub voicegroup_ptr: u32,
    pub track_ptrs: Vec<u32>,
}

/// Parse a GBA `m4a` song header at `offset` via [`::m4a::SongHeaderView`].
pub fn parse_song_header(bytes: &[u8], offset: usize) -> Result<SongHeader> {
    let slice = bytes.get(offset..).ok_or(Error::Truncated {
        offset,
        needed: 8,
        available: 0,
    })?;
    let view = ::m4a::SongHeaderView::parse(slice).map_err(|e| match e {
        ::m4a::Error::UnexpectedEof => Error::Truncated {
            offset,
            needed: 8,
            available: slice.len(),
        },
        _ => Error::InvalidField {
            what: "m4a_track_count",
            value: slice.first().copied().unwrap_or(0) as u64,
        },
    })?;

    if view.track_count > 0 && !is_rom_bus_ptr(view.voicegroup_ptr) {
        return Err(Error::InvalidField {
            what: "m4a_voicegroup_ptr",
            value: view.voicegroup_ptr as u64,
        });
    }

    let mut track_ptrs = Vec::with_capacity(view.track_count as usize);
    for idx in 0..view.track_count as usize {
        let ptr = view.track_ptr(idx).ok_or(Error::Truncated {
            offset: offset + 8 + idx * 4,
            needed: 4,
            available: slice.len().saturating_sub(8 + idx * 4),
        })?;
        if !is_rom_bus_ptr(ptr) {
            return Err(Error::InvalidField {
                what: "m4a_track_ptr",
                value: ptr as u64,
            });
        }
        track_ptrs.push(ptr);
    }

    Ok(SongHeader {
        track_count: view.track_count,
        block_count: view.block_count,
        priority: view.priority,
        reverb: view.reverb,
        voicegroup_ptr: view.voicegroup_ptr,
        track_ptrs,
    })
}

/// An 8-byte entry in a GBA `m4a` song table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SongTableEntry {
    pub header_ptr: u32,
    pub player_group: u16,
    pub player_group_copy: u16,
}

/// Parse `count` consecutive 8-byte GBA `m4a` song table entries starting at `offset`.
pub fn parse_song_table(bytes: &[u8], offset: usize, count: usize) -> Result<Vec<SongTableEntry>> {
    let total = count
        .checked_mul(SONG_TABLE_ENTRY_SIZE)
        .ok_or(Error::InvalidField {
            what: "m4a_song_table_count",
            value: count as u64,
        })?;
    let end = offset.checked_add(total).ok_or(Error::InvalidField {
        what: "m4a_song_table_offset",
        value: offset as u64,
    })?;
    if end > bytes.len() {
        return Err(Error::Truncated {
            offset,
            needed: total,
            available: bytes.len().saturating_sub(offset),
        });
    }

    let mut r = Reader::new(&bytes[offset..end]);
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let header_ptr = r.u32_le()?;
        let player_group = r.u16_le()?;
        let player_group_copy = r.u16_le()?;
        entries.push(SongTableEntry {
            header_ptr,
            player_group,
            player_group_copy,
        });
    }
    Ok(entries)
}

/// Decoded GBA `m4a` track bytecode event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum M4aEvent {
    /// `0x80..=0xB0`: Wait for the decoded number of ticks.
    Wait(u8),
    /// `0xB1`: End of track.
    Fine,
    /// `0xB2`: Unconditional jump to target ROM bus address.
    Goto(u32),
    /// `0xB3`: Call pattern subroutine at target ROM bus address.
    Patt(u32),
    /// `0xB4`: End of pattern subroutine.
    Pend,
    /// `0xB5`: Repeat loop (`count` iterations jumping to `target`).
    Rept { count: u8, target: u32 },
    /// `0xB9`: Memory access command (`op`, `address`, `data`).
    MemAcc { op: u8, address: u8, data: u8 },
    /// `0xBA`: Track priority.
    Prio(u8),
    /// `0xBB`: Song tempo (BPM = value * 2).
    Tempo(u8),
    /// `0xBC`: Signed key shift (transpose in semitones).
    Keysh(i8),
    /// `0xBD`: Instrument / voice selection.
    Voice(u8),
    /// `0xBE`: Track volume (0..=127).
    Vol(u8),
    /// `0xBF`: Track pan (0..=127, center 64).
    Pan(u8),
    /// `0xC0`: Pitch bend (0..=127, center 64).
    Bend(u8),
    /// `0xC1`: Pitch bend range (semitones).
    Bendr(u8),
    /// `0xC2`: LFO speed.
    Lfos(u8),
    /// `0xC3`: LFO delay.
    Lfodl(u8),
    /// `0xC4`: Modulation depth.
    Mod(u8),
    /// `0xC5`: Modulation type.
    Modt(u8),
    /// `0xC8`: Fine tune (0..=127, center 64).
    Tune(u8),
    /// `0xCD`: Extended sound command.
    Xcmd { sub_cmd: u8, args: Vec<u8> },
    /// `0xCE`: End of tied note (optional key byte).
    Eot(Option<u8>),
    /// `0xCF`: Tied note on (`key`, `velocity`).
    Tie { key: u8, velocity: u8 },
    /// `0xD0..=0xFF`: Note event with decoded tick `length`, `key`, and `velocity`.
    Note { length: u8, key: u8, velocity: u8 },
}

fn read_u8(bytes: &[u8], pos: &mut usize) -> Result<u8> {
    if *pos >= bytes.len() {
        return Err(Error::Truncated {
            offset: *pos,
            needed: 1,
            available: 0,
        });
    }
    let b = bytes[*pos];
    *pos += 1;
    Ok(b)
}

fn read_u32_le(bytes: &[u8], pos: &mut usize) -> Result<u32> {
    let end = pos.checked_add(4).ok_or(Error::InvalidField {
        what: "m4a_track_offset",
        value: *pos as u64,
    })?;
    if end > bytes.len() {
        return Err(Error::Truncated {
            offset: *pos,
            needed: 4,
            available: bytes.len().saturating_sub(*pos),
        });
    }
    let val = u32::from_le_bytes([
        bytes[*pos],
        bytes[*pos + 1],
        bytes[*pos + 2],
        bytes[*pos + 3],
    ]);
    *pos = end;
    Ok(val)
}

fn decode_single_param(cmd: u8, param: u8) -> Option<M4aEvent> {
    match cmd {
        0xBA => Some(M4aEvent::Prio(param)),
        0xBB => Some(M4aEvent::Tempo(param)),
        0xBC => Some(M4aEvent::Keysh(param as i8)),
        0xBD => Some(M4aEvent::Voice(param)),
        0xBE => Some(M4aEvent::Vol(param)),
        0xBF => Some(M4aEvent::Pan(param)),
        0xC0 => Some(M4aEvent::Bend(param)),
        0xC1 => Some(M4aEvent::Bendr(param)),
        0xC2 => Some(M4aEvent::Lfos(param)),
        0xC3 => Some(M4aEvent::Lfodl(param)),
        0xC4 => Some(M4aEvent::Mod(param)),
        0xC5 => Some(M4aEvent::Modt(param)),
        0xC8 => Some(M4aEvent::Tune(param)),
        _ => None,
    }
}

/// Parse up to `max_events` GBA `m4a` track bytecode events starting at `offset`.
pub fn parse_track_events(
    bytes: &[u8],
    offset: usize,
    max_events: usize,
) -> Result<(Vec<M4aEvent>, usize)> {
    if offset > bytes.len() {
        return Err(Error::Truncated {
            offset,
            needed: 1,
            available: 0,
        });
    }

    let mut pos = offset;
    let mut events = Vec::new();
    let mut running_cmd: Option<u8> = None;
    let mut last_key: u8 = 0;
    let mut last_velocity: u8 = 0;

    while pos < bytes.len() && events.len() < max_events {
        let byte = read_u8(bytes, &mut pos)?;

        if byte < 0x80 {
            let cmd = running_cmd.ok_or(Error::InvalidField {
                what: "m4a_running_status",
                value: byte as u64,
            })?;
            let event = match cmd {
                0xBA..=0xC5 | 0xC8 => {
                    decode_single_param(cmd, byte).ok_or(Error::InvalidField {
                        what: "m4a_running_status",
                        value: cmd as u64,
                    })?
                }
                0xCD => {
                    let sub_cmd = byte;
                    let count = if sub_cmd == 0x08 { 3 } else { 1 };
                    let mut args = Vec::with_capacity(count);
                    for _ in 0..count {
                        if pos < bytes.len() && bytes[pos] < 0x80 {
                            args.push(read_u8(bytes, &mut pos)?);
                        }
                    }
                    M4aEvent::Xcmd { sub_cmd, args }
                }
                0xCE => {
                    last_key = byte;
                    M4aEvent::Eot(Some(byte))
                }
                0xCF => {
                    last_key = byte;
                    if pos < bytes.len() && bytes[pos] < 0x80 {
                        last_velocity = read_u8(bytes, &mut pos)?;
                    }
                    M4aEvent::Tie {
                        key: last_key,
                        velocity: last_velocity,
                    }
                }
                0xD0..=0xFF => {
                    let mut length = CLOCK_TABLE[(cmd - 0xCF) as usize];
                    last_key = byte;
                    if pos < bytes.len() && bytes[pos] < 0x80 {
                        last_velocity = read_u8(bytes, &mut pos)?;
                        if pos < bytes.len() && bytes[pos] <= 3 {
                            let extra = read_u8(bytes, &mut pos)?;
                            length = length.saturating_add(extra);
                        }
                    }
                    M4aEvent::Note {
                        length,
                        key: last_key,
                        velocity: last_velocity,
                    }
                }
                other => {
                    return Err(Error::InvalidField {
                        what: "m4a_running_status",
                        value: other as u64,
                    });
                }
            };
            events.push(event);
            continue;
        }

        let event = match byte {
            0x80..=0xB0 => M4aEvent::Wait(CLOCK_TABLE[(byte - 0x80) as usize]),
            0xB1 => {
                events.push(M4aEvent::Fine);
                break;
            }
            0xB2 => M4aEvent::Goto(read_u32_le(bytes, &mut pos)?),
            0xB3 => M4aEvent::Patt(read_u32_le(bytes, &mut pos)?),
            0xB4 => M4aEvent::Pend,
            0xB5 => {
                let count = read_u8(bytes, &mut pos)?;
                let target = read_u32_le(bytes, &mut pos)?;
                M4aEvent::Rept { count, target }
            }
            0xB9 => {
                let op = read_u8(bytes, &mut pos)?;
                let address = read_u8(bytes, &mut pos)?;
                let data = read_u8(bytes, &mut pos)?;
                M4aEvent::MemAcc { op, address, data }
            }
            0xBA..=0xC5 | 0xC8 => {
                running_cmd = Some(byte);
                let param = read_u8(bytes, &mut pos)?;
                decode_single_param(byte, param).ok_or(Error::InvalidField {
                    what: "m4a_opcode",
                    value: byte as u64,
                })?
            }
            0xCD => {
                running_cmd = Some(byte);
                let sub_cmd = read_u8(bytes, &mut pos)?;
                let count = if sub_cmd == 0x08 { 3 } else { 1 };
                let mut args = Vec::with_capacity(count);
                for _ in 0..count {
                    if pos < bytes.len() && bytes[pos] < 0x80 {
                        args.push(read_u8(bytes, &mut pos)?);
                    }
                }
                M4aEvent::Xcmd { sub_cmd, args }
            }
            0xCE => {
                running_cmd = Some(byte);
                let key = if pos < bytes.len() && bytes[pos] < 0x80 {
                    let k = read_u8(bytes, &mut pos)?;
                    last_key = k;
                    Some(k)
                } else {
                    None
                };
                M4aEvent::Eot(key)
            }
            0xCF => {
                running_cmd = Some(byte);
                if pos < bytes.len() && bytes[pos] < 0x80 {
                    last_key = read_u8(bytes, &mut pos)?;
                    if pos < bytes.len() && bytes[pos] < 0x80 {
                        last_velocity = read_u8(bytes, &mut pos)?;
                    }
                }
                M4aEvent::Tie {
                    key: last_key,
                    velocity: last_velocity,
                }
            }
            0xD0..=0xFF => {
                running_cmd = Some(byte);
                let mut length = CLOCK_TABLE[(byte - 0xCF) as usize];
                if pos < bytes.len() && bytes[pos] < 0x80 {
                    last_key = read_u8(bytes, &mut pos)?;
                    if pos < bytes.len() && bytes[pos] < 0x80 {
                        last_velocity = read_u8(bytes, &mut pos)?;
                        if pos < bytes.len() && bytes[pos] <= 3 {
                            let extra = read_u8(bytes, &mut pos)?;
                            length = length.saturating_add(extra);
                        }
                    }
                }
                M4aEvent::Note {
                    length,
                    key: last_key,
                    velocity: last_velocity,
                }
            }
            other => {
                return Err(Error::InvalidField {
                    what: "m4a_opcode",
                    value: other as u64,
                });
            }
        };

        events.push(event);
    }

    Ok((events, pos - offset))
}

/// A timed MIDI event ready for delta-time encoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MidiEventKind {
    NoteOn { channel: u8, key: u8, velocity: u8 },
    NoteOff { channel: u8, key: u8 },
    ProgramChange { channel: u8, program: u8 },
    ControlChange { channel: u8, controller: u8, value: u8 },
    PitchBend { channel: u8, value: u16 },
    SetTempo { bpm: u32 },
}

/// A timed MIDI event with absolute playback tick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimedMidiEvent {
    pub tick: u64,
    pub event: MidiEventKind,
}

/// Decompile a single GBA `m4a` track channel into timed MIDI events.
///
/// Follows pattern subroutines (`0xB3` Patt / `0xB4` Pend), expands repeat loops (`0xB5` Rept),
/// handles running status and keyshift transpositions, and terminates cleanly on `0xB1` Fine
/// or backwards loop `0xB2` Goto.
pub fn decompile_track_to_midi_events(
    rom: &[u8],
    track_ptr: u32,
    channel: u8,
    max_ticks: u64,
) -> Result<Vec<TimedMidiEvent>> {
    let start_offset = rom_ptr_to_offset(track_ptr, rom.len()).ok_or(Error::InvalidField {
        what: "m4a_track_ptr",
        value: track_ptr as u64,
    })?;

    let mut pos = start_offset;
    let mut events = Vec::new();
    let mut tick: u64 = 0;
    let mut running_cmd: Option<u8> = None;
    let mut last_key: u8 = 60;
    let mut last_vel: u8 = 100;
    let mut keysh: i8 = 0;
    let mut call_stack: Vec<usize> = Vec::new();
    let mut visited_gotos: HashSet<u32> = HashSet::new();
    let mut active_ties: HashSet<u8> = HashSet::new();
    let mut steps = 0;
    const MAX_STEPS: usize = 50_000;

    while pos < rom.len() && tick < max_ticks && steps < MAX_STEPS {
        steps += 1;
        let byte = rom[pos];
        pos += 1;

        if byte < 0x80 {
            // Running status operand
            let cmd = match running_cmd {
                Some(c) => c,
                None => break,
            };

            match cmd {
                0xD0..=0xFF => {
                    let mut length = CLOCK_TABLE[(cmd - 0xCF) as usize];
                    last_key = byte;
                    if pos < rom.len() && rom[pos] < 0x80 {
                        last_vel = rom[pos];
                        pos += 1;
                        if pos < rom.len() && rom[pos] <= 3 {
                            length = length.saturating_add(rom[pos]);
                            pos += 1;
                        }
                    }
                    let k = (last_key as i16 + keysh as i16).clamp(0, 127) as u8;
                    events.push(TimedMidiEvent {
                        tick,
                        event: MidiEventKind::NoteOn { channel, key: k, velocity: last_vel },
                    });
                    events.push(TimedMidiEvent {
                        tick: tick.saturating_add(length as u64),
                        event: MidiEventKind::NoteOff { channel, key: k },
                    });
                }
                0xBD => {
                    events.push(TimedMidiEvent {
                        tick,
                        event: MidiEventKind::ProgramChange { channel, program: byte & 0x7F },
                    });
                }
                0xBE => {
                    events.push(TimedMidiEvent {
                        tick,
                        event: MidiEventKind::ControlChange { channel, controller: 7, value: byte & 0x7F },
                    });
                }
                0xBF => {
                    events.push(TimedMidiEvent {
                        tick,
                        event: MidiEventKind::ControlChange { channel, controller: 10, value: byte & 0x7F },
                    });
                }
                0xC0 => {
                    let val14 = ((byte & 0x7F) as u16) << 7;
                    events.push(TimedMidiEvent {
                        tick,
                        event: MidiEventKind::PitchBend { channel, value: val14 },
                    });
                }
                0xC4 => {
                    events.push(TimedMidiEvent {
                        tick,
                        event: MidiEventKind::ControlChange { channel, controller: 1, value: byte & 0x7F },
                    });
                }
                _ => {}
            }
            continue;
        }

        // Opcode >= 0x80
        match byte {
            0x80..=0xB0 => {
                tick = tick.saturating_add(CLOCK_TABLE[(byte - 0x80) as usize] as u64);
            }
            0xB1 => {
                // Fine - End of track
                break;
            }
            0xB2 => {
                // Goto
                if pos + 4 > rom.len() {
                    break;
                }
                let target = u32::from_le_bytes([rom[pos], rom[pos + 1], rom[pos + 2], rom[pos + 3]]);
                if visited_gotos.contains(&target) || target <= (start_offset as u32 + ROM_BUS_START) {
                    // Loop point reached: stop linear track export
                    break;
                }
                visited_gotos.insert(target);
                if let Some(target_off) = rom_ptr_to_offset(target, rom.len()) {
                    pos = target_off;
                } else {
                    break;
                }
            }
            0xB3 => {
                // Patt - Subroutine call
                if pos + 4 > rom.len() {
                    break;
                }
                let target = u32::from_le_bytes([rom[pos], rom[pos + 1], rom[pos + 2], rom[pos + 3]]);
                pos += 4;
                if call_stack.len() < 8 {
                    if let Some(target_off) = rom_ptr_to_offset(target, rom.len()) {
                        call_stack.push(pos);
                        pos = target_off;
                    }
                }
            }
            0xB4 => {
                // Pend - Return from subroutine
                if let Some(ret_pos) = call_stack.pop() {
                    pos = ret_pos;
                } else {
                    break;
                }
            }
            0xB5 => {
                // Rept - Repeat loop
                if pos + 5 > rom.len() {
                    break;
                }
                let _count = rom[pos];
                pos += 1;
                let target = u32::from_le_bytes([rom[pos], rom[pos + 1], rom[pos + 2], rom[pos + 3]]);
                pos += 4;
                if call_stack.len() < 8 {
                    if let Some(target_off) = rom_ptr_to_offset(target, rom.len()) {
                        call_stack.push(pos);
                        pos = target_off;
                    }
                }
            }
            0xB9 => {
                // MemAcc
                pos = pos.saturating_add(3);
            }
            0xBA => {
                // Prio
                if pos < rom.len() {
                    pos += 1;
                }
            }
            0xBB => {
                // Tempo
                if pos < rom.len() {
                    let tempo = rom[pos];
                    pos += 1;
                    let bpm = (tempo as u32).saturating_mul(2).max(1);
                    events.push(TimedMidiEvent {
                        tick,
                        event: MidiEventKind::SetTempo { bpm },
                    });
                }
            }
            0xBC => {
                // Keysh
                if pos < rom.len() {
                    keysh = rom[pos] as i8;
                    pos += 1;
                }
            }
            0xBD => {
                // Voice
                running_cmd = Some(byte);
                if pos < rom.len() {
                    let v = rom[pos] & 0x7F;
                    pos += 1;
                    events.push(TimedMidiEvent {
                        tick,
                        event: MidiEventKind::ProgramChange { channel, program: v },
                    });
                }
            }
            0xBE => {
                // Vol
                running_cmd = Some(byte);
                if pos < rom.len() {
                    let vol = rom[pos] & 0x7F;
                    pos += 1;
                    events.push(TimedMidiEvent {
                        tick,
                        event: MidiEventKind::ControlChange { channel, controller: 7, value: vol },
                    });
                }
            }
            0xBF => {
                // Pan
                running_cmd = Some(byte);
                if pos < rom.len() {
                    let pan = rom[pos] & 0x7F;
                    pos += 1;
                    events.push(TimedMidiEvent {
                        tick,
                        event: MidiEventKind::ControlChange { channel, controller: 10, value: pan },
                    });
                }
            }
            0xC0 => {
                // Bend
                running_cmd = Some(byte);
                if pos < rom.len() {
                    let bend = rom[pos] & 0x7F;
                    pos += 1;
                    let val14 = (bend as u16) << 7;
                    events.push(TimedMidiEvent {
                        tick,
                        event: MidiEventKind::PitchBend { channel, value: val14 },
                    });
                }
            }
            0xC1..=0xC3 | 0xC5 | 0xC8 => {
                running_cmd = Some(byte);
                if pos < rom.len() {
                    pos += 1;
                }
            }
            0xC4 => {
                // Mod
                running_cmd = Some(byte);
                if pos < rom.len() {
                    let m = rom[pos] & 0x7F;
                    pos += 1;
                    events.push(TimedMidiEvent {
                        tick,
                        event: MidiEventKind::ControlChange { channel, controller: 1, value: m },
                    });
                }
            }
            0xCD => {
                running_cmd = Some(byte);
                if pos < rom.len() {
                    let sub = rom[pos];
                    pos += 1;
                    let arg_count = if sub == 0x08 { 3 } else { 1 };
                    for _ in 0..arg_count {
                        if pos < rom.len() && rom[pos] < 0x80 {
                            pos += 1;
                        }
                    }
                }
            }
            0xCE => {
                // Eot
                running_cmd = Some(byte);
                let opt_key = if pos < rom.len() && rom[pos] < 0x80 {
                    let k = rom[pos];
                    pos += 1;
                    Some(k)
                } else {
                    None
                };
                if let Some(k) = opt_key {
                    let nk = (k as i16 + keysh as i16).clamp(0, 127) as u8;
                    active_ties.remove(&nk);
                    events.push(TimedMidiEvent {
                        tick,
                        event: MidiEventKind::NoteOff { channel, key: nk },
                    });
                } else {
                    for &nk in &active_ties {
                        events.push(TimedMidiEvent {
                            tick,
                            event: MidiEventKind::NoteOff { channel, key: nk },
                        });
                    }
                    active_ties.clear();
                }
            }
            0xCF => {
                // Tie
                running_cmd = Some(byte);
                if pos < rom.len() && rom[pos] < 0x80 {
                    last_key = rom[pos];
                    pos += 1;
                    if pos < rom.len() && rom[pos] < 0x80 {
                        last_vel = rom[pos];
                        pos += 1;
                    }
                }
                let nk = (last_key as i16 + keysh as i16).clamp(0, 127) as u8;
                active_ties.insert(nk);
                events.push(TimedMidiEvent {
                    tick,
                    event: MidiEventKind::NoteOn { channel, key: nk, velocity: last_vel },
                });
            }
            0xD0..=0xFF => {
                // Note
                running_cmd = Some(byte);
                let mut length = CLOCK_TABLE[(byte - 0xCF) as usize];
                if pos < rom.len() && rom[pos] < 0x80 {
                    last_key = rom[pos];
                    pos += 1;
                    if pos < rom.len() && rom[pos] < 0x80 {
                        last_vel = rom[pos];
                        pos += 1;
                        if pos < rom.len() && rom[pos] <= 3 {
                            length = length.saturating_add(rom[pos]);
                            pos += 1;
                        }
                    }
                }
                let nk = (last_key as i16 + keysh as i16).clamp(0, 127) as u8;
                events.push(TimedMidiEvent {
                    tick,
                    event: MidiEventKind::NoteOn { channel, key: nk, velocity: last_vel },
                });
                events.push(TimedMidiEvent {
                    tick: tick.saturating_add(length as u64),
                    event: MidiEventKind::NoteOff { channel, key: nk },
                });
            }
            _ => {
                // Unknown opcode: advance safely
                break;
            }
        }
    }

    // Flush any pending tied notes
    for &nk in &active_ties {
        events.push(TimedMidiEvent {
            tick,
            event: MidiEventKind::NoteOff { channel, key: nk },
        });
    }

    // Sort events by tick, with priority: SetTempo (0) -> CC (1) -> PC (2) -> PitchBend (3) -> NoteOff (4) -> NoteOn (5)
    events.sort_by(|a, b| {
        if a.tick != b.tick {
            a.tick.cmp(&b.tick)
        } else {
            fn priority(kind: &MidiEventKind) -> u8 {
                match kind {
                    MidiEventKind::SetTempo { .. } => 0,
                    MidiEventKind::ControlChange { .. } => 1,
                    MidiEventKind::ProgramChange { .. } => 2,
                    MidiEventKind::PitchBend { .. } => 3,
                    MidiEventKind::NoteOff { .. } => 4,
                    MidiEventKind::NoteOn { .. } => 5,
                }
            }
            priority(&a.event).cmp(&priority(&b.event))
        }
    });

    Ok(events)
}

/// Encode an unsigned 32-bit integer as a Standard MIDI variable-length quantity (VLQ).
pub fn write_vlq(out: &mut Vec<u8>, val: u32) {
    let _ = formatkit_codec::smf::write_smf_vlq(out, val & formatkit_codec::smf::SMF_MAX_VLQ);
}

fn bpm_to_micros_per_quarter(bpm: u32) -> u32 {
    (60_000_000u64 / u64::from(bpm).max(1)).clamp(1, 0x00FF_FFFF) as u32
}

/// Convert a parsed GBA `m4a` `SongHeader` and its track bytecode into Standard MIDI File Format 1 (`.mid`).
pub fn export_song_to_midi(
    rom: &[u8],
    song_header: &SongHeader,
    song_name: Option<&str>,
) -> Result<Vec<u8>> {
    use formatkit_codec::smf::{
        write_smf, SmfDivision, SmfEvent, SmfFormat, SmfTrack, SmfWriteOptions,
    };

    let track_count = song_header.track_count as usize;
    let mut decompiled_tracks: Vec<Vec<TimedMidiEvent>> = Vec::with_capacity(track_count);
    let mut initial_bpm: Option<u32> = None;

    for (t, &tr_ptr) in song_header.track_ptrs.iter().enumerate() {
        let channel = (t % 16) as u8;
        let events =
            decompile_track_to_midi_events(rom, tr_ptr, channel, 100_000).unwrap_or_default();
        if initial_bpm.is_none() {
            initial_bpm = events.iter().find_map(|ev| match ev.event {
                MidiEventKind::SetTempo { bpm } => Some(bpm),
                _ => None,
            });
        }
        decompiled_tracks.push(events);
    }

    // Track 0: Conductor Track (Sequence name, Time Signature 4/4, Initial Tempo)
    let mut conductor = SmfTrack::new();
    conductor.push(0, SmfEvent::track_name(song_name.unwrap_or("M4A Sound")));
    conductor.push(
        0,
        SmfEvent::TimeSignature {
            numerator: 4,
            denominator_log2: 2,
            clocks_per_click: 24,
            thirty_seconds_per_quarter: 8,
        },
    );
    conductor.push(
        0,
        SmfEvent::Tempo {
            micros_per_quarter: bpm_to_micros_per_quarter(initial_bpm.unwrap_or(120)),
        },
    );

    let mut tracks = Vec::with_capacity(decompiled_tracks.len() + 1);
    tracks.push(conductor);

    // Tracks 1..N: Musical Tracks
    for (t, events) in decompiled_tracks.into_iter().enumerate() {
        let mut track = SmfTrack::new();
        track.push_ranked(0, 0, SmfEvent::track_name(format!("Track {t}")));

        for ev in events {
            let smf_ev = match ev.event {
                MidiEventKind::NoteOn {
                    channel,
                    key,
                    velocity,
                } => SmfEvent::NoteOn {
                    channel: channel & 0x0F,
                    key: key & 0x7F,
                    velocity: velocity & 0x7F,
                },
                MidiEventKind::NoteOff { channel, key } => SmfEvent::NoteOff {
                    channel: channel & 0x0F,
                    key: key & 0x7F,
                    velocity: 0x40,
                },
                MidiEventKind::ProgramChange { channel, program } => SmfEvent::ProgramChange {
                    channel: channel & 0x0F,
                    program: program & 0x7F,
                },
                MidiEventKind::ControlChange {
                    channel,
                    controller,
                    value,
                } => SmfEvent::ControlChange {
                    channel: channel & 0x0F,
                    controller: controller & 0x7F,
                    value: value & 0x7F,
                },
                MidiEventKind::PitchBend { channel, value } => SmfEvent::PitchBend {
                    channel: channel & 0x0F,
                    value: value & 0x3FFF,
                },
                MidiEventKind::SetTempo { bpm } => SmfEvent::Tempo {
                    micros_per_quarter: bpm_to_micros_per_quarter(bpm),
                },
            };
            track.push_ranked(ev.tick, 1, smf_ev);
        }

        tracks.push(track);
    }

    write_smf(
        SmfFormat::MultiTrack,
        SmfDivision::TicksPerQuarter(PPQN),
        &tracks,
        &SmfWriteOptions::default(),
    )
}

/// Comprehensive scan result for GBA Nintendo `m4a` sound engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct M4aEngineScan {
    pub song_table_offset: usize,
    pub song_table_address: u32,
    pub song_count: usize,
    pub playable_song_count: usize,
    pub max_tracks: u8,
    pub voicegroup_count: usize,
    pub directsound_sample_count: usize,
    pub voicegroups: Vec<u32>,
    pub sample_pointers: Vec<u32>,
    /// Detected ARM7TDMI `SoundMainRAM` mixer variant name (via `m4a::detect_mixer_variant`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mixer_variant: Option<String>,
}

/// Returns true if `ptr` falls within GBA EWRAM (`0x0200_0000..0x0204_0000`)
/// or IWRAM (`0x0300_0000..0x0300_8000`).
fn is_gba_ram_ptr(ptr: u32) -> bool {
    (0x0200_0000..0x0204_0000).contains(&ptr) || (0x0300_0000..0x0300_8000).contains(&ptr)
}

/// Returns true if `rom` contains the M4A `SoundMain` magic (`"Smsh"` at a 4-byte
/// aligned offset) or the canonical 49-byte M4A `CLOCK_TABLE`.
fn has_m4a_engine_marker(rom: &[u8]) -> bool {
    has_aligned_smsh(rom) || has_clock_table(rom)
}

fn has_aligned_smsh(rom: &[u8]) -> bool {
    rom.as_chunks::<4>().0.iter().any(|c| c == b"Smsh")
}

fn has_clock_table(rom: &[u8]) -> bool {
    rom.windows(CLOCK_TABLE.len()).any(|w| w == CLOCK_TABLE)
}

/// Validate if an offset points to a valid `SongHeader`.
fn validate_song_header_fast(rom: &[u8], offset: usize, allow_ram_vg: bool) -> Option<(u8, u32)> {
    if offset + 8 > rom.len() {
        return None;
    }
    let tracks = rom[offset];
    let vg_ptr = u32::from_le_bytes([rom[offset + 4], rom[offset + 5], rom[offset + 6], rom[offset + 7]]);
    if tracks == 0 {
        return Some((0, vg_ptr));
    }
    if tracks > MAX_TRACKS {
        return None;
    }
    let vg_is_rom = is_rom_bus_ptr(vg_ptr);
    if !vg_is_rom && !(allow_ram_vg && is_gba_ram_ptr(vg_ptr) && (vg_ptr & 3) == 0) {
        return None;
    }
    if offset + 8 + (tracks as usize) * 4 > rom.len() {
        return None;
    }
    for t in 0..tracks as usize {
        let tp = u32::from_le_bytes([
            rom[offset + 8 + t * 4],
            rom[offset + 9 + t * 4],
            rom[offset + 10 + t * 4],
            rom[offset + 11 + t * 4],
        ]);
        let to = rom_ptr_to_offset(tp, rom.len())?;
        if rom[to] < 0x80 {
            return None;
        }
    }
    // Verify voicegroup starting entry when voicegroup resides in ROM
    if vg_is_rom {
        if let Some(vg_off) = rom_ptr_to_offset(vg_ptr, rom.len()) {
            if vg_off + 12 <= rom.len() {
                let kind = rom[vg_off];
                if !matches!(kind, 0x00 | 0x08 | 0x10 | 0x01 | 0x02 | 0x03 | 0x04 | 0x09 | 0x0A | 0x0B | 0x0C | 0x40 | 0x80) {
                    return None;
                }
            }
        }
    }
    Some((tracks, vg_ptr))
}

/// Measure `(total_songs, playable_songs)` for a candidate song table at `offset`.
fn measure_song_table(rom: &[u8], offset: usize, allow_ram_vg: bool) -> (usize, usize) {
    let rom_len = rom.len();
    let mut curr = offset;
    let mut total_songs = 0usize;
    let mut playable_songs = 0usize;

    while curr + 8 <= rom_len {
        let hdr_ptr = u32::from_le_bytes([rom[curr], rom[curr + 1], rom[curr + 2], rom[curr + 3]]);
        let Some(hdr_off) = rom_ptr_to_offset(hdr_ptr, rom_len) else {
            break;
        };
        let Some((tracks, _)) = validate_song_header_fast(rom, hdr_off, allow_ram_vg) else {
            break;
        };
        total_songs += 1;
        if tracks > 0 {
            playable_songs += 1;
        }
        curr += 8;
    }

    (total_songs, playable_songs)
}

/// Validate a literal-pool ROM pointer as a candidate M4A song table and record it if playable.
fn push_code_table_candidate(
    rom: &[u8],
    ptr: u32,
    allow_ram_vg_fallback: bool,
    seen: &mut HashSet<usize>,
    out: &mut Vec<(usize, usize, usize, bool)>,
) {
    let rom_len = rom.len();
    let Some(off) = rom_ptr_to_offset(ptr, rom_len) else {
        return;
    };
    if (off & 3) != 0 || off + 8 > rom_len || !seen.insert(off) {
        return;
    }

    let (total, playable) = measure_song_table(rom, off, false);
    if playable >= 1 {
        out.push((off, total, playable, false));
        return;
    }

    if allow_ram_vg_fallback {
        let (total_ram, playable_ram) = measure_song_table(rom, off, true);
        if playable_ram >= 1 {
            out.push((off, total_ram, playable_ram, true));
        }
    }
}

/// Decode any Thumb `LDR r0..r7, [pc, #imm8*4]` instructions in `rom[start..end]` and evaluate
/// their resolved literal-pool pointers as candidate song tables.
fn collect_thumb_pc_rel_candidates(
    rom: &[u8],
    start: usize,
    end: usize,
    allow_ram_vg_fallback: bool,
    seen: &mut HashSet<usize>,
    out: &mut Vec<(usize, usize, usize, bool)>,
) {
    let rom_len = rom.len();
    let limit = min(end, rom_len.saturating_sub(2));
    for pc in (start..limit).step_by(2) {
        let insn = u16::from_le_bytes([rom[pc], rom[pc + 1]]);
        if (insn & 0xF800) == 0x4800 {
            let imm = ((insn & 0x00FF) as usize) * 4;
            let lit = ((pc + 4) & !3) + imm;
            if lit + 4 <= rom_len {
                let ptr = u32::from_le_bytes([rom[lit], rom[lit + 1], rom[lit + 2], rom[lit + 3]]);
                push_code_table_candidate(rom, ptr, allow_ram_vg_fallback, seen, out);
            }
        }
    }
}

/// Scan a GBA ROM image for the Nintendo `m4a` / Sappy sound engine, locating the
/// master `SongTable`, referenced voicegroups, track counts, and DirectSound sample headers.
pub fn scan_m4a_engine(rom: &[u8]) -> Option<M4aEngineScan> {
    let rom_len = rom.len();
    if rom_len < 0x1000 {
        return None;
    }

    let has_marker = has_m4a_engine_marker(rom);
    let mut seen_code_offsets = HashSet::new();
    // (offset, total_songs, playable_songs, allow_ram_vg)
    let mut code_candidates: Vec<(usize, usize, usize, bool)> = Vec::new();

    // 1a. Thumb `m4aSongNumStart` / `SelectSong` signature scan:
    //   Variant 1: `LSL r0, r0, #16` (0x0400) followed within 24 bytes by either:
    //     - `LSR r0, r0, #13` (0x0B40) + `ADD r0, r0, r1/r2/r3` (0x1840 / 0x1880 / 0x18C0)
    //     - `LSL r0, r3, #3; ADD r1, r0, r1` (0x00D8, 0x1841)
    //   Variant 2: direct `LSL r0, r0, #3` (0x00C0) + `ADD r0, r0, r1/r2/r3` + `LDRH r?, [r0, #4]`
    for i in (0..rom_len.saturating_sub(48)).step_by(2) {
        let w0 = u16::from_le_bytes([rom[i], rom[i + 1]]);
        if w0 == 0x0400 {
            let window = &rom[i..i + 24];
            let has_sub = window.windows(4).any(|w| {
                let a = u16::from_le_bytes([w[0], w[1]]);
                let b = u16::from_le_bytes([w[2], w[3]]);
                (a == 0x0B40 && (b & 0xFF3F) == 0x1800 && (b & 0x00C0) != 0)
                    || (a == 0x00D8 && b == 0x1841)
            });
            if has_sub {
                // Evaluate decoded Thumb PC-relative loads (handles relocated literal pools up to +1020 bytes)
                collect_thumb_pc_rel_candidates(
                    rom,
                    i,
                    i + 24,
                    has_marker,
                    &mut seen_code_offsets,
                    &mut code_candidates,
                );
                // Also scan 4-byte aligned literal pool in the following 80 bytes
                let lit_start = (i + 3) & !3;
                for lit in (lit_start..min(rom_len.saturating_sub(3), i + 80)).step_by(4) {
                    let ptr = u32::from_le_bytes([rom[lit], rom[lit + 1], rom[lit + 2], rom[lit + 3]]);
                    push_code_table_candidate(
                        rom,
                        ptr,
                        has_marker,
                        &mut seen_code_offsets,
                        &mut code_candidates,
                    );
                }
            }
        } else if w0 == 0x00C0 {
            let w1 = u16::from_le_bytes([rom[i + 2], rom[i + 3]]);
            if (w1 & 0xFF3F) == 0x1800 && (w1 & 0x00C0) != 0 {
                let tail = &rom[i + 4..min(rom_len, i + 16)];
                let has_ldrh_group = tail
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .any(|&c| (u16::from_le_bytes(c) & 0xFFF8) == 0x8880);
                if has_ldrh_group {
                    let scan_start = i.saturating_sub(12);
                    collect_thumb_pc_rel_candidates(
                        rom,
                        scan_start,
                        i + 16,
                        has_marker,
                        &mut seen_code_offsets,
                        &mut code_candidates,
                    );
                }
            }
        }
    }

    // 1b. ARM-mode `m4aSongNumStart` / `SelectSong` signature scan:
    //   `ADD r0, r1/r2/r3, r0, LSL #3` (0xE0810180 / 0xE0820180 / 0xE0830180)
    //   preceded within 32 bytes by ARM PC-relative `LDR r1/r2/r3, [pc, #±imm12]`.
    for i in (0..rom_len.saturating_sub(16)).step_by(4) {
        let insn = u32::from_le_bytes([rom[i], rom[i + 1], rom[i + 2], rom[i + 3]]);
        if matches!(insn, 0xE081_0180 | 0xE082_0180 | 0xE083_0180) {
            let start = i.saturating_sub(32);
            for pc in (start..i).step_by(4) {
                let ldr = u32::from_le_bytes([rom[pc], rom[pc + 1], rom[pc + 2], rom[pc + 3]]);
                let is_pc_ldr = (ldr & 0x0F7F_0000) == 0x051F_0000;
                if is_pc_ldr {
                    let add = (ldr & 0x0080_0000) != 0;
                    let imm = (ldr & 0x0FFF) as usize;
                    let base = pc + 8;
                    let lit = if add {
                        base.checked_add(imm)
                    } else {
                        base.checked_sub(imm)
                    };
                    if let Some(lit_off) = lit {
                        if lit_off + 4 <= rom_len {
                            let ptr = u32::from_le_bytes([
                                rom[lit_off],
                                rom[lit_off + 1],
                                rom[lit_off + 2],
                                rom[lit_off + 3],
                            ]);
                            push_code_table_candidate(
                                rom,
                                ptr,
                                has_marker,
                                &mut seen_code_offsets,
                                &mut code_candidates,
                            );
                        }
                    }
                }
            }
        }
    }

    // 2. Structural pointer table scan (fallback when code signature is absent)
    let mut best_choice: Option<(usize, bool)> = None;
    if !code_candidates.is_empty() {
        code_candidates.sort_by_key(|a| std::cmp::Reverse((a.2, a.1)));
        best_choice = code_candidates.first().map(|c| (c.0, c.3));
    }

    let (best_offset, best_allow_ram_vg) = if let Some(choice) = best_choice {
        choice
    } else {
        let confirmed_m4a = has_aligned_smsh(rom) && has_clock_table(rom);
        let min_total = if confirmed_m4a { 2 } else { 8 };
        let min_playable = if confirmed_m4a { 1 } else { 5 };

        let mut candidates: Vec<(usize, usize, usize)> = Vec::new(); // (offset, total_songs, playable_songs)
        // Scan each 8-byte-entry alignment parity (0 mod 8 and 4 mod 8) in O(N) time,
        // advancing past failed long runs so crafted pointer tables cannot trigger O(N²) rescans.
        for parity in [0usize, 4usize] {
            let mut i = parity;
            while i < rom_len.saturating_sub(64) {
                let mut curr = i;
                let mut total_songs = 0;
                let mut playable_songs = 0;
                let mut valid_groups = true;

                while curr + 8 <= rom_len {
                    let hdr_ptr =
                        u32::from_le_bytes([rom[curr], rom[curr + 1], rom[curr + 2], rom[curr + 3]]);
                    if let Some(hdr_off) = rom_ptr_to_offset(hdr_ptr, rom_len) {
                        if let Some((tracks, _)) = validate_song_header_fast(rom, hdr_off, false) {
                            if confirmed_m4a && total_songs < 4 {
                                let g0 = u16::from_le_bytes([rom[curr + 4], rom[curr + 5]]);
                                let g1 = u16::from_le_bytes([rom[curr + 6], rom[curr + 7]]);
                                if g0 != g1 || g0 >= 32 {
                                    valid_groups = false;
                                }
                            }
                            total_songs += 1;
                            if tracks > 0 {
                                playable_songs += 1;
                            }
                            curr += 8;
                            continue;
                        }
                    }
                    break;
                }

                if total_songs >= min_total
                    && playable_songs >= min_playable
                    && (!confirmed_m4a || total_songs >= 8 || valid_groups)
                {
                    candidates.push((i, total_songs, playable_songs));
                    i = curr;
                } else if curr > i && total_songs >= 8 {
                    // Long run failed only due to playable_songs < min_playable;
                    // no suffix of [i, curr) can have more playable songs.
                    i = curr;
                } else {
                    i += 8;
                }
            }
        }

        candidates.sort_by_key(|a| std::cmp::Reverse((a.2, a.1)));
        (candidates.first().map(|c| c.0)?, false)
    };

    // Analyze full song table at best_offset
    let mut curr = best_offset;
    let mut song_count = 0;
    let mut playable_song_count = 0;
    let mut max_tracks: u8 = 0;
    let mut voicegroups_set = HashSet::new();
    let mut voicegroups = Vec::new();

    while curr + 8 <= rom_len {
        let hdr_ptr = u32::from_le_bytes([rom[curr], rom[curr + 1], rom[curr + 2], rom[curr + 3]]);
        let hdr_off = match rom_ptr_to_offset(hdr_ptr, rom_len) {
            Some(o) => o,
            None => break,
        };
        let (tracks, vg_ptr) = match validate_song_header_fast(rom, hdr_off, best_allow_ram_vg) {
            Some(res) => res,
            None => break,
        };

        song_count += 1;
        if tracks > 0 {
            playable_song_count += 1;
            max_tracks = max_tracks.max(tracks);
            if is_rom_bus_ptr(vg_ptr) && voicegroups_set.insert(vg_ptr) {
                voicegroups.push(vg_ptr);
            }
        }
        curr += 8;
    }

    // Collect all DirectSound samples referenced across voicegroups
    let mut sample_pointers_set = HashSet::new();
    let mut sample_pointers = Vec::new();

    for &vg_ptr in &voicegroups {
        let samples = collect_voicegroup_sample_pointers(rom, vg_ptr);
        for s_ptr in samples {
            if sample_pointers_set.insert(s_ptr) {
                sample_pointers.push(s_ptr);
            }
        }
    }

    let mixer_variant = detect_mixer_variant(rom).map(|v| m4a_mixer_variant_name(v).to_string());

    Some(M4aEngineScan {
        song_table_offset: best_offset,
        song_table_address: offset_to_rom_ptr(best_offset),
        song_count,
        playable_song_count,
        max_tracks,
        voicegroup_count: voicegroups.len(),
        directsound_sample_count: sample_pointers.len(),
        voicegroups,
        sample_pointers,
        mixer_variant,
    })
}

fn min(a: usize, b: usize) -> usize {
    if a < b { a } else { b }
}

/// Iteratively collect unique DirectSound sample ROM pointers referenced by `voicegroup_ptr`.
///
/// Uses an explicit worklist and visited set so cyclic or deeply nested voicegroups
/// can never overflow the call stack.
pub fn collect_voicegroup_sample_pointers(rom: &[u8], voicegroup_ptr: u32) -> Vec<u32> {
    let mut visited = HashSet::new();
    let mut seen_samples = HashSet::new();
    let mut out = Vec::new();
    let mut worklist = vec![voicegroup_ptr];

    while let Some(vg_ptr) = worklist.pop() {
        if !visited.insert(vg_ptr) {
            continue;
        }
        let Some(vg_off) = rom_ptr_to_offset(vg_ptr, rom.len()) else {
            continue;
        };

        let mut child_vgs = Vec::new();
        for i in 0..128 {
            let entry_off = vg_off + i * VOICE_ENTRY_SIZE;
            if entry_off + VOICE_ENTRY_SIZE > rom.len() {
                break;
            }
            let kind = VoiceKind::from_raw(rom[entry_off]);
            let data_ptr = u32::from_le_bytes([
                rom[entry_off + 4],
                rom[entry_off + 5],
                rom[entry_off + 6],
                rom[entry_off + 7],
            ]);

            let is_directsound_or_dpcm = kind.is_directsound()
                || matches!(kind, VoiceKind::Unknown(raw) if (raw & 0x20) != 0 && (raw & 0x07) == 0);

            if is_directsound_or_dpcm {
                if let Some(wav_off) = rom_ptr_to_offset(data_ptr, rom.len()) {
                    if let Ok(view) = M4aSampleView::from_wavdata_bytes(&rom[wav_off..]) {
                        let h = view.header;
                        if (h.wave_type == 0 || h.wave_type == 1)
                            && h.stat == 0
                            && (h.flags == 0 || h.flags == 0x40 || h.flags == 0xC0)
                            && seen_samples.insert(data_ptr)
                        {
                            out.push(data_ptr);
                        }
                    }
                }
            } else if matches!(kind, VoiceKind::Keysplit | VoiceKind::Drumkit)
                && is_rom_bus_ptr(data_ptr)
                && !visited.contains(&data_ptr)
            {
                child_vgs.push(data_ptr);
            }
        }

        // Push children in reverse order to preserve depth-first traversal order
        for child_ptr in child_vgs.into_iter().rev() {
            worklist.push(child_ptr);
        }
    }

    out
}

/// Extract all DirectSound samples from a voicegroup as clean 16-bit uncompressed WAV files.
///
/// Automatically decodes Pokemon Gen 3 4-bit quadratic DPCM samples (`wave_type != 0`)
/// as well as standard uncompressed 8-bit signed PCM samples.
pub fn extract_voicegroup_samples_wav16(
    rom: &[u8],
    voicegroup_ptr: u32,
) -> Result<Vec<(u32, WaveDataHeader, Vec<u8>)>> {
    let pointers = collect_voicegroup_sample_pointers(rom, voicegroup_ptr);
    let mut results = Vec::with_capacity(pointers.len());

    for ptr in pointers {
        if let Some(off) = rom_ptr_to_offset(ptr, rom.len()) {
            if let Ok((hdr, pcm_i8)) = extract_pcm_i8(&rom[off..], 1_000_000) {
                let rate = hdr.base_sample_rate_hz().max(1);
                if let Ok(wav_bytes) = encode_wav_i16_mono(rate, &pcm_i8) {
                    results.push((ptr, hdr, wav_bytes));
                }
            }
        }
    }

    Ok(results)
}

#[cfg(test)]
mod m4a_variant_tests {
    use super::*;

    /// Build a minimal synthetic GBA ROM containing a valid M4A voicegroup,
    /// DirectSound sample, track bytecode, song header, and 1-entry song table.
    fn build_minimal_m4a_rom(
        song_table_off: usize,
        voicegroup_ptr: u32,
        include_smsh: bool,
    ) -> Vec<u8> {
        let mut rom = vec![0u8; 0x4000];

        if include_smsh {
            rom[0x1000..0x1004].copy_from_slice(b"Smsh");
        }

        // DirectSound sample header at 0x2400 (16-byte WaveDataHeader + 32 bytes PCM)
        let sample_off = 0x2400usize;
        rom[sample_off..sample_off + 2].copy_from_slice(&0u16.to_le_bytes()); // type = 0
        rom[sample_off + 2..sample_off + 4].copy_from_slice(&0u16.to_le_bytes()); // stat = 0
        rom[sample_off + 4..sample_off + 8].copy_from_slice(&(13379u32 * 1024).to_le_bytes());
        rom[sample_off + 8..sample_off + 12].copy_from_slice(&0u32.to_le_bytes()); // loop
        rom[sample_off + 12..sample_off + 16].copy_from_slice(&32u32.to_le_bytes()); // size

        // Voicegroup at 0x2200 (if ROM-resident)
        let vg_off = 0x2200usize;
        rom[vg_off] = 0x00; // DirectSound
        rom[vg_off + 1] = 60; // base_key
        rom[vg_off + 4..vg_off + 8]
            .copy_from_slice(&offset_to_rom_ptr(sample_off).to_le_bytes());
        rom[vg_off + 8..vg_off + 12].copy_from_slice(&[0xFF, 0x00, 0xFF, 0x00]);

        // Track bytecode at 0x2100: Tempo(60), Voice(0), Note(60, 100), Fine
        let track_off = 0x2100usize;
        rom[track_off..track_off + 8]
            .copy_from_slice(&[0xBB, 60, 0xBD, 0, 0xD0, 60, 100, 0xB1]);

        // SongHeader at 0x2000: 1 track
        let hdr_off = 0x2000usize;
        rom[hdr_off] = 1; // track_count
        rom[hdr_off + 4..hdr_off + 8].copy_from_slice(&voicegroup_ptr.to_le_bytes());
        rom[hdr_off + 8..hdr_off + 12]
            .copy_from_slice(&offset_to_rom_ptr(track_off).to_le_bytes());

        // SongTable at song_table_off: 1 playable entry
        rom[song_table_off..song_table_off + 4]
            .copy_from_slice(&offset_to_rom_ptr(hdr_off).to_le_bytes());
        rom[song_table_off + 4..song_table_off + 6].copy_from_slice(&0u16.to_le_bytes());
        rom[song_table_off + 6..song_table_off + 8].copy_from_slice(&0u16.to_le_bytes());

        rom
    }

    #[test]
    fn detects_thumb_m4a_song_num_start_at_halfword_aligned_offset() {
        let song_table_off = 0x1800;
        let mut rom = build_minimal_m4a_rom(song_table_off, offset_to_rom_ptr(0x2200), true);

        // Place `push {lr}` at 0x0400 (4-byte aligned), so `lsl r0, r0, #16` (0x0400)
        // sits at 0x0402 (4k + 2) while its literal pool is 4-byte aligned at 0x0428.
        let func = 0x0400usize;
        let code: [u16; 7] = [
            0xB500, // push {lr}
            0x0400, // lsl r0, r0, #16        @ 0x0402
            0x4A08, // ldr r2, [pc, #32]      @ 0x0404 -> 0x0428
            0x4908, // ldr r1, [pc, #32]      @ 0x0406 -> 0x0428
            0x0B40, // lsr r0, r0, #13        @ 0x0408
            0x1840, // add r0, r0, r1         @ 0x040A
            0x4770, // bx lr                  @ 0x040C
        ];
        for (idx, insn) in code.iter().enumerate() {
            let pos = func + idx * 2;
            rom[pos..pos + 2].copy_from_slice(&insn.to_le_bytes());
        }
        rom[0x0428..0x042C].copy_from_slice(&offset_to_rom_ptr(song_table_off).to_le_bytes());

        let scan = scan_m4a_engine(&rom).expect("should detect 4k+2 aligned m4aSongNumStart");
        assert_eq!(scan.song_table_offset, song_table_off);
        assert_eq!(scan.song_count, 1);
        assert_eq!(scan.playable_song_count, 1);
        assert_eq!(scan.voicegroup_count, 1);
        assert_eq!(scan.directsound_sample_count, 1);
    }

    #[test]
    fn detects_thumb_register_variant_with_relocated_literal_pool() {
        let song_table_off = 0x1900;
        let mut rom = build_minimal_m4a_rom(song_table_off, offset_to_rom_ptr(0x2200), false);

        // `LSL r0, #16; LDR r3, [pc, #160]; LSR r0, #13; ADD r0, r0, r3`
        // Literal pool is +164 bytes away (beyond the 80-byte inline scan window).
        let pc_start = 0x0500usize;
        let code: [u16; 4] = [
            0x0400, // lsl r0, r0, #16        @ 0x0500
            0x4B28, // ldr r3, [pc, #160]     @ 0x0502 -> ((0x0506) & !3) + 160 = 0x05A4
            0x0B40, // lsr r0, r0, #13        @ 0x0504
            0x18C0, // add r0, r0, r3         @ 0x0506
        ];
        for (idx, insn) in code.iter().enumerate() {
            let pos = pc_start + idx * 2;
            rom[pos..pos + 2].copy_from_slice(&insn.to_le_bytes());
        }
        let lit = ((0x0502 + 4) & !3) + 0x28 * 4;
        rom[lit..lit + 4].copy_from_slice(&offset_to_rom_ptr(song_table_off).to_le_bytes());

        let scan = scan_m4a_engine(&rom).expect("should detect register-variant relocated pool");
        assert_eq!(scan.song_table_offset, song_table_off);
        assert_eq!(scan.playable_song_count, 1);
    }

    #[test]
    fn detects_thumb_direct_lsl3_variant() {
        let song_table_off = 0x1A00;
        let mut rom = build_minimal_m4a_rom(song_table_off, offset_to_rom_ptr(0x2200), false);

        // `LDR r1, [pc, #16]; LSL r0, r0, #3; ADD r0, r0, r1; LDRH r2, [r0, #4]`
        let pc_start = 0x0600usize;
        let code: [u16; 4] = [
            0x4904, // ldr r1, [pc, #16]      @ 0x0600 -> 0x0614
            0x00C0, // lsl r0, r0, #3         @ 0x0602
            0x1840, // add r0, r0, r1         @ 0x0604
            0x8882, // ldrh r2, [r0, #4]      @ 0x0606
        ];
        for (idx, insn) in code.iter().enumerate() {
            let pos = pc_start + idx * 2;
            rom[pos..pos + 2].copy_from_slice(&insn.to_le_bytes());
        }
        rom[0x0614..0x0618].copy_from_slice(&offset_to_rom_ptr(song_table_off).to_le_bytes());

        let scan = scan_m4a_engine(&rom).expect("should detect direct LSL #3 Thumb variant");
        assert_eq!(scan.song_table_offset, song_table_off);
        assert_eq!(scan.playable_song_count, 1);
    }

    #[test]
    fn detects_arm_mode_select_song_variant() {
        let song_table_off = 0x1B00;
        let mut rom = build_minimal_m4a_rom(song_table_off, offset_to_rom_ptr(0x2200), false);

        // ARM mode at 0x0700:
        //   0x0700: LDR r1, [pc, #8]          (0xE59F1008) -> 0x0700 + 8 + 8 = 0x0710
        //   0x0704: ADD r0, r1, r0, LSL #3    (0xE0810180)
        //   0x0708: BX lr                     (0xE12FFF1E)
        //   0x0710: .word song_table_ptr
        rom[0x0700..0x0704].copy_from_slice(&0xE59F_1008u32.to_le_bytes());
        rom[0x0704..0x0708].copy_from_slice(&0xE081_0180u32.to_le_bytes());
        rom[0x0708..0x070C].copy_from_slice(&0xE12F_FF1Eu32.to_le_bytes());
        rom[0x0710..0x0714].copy_from_slice(&offset_to_rom_ptr(song_table_off).to_le_bytes());

        let scan = scan_m4a_engine(&rom).expect("should detect ARM-mode SelectSong");
        assert_eq!(scan.song_table_offset, song_table_off);
        assert_eq!(scan.playable_song_count, 1);
    }

    #[test]
    fn detects_ewram_dynamic_voicegroup_when_m4a_marker_present() {
        let song_table_off = 0x1C00;
        // Voicegroup resides in EWRAM (0x0200_0000), as used by dynamic-bank M4A builds.
        let mut rom = build_minimal_m4a_rom(song_table_off, 0x0200_0000, true);

        let func = 0x0800usize;
        let code: [u16; 6] = [
            0xB500, // push {lr}
            0x0400, // lsl r0, r0, #16
            0x4A06, // ldr r2, [pc, #24]
            0x4906, // ldr r1, [pc, #24]
            0x0B40, // lsr r0, r0, #13
            0x1840, // add r0, r0, r1
        ];
        for (idx, insn) in code.iter().enumerate() {
            let pos = func + idx * 2;
            rom[pos..pos + 2].copy_from_slice(&insn.to_le_bytes());
        }
        let lit = ((0x0806 + 4) & !3) + 6 * 4;
        rom[lit..lit + 4].copy_from_slice(&offset_to_rom_ptr(song_table_off).to_le_bytes());

        let scan = scan_m4a_engine(&rom).expect("should detect EWRAM-voicegroup M4A build");
        assert_eq!(scan.song_table_offset, song_table_off);
        assert_eq!(scan.playable_song_count, 1);
        assert_eq!(scan.voicegroup_count, 0);
    }

    #[test]
    fn rejects_zero_playable_decoy_and_picks_real_song_table() {
        let decoy_off = 0x1400;
        let real_off = 0x1800;
        let mut rom = build_minimal_m4a_rom(real_off, offset_to_rom_ptr(0x2200), true);

        // Decoy table at 0x1400 pointing to a zero-track dummy header at 0x1500
        rom[decoy_off..decoy_off + 4].copy_from_slice(&offset_to_rom_ptr(0x1500).to_le_bytes());

        // Earlier code site referencing the decoy table (0 playable songs)
        let decoy_code: [u16; 5] = [0x0400, 0x4902, 0x0B40, 0x1840, 0x4770];
        for (idx, insn) in decoy_code.iter().enumerate() {
            let pos = 0x0200 + idx * 2;
            rom[pos..pos + 2].copy_from_slice(&insn.to_le_bytes());
        }
        rom[0x020C..0x0210].copy_from_slice(&offset_to_rom_ptr(decoy_off).to_le_bytes());

        // Real code site referencing the playable song table
        for (idx, insn) in decoy_code.iter().enumerate() {
            let pos = 0x0300 + idx * 2;
            rom[pos..pos + 2].copy_from_slice(&insn.to_le_bytes());
        }
        rom[0x030C..0x0310].copy_from_slice(&offset_to_rom_ptr(real_off).to_le_bytes());

        let scan = scan_m4a_engine(&rom).expect("should skip 0-playable decoy");
        assert_eq!(scan.song_table_offset, real_off);
        assert_eq!(scan.playable_song_count, 1);
    }

    #[test]
    fn zero_false_positives_on_non_m4a_rom() {
        let mut rom = vec![0xAAu8; 0x4000];
        rom[0x100..0x110].copy_from_slice(b"GAX Sound Engine");
        assert!(scan_m4a_engine(&rom).is_none());
    }

    #[test]
    fn iterative_voicegroup_traversal_handles_deep_chain_and_cycles() {
        // Build a chain of 512 nested Keysplit voicegroups + a cycle back to root + a valid sample at the end.
        let chain_len = 512usize;
        let vg_stride = 16usize; // 12-byte entry + 4-byte padding
        let sample_off = 0x4000usize;
        let mut rom = vec![0xFFu8; sample_off + 64];

        // Place a valid 16-sample PCM8 WaveData header at sample_off
        rom[sample_off..sample_off + 2].copy_from_slice(&0u16.to_le_bytes()); // wave_type = 0
        rom[sample_off + 2] = 0; // stat
        rom[sample_off + 3] = 0; // flags
        rom[sample_off + 4..sample_off + 8].copy_from_slice(&(13_379u32 * 1024).to_le_bytes());
        rom[sample_off + 8..sample_off + 12].copy_from_slice(&0u32.to_le_bytes()); // loop_start
        rom[sample_off + 12..sample_off + 16].copy_from_slice(&16u32.to_le_bytes()); // size
        rom[sample_off + 16..sample_off + 32].fill(0x10);

        let root_off = 0x0100usize;
        for idx in 0..chain_len {
            let off = root_off + idx * vg_stride;
            if idx + 1 < chain_len {
                let next_ptr = offset_to_rom_ptr(root_off + (idx + 1) * vg_stride);
                rom[off] = VoiceKind::Keysplit.to_raw();
                rom[off + 4..off + 8].copy_from_slice(&next_ptr.to_le_bytes());
            } else {
                // Terminal voicegroup: entry 0 points to sample, entry 1 cycles back to root
                let sample_ptr = offset_to_rom_ptr(sample_off);
                rom[off] = VoiceKind::DirectSound.to_raw();
                rom[off + 4..off + 8].copy_from_slice(&sample_ptr.to_le_bytes());
                let root_ptr = offset_to_rom_ptr(root_off);
                rom[off + VOICE_ENTRY_SIZE] = VoiceKind::Drumkit.to_raw();
                rom[off + VOICE_ENTRY_SIZE + 4..off + VOICE_ENTRY_SIZE + 8]
                    .copy_from_slice(&root_ptr.to_le_bytes());
            }
        }

        let ptrs = collect_voicegroup_sample_pointers(&rom, offset_to_rom_ptr(root_off));
        assert_eq!(ptrs, vec![offset_to_rom_ptr(sample_off)]);
    }
}
