use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("gbakit_cli_{tag}_{pid}_{id}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn gbakit_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gbakit")
}

#[test]
fn cli_ereader_vpk_roundtrip_methods_0_and_1() {
    let dir = TempDir::new("vpk");
    let src = dir.path.join("input.bin");
    let payload: Vec<u8> = b"Nintendo GBA e-Reader vpk0 compression roundtrip test! "
        .iter()
        .copied()
        .cycle()
        .take(1024)
        .collect();
    fs::write(&src, &payload).unwrap();

    for method in ["0", "1"] {
        let vpk = dir.path.join(format!("out_m{method}.vpk"));
        let dec = dir.path.join(format!("dec_m{method}.bin"));

        let st = Command::new(gbakit_bin())
            .args(["ereader", "vpk-encode"])
            .arg(&src)
            .args(["-o"])
            .arg(&vpk)
            .args(["--method", method])
            .status()
            .unwrap();
        assert!(st.success(), "vpk-encode method {method} failed");

        let vpk_bytes = fs::read(&vpk).unwrap();
        assert_eq!(&vpk_bytes[0..4], b"vpk0");
        assert_eq!(vpk_bytes[8], method.parse::<u8>().unwrap());

        let st = Command::new(gbakit_bin())
            .args(["ereader", "vpk-decode"])
            .arg(&vpk)
            .args(["-o"])
            .arg(&dec)
            .status()
            .unwrap();
        assert!(st.success(), "vpk-decode method {method} failed");
        assert_eq!(fs::read(&dec).unwrap(), payload);
    }
}

#[test]
fn cli_ereader_encode_decode_repair_and_render() {
    let dir = TempDir::new("ereader");
    let plain = b"Hello from GBAKit e-Reader CLI roundtrip test!";
    let vpk_payload = gba_ereader::vpk0_compress(plain).unwrap();
    let bin_bytes = gba_ereader::build_bin(1, 0, 0, 1, &vpk_payload).unwrap();

    let in_bin = dir.path.join("card.bin");
    let out_raw = dir.path.join("card.raw");
    let out_bin = dir.path.join("repaired.bin");
    let out_plain = dir.path.join("decompressed.bin");
    let out_bmp = dir.path.join("strip.bmp");
    let out_pbm = dir.path.join("strip.pbm");
    let out_pgm = dir.path.join("strip.pgm");
    fs::write(&in_bin, &bin_bytes).unwrap();

    // 1. Encode short strip
    let st = Command::new(gbakit_bin())
        .args(["ereader", "encode"])
        .arg(&in_bin)
        .args(["-o"])
        .arg(&out_raw)
        .arg("--short")
        .status()
        .unwrap();
    assert!(st.success(), "ereader encode --short failed");
    let mut raw = fs::read(&out_raw).unwrap();
    assert_eq!(raw.len(), gba_ereader::SHORT_STRIP_RAW_LEN);

    // Inject 4 byte errors and verify RS(64,48) repair during decode
    raw[0] ^= 0xFF;
    raw[104 + 10] ^= 0xAA;
    raw[208 + 20] ^= 0x55;
    raw[312 + 30] ^= 0x77;
    fs::write(&out_raw, &raw).unwrap();

    // 2. Decode raw -> .bin
    let st = Command::new(gbakit_bin())
        .args(["ereader", "decode"])
        .arg(&out_raw)
        .args(["-o"])
        .arg(&out_bin)
        .status()
        .unwrap();
    assert!(st.success(), "ereader decode failed");
    assert_eq!(fs::read(&out_bin).unwrap(), bin_bytes);

    // 3. Decode raw + --decompress -> original plain payload
    let st = Command::new(gbakit_bin())
        .args(["ereader", "decode"])
        .arg(&out_raw)
        .args(["-o"])
        .arg(&out_plain)
        .arg("--decompress")
        .status()
        .unwrap();
    assert!(st.success(), "ereader decode --decompress failed");
    assert_eq!(fs::read(&out_plain).unwrap(), plain);

    // 4. Render to .bmp, .pbm, .pgm
    for (out, magic, dpi) in [
        (&out_bmp, &b"BM"[..], "300"),
        (&out_pbm, &b"P4\n"[..], "600"),
        (&out_pgm, &b"P5\n"[..], "300"),
    ] {
        let st = Command::new(gbakit_bin())
            .args(["ereader", "render"])
            .arg(&out_raw)
            .args(["-o"])
            .arg(out)
            .args(["--dpi", dpi])
            .status()
            .unwrap();
        assert!(st.success(), "ereader render {} failed", out.display());
        let data = fs::read(out).unwrap();
        assert!(data.starts_with(magic), "bad header for {}", out.display());
    }

    // 5. Encode --long
    let long_raw = dir.path.join("card_long.raw");
    let st = Command::new(gbakit_bin())
        .args(["ereader", "encode"])
        .arg(&in_bin)
        .args(["-o"])
        .arg(&long_raw)
        .arg("--long")
        .status()
        .unwrap();
    assert!(st.success(), "ereader encode --long failed");
    assert_eq!(
        fs::read(&long_raw).unwrap().len(),
        gba_ereader::LONG_STRIP_RAW_LEN
    );
}

/// Build a minimal structurally valid Mobiclip GBA v1 stream (64x48, 2 I-frames, 1 mono ADPCM frame).
fn build_synthetic_mobiclip_v1_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x400];
    // Embed v1 ARM signatures so MobiclipVersion::detect_from_bytes returns GbaV1
    rom[0x10..0x18].copy_from_slice(&mobiclip::version::SIG_ADPCM2);
    rom[0x20..0x28].copy_from_slice(&mobiclip::version::SIG_BITREF);

    // Stream at offset 0x100
    let mut s = Vec::new();
    let width: u32 = 64;
    let height: u32 = 48;
    let fps: f32 = 15.0;
    let nvf: u32 = 2;
    let naf: u32 = 1;
    let audio_ch: u32 = 1;
    let audio_rate: u32 = 16000;
    let audio_codec: u32 = 2;

    s.extend_from_slice(&width.to_le_bytes());
    s.extend_from_slice(&height.to_le_bytes());
    s.extend_from_slice(&fps.to_bits().to_le_bytes());
    s.extend_from_slice(&nvf.to_le_bytes());
    s.extend_from_slice(&naf.to_le_bytes());
    s.extend_from_slice(&audio_ch.to_le_bytes());
    s.extend_from_slice(&audio_rate.to_le_bytes());
    s.extend_from_slice(&audio_codec.to_le_bytes());

    // Video type table: 2 I-frames (type 0)
    s.extend_from_slice(&nvf.to_le_bytes());
    s.extend_from_slice(&[0u8, 0u8]);

    // Each 64x48 I-frame is (64/16)*(48/16)*8 = 4*3*8 = 96 bytes
    let iframe_sz: u16 = 96;
    let v_size_table: u32 = 2 + 2 * nvf;
    let v_data_size: u32 = (iframe_sz as u32) * nvf;
    s.extend_from_slice(&v_size_table.to_le_bytes());
    s.extend_from_slice(&v_data_size.to_le_bytes());
    s.extend_from_slice(&iframe_sz.to_le_bytes()); // max
    s.extend_from_slice(&iframe_sz.to_le_bytes());
    s.extend_from_slice(&iframe_sz.to_le_bytes());
    s.extend(std::iter::repeat_n(0x1Fu8, v_data_size as usize));

    // Audio type table
    s.extend_from_slice(&0u32.to_le_bytes());
    // Audio frame: 4-byte num_pcm16_bytes (= 64 -> 32 samples -> 2 words = 8 bytes) + 4-byte ADPCM header + 8-byte words = 16 bytes
    let a_frame_sz: u16 = 16;
    let a_size_table: u32 = 2 + 2 * naf;
    let a_data_size: u32 = a_frame_sz as u32;
    s.extend_from_slice(&a_size_table.to_le_bytes());
    s.extend_from_slice(&a_data_size.to_le_bytes());
    s.extend_from_slice(&a_frame_sz.to_le_bytes()); // max
    s.extend_from_slice(&a_frame_sz.to_le_bytes());
    s.extend_from_slice(&64u32.to_le_bytes()); // num_pcm16_bytes
    s.extend_from_slice(&[0u8; 4]); // pred=0, step_idx=0
    s.extend_from_slice(&[0x55u8; 8]); // 2 words

    rom.resize(0x100 + s.len(), 0);
    rom[0x100..0x100 + s.len()].copy_from_slice(&s);
    rom
}

#[test]
fn cli_video_scan_and_export_synthetic_v1() {
    let dir = TempDir::new("video");
    let rom_path = dir.path.join("mobiclip_v1.gba");
    let out_dir = dir.path.join("export");
    fs::write(&rom_path, build_synthetic_mobiclip_v1_rom()).unwrap();

    // 1. video scan --json
    let out = Command::new(gbakit_bin())
        .args(["video", "scan"])
        .arg(&rom_path)
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success(), "video scan failed");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["detected"], true);
    assert_eq!(json["version"], "GbaV1");
    assert_eq!(json["v1_stream_count"], 1);
    assert_eq!(json["v1_streams"][0]["width"], 64);
    assert_eq!(json["v1_streams"][0]["height"], 48);
    assert_eq!(json["v1_streams"][0]["audio_sample_rate"], 16000);

    // 2. video export --max-frames 2
    let st = Command::new(gbakit_bin())
        .args(["video", "export"])
        .arg(&rom_path)
        .args(["-o"])
        .arg(&out_dir)
        .args(["--max-frames", "2"])
        .status()
        .unwrap();
    assert!(st.success(), "video export failed");

    let wav = fs::read(out_dir.join("stream_v1_00_audio.wav")).unwrap();
    assert_eq!(&wav[0..4], b"RIFF");
    let ppm0 = fs::read(out_dir.join("stream_v1_00_frame_0000.ppm")).unwrap();
    assert!(ppm0.starts_with(b"P6\n64 48\n255\n"));
    let ppm1 = fs::read(out_dir.join("stream_v1_00_frame_0001.ppm")).unwrap();
    assert!(ppm1.starts_with(b"P6\n64 48\n255\n"));
}

#[test]
fn cli_audio_scan_ead_sma_and_engine_sw() {
    let dir = TempDir::new("audio_new");

    // Synthetic EAD SMA4 ROM
    let mut sma4 = vec![0u8; 2048];
    const B_CONV: [u8; 24] = [
        0x30, 0x00, 0x2D, 0xE9, 0x7F, 0x40, 0xA0, 0xE3, 0xFF, 0x5C, 0xA0, 0xE3, 0x80, 0x50,
        0x85, 0xE3, 0xF2, 0x30, 0xD0, 0xE0, 0x00, 0x00, 0x53, 0xE3,
    ];
    const B_MIX: [u8; 24] = [
        0x0D, 0xC0, 0xA0, 0xE1, 0xF0, 0x01, 0x2D, 0xE9, 0xF0, 0x00, 0x9C, 0xE8, 0x01, 0xC0,
        0x43, 0xE0, 0x07, 0xC0, 0x0C, 0xE2, 0x00, 0x00, 0x5C, 0xE3,
    ];
    const B_COMB: [u8; 24] = [
        0x30, 0x00, 0x2D, 0xE9, 0x01, 0x40, 0xA0, 0xE3, 0x14, 0x43, 0xA0, 0xE1, 0x01, 0x40,
        0x44, 0xE2, 0xF0, 0x50, 0xD0, 0xE1, 0xF0, 0xC0, 0xD1, 0xE1,
    ];
    sma4[0x40..0x40 + 24].copy_from_slice(&B_CONV);
    sma4[0x40 + 0x120..0x40 + 0x120 + 24].copy_from_slice(&B_MIX);
    sma4[0x40 + 0x218..0x40 + 0x218 + 24].copy_from_slice(&B_COMB);

    let sma4_path = dir.path.join("sma4.gba");
    fs::write(&sma4_path, &sma4).unwrap();
    let out = Command::new(gbakit_bin())
        .args(["audio", "scan"])
        .arg(&sma4_path)
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success(), "audio scan sma4 failed");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["detected"], true);
    assert_eq!(json["engine"], "ead_sma");
    assert_eq!(json["ead_sma_version"], "Sma4Mla");

    // Synthetic Engine Software EIRE v2.0 ROM
    let mut eire = vec![0u8; 256];
    const STEREO_SIG: [u8; 16] = [
        0xf6, 0x4f, 0x2d, 0xe9, 0x3c, 0x10, 0x9f, 0xe5, 0x3c, 0x00, 0x91, 0xe8, 0x00, 0x60,
        0x93, 0xe0,
    ];
    eire[0x20..0x30].copy_from_slice(&STEREO_SIG);
    let eire_path = dir.path.join("eire.gba");
    fs::write(&eire_path, &eire).unwrap();
    let out = Command::new(gbakit_bin())
        .args(["audio", "scan"])
        .arg(&eire_path)
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success(), "audio scan eire failed");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["detected"], true);
    assert_eq!(json["engine"], "engine_sw");
    assert_eq!(json["engine_sw_version"], "V20Standard");
}
