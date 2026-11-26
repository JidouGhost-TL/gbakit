//! Comprehensive unit tests for M4A sound engine, track bytecode decompiler,
//! MIDI synthesizer / exporter, and DirectSound 16-bit WAV encoder.

use crate::m4a::*;
use crate::*;

fn encode_voice(
    kind: u8,
    base_key: u8,
    length: u8,
    pan_or_sweep: u8,
    data_ptr: u32,
    adsr: [u8; 4],
) -> [u8; VOICE_ENTRY_SIZE] {
    let mut out = [0u8; VOICE_ENTRY_SIZE];
    out[0] = kind;
    out[1] = base_key;
    out[2] = length;
    out[3] = pan_or_sweep;
    out[4..8].copy_from_slice(&data_ptr.to_le_bytes());
    out[8..12].copy_from_slice(&adsr);
    out
}

#[test]
fn test_looped_wavedata_and_pcm_extraction() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&0u16.to_le_bytes()); // type
    buf.extend_from_slice(&0x4000u16.to_le_bytes()); // looped
    buf.extend_from_slice(&(13379u32 * 1024).to_le_bytes()); // ~13.379 kHz
    buf.extend_from_slice(&4u32.to_le_bytes()); // loop_start
    buf.extend_from_slice(&8u32.to_le_bytes()); // size
    buf.extend_from_slice(&[0x00, 0x10, 0x20, 0x10, 0x00, 0xF0, 0xE0, 0xF0]);

    let (hdr, pcm) = extract_pcm_i8(&buf, 1024).unwrap();
    assert!(hdr.is_looped());
    assert_eq!(hdr.loop_start, 4);
    assert_eq!(hdr.size, 8);
    assert_eq!(hdr.base_sample_rate_hz(), 13379);
    assert_eq!(pcm, vec![0, 16, 32, 16, 0, -16, -32, -16]);

    let (hdr_pcm, formatkit_pcm) = extract_pcm(&buf, 1024).unwrap();
    assert_eq!(hdr_pcm.size, 8);
    assert_eq!(formatkit_pcm.channels(), 1);
    assert_eq!(formatkit_pcm.sample_rate(), 13379);
    assert_eq!(
        formatkit_pcm.samples(),
        &[0, 16 << 8, 32 << 8, 16 << 8, 0, -16 << 8, -32 << 8, -16 << 8]
    );
}

#[test]
fn test_wav16_encoder() {
    let pcm_i8: [i8; 4] = [0, 64, -64, 127];
    let wav = encode_wav_i16_mono(22050, &pcm_i8).unwrap();
    assert_eq!(wav.len(), 44 + 8);
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(&wav[12..16], b"fmt ");
    assert_eq!(&wav[36..40], b"data");

    // Sample rate at 24..28
    let rate = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
    assert_eq!(rate, 22050);

    // Bits per sample at 34..36
    let bits = u16::from_le_bytes([wav[34], wav[35]]);
    assert_eq!(bits, 16);

    // Verify converted 16-bit samples
    let s0 = i16::from_le_bytes([wav[44], wav[45]]);
    let s1 = i16::from_le_bytes([wav[46], wav[47]]);
    let s2 = i16::from_le_bytes([wav[48], wav[49]]);
    let s3 = i16::from_le_bytes([wav[50], wav[51]]);
    assert_eq!(s0, 0);
    assert_eq!(s1, 64 << 8);
    assert_eq!(s2, -64 << 8);
    assert_eq!(s3, (127i16) << 8);
}

#[test]
fn test_voicegroup_kinds_and_entries() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&encode_voice(0x00, 60, 0, 0x80, 0x0810_0000, [255, 0, 255, 192]));
    buf.extend_from_slice(&encode_voice(0x08, 60, 0, 0, 0x0810_1000, [255, 0, 255, 0]));
    buf.extend_from_slice(&encode_voice(0x10, 60, 0, 0, 0x0810_2000, [128, 64, 192, 32]));
    buf.extend_from_slice(&encode_voice(0x01, 0, 0, 0, 2, [0, 7, 15, 3]));
    buf.extend_from_slice(&encode_voice(0x02, 0, 0, 0, 1, [0, 5, 12, 2]));
    buf.extend_from_slice(&encode_voice(0x03, 0, 0, 0, 0x0810_3000, [0, 0, 15, 0]));
    buf.extend_from_slice(&encode_voice(0x04, 0, 0, 0, 0, [0, 3, 10, 1]));
    buf.extend_from_slice(&encode_voice(0x40, 0, 0, 0, 0x0810_4000, [0x00, 0x50, 0x10, 0x08]));
    buf.extend_from_slice(&encode_voice(0x80, 0, 0, 0, 0x0810_6000, [0, 0, 0, 0]));

    let vg = parse_voicegroup(&buf, 0, 9).unwrap();
    assert_eq!(vg.len(), 9);
    assert_eq!(vg[0].kind, VoiceKind::DirectSound);
    assert_eq!(vg[0].base_key, 60);
    assert_eq!(vg[0].pan_or_sweep, 0x80);
    assert_eq!(vg[0].data_ptr, 0x0810_0000);
    assert_eq!((vg[0].attack, vg[0].decay, vg[0].sustain, vg[0].release), (255, 0, 255, 192));
    assert_eq!(vg[1].kind, VoiceKind::DirectSoundFixed);
    assert_eq!(vg[2].kind, VoiceKind::DirectSoundAlt);
    assert_eq!(vg[3].kind, VoiceKind::Square1);
    assert_eq!(vg[4].kind, VoiceKind::Square2);
    assert_eq!(vg[5].kind, VoiceKind::ProgrammableWave);
    assert_eq!(vg[6].kind, VoiceKind::Noise);
    assert_eq!(vg[7].kind, VoiceKind::Keysplit);
    assert_eq!(vg[8].kind, VoiceKind::Drumkit);
}

#[test]
fn test_song_header_and_table_bounds() {
    let mut buf = vec![0u8; 4]; // padding
    buf.extend_from_slice(&[2, 0, 10, 0x80]); // tracks=2, blocks=0, prio=10, reverb=0x80
    buf.extend_from_slice(&0x0820_0000u32.to_le_bytes()); // vg_ptr
    buf.extend_from_slice(&0x0820_0100u32.to_le_bytes()); // tr0
    buf.extend_from_slice(&0x0820_0200u32.to_le_bytes()); // tr1

    let hdr = parse_song_header(&buf, 4).unwrap();
    assert_eq!(hdr.track_count, 2);
    assert_eq!(hdr.voicegroup_ptr, 0x0820_0000);
    assert_eq!(hdr.track_ptrs, vec![0x0820_0100, 0x0820_0200]);

    // Table parsing
    let mut tbl_buf = Vec::new();
    tbl_buf.extend_from_slice(&0x0830_0000u32.to_le_bytes());
    tbl_buf.extend_from_slice(&1u16.to_le_bytes());
    tbl_buf.extend_from_slice(&1u16.to_le_bytes());
    tbl_buf.extend_from_slice(&0x0830_0040u32.to_le_bytes());
    tbl_buf.extend_from_slice(&2u16.to_le_bytes());
    tbl_buf.extend_from_slice(&2u16.to_le_bytes());

    let entries = parse_song_table(&tbl_buf, 0, 2).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].header_ptr, 0x0830_0000);
    assert_eq!(entries[0].player_group, 1);
    assert_eq!(entries[1].header_ptr, 0x0830_0040);
    assert_eq!(entries[1].player_group, 2);
}

#[test]
fn test_track_bytecode_decompiler_and_events() {
    let mut track = Vec::new();
    track.extend_from_slice(&[0xBA, 10]); // Prio(10)
    track.extend_from_slice(&[0xBB, 60]); // Tempo(60) -> 120 BPM
    track.extend_from_slice(&[0xBC, (-2i8) as u8]); // Keysh(-2)
    track.extend_from_slice(&[0xBD, 12]); // Voice(12)
    track.extend_from_slice(&[0xBE, 90]); // Vol(90)
    track.extend_from_slice(&[0xBF, 64]); // Pan(64)
    track.extend_from_slice(&[0xC0, 64]); // Bend(64)
    track.extend_from_slice(&[0xC4, 15]); // Mod(15)

    // Note event: N24 (0xE7), key 60, vel 100
    track.extend_from_slice(&[0xE7, 60, 100]);
    // Wait 24 ticks (0x98)
    track.push(0x98);
    // Running status note: key 64, vel 95, extra gate 2 -> length 26
    track.extend_from_slice(&[64, 95, 2]);
    // Wait 96 ticks (0xB0)
    track.push(0xB0);

    // Tied note + eot
    track.extend_from_slice(&[0xCF, 72, 85]);
    track.push(0x8C); // Wait 12 ticks
    track.extend_from_slice(&[0xCE, 72]);

    // Fine
    track.push(0xB1);

    let (events, consumed) = parse_track_events(&track, 0, 50).unwrap();
    assert_eq!(consumed, track.len());
    assert_eq!(events.len(), 16);
    assert_eq!(events[0], M4aEvent::Prio(10));
    assert_eq!(events[1], M4aEvent::Tempo(60));
    assert_eq!(events[2], M4aEvent::Keysh(-2));
    assert_eq!(events[3], M4aEvent::Voice(12));
    assert_eq!(events[4], M4aEvent::Vol(90));
    assert_eq!(events[5], M4aEvent::Pan(64));
    assert_eq!(events[6], M4aEvent::Bend(64));
    assert_eq!(events[7], M4aEvent::Mod(15));
    assert_eq!(events[8], M4aEvent::Note { length: 24, key: 60, velocity: 100 });
    assert_eq!(events[9], M4aEvent::Wait(24));
    assert_eq!(events[10], M4aEvent::Note { length: 26, key: 64, velocity: 95 });
    assert_eq!(events[11], M4aEvent::Wait(96));
    assert_eq!(events[12], M4aEvent::Tie { key: 72, velocity: 85 });
    assert_eq!(events[13], M4aEvent::Wait(12));
    assert_eq!(events[14], M4aEvent::Eot(Some(72)));
    assert_eq!(events[15], M4aEvent::Fine);
}

#[test]
fn test_pattern_expansion_and_decompile_to_midi() {
    let mut rom = vec![0u8; 0x1000];

    // Pattern subroutine at offset 0x200 (address 0x08000200):
    // Plays N12 key 70, Wait 12, then Pend (0xB4)
    let patt_off = 0x200;
    rom[patt_off..patt_off + 6].copy_from_slice(&[
        0xDB, 70, 110, // Note length 12, key 70, vel 110
        0x8C,          // Wait 12
        0xB4, 0x00,    // Pend
    ]);

    // Main track at offset 0x100 (address 0x08000100):
    // Tempo 60, Voice 5, call Patt 0x08000200, Wait 24, Fine (0xB1)
    let tr_off = 0x100;
    rom[tr_off..tr_off + 14].copy_from_slice(&[
        0xBB, 60,       // Tempo 60 (120 BPM)
        0xBD, 5,        // Voice 5
        0xB3, 0x00, 0x02, 0x00, 0x08, // Patt 0x08000200
        0x98,           // Wait 24
        0xB1,           // Fine
        0x00, 0x00, 0x00,
    ]);

    let timed_events = decompile_track_to_midi_events(&rom, 0x0800_0100, 0, 10_000).unwrap();
    assert!(!timed_events.is_empty());

    // Should contain SetTempo (120 BPM), ProgramChange (5), NoteOn (70), NoteOff (70)
    let has_tempo = timed_events.iter().any(|e| matches!(e.event, MidiEventKind::SetTempo { bpm: 120 }));
    let has_voice = timed_events.iter().any(|e| matches!(e.event, MidiEventKind::ProgramChange { program: 5, .. }));
    let has_note_on = timed_events.iter().any(|e| matches!(e.event, MidiEventKind::NoteOn { key: 70, velocity: 110, .. }));
    let has_note_off = timed_events.iter().any(|e| matches!(e.event, MidiEventKind::NoteOff { key: 70, .. }));

    assert!(has_tempo, "Expected 120 BPM tempo event");
    assert!(has_voice, "Expected voice 5 program change");
    assert!(has_note_on, "Expected note on key 70 from subroutine");
    assert!(has_note_off, "Expected note off key 70 from subroutine");
}

#[test]
fn test_vlq_and_midi_file_format_export() {
    let mut vlq_out = Vec::new();
    write_vlq(&mut vlq_out, 0);
    assert_eq!(vlq_out, vec![0x00]);
    vlq_out.clear();
    write_vlq(&mut vlq_out, 127);
    assert_eq!(vlq_out, vec![0x7F]);
    vlq_out.clear();
    write_vlq(&mut vlq_out, 128);
    assert_eq!(vlq_out, vec![0x81, 0x00]);
    vlq_out.clear();
    write_vlq(&mut vlq_out, 106903);
    assert_eq!(vlq_out, vec![0x86, 0xC3, 0x17]);

    // Construct synthetic SongHeader and track
    let mut rom = vec![0u8; 0x2000];
    let tr_off = 0x1000;
    rom[tr_off..tr_off + 9].copy_from_slice(&[
        0xBB, 75,       // Tempo 75 -> 150 BPM
        0xBD, 1,        // Voice 1
        0xE7, 60, 100,  // Note length 24, key 60, vel 100
        0x98,           // Wait 24
        0xB1,           // Fine
    ]);

    let song_hdr = SongHeader {
        track_count: 1,
        block_count: 0,
        priority: 5,
        reverb: 0x80,
        voicegroup_ptr: 0x0800_0500,
        track_ptrs: vec![0x0800_1000],
    };

    let midi_bytes = export_song_to_midi(&rom, &song_hdr, Some("Test Song")).unwrap();
    assert_eq!(&midi_bytes[0..4], b"MThd");
    assert_eq!(&midi_bytes[8..10], &[0x00, 0x01]); // SMF Format 1
    assert_eq!(&midi_bytes[10..12], &[0x00, 0x02]); // 2 tracks (conductor + 1 channel)
    assert_eq!(&midi_bytes[12..14], &[0x00, 0x18]); // PPQN = 24

    // Conductor MTrk
    assert_eq!(&midi_bytes[14..18], b"MTrk");
    let cond_len = u32::from_be_bytes([midi_bytes[18], midi_bytes[19], midi_bytes[20], midi_bytes[21]]) as usize;
    let track1_start = 22 + cond_len;
    assert_eq!(&midi_bytes[track1_start..track1_start + 4], b"MTrk");
}

#[test]
fn test_m4a_sound_engine_scan_synthetic() {
    let mut rom = vec![0u8; 0x10000];

    // Setup DirectSound WaveData at 0x1000
    let wav_off = 0x1000;
    rom[wav_off..wav_off + 16].copy_from_slice(&[
        0x00, 0x00,       // type 0
        0x00, 0x40,       // looped (0x4000)
        0x00, 0x40, 0x00, 0x00, // freq
        0x00, 0x00, 0x00, 0x00, // loop_start
        0x10, 0x00, 0x00, 0x00, // size 16
    ]);
    for b in 0..16 {
        rom[wav_off + 16 + b] = (b * 8) as u8;
    }

    // Setup Voicegroup at 0x2000
    let vg_off = 0x2000;
    let vg_ptr = 0x0800_2000;
    let ve = encode_voice(0x00, 60, 0, 0, 0x0800_1000, [255, 0, 255, 0]);
    rom[vg_off..vg_off + 12].copy_from_slice(&ve);

    // Setup 10 SongHeaders and tracks starting at 0x3000
    let song_table_off = 0x5000;
    let song_table_ptr: u32 = 0x0800_5000;

    for s in 0..10 {
        let tr_off = 0x4000 + s * 32;
        let tr_ptr = 0x0800_4000 + (s as u32) * 32;
        rom[tr_off..tr_off + 6].copy_from_slice(&[0xBB, 60, 0xE7, 60, 100, 0xB1]);

        let hdr_off = 0x3000 + s * 16;
        let hdr_ptr = 0x0800_3000 + (s as u32) * 16;
        rom[hdr_off..hdr_off + 12].copy_from_slice(&[
            1, 0, 10, 0, // tracks=1
            0x00, 0x20, 0x00, 0x08, // vg_ptr = 0x0800_2000
            0x00, 0x00, 0x00, 0x00,
        ]);
        rom[hdr_off + 8..hdr_off + 12].copy_from_slice(&tr_ptr.to_le_bytes());

        // Song table entry
        let entry_off = song_table_off + s * 8;
        rom[entry_off..entry_off + 4].copy_from_slice(&hdr_ptr.to_le_bytes());
        rom[entry_off + 4..entry_off + 6].copy_from_slice(&(s as u16).to_le_bytes());
        rom[entry_off + 6..entry_off + 8].copy_from_slice(&(s as u16).to_le_bytes());
    }

    // Embed Thumb m4aSongNumStart signature at 0x800
    let sig_off = 0x800;
    rom[sig_off..sig_off + 12].copy_from_slice(&[
        0x00, 0x04, // LSL r0, r0, #16 (0x0400)
        0x07, 0x4a, // LDR r2, [pc, #28]
        0x08, 0x49, // LDR r1, [pc, #32]
        0x40, 0x0b, // LSR r0, r0, #13
        0x40, 0x18, // ADD r0, r0, r1
        0x83, 0x88, // LDRH r3, [r0, #4]
    ]);
    // Literal pool at sig_off + 40 (0x828) points to song table
    rom[0x828..0x82C].copy_from_slice(&song_table_ptr.to_le_bytes());

    let scan = scan_m4a_engine(&rom).expect("Expected successful M4A engine scan");
    assert_eq!(scan.song_table_offset, song_table_off);
    assert_eq!(scan.song_table_address, song_table_ptr);
    assert_eq!(scan.song_count, 10);
    assert_eq!(scan.playable_song_count, 10);
    assert_eq!(scan.max_tracks, 1);
    assert_eq!(scan.voicegroup_count, 1);
    assert_eq!(scan.directsound_sample_count, 1);

    // Test sample collection
    let samples = extract_voicegroup_samples_wav16(&rom, vg_ptr).unwrap();
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].0, 0x0800_1000);
    assert_eq!(&samples[0].2[0..4], b"RIFF");
}

#[test]
fn test_adsr_conversions() {
    // Attack
    assert_eq!(m4a_attack_to_timecents(255), -12000);
    assert_eq!(m4a_attack_to_timecents(250), -12000);
    assert!(m4a_attack_to_timecents(0) > -12000);
    assert!(m4a_attack_to_timecents(100) < m4a_attack_to_timecents(0));

    // Decay
    assert_eq!(m4a_decay_to_timecents(0), 8000);
    assert!(m4a_decay_to_timecents(255) < 0);

    // Sustain
    assert_eq!(m4a_sustain_to_centibels(255), 0);
    assert_eq!(m4a_sustain_to_centibels(0), 1000);
    assert_eq!(m4a_sustain_to_centibels(128), 60);

    // Release
    assert_eq!(m4a_release_to_timecents(255), -12000);
    assert_eq!(m4a_release_to_timecents(0), -3000);
    assert!(m4a_release_to_timecents(100) < 5000);
}

#[test]
fn test_sf2_chunk_structures_and_terminal_records() {
    let pcm = vec![10i8, 20, 30, -10, -20, -30];
    let mut smpl_bytes = Vec::new();
    for &s in &pcm {
        smpl_bytes.extend_from_slice(&((s as i16) << 8).to_le_bytes());
    }
    for _ in 0..SAMPLE_TERMINAL_PADDING {
        smpl_bytes.extend_from_slice(&0i16.to_le_bytes());
    }

    let samples = vec![SampleRecord {
        name: encode_fixed_string_20("TestSample"),
        dw_start: 0,
        dw_end: pcm.len() as u32,
        dw_startloop: 0,
        dw_endloop: pcm.len() as u32,
        dw_sample_rate: 13379,
        by_original_pitch: 60,
        ch_pitch_correction: 0,
        w_sample_link: 0,
        sf_sample_type: 1,
    }];

    let instruments = vec![InstrumentBuilder {
        name: "TestInst".to_string(),
        zones: vec![InstrumentZoneBuilder {
            key_range: (0, 127),
            vel_range: (0, 127),
            root_key: 60,
            sample_modes: 1,
            pan: None,
            initial_attenuation: 0,
            attack_tc: -12000,
            hold_tc: -12000,
            decay_tc: 0,
            sustain_cb: 0,
            release_tc: -3000,
            scale_tuning: None,
            sample_id: 0,
        }],
    }];

    let presets = vec![PresetBuilder {
        bank: 0,
        preset: 1,
        name: "TestPreset".to_string(),
        zones: vec![PresetZoneBuilder {
            key_range: (0, 127),
            vel_range: (0, 127),
            instrument_id: 0,
        }],
    }];

    let sf2 = assemble_sf2_binary(
        "Unit Test Soundbank",
        SF2_SOUND_ENGINE,
        &smpl_bytes,
        &samples,
        &instruments,
        &presets,
    )
    .expect("assemble SF2");

    // Validate standard RIFF and SF2 chunks
    assert_eq!(&sf2[0..4], b"RIFF");
    assert_eq!(&sf2[8..12], b"sfbk");
    let val = validate_soundfont(&sf2).expect("validate assembled SF2");
    assert_eq!(val.major_version, 2);
    assert_eq!(val.minor_version, 4);
    assert_eq!(val.sound_engine, SF2_SOUND_ENGINE);
    assert_eq!(val.bank_name, "Unit Test Soundbank");
    assert_eq!(val.sample_count, 1);
    assert_eq!(val.preset_count, 1);
    assert_eq!(val.instrument_count, 1);
    assert_eq!(val.pbag_count, 1);
    assert_eq!(val.ibag_count, 1);
}

#[test]
fn test_sf2_compilation_from_synthetic_m4a_rom() {
    let mut rom = vec![0u8; 0x20000];

    // ROM title at 0xA0
    rom[0xA0..0xA0 + 10].copy_from_slice(b"SYNTHETICS");

    // 1. DirectSound Sample 1 at 0x1000 (looped)
    let s1_off = 0x1000;
    let s1_ptr = 0x0800_1000u32;
    rom[s1_off..s1_off + 16].copy_from_slice(&[
        0x00, 0x00, // type 0
        0x00, 0x40, // looped (0x4000)
        0x00, 0x40, 0x00, 0x00, // freq
        0x02, 0x00, 0x00, 0x00, // loop_start = 2
        0x10, 0x00, 0x00, 0x00, // size = 16
    ]);
    for i in 0..16 {
        rom[s1_off + 16 + i] = (i * 8) as u8;
    }

    // 2. DirectSound Sample 2 at 0x1200 (unlooped drum hit)
    let s2_off = 0x1200;
    let s2_ptr = 0x0800_1200u32;
    rom[s2_off..s2_off + 16].copy_from_slice(&[
        0x00, 0x00, // type 0
        0x00, 0x00, // unlooped
        0x00, 0x40, 0x00, 0x00, // freq
        0x00, 0x00, 0x00, 0x00, // loop_start
        0x20, 0x00, 0x00, 0x00, // size = 32
    ]);
    for i in 0..32 {
        rom[s2_off + 16 + i] = (i * 4) as u8;
    }

    // 3. Child voice table for Keysplit at 0x1800
    let child_vg_off = 0x1800;
    let child_vg_ptr = 0x0800_1800u32;
    let cv0 = encode_voice(0x00, 48, 0, 0, s1_ptr, [255, 0, 255, 200]);
    let cv1 = encode_voice(0x00, 72, 0, 0, s2_ptr, [255, 10, 200, 150]);
    rom[child_vg_off..child_vg_off + 12].copy_from_slice(&cv0);
    rom[child_vg_off + 12..child_vg_off + 24].copy_from_slice(&cv1);

    // 4. Keysplit table at 0x1900 (keys 0..59 -> 0, keys 60..127 -> 1)
    let ktable_off = 0x1900;
    let ktable_ptr = 0x0800_1900u32;
    for k in 0..60 {
        rom[ktable_off + k] = 0;
    }
    for k in 60..128 {
        rom[ktable_off + k] = 1;
    }

    // 5. Child voice table for Drumkit at 0x1A00
    let drum_vg_off = 0x1A00;
    let drum_vg_ptr = 0x0800_1A00u32;
    // Note 36 (Bass drum) -> sample 2
    let dv36 = encode_voice(0x08, 60, 0, 0xC0, s2_ptr, [255, 0, 255, 100]);
    rom[drum_vg_off + 36 * 12..drum_vg_off + 37 * 12].copy_from_slice(&dv36);

    // 6. Master VoiceGroup at 0x2000
    let vg_off = 0x2000;
    let _vg_ptr = 0x0800_2000u32;
    // Program 0: Standard DirectSound (Sample 1)
    let v0 = encode_voice(0x00, 60, 0, 0x80, s1_ptr, [255, 0, 255, 192]);
    rom[vg_off..vg_off + 12].copy_from_slice(&v0);
    // Program 1: Keysplit (0x40) pointing to child_vg_ptr and ktable_ptr
    let v1 = encode_voice(0x40, 0, 0, 0, child_vg_ptr, ktable_ptr.to_le_bytes());
    rom[vg_off + 12..vg_off + 24].copy_from_slice(&v1);
    // Program 127: Drumkit (0x80) pointing to drum_vg_ptr
    let v127 = encode_voice(0x80, 0, 0, 0, drum_vg_ptr, [0, 0, 0, 0]);
    rom[vg_off + 127 * 12..vg_off + 128 * 12].copy_from_slice(&v127);

    // 7. Song header at 0x3000
    let hdr_off = 0x3000;
    let hdr_ptr = 0x0800_3000u32;
    let trk_off = 0x4000;
    let trk_ptr = 0x0800_4000u32;
    rom[trk_off..trk_off + 4].copy_from_slice(&[0xBB, 60, 0xE7, 0xB1]);

    rom[hdr_off..hdr_off + 12].copy_from_slice(&[
        1, 0, 10, 0, // tracks=1
        0x00, 0x20, 0x00, 0x08, // vg_ptr = 0x0800_2000
        0x00, 0x00, 0x00, 0x00,
    ]);
    rom[hdr_off + 8..hdr_off + 12].copy_from_slice(&trk_ptr.to_le_bytes());

    // 8. Song table at 0x5000
    let song_table_off = 0x5000;
    let song_table_ptr = 0x0800_5000u32;
    rom[song_table_off..song_table_off + 4].copy_from_slice(&hdr_ptr.to_le_bytes());
    rom[song_table_off + 4..song_table_off + 6].copy_from_slice(&0u16.to_le_bytes());
    rom[song_table_off + 6..song_table_off + 8].copy_from_slice(&0u16.to_le_bytes());

    // 9. Embed Thumb m4aSongNumStart signature at 0x800
    let sig_off = 0x800;
    rom[sig_off..sig_off + 12].copy_from_slice(&[
        0x00, 0x04, 0x07, 0x4a, 0x08, 0x49, 0x40, 0x0b, 0x40, 0x18, 0x83, 0x88,
    ]);
    rom[0x828..0x82C].copy_from_slice(&song_table_ptr.to_le_bytes());

    // Compile SoundFont from synthetic ROM
    let export = compile_rom_soundfont(&rom, None).expect("compile SoundFont from ROM");
    assert!(!export.sf2_bytes.is_empty());
    assert_eq!(export.report.title, "SYNTHETICS");
    assert_eq!(export.report.engine, SF2_SOUND_ENGINE);
    assert_eq!(export.report.version, "2.04");
    assert_eq!(export.report.total_samples, 2);
    assert!(export.report.total_presets >= 3); // Melodic Preset 0, Keysplit Preset 1, Drumkit Preset on Bank 128

    // Validate generated SF2 binary
    let val = validate_soundfont(&export.sf2_bytes).expect("validate generated SF2");
    assert_eq!(val.major_version, 2);
    assert_eq!(val.minor_version, 4);
    assert_eq!(val.sample_count, 2);
    assert!(val.preset_count >= 3);
    assert!(val.instrument_count >= 3);
    assert!(val.pbag_count >= 3);
    assert!(val.ibag_count >= 4); // Zone count includes multi-sample key splits
    assert!(val.smpl_byte_len > 0);
}

#[test]
fn test_multi_engine_detection() {
    let mut rom_gax = vec![0u8; 1024];
    rom_gax[100..120].copy_from_slice(b"GAX 3.05 SoundEngine");
    assert_eq!(
        detect_sound_engine(&rom_gax),
        DetectedSoundEngine::Gax(Some(gax::version::GaxVersion::V3))
    );

    let mut rom_musyx = vec![0u8; 1024];
    rom_musyx[48..60].copy_from_slice(b"MUSX project");
    assert_eq!(
        detect_sound_engine(&rom_musyx),
        DetectedSoundEngine::Musyx(Some(musyx::version::MusyXVersion::V2_0))
    );

    let mut rom_false_musx = vec![0u8; 1024];
    rom_false_musx[50..54].copy_from_slice(b"MUSX");
    assert_eq!(detect_sound_engine(&rom_false_musx), DetectedSoundEngine::Custom);

    let rom_custom = vec![0u8; 1024];
    assert_eq!(detect_sound_engine(&rom_custom), DetectedSoundEngine::Custom);
    assert!(detect_sound_engines(&rom_custom).is_empty());

    let mut rom_krawall = vec![0u8; 1024];
    rom_krawall[64..76].copy_from_slice(&[
        0x08, 0x50, 0x90, 0xE5, 0x14, 0x60, 0x90, 0xE5, 0xBC, 0x71, 0xD0, 0xE1,
    ]);
    rom_krawall[80..84].copy_from_slice(&0xE1A0_A824u32.to_le_bytes());
    assert_eq!(
        detect_sound_engine(&rom_krawall),
        DetectedSoundEngine::Krawall(Some(krawall::KrawallVersion::Standard))
    );

    // Dual-engine ROM (GAX + MusyX) must report both via detect_sound_engines()
    let mut rom_dual = vec![0u8; 1024];
    rom_dual[100..120].copy_from_slice(b"GAX 3.05 SoundEngine");
    rom_dual[240..252].copy_from_slice(b"MUSX project");
    let engines = detect_sound_engines(&rom_dual);
    assert_eq!(
        engines,
        vec![
            DetectedSoundEngine::Gax(Some(gax::version::GaxVersion::V3)),
            DetectedSoundEngine::Musyx(Some(musyx::version::MusyXVersion::V2_0)),
        ]
    );
}

#[test]
fn test_pokemon_gen3_dpcm_wavedata_extraction_and_wav16() {
    let mut buf = Vec::new();
    buf.extend_from_slice(&1u16.to_le_bytes()); // wave_type = 1 (Pokemon Gen 3 4-bit quadratic DPCM)
    buf.extend_from_slice(&0x0000u16.to_le_bytes()); // unlooped
    buf.extend_from_slice(&(13379u32 * 1024).to_le_bytes()); // 13,379 Hz
    buf.extend_from_slice(&0u32.to_le_bytes()); // loop_start
    buf.extend_from_slice(&64u32.to_le_bytes()); // 64 decoded samples (1 DPCM block = 33 bytes)

    // 1 DPCM block: initial=10, byte1 low nibble=1 (+1 -> 11), remaining nibbles=0 (hold 11)
    let mut block = [0u8; DPCM_BLOCK_BYTES];
    block[0] = 10u8;
    block[1] = 0x01;
    buf.extend_from_slice(&block);

    let (hdr, pcm) = extract_pcm_i8(&buf, 1024).expect("decode Pokemon Gen 3 DPCM");
    assert!(hdr.is_compressed_dpcm());
    assert_eq!(hdr.base_sample_rate_hz(), 13379);
    assert_eq!(pcm.len(), 64);
    assert_eq!(pcm[0], 10);
    assert_eq!(pcm[1], 11);
    assert_eq!(pcm[63], 11);

    let (_hdr2, wav) = export_wavedata_to_wav16(&buf, 1024).expect("export DPCM to WAV16");
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(wav.len(), 44 + 64 * 2);
}

#[test]
fn test_gax_soundpool_scanning_and_export() {
    let mut rom = vec![0u8; 0x10000];
    // Game Title at 0xA0
    rom[0xA0..0xAA].copy_from_slice(b"GAXTESTING");

    // Engine signature
    let sig = b"GAX Sound Engine 3.05 Shin'en";
    rom[0x500..0x500 + sig.len()].copy_from_slice(sig);

    // Create 5 contiguous sample audio payloads starting at 0x4000
    let s0_off = 0x4000usize;
    let s0_sz = 64usize;
    for (i, b) in rom[s0_off..s0_off + s0_sz].iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(3);
    }

    let s1_off = s0_off + s0_sz;
    let s1_sz = 128usize;
    for (i, b) in rom[s1_off..s1_off + s1_sz].iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(5);
    }

    let s2_off = s1_off + s1_sz;
    let s2_sz = 96usize;
    for (i, b) in rom[s2_off..s2_off + s2_sz].iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7);
    }

    let s3_off = s2_off + s2_sz;
    let s3_sz = 160usize;
    for (i, b) in rom[s3_off..s3_off + s3_sz].iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(9);
    }

    let s4_off = s3_off + s3_sz;
    let s4_sz = 80usize;
    for (i, b) in rom[s4_off..s4_off + s4_sz].iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(11);
    }

    // Pointer table at 0x2000
    let tbl_off = 0x2000usize;
    let sample_specs = [
        (s0_off, s0_sz),
        (s1_off, s1_sz),
        (s2_off, s2_sz),
        (s3_off, s3_sz),
        (s4_off, s4_sz),
    ];
    for (idx, &(off, sz)) in sample_specs.iter().enumerate() {
        let bus_ptr = (ROM_BUS_START as usize + off) as u32;
        let entry_off = tbl_off + idx * 8;
        rom[entry_off..entry_off + 4].copy_from_slice(&bus_ptr.to_le_bytes());
        rom[entry_off + 4..entry_off + 8].copy_from_slice(&(sz as u32).to_le_bytes());
    }

    // 1. Scan Soundpool
    let samples = scan_gax_soundpool(&rom);
    assert_eq!(samples.len(), 5);
    assert_eq!(samples[0].rom_offset, s0_off);
    assert_eq!(samples[0].size, s0_sz);
    assert_eq!(samples[4].rom_offset, s4_off);
    assert_eq!(samples[4].size, s4_sz);

    // 2. Export WAV files
    let wavs = export_gax_samples_wav16(&rom).expect("export GAX WAVs");
    assert_eq!(wavs.len(), 5);
    for (_idx, name, wav_data) in &wavs {
        assert!(name.ends_with(".wav"));
        assert!(&wav_data[0..4] == b"RIFF");
        assert!(&wav_data[8..12] == b"WAVE");
    }

    // 3. Compile SoundFont
    let export = compile_gax_soundfont(&rom, None).expect("compile GAX SF2");
    assert_eq!(export.report.total_samples, 5);
    assert_eq!(export.report.total_presets, 5);
    assert_eq!(export.report.total_instruments, 5);

    // 4. Validate SoundFont binary
    let val = validate_soundfont(&export.sf2_bytes).expect("validate GAX SF2");
    assert_eq!(val.major_version, 2);
    assert_eq!(val.minor_version, 4);
    assert_eq!(val.sample_count, 5);
    assert_eq!(val.preset_count, 5);

    // 5. Test unified compile_rom_soundfont
    let unified = compile_rom_soundfont(&rom, None).expect("compile unified SF2");
    assert_eq!(unified.report.engine, "Shin'en GAX");
    assert_eq!(unified.report.total_samples, 5);
}

#[test]
fn test_musyx_soundpool_scanning_and_export() {
    let mut rom = vec![0u8; 0x10000];
    let title = b"MUSYXTESTING";
    rom[0xA0..0xA0 + title.len()].copy_from_slice(title);
    rom[0x300..0x304].copy_from_slice(b"MUSX");

    // Create 4 sequential MusyX sample headers starting at 0x3000
    let mut off = 0x3000usize;
    let s0_off = off;
    let s0_len = 64u32;
    rom[off..off + 4].copy_from_slice(&s0_len.to_le_bytes());
    rom[off + 4..off + 8].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // unlooped
    rom[off + 8..off + 10].copy_from_slice(&11025u16.to_le_bytes());
    rom[off + 10..off + 12].copy_from_slice(&60u16.to_le_bytes());
    rom[off + 12..off + 16].copy_from_slice(&0u32.to_le_bytes()); // Pcm8
    for i in 0..s0_len as usize {
        rom[off + 16 + i] = (i as u8).wrapping_mul(4);
    }
    off += 16 + s0_len as usize;

    let s1_off = off;
    let s1_len = 128u32;
    rom[off..off + 4].copy_from_slice(&s1_len.to_le_bytes());
    rom[off + 4..off + 8].copy_from_slice(&32u32.to_le_bytes()); // loop_start = 32
    rom[off + 8..off + 10].copy_from_slice(&22050u16.to_le_bytes());
    rom[off + 10..off + 12].copy_from_slice(&62u16.to_le_bytes());
    rom[off + 12..off + 16].copy_from_slice(&0u32.to_le_bytes()); // Pcm8
    for i in 0..s1_len as usize {
        rom[off + 16 + i] = (i as u8).wrapping_mul(6);
    }
    off += 16 + s1_len as usize;

    let s2_off = off;
    let s2_len = 64u32;
    rom[off..off + 4].copy_from_slice(&s2_len.to_le_bytes());
    rom[off + 4..off + 8].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    rom[off + 8..off + 10].copy_from_slice(&16000u16.to_le_bytes());
    rom[off + 10..off + 12].copy_from_slice(&64u16.to_le_bytes());
    rom[off + 12..off + 16].copy_from_slice(&1u32.to_le_bytes()); // Pcm16
    for i in 0..s2_len as usize {
        rom[off + 16 + i] = (i as u8).wrapping_mul(8);
    }
    off += 16 + s2_len as usize;

    let s3_off = off;
    let s3_len = 32u32;
    rom[off..off + 4].copy_from_slice(&s3_len.to_le_bytes());
    rom[off + 4..off + 8].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    rom[off + 8..off + 10].copy_from_slice(&11025u16.to_le_bytes());
    rom[off + 10..off + 12].copy_from_slice(&65u16.to_le_bytes());
    rom[off + 12..off + 16].copy_from_slice(&2u32.to_le_bytes()); // Adpcm4
    for i in 0..s3_len as usize {
        rom[off + 16 + i] = (i as u8).wrapping_mul(10);
    }

    // 1. Scan MusyX soundpool
    let samples = scan_musyx_soundpool(&rom);
    assert_eq!(samples.len(), 4);
    assert_eq!(samples[0].rom_offset, s0_off);
    assert_eq!(samples[0].format, "Pcm8");
    assert_eq!(samples[0].sample_rate, 11025);
    assert_eq!(samples[1].rom_offset, s1_off);
    assert_eq!(samples[1].loop_point, Some(32));
    assert_eq!(samples[2].rom_offset, s2_off);
    assert_eq!(samples[2].format, "Pcm16");
    assert_eq!(samples[3].rom_offset, s3_off);
    assert_eq!(samples[3].format, "Adpcm4");

    // 2. Export WAV files
    let wavs = export_musyx_samples_wav16(&rom).expect("export MusyX WAVs");
    assert_eq!(wavs.len(), 4);
    for (_idx, name, wav_data) in &wavs {
        assert!(name.ends_with(".wav"));
        assert!(&wav_data[0..4] == b"RIFF");
        assert!(&wav_data[8..12] == b"WAVE");
    }

    // 3. Compile SoundFont
    let export = compile_musyx_soundfont(&rom, None).expect("compile MusyX SF2");
    assert_eq!(export.report.total_samples, 4);
    assert_eq!(export.report.total_presets, 4);
    assert_eq!(export.report.total_instruments, 4);

    // 4. Validate SoundFont binary
    let val = validate_soundfont(&export.sf2_bytes).expect("validate MusyX SF2");
    assert_eq!(val.major_version, 2);
    assert_eq!(val.minor_version, 4);
    assert_eq!(val.sample_count, 4);
    assert_eq!(val.preset_count, 4);

    // 5. Test unified compile_rom_soundfont
    let unified = compile_rom_soundfont(&rom, None).expect("compile unified SF2");
    assert_eq!(unified.report.engine, "Factor 5 MusyX");
    assert_eq!(unified.report.total_samples, 4);
}
