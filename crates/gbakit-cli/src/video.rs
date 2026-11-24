//! CLI subcommands for ActImagine Mobiclip GBA v1/v2 & Game Boy Advance Video ROMs.

use std::fs;
use std::path::Path;

use mobiclip::{
    bgr555_to_rgb888, decode_frame, i8_to_i16_wav, scan_mobiclip_streams, scan_vxgb_streams,
    AudioDecoder, MobiclipStreamView, MobiclipVersion, SfcdArchive, VxgbStreamView,
};

fn version_name(v: MobiclipVersion) -> &'static str {
    match v {
        MobiclipVersion::GbaV1 => "GbaV1",
        MobiclipVersion::GbaV1Video => "GbaV1Video",
        MobiclipVersion::GbaV2 => "GbaV2",
        MobiclipVersion::GbaVideoMajescoV1 => "GbaVideoMajescoV1",
        MobiclipVersion::GbaVideoMajescoV2 => "GbaVideoMajescoV2",
        _ => "Unknown",
    }
}

fn encode_wav16(sample_rate: u32, channels: u16, pcm_i8: &[i8]) -> Result<Vec<u8>, String> {
    let rate = sample_rate.max(1);
    let data_bytes = (pcm_i8.len() as u64)
        .checked_mul(2)
        .ok_or_else(|| "WAV data size overflow".to_string())?;
    let header = formatkit_core::wav_header_pcm(rate, channels, 16, data_bytes)
        .map_err(|e| format!("WAV header: {e}"))?;
    let mut out = Vec::with_capacity(44 + pcm_i8.len() * 2);
    out.extend_from_slice(&header);
    for &s in pcm_i8 {
        out.extend_from_slice(&i8_to_i16_wav(s).to_le_bytes());
    }
    Ok(out)
}

/// `gbakit video scan <rom.gba> [--json]`
pub fn cmd_video_scan(rom_path: &Path, as_json: bool) -> Result<(), String> {
    let bytes = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let version = MobiclipVersion::detect_from_bytes(&bytes);
    let v1_streams = scan_mobiclip_streams(&bytes);
    let v2_streams = scan_vxgb_streams(&bytes);

    let sfcd_info = SfcdArchive::find_in_rom(&bytes).and_then(|arch| {
        let off = arch.archive_offset();
        SfcdArchive::parse_at(&bytes, off).ok()
    });

    let detected = version.is_some()
        || !v1_streams.is_empty()
        || !v2_streams.is_empty()
        || sfcd_info.is_some();

    if as_json {
        let v1_json: Vec<serde_json::Value> = v1_streams
            .iter()
            .map(|s| {
                serde_json::json!({
                    "offset": format!("0x{:X}", s.offset),
                    "width": s.header.width,
                    "height": s.header.height,
                    "fps": s.header.fps(),
                    "num_video_frames": s.header.num_video_frames,
                    "num_audio_frames": s.header.num_audio_frames,
                    "audio_channels": s.header.audio_channels,
                    "audio_sample_rate": s.header.audio_sample_rate,
                    "audio_codec_id": s.header.audio_codec_id,
                    "total_size": s.total_size,
                })
            })
            .collect();

        let v2_json: Vec<serde_json::Value> = v2_streams
            .iter()
            .map(|s| {
                let magic = String::from_utf8_lossy(&s.header.magic).to_string();
                let chapters = VxgbStreamView::parse_at(&bytes, s.offset)
                    .ok()
                    .and_then(|v| v.chapter_count().ok());
                serde_json::json!({
                    "offset": format!("0x{:X}", s.offset),
                    "magic": magic,
                    "width": s.header.width,
                    "height": s.header.height,
                    "num_frames": s.header.num_frames,
                    "audio_sample_rate": s.header.audio_sample_rate,
                    "chapter_count": chapters,
                })
            })
            .collect();

        let sfcd_json = sfcd_info.map(|arch| {
            let entries: Vec<serde_json::Value> = arch
                .entries()
                .flatten()
                .map(|e| {
                    serde_json::json!({
                        "name": e.name,
                        "offset": format!("0x{:X}", e.offset),
                        "size": e.size,
                    })
                })
                .collect();
            serde_json::json!({
                "archive_offset": format!("0x{:X}", arch.archive_offset()),
                "file_count": arch.file_count(),
                "entries": entries,
            })
        });

        let payload = serde_json::json!({
            "detected": detected,
            "version": version.map(version_name),
            "v1_stream_count": v1_streams.len(),
            "v1_streams": v1_json,
            "v2_stream_count": v2_streams.len(),
            "v2_streams": v2_json,
            "sfcd": sfcd_json,
            "rom_path": rom_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        println!("Video Codec Scan: {}", rom_path.display());
        println!(
            "  Version:           {}",
            version.map(version_name).unwrap_or("None")
        );
        println!("  Mobiclip v1:       {} stream(s)", v1_streams.len());
        for (i, s) in v1_streams.iter().enumerate() {
            println!(
                "    [{i:02}] @ 0x{:X}  {}x{} @ {:.2} fps, {} video frames, audio: {} ch {} Hz (codec {}, {} frames), {} bytes",
                s.offset,
                s.header.width,
                s.header.height,
                s.header.fps(),
                s.header.num_video_frames,
                s.header.audio_channels,
                s.header.audio_sample_rate,
                s.header.audio_codec_id,
                s.header.num_audio_frames,
                s.total_size,
            );
        }
        println!("  Mobiclip v2 VXGB:  {} stream(s)", v2_streams.len());
        for (i, s) in v2_streams.iter().enumerate() {
            let magic = String::from_utf8_lossy(&s.header.magic);
            let chapters = VxgbStreamView::parse_at(&bytes, s.offset)
                .ok()
                .and_then(|v| v.chapter_count().ok())
                .unwrap_or(0);
            println!(
                "    [{i:02}] @ 0x{:X}  [{magic}] {}x{}, {} frames, FastAudio {} Hz, {} chapters",
                s.offset,
                s.header.width,
                s.header.height,
                s.header.num_frames,
                s.header.audio_sample_rate,
                chapters,
            );
        }
        if let Some(arch) = sfcd_info {
            println!(
                "  Majesco SFCD:      @ 0x{:X} ({} file(s))",
                arch.archive_offset(),
                arch.file_count()
            );
            for entry in arch.entries().flatten() {
                println!(
                    "    - {} @ 0x{:X} ({} bytes)",
                    entry.name, entry.offset, entry.size
                );
            }
        }
    }
    Ok(())
}

/// `gbakit video export <rom.gba> -o <out_dir> [--max-frames <n>]`
pub fn cmd_video_export(
    rom_path: &Path,
    out_dir: &Path,
    max_frames: Option<usize>,
) -> Result<(), String> {
    let bytes = fs::read(rom_path).map_err(|e| format!("read {}: {e}", rom_path.display()))?;
    let v1_streams = scan_mobiclip_streams(&bytes);
    let v2_streams = scan_vxgb_streams(&bytes);

    if v1_streams.is_empty() && v2_streams.is_empty() {
        return Err(format!(
            "no Mobiclip v1 or v2 (VXGB/VX++) streams found in {}",
            rom_path.display()
        ));
    }

    fs::create_dir_all(out_dir)
        .map_err(|e| format!("create output dir {}: {e}", out_dir.display()))?;

    let mut wav_exported = 0usize;
    let mut ppm_exported = 0usize;

    // 1. Export Mobiclip v1 streams (2-bit ADPCM / PCM16 audio + optional BGR555 video frames)
    for (idx, info) in v1_streams.iter().enumerate() {
        let view = MobiclipStreamView::parse_at(&bytes, info.offset)
            .map_err(|e| format!("parse v1 stream {idx} @ 0x{:X}: {e}", info.offset))?;

        if view.header.has_audio() {
            let dec = AudioDecoder::new(view.header.audio_channels, view.header.audio_codec_id);
            let mut all_pcm: Vec<i8> = Vec::new();
            let mut scratch: Vec<i8> = Vec::new();
            let audio_limit = max_frames.unwrap_or(usize::MAX);
            for (f_idx, frame) in view.audio_frames().enumerate() {
                if f_idx >= audio_limit {
                    break;
                }
                if frame.len() < 4 {
                    continue;
                }
                const MAX_MOBICLIP_FRAME_SAMPLES: usize = 256 * 1024;
                let num_pcm16_bytes =
                    u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
                let frame_samples = num_pcm16_bytes / 2;
                if frame_samples > MAX_MOBICLIP_FRAME_SAMPLES {
                    return Err(format!(
                        "v1 stream {idx} audio frame {f_idx} declares {frame_samples} samples, exceeding per-frame limit {MAX_MOBICLIP_FRAME_SAMPLES}"
                    ));
                }
                let max_out = frame_samples + 64;
                if scratch.len() < max_out {
                    scratch.resize(max_out, 0);
                }
                match dec.decode_frame(frame, &mut scratch) {
                    Ok(n) => all_pcm.extend_from_slice(&scratch[..n]),
                    Err(_) => break,
                }
            }
            if !all_pcm.is_empty() {
                let wav = encode_wav16(
                    view.header.audio_sample_rate,
                    view.header.audio_channels as u16,
                    &all_pcm,
                )?;
                let wav_path = out_dir.join(format!("stream_v1_{idx:02}_audio.wav"));
                fs::write(&wav_path, &wav)
                    .map_err(|e| format!("write {}: {e}", wav_path.display()))?;
                wav_exported += 1;
            }
        }

        // Export BGR555 video frames as PPM if --max-frames > 0
        if let Some(frame_cap) = max_frames {
            if frame_cap > 0 {
                let w = view.header.width;
                let h = view.header.height;
                let pixel_count = (w as usize) * (h as usize);
                let mut prev = vec![0u16; pixel_count];
                let mut cur = vec![0u16; pixel_count];
                let mut rgb = vec![0u8; pixel_count * 3];
                let ppm_hdr = format!("P6\n{w} {h}\n255\n");

                for (f_idx, frame) in view.video_frames().enumerate() {
                    if f_idx >= frame_cap {
                        break;
                    }
                    let ftype = view
                        .video_frame_type(f_idx as u32)
                        .map_err(|e| format!("frame type {f_idx}: {e}"))?;
                    if decode_frame(ftype, frame, &prev, &mut cur, w, h).is_err() {
                        break;
                    }
                    bgr555_to_rgb888(&cur, &mut rgb)
                        .map_err(|e| format!("bgr555->rgb888 frame {f_idx}: {e}"))?;

                    let mut ppm = Vec::with_capacity(ppm_hdr.len() + rgb.len());
                    ppm.extend_from_slice(ppm_hdr.as_bytes());
                    ppm.extend_from_slice(&rgb);
                    let ppm_path =
                        out_dir.join(format!("stream_v1_{idx:02}_frame_{f_idx:04}.ppm"));
                    fs::write(&ppm_path, &ppm)
                        .map_err(|e| format!("write {}: {e}", ppm_path.display()))?;
                    ppm_exported += 1;
                    std::mem::swap(&mut prev, &mut cur);
                }
            }
        }
    }

    // 2. Export Mobiclip v2 VXGB/VX++ FastAudio tracks
    for (idx, info) in v2_streams.iter().enumerate() {
        let view = VxgbStreamView::parse_at(&bytes, info.offset)
            .map_err(|e| format!("parse v2 stream {idx} @ 0x{:X}: {e}", info.offset))?;
        let mut dec = view
            .fastaudio_decoder()
            .map_err(|e| format!("init FastAudio decoder for stream {idx}: {e}"))?;
        let packets = view.fastaudio_packets();
        let pkt_cap = max_frames.unwrap_or(packets.len() / 10);
        let pcm = dec
            .decode_stream(packets, pkt_cap)
            .map_err(|e| format!("decode FastAudio stream {idx}: {e}"))?;
        if !pcm.is_empty() {
            let wav = encode_wav16(view.header.audio_sample_rate, 1, &pcm)?;
            let wav_path = out_dir.join(format!("stream_v2_{idx:02}_audio.wav"));
            fs::write(&wav_path, &wav)
                .map_err(|e| format!("write {}: {e}", wav_path.display()))?;
            wav_exported += 1;
        }
    }

    println!("Video Export Summary:");
    println!("  Output Directory:   {}", out_dir.display());
    println!("  WAV Tracks Written: {wav_exported}");
    println!("  PPM Frames Written: {ppm_exported}");
    Ok(())
}
