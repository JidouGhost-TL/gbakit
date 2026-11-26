//! End-to-end CLI round-trip tests for `gbakit gfx replace`, `gbakit audio replace`, and
//! Cosmigo Sonix detection in `gbakit audio scan`.
//!
//! All ROMs are synthetic, generated in-test; no retail ROM data is used or committed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("gbakit_modding_{tag}_{pid}_{id}"));
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

fn run(args: &[&str]) -> Output {
    Command::new(gbakit_bin()).args(args).output().expect("spawn gbakit")
}

fn run_ok_json(args: &[&str]) -> serde_json::Value {
    let out = run(args);
    assert!(
        out.status.success(),
        "gbakit {args:?} failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("valid JSON stdout")
}

fn p(path: &Path) -> &str {
    path.to_str().expect("utf-8 temp path")
}

const ROM_SIZE: usize = 0x2_0000;
const FREE_START: usize = 0x1_0000;
const PTR_OFF: usize = 0x400;

/// Synthetic ROM: zero-filled first half, `0xFF` erased free space in the back half.
fn blank_rom() -> Vec<u8> {
    let mut rom = vec![0u8; ROM_SIZE];
    rom[FREE_START..].fill(0xFF);
    rom
}

fn read_u32(rom: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([rom[off], rom[off + 1], rom[off + 2], rom[off + 3]])
}

fn parse_hex(v: &serde_json::Value) -> usize {
    let s = v.as_str().expect("hex string");
    usize::from_str_radix(s.trim_start_matches("0x"), 16).expect("hex")
}

// -----------------------------------------------------------------------------
// gfx replace
// -----------------------------------------------------------------------------

fn make_ppm(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    out.extend_from_slice(rgb);
    out
}

/// Exact BGR555-representable palette (component values are multiples of 8).
const COLORS: [[u8; 3]; 15] = [
    [248, 0, 0],
    [0, 248, 0],
    [0, 0, 248],
    [248, 248, 0],
    [0, 248, 248],
    [248, 0, 248],
    [248, 248, 248],
    [128, 64, 32],
    [32, 64, 128],
    [64, 128, 32],
    [200, 120, 40],
    [40, 200, 120],
    [120, 40, 200],
    [96, 96, 96],
    [16, 16, 16],
];

fn image_rgb(width: u32, height: u32, ncolors: usize, noisy: bool) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    let mut state = 0x1234_5678u32;
    for y in 0..height as usize {
        for x in 0..width as usize {
            let idx = if noisy {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state as usize) % ncolors
            } else {
                ((x / 4) + (y / 4)) % ncolors
            };
            rgb.extend_from_slice(&COLORS[idx]);
        }
    }
    rgb
}

/// Decode GBA 4bpp linear tiles + BGR555 palette back to 5-bit-per-channel RGB pixels.
fn decode_tiles_4bpp_rgb5(tiles: &[u8], palette: &[u8], width: u32, height: u32) -> Vec<[u8; 3]> {
    let wt = (width / 8) as usize;
    let mut out = vec![[0u8; 3]; (width * height) as usize];
    for (t, tile) in tiles.chunks(32).enumerate() {
        let (tx, ty) = (t % wt, t / wt);
        for (i, &b) in tile.iter().enumerate() {
            for (k, idx) in [b & 0x0F, b >> 4].into_iter().enumerate() {
                let px = i * 2 + k;
                let (x, y) = (tx * 8 + px % 8, ty * 8 + px / 8);
                let c = u16::from_le_bytes([palette[idx as usize * 2], palette[idx as usize * 2 + 1]]);
                out[y * width as usize + x] =
                    [(c & 0x1F) as u8, ((c >> 5) & 0x1F) as u8, ((c >> 10) & 0x1F) as u8];
            }
        }
    }
    out
}

fn assert_image_roundtrip(rom: &[u8], tile_off: usize, pal_off: usize, rgb: &[u8], w: u32, h: u32) {
    let decoded = gbakit::compress::decode_lz77(&rom[tile_off..], 1 << 20).expect("re-decode LZ77");
    assert_eq!(decoded.bytes.len(), (w * h / 2) as usize, "decoded tile byte count");
    let pixels = decode_tiles_4bpp_rgb5(&decoded.bytes, &rom[pal_off..pal_off + 32], w, h);
    for (i, px) in pixels.iter().enumerate() {
        let src = [rgb[i * 3] >> 3, rgb[i * 3 + 1] >> 3, rgb[i * 3 + 2] >> 3];
        assert_eq!(*px, src, "pixel {i} mismatch after round-trip");
    }
}

#[test]
fn cli_gfx_replace_lz77_in_place_and_redecode() {
    let dir = TempDir::new("gfx_inplace");
    const TILE_OFF: usize = 0x1000;
    const PAL_OFF: usize = 0x800;

    // Original asset: an LZ77 stream of pseudo-random (poorly compressible) 16x16 4bpp tiles.
    let mut state = 0xDEAD_BEEFu32;
    let original_tiles: Vec<u8> = (0..128)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (state >> 16) as u8
        })
        .collect();
    let original = gbakit::gfx::compress_gfx(&original_tiles, gbakit::gfx::GfxCompression::Lz77Vram).unwrap();
    let mut rom = blank_rom();
    rom[TILE_OFF..TILE_OFF + original.len()].copy_from_slice(&original);
    rom[PTR_OFF..PTR_OFF + 4].copy_from_slice(&(0x0800_0000u32 + TILE_OFF as u32).to_le_bytes());

    let rom_path = dir.path.join("in.gba");
    let out_path = dir.path.join("out.gba");
    let img_path = dir.path.join("img.ppm");
    fs::write(&rom_path, &rom).unwrap();
    let rgb = image_rgb(16, 16, 4, false);
    fs::write(&img_path, make_ppm(16, 16, &rgb)).unwrap();

    let json = run_ok_json(&[
        "gfx", "replace", p(&rom_path), "--offset", "0x1000", "--image", p(&img_path), "-o",
        p(&out_path), "--palette-offset", "0x800", "--write-palette", "--json",
    ]);
    assert_eq!(json["compression"], "lz77-vram");
    assert_eq!(json["relocated"], false);
    assert_eq!(json["written_offset"], "0x1000");
    assert_eq!(json["original_capacity"].as_u64().unwrap() as usize, original.len());
    assert_eq!(json["tile_count"], 4);
    assert_eq!(json["palette_mode"], "generated_written");

    // Input ROM is never modified in place.
    assert_eq!(fs::read(&rom_path).unwrap(), rom);
    let out = fs::read(&out_path).unwrap();
    assert_eq!(out.len(), rom.len());
    assert_eq!(read_u32(&out, PTR_OFF), 0x0800_1000, "pointer untouched for in-place write");
    assert_image_roundtrip(&out, TILE_OFF, PAL_OFF, &rgb, 16, 16);

    // Second pass: match against the palette now present in the ROM (no --write-palette).
    let out2_path = dir.path.join("out2.gba");
    let json2 = run_ok_json(&[
        "gfx", "replace", p(&out_path), "--offset", "0x1000", "--image", p(&img_path), "-o",
        p(&out2_path), "--palette-offset", "0x800", "--json",
    ]);
    assert_eq!(json2["palette_mode"], "matched_existing");
    let out2 = fs::read(&out2_path).unwrap();
    assert_eq!(&out2[PAL_OFF..PAL_OFF + 32], &out[PAL_OFF..PAL_OFF + 32]);
    assert_image_roundtrip(&out2, TILE_OFF, PAL_OFF, &rgb, 16, 16);

    // Writing over the input path is refused.
    let refuse = run(&[
        "gfx", "replace", p(&rom_path), "--offset", "0x1000", "--image", p(&img_path), "-o",
        p(&rom_path),
    ]);
    assert!(!refuse.status.success());
    assert_eq!(fs::read(&rom_path).unwrap(), rom);
}

#[test]
fn cli_gfx_replace_lz77_relocation_patches_pointers() {
    let dir = TempDir::new("gfx_reloc");
    const TILE_OFF: usize = 0x1000;
    const PAL_OFF: usize = 0x800;

    // Original asset: a tiny, highly compressible LZ77 stream (256 zero bytes).
    let original = gbakit::gfx::compress_gfx(&[0u8; 256], gbakit::gfx::GfxCompression::Lz77Vram).unwrap();
    let mut rom = blank_rom();
    rom[TILE_OFF..TILE_OFF + original.len()].copy_from_slice(&original);
    rom[PTR_OFF..PTR_OFF + 4].copy_from_slice(&0x0800_1000u32.to_le_bytes());
    rom[PTR_OFF + 0x40..PTR_OFF + 0x44].copy_from_slice(&0x0800_1000u32.to_le_bytes());

    let rom_path = dir.path.join("in.gba");
    let img_path = dir.path.join("noisy.ppm");
    fs::write(&rom_path, &rom).unwrap();
    let rgb = image_rgb(32, 32, 15, true);
    fs::write(&img_path, make_ppm(32, 32, &rgb)).unwrap();

    // Without --allow-relocate the oversized stream is rejected and nothing is written.
    let blocked_path = dir.path.join("blocked.gba");
    let blocked = run(&[
        "gfx", "replace", p(&rom_path), "--offset", "0x1000", "--image", p(&img_path), "-o",
        p(&blocked_path), "--palette-offset", "0x800", "--write-palette",
    ]);
    assert!(!blocked.status.success(), "oversized replacement must fail without --allow-relocate");
    assert!(!blocked_path.exists());

    let out_path = dir.path.join("out.gba");
    let json = run_ok_json(&[
        "gfx", "replace", p(&rom_path), "--offset", "0x1000", "--image", p(&img_path), "-o",
        p(&out_path), "--palette-offset", "0x800", "--write-palette", "--compression", "lz77-vram",
        "--allow-relocate", "--json",
    ]);
    assert_eq!(json["relocated"], true);
    assert_eq!(json["patched_pointers"], 2);
    let new_off = parse_hex(&json["written_offset"]);
    assert!(new_off >= FREE_START, "relocated into 0xFF free space");
    assert_eq!(new_off % 4, 0);

    let out = fs::read(&out_path).unwrap();
    let new_bus = 0x0800_0000u32 + new_off as u32;
    assert_eq!(read_u32(&out, PTR_OFF), new_bus);
    assert_eq!(read_u32(&out, PTR_OFF + 0x40), new_bus);
    assert_eq!(fs::read(&rom_path).unwrap(), rom, "input unchanged");
    assert_image_roundtrip(&out, new_off, PAL_OFF, &rgb, 32, 32);
}

// -----------------------------------------------------------------------------
// audio replace
// -----------------------------------------------------------------------------

const WAVE_OFF: usize = 0x2000;
const RATE: u32 = 13_379;

fn sine_i16(len: usize, period: f64) -> Vec<i16> {
    (0..len)
        .map(|i| ((i as f64 * std::f64::consts::TAU / period).sin() * 20_000.0) as i16)
        .collect()
}

fn wav16(rate: u32, samples: &[i16]) -> Vec<u8> {
    let mut out = formatkit_core::wav_header_pcm(rate, 1, 16, (samples.len() * 2) as u64)
        .unwrap()
        .to_vec();
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

fn rom_with_wavedata(wavedata: &[u8]) -> Vec<u8> {
    let mut rom = blank_rom();
    rom[WAVE_OFF..WAVE_OFF + wavedata.len()].copy_from_slice(wavedata);
    rom[PTR_OFF..PTR_OFF + 4].copy_from_slice(&(0x0800_0000u32 + WAVE_OFF as u32).to_le_bytes());
    rom
}

/// Re-extract a sample via `gbakit sound wav` and return (json, signed 8-bit PCM).
fn sound_wav(dir: &Path, rom: &Path, offset: usize, tag: &str) -> (serde_json::Value, Vec<i8>) {
    let wav_path = dir.join(format!("{tag}.wav"));
    let off = format!("0x{offset:X}");
    let json = run_ok_json(&["sound", "wav", p(rom), "--offset", &off, "-o", p(&wav_path)]);
    let wav = fs::read(&wav_path).unwrap();
    assert_eq!(&wav[0..4], b"RIFF");
    let pcm: Vec<i8> = wav[44..].iter().map(|&b| (b as i16 - 128) as i8).collect();
    (json, pcm)
}

fn expected_pcm(samples: &[i16], dpcm: bool) -> Vec<i8> {
    let opts = if dpcm {
        gbakit::sound::M4aWaveBuildOptions::dpcm(RATE)
    } else {
        gbakit::sound::M4aWaveBuildOptions::pcm8(RATE)
    };
    let wd = gbakit::sound::build_m4a_wavedata_from_i16(samples, opts).unwrap();
    gbakit::sound::extract_pcm_i8(&wd, 1 << 20).unwrap().1
}

fn audio_roundtrip(codec: &str, dpcm: bool) {
    let dir = TempDir::new(&format!("audio_{codec}"));
    let original_opts = if dpcm {
        gbakit::sound::M4aWaveBuildOptions::dpcm(RATE)
    } else {
        gbakit::sound::M4aWaveBuildOptions::pcm8(RATE)
    };
    let original = gbakit::sound::build_m4a_wavedata_from_i16(&sine_i16(2000, 50.0), original_opts).unwrap();
    let rom = rom_with_wavedata(&original);
    let rom_path = dir.path.join("in.gba");
    fs::write(&rom_path, &rom).unwrap();

    // --- In-place: shorter sample, rate preserved from the original WaveData header. ---
    let short = sine_i16(1200, 33.0);
    let short_wav = dir.path.join("short.wav");
    fs::write(&short_wav, wav16(RATE, &short)).unwrap();
    let out_path = dir.path.join("inplace.gba");
    let json = run_ok_json(&[
        "audio", "replace", p(&rom_path), "--offset", "0x2000", "--wav", p(&short_wav), "-o",
        p(&out_path), "--codec", codec, "--json",
    ]);
    assert_eq!(json["mode"], "in_place");
    assert_eq!(json["container"], "m4a_wavedata");
    assert_eq!(json["sample_rate_hz"], RATE);
    assert_eq!(json["written_offset"], "0x2000");
    assert_eq!(json["original_capacity"].as_u64().unwrap() as usize, original.len());
    assert_eq!(fs::read(&rom_path).unwrap(), rom, "input unchanged");

    let out = fs::read(&out_path).unwrap();
    assert_eq!(u16::from_le_bytes([out[WAVE_OFF], out[WAVE_OFF + 1]]), u16::from(dpcm), "wave_type");
    let (wj, pcm) = sound_wav(&dir.path, &out_path, WAVE_OFF, "inplace");
    assert_eq!(wj["samples"], 1200);
    assert_eq!(wj["sample_rate_hz"], RATE);
    assert_eq!(pcm, expected_pcm(&short, dpcm));

    // --- Oversized without --allow-relocate fails. ---
    let long = sine_i16(6000, 77.0);
    let long_wav = dir.path.join("long.wav");
    fs::write(&long_wav, wav16(RATE, &long)).unwrap();
    let blocked = run(&[
        "audio", "replace", p(&rom_path), "--offset", "0x2000", "--wav", p(&long_wav), "-o",
        p(&dir.path.join("blocked.gba")), "--codec", codec,
    ]);
    assert!(!blocked.status.success());

    // --- Relocation with pointer patching. ---
    let reloc_path = dir.path.join("reloc.gba");
    let json = run_ok_json(&[
        "audio", "replace", p(&rom_path), "--offset", "0x2000", "--wav", p(&long_wav), "-o",
        p(&reloc_path), "--codec", codec, "--allow-relocate", "--json",
    ]);
    assert_eq!(json["mode"], "relocated");
    assert_eq!(json["patched_pointers"], 1);
    let new_off = parse_hex(&json["written_offset"]);
    assert!(new_off >= FREE_START);
    let out = fs::read(&reloc_path).unwrap();
    assert_eq!(read_u32(&out, PTR_OFF), 0x0800_0000 + new_off as u32);
    let (wj, pcm) = sound_wav(&dir.path, &reloc_path, new_off, "reloc");
    assert_eq!(wj["samples"], 6000);
    assert_eq!(pcm, expected_pcm(&long, dpcm));
}

#[test]
fn cli_audio_replace_m4a_pcm8_in_place_and_relocate() {
    audio_roundtrip("pcm8", false);
}

#[test]
fn cli_audio_replace_gen3_dpcm_in_place_and_relocate() {
    audio_roundtrip("dpcm", true);
}

#[test]
fn cli_audio_replace_raw_unsigned_with_capacity_and_rate_override() {
    let dir = TempDir::new("audio_raw");
    const RAW_OFF: usize = 0x3000;
    let mut rom = blank_rom();
    rom[RAW_OFF..RAW_OFF + 1024].fill(0x80);
    let rom_path = dir.path.join("in.gba");
    fs::write(&rom_path, &rom).unwrap();

    // 22050 Hz input resampled to 11025 Hz -> 500 samples (fits 1024 bytes).
    let samples = sine_i16(1000, 40.0);
    let wav_path = dir.path.join("in.wav");
    fs::write(&wav_path, wav16(22_050, &samples)).unwrap();
    let out_path = dir.path.join("out.gba");
    let json = run_ok_json(&[
        "audio", "replace", p(&rom_path), "--offset", "0x3000", "--wav", p(&wav_path), "-o",
        p(&out_path), "--codec", "pcm8-unsigned", "--rate", "11025", "--raw-capacity", "1024",
        "--json",
    ]);
    assert_eq!(json["container"], "raw");
    assert_eq!(json["mode"], "in_place");
    assert_eq!(json["samples"], 500);
    assert_eq!(json["written_bytes"], 500);

    let resampled = gbakit::sound::resample_pcm_i16_linear(&samples, 22_050, 11_025).unwrap();
    let expected = gbakit::sound::encode_pcm8_unsigned(&resampled);
    let out = fs::read(&out_path).unwrap();
    assert_eq!(&out[RAW_OFF..RAW_OFF + 500], expected.as_slice());
    assert!(out[RAW_OFF + 500..RAW_OFF + 1024].iter().all(|&b| b == 0), "slack zeroed");

    // Non-M4A codecs require --raw-capacity.
    let missing = run(&[
        "audio", "replace", p(&rom_path), "--offset", "0x3000", "--wav", p(&wav_path), "-o",
        p(&dir.path.join("x.gba")), "--codec", "ima-adpcm",
    ]);
    assert!(!missing.status.success());
}

// -----------------------------------------------------------------------------
// audio scan: Cosmigo Sonix
// -----------------------------------------------------------------------------

/// Common Sonix ARM mixer prologue (public `cosmigo-sonix` detection signature).
const SONIX_MIXER_SIG: [u8; 64] = [
    0x12, 0x40, 0x2D, 0xE9, 0x00, 0x00, 0xA0, 0xE3, 0xBA, 0x00, 0xCC, 0xE1, 0x04, 0x00, 0xDC,
    0xE5, 0x02, 0x00, 0xC0, 0xE3, 0x04, 0x00, 0xCC, 0xE5, 0x00, 0x10, 0xDC, 0xE5, 0x0E, 0xE0,
    0xDC, 0xE5, 0x81, 0xB2, 0x8C, 0xE0, 0x0C, 0xA0, 0x9B, 0xE5, 0x00, 0x80, 0x9D, 0xE5, 0x80,
    0x00, 0x58, 0xE3, 0x80, 0x80, 0xA0, 0x23, 0x00, 0x48, 0x2D, 0xE9, 0x05, 0x00, 0xDB, 0xE5,
    0x03, 0x20, 0xDB, 0xE5,
];

#[test]
fn cli_audio_scan_detects_cosmigo_sonix_revisions() {
    let dir = TempDir::new("sonix");
    const MIXER: usize = 0x1000;

    // v1.16 via embedded banner.
    let mut v116 = vec![0u8; 0x4000];
    v116[MIXER..MIXER + 64].copy_from_slice(&SONIX_MIXER_SIG);
    let banner = b"Sonix Sound System for GBA v1.16";
    v116[0x3000..0x3000 + banner.len()].copy_from_slice(banner);
    let v116_path = dir.path.join("sonix116.gba");
    fs::write(&v116_path, &v116).unwrap();

    let json = run_ok_json(&["audio", "scan", p(&v116_path), "--json"]);
    assert_eq!(json["detected"], true);
    assert_eq!(json["engine"], "cosmigo_sonix");
    assert_eq!(json["family"], "cosmigo_sonix");
    assert_eq!(json["sonix_version"], "V116Gsm");
    assert_eq!(json["has_gsm"], true);

    let human = run(&["audio", "scan", p(&v116_path)]);
    assert!(human.status.success());
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("Cosmigo Sonix Sound System"), "{text}");
    assert!(text.contains("v1.16 + GSM (V116Gsm)"), "{text}");

    // v1.09c via the store-path mask opcode fallback (no banner).
    let mut v109 = vec![0u8; 0x4000];
    v109[MIXER..MIXER + 64].copy_from_slice(&SONIX_MIXER_SIG);
    v109[MIXER + 0x108..MIXER + 0x10C].copy_from_slice(&0xE3E0_1903u32.to_le_bytes());
    let v109_path = dir.path.join("sonix109.gba");
    fs::write(&v109_path, &v109).unwrap();
    let json = run_ok_json(&["audio", "scan", p(&v109_path), "--json"]);
    assert_eq!(json["sonix_version"], "V109c");
    assert_eq!(json["has_gsm"], false);

    // v1.096 via mask opcode fallback.
    let mut v1096 = v109.clone();
    v1096[MIXER + 0x108..MIXER + 0x10C].copy_from_slice(&0xE3E0_1A0Eu32.to_le_bytes());
    let v1096_path = dir.path.join("sonix1096.gba");
    fs::write(&v1096_path, &v1096).unwrap();
    let json = run_ok_json(&["audio", "scan", p(&v1096_path), "--json"]);
    assert_eq!(json["sonix_version"], "V1096Gsm");

    // `audio export` on a mixer-only ROM without an SNX3 bank reports a clean Cosmigo Sonix error.
    let export = run(&["audio", "export", p(&v1096_path), "-o", p(&dir.path.join("exp"))]);
    assert!(!export.status.success());
    assert!(
        String::from_utf8_lossy(&export.stderr).contains("Cosmigo Sonix"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&export.stderr)
    );
}
