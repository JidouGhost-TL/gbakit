//! End-to-end CLI integration tests for M54:
//! - `gbakit gfx replace-map` (Tiled `.tmx` CSV + raw `.scr`, in-place LZ77 & relocated pointer patching)
//! - `gbakit archive scan|extract|repack|replace` (`PACK`, `WAYFORWARD`, `SFCD`)
//!
//! All ROMs and archives are synthetic, generated in-test; no retail ROM data is used.

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
        let path = std::env::temp_dir().join(format!("gbakit_m54_{tag}_{pid}_{id}"));
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
    Command::new(gbakit_bin())
        .args(args)
        .output()
        .expect("spawn gbakit")
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

fn read_u32(rom: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([rom[off], rom[off + 1], rom[off + 2], rom[off + 3]])
}

fn parse_hex(v: &serde_json::Value) -> usize {
    let s = v.as_str().expect("hex string");
    usize::from_str_radix(s.trim_start_matches("0x"), 16).expect("hex")
}

fn make_rom_with_free_space() -> Vec<u8> {
    let mut rom = vec![0xAAu8; 0x1_0000];
    rom[0xB2] = 0x96;
    rom[0x8000..].fill(0xFF);
    rom
}

#[test]
fn cli_gfx_replace_map_tmx_inplace_and_relocated_scr() {
    let dir = TempDir::new("gfx_replace_map");
    const MAP_OFF: usize = 0x1000;
    const PTR_OFF: usize = 0x400;

    // Seed an original noisy 32x32 LZ77-compressed SCR at 0x1000.
    let mut noisy_scr = vec![0u8; 2048];
    for (i, b) in noisy_scr.iter_mut().enumerate() {
        *b = ((i * 53 + 7) & 0xFF) as u8;
    }
    let orig_lz = gbakit::gfx::compress_gfx(&noisy_scr, gbakit::gfx::GfxCompression::Lz77Vram).unwrap();

    let mut rom = make_rom_with_free_space();
    rom[MAP_OFF..MAP_OFF + orig_lz.len()].copy_from_slice(&orig_lz);
    rom[PTR_OFF..PTR_OFF + 4].copy_from_slice(&(0x0800_0000u32 + MAP_OFF as u32).to_le_bytes());

    let rom_path = dir.path.join("in.gba");
    fs::write(&rom_path, &rom).unwrap();

    // Build a 32x32 TMX CSV with H-flip on tile 0 and V-flip on tile 1.
    let mut gids = Vec::with_capacity(1024);
    for i in 0..1024u32 {
        let gid = match i {
            0 => 1 | gbakit::gfx::TILED_FLIP_H,
            1 => 2 | gbakit::gfx::TILED_FLIP_V,
            _ => 1,
        };
        gids.push(gid.to_string());
    }
    let tmx_text = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <map width=\"32\" height=\"32\" tilewidth=\"8\" tileheight=\"8\">\n\
         <tileset firstgid=\"1\" source=\"tiles.tsx\"/>\n\
         <layer id=\"1\" name=\"BG0\" width=\"32\" height=\"32\">\n\
         <data encoding=\"csv\">{}</data>\n\
         </layer>\n</map>\n",
        gids.join(",")
    );
    let tmx_path = dir.path.join("level.tmx");
    fs::write(&tmx_path, &tmx_text).unwrap();

    // 1) In-place TMX write-back.
    let out_inplace = dir.path.join("out_inplace.gba");
    let json = run_ok_json(&[
        "gfx",
        "replace-map",
        p(&rom_path),
        "--offset",
        "0x1000",
        "--map",
        p(&tmx_path),
        "-o",
        p(&out_inplace),
        "--palette-bank",
        "4",
        "--json",
    ]);
    assert_eq!(json["input_format"], "tmx");
    assert_eq!(json["compression"], "lz77-vram");
    assert_eq!(json["relocated"], false);
    assert_eq!(json["written_offset"], "0x1000");
    assert_eq!(json["entry_count"], 1024);
    assert_eq!(fs::read(&rom_path).unwrap(), rom, "input ROM untouched");

    let out_bytes = fs::read(&out_inplace).unwrap();
    let dec = gbakit::compress::decode_lz77(&out_bytes[MAP_OFF..], 1 << 20).unwrap();
    assert_eq!(dec.bytes.len(), 2048);
    let e0 = gbakit::gfx::ScrEntry::from_u16(u16::from_le_bytes([dec.bytes[0], dec.bytes[1]]));
    let e1 = gbakit::gfx::ScrEntry::from_u16(u16::from_le_bytes([dec.bytes[2], dec.bytes[3]]));
    assert_eq!(e0.tile_index, 0);
    assert!(e0.h_flip);
    assert!(!e0.v_flip);
    assert_eq!(e0.palette_bank, 4);
    assert_eq!(e1.tile_index, 1);
    assert!(!e1.h_flip);
    assert!(e1.v_flip);
    assert_eq!(e1.palette_bank, 4);

    // 2) Refuse overwriting input ROM in place.
    let refuse = run(&[
        "gfx",
        "replace-map",
        p(&rom_path),
        "--offset",
        "0x1000",
        "--map",
        p(&tmx_path),
        "-o",
        p(&rom_path),
    ]);
    assert!(!refuse.status.success());

    // 3) Relocated raw .scr write-back into 0xFF free space with pointer patching.
    let scr_path = dir.path.join("big.scr");
    fs::write(&scr_path, &noisy_scr).unwrap();
    let out_reloc = dir.path.join("out_reloc.gba");
    let json_reloc = run_ok_json(&[
        "gfx",
        "replace-map",
        p(&out_inplace),
        "--offset",
        "0x1000",
        "--map",
        p(&scr_path),
        "-o",
        p(&out_reloc),
        "--allow-relocate",
        "--json",
    ]);
    assert_eq!(json_reloc["input_format"], "scr");
    assert_eq!(json_reloc["relocated"], true);
    assert_eq!(json_reloc["patched_pointers"], 1);
    let new_off = parse_hex(&json_reloc["written_offset"]);
    assert!(new_off >= 0x8000);
    let reloc_bytes = fs::read(&out_reloc).unwrap();
    assert_eq!(read_u32(&reloc_bytes, PTR_OFF), 0x0800_0000 + new_off as u32);
    let dec_reloc = gbakit::compress::decode_lz77(&reloc_bytes[new_off..], 1 << 20).unwrap();
    assert_eq!(dec_reloc.bytes, noisy_scr);
}

#[test]
fn cli_archive_scan_extract_repack_and_replace_all_formats() {
    let dir = TempDir::new("archive_cli");
    let mut rom = make_rom_with_free_space();

    let pack_bin = gbakit::rom::pack_archive(
        gbakit::rom::ArchiveFormat::Pack,
        &[("p0", b"PACK_ENTRY_ZERO"), ("p1", b"PACK_ENTRY_ONE!")],
    )
    .unwrap();
    let wf_bin = gbakit::rom::pack_archive(
        gbakit::rom::ArchiveFormat::WayForward,
        &[("w0", b"WF_BLOCK_0"), ("w1", b"WF_BLOCK_1")],
    )
    .unwrap();
    let sfcd_bin = gbakit::rom::pack_archive(
        gbakit::rom::ArchiveFormat::Sfcd,
        &[("STAGE01", b"STAGE_DATA_ONE"), ("STAGE02", b"STAGE_DATA_TWO")],
    )
    .unwrap();

    const PACK_OFF: usize = 0x1000;
    const WF_OFF: usize = 0x2000;
    const SFCD_OFF: usize = 0x3000;
    const SFCD_PTR_OFF: usize = 0x500;

    rom[PACK_OFF..PACK_OFF + pack_bin.len()].copy_from_slice(&pack_bin);
    rom[WF_OFF..WF_OFF + wf_bin.len()].copy_from_slice(&wf_bin);
    rom[SFCD_OFF..SFCD_OFF + sfcd_bin.len()].copy_from_slice(&sfcd_bin);
    rom[SFCD_PTR_OFF..SFCD_PTR_OFF + 4]
        .copy_from_slice(&(0x0800_0000u32 + SFCD_OFF as u32).to_le_bytes());

    let rom_path = dir.path.join("studio.gba");
    fs::write(&rom_path, &rom).unwrap();

    // 1) `archive scan` finds all 3 formats.
    let scan = run_ok_json(&["archive", "scan", p(&rom_path), "--json"]);
    assert_eq!(scan["archive_count"], 3);
    assert_eq!(scan["archives"][0]["format"], "pack");
    assert_eq!(scan["archives"][1]["format"], "wayforward");
    assert_eq!(scan["archives"][2]["format"], "sfcd");

    // 2) `archive extract` -> `archive repack` round-trip for SFCD.
    let ext_dir = dir.path.join("sfcd_ext");
    let ext = run_ok_json(&[
        "archive",
        "extract",
        p(&rom_path),
        "--offset",
        "0x3000",
        "-o",
        p(&ext_dir),
        "--json",
    ]);
    assert_eq!(ext["format"], "sfcd");
    assert_eq!(ext["entry_count"], 2);
    assert!(ext_dir.join("manifest.json").is_file());
    assert_eq!(
        fs::read(ext_dir.join("0000_STAGE01.bin")).unwrap(),
        b"STAGE_DATA_ONE"
    );

    let repacked_path = dir.path.join("repacked.sfcd");
    let rep = run_ok_json(&[
        "archive",
        "repack",
        p(&ext_dir),
        "-o",
        p(&repacked_path),
        "--json",
    ]);
    assert_eq!(rep["format"], "sfcd");
    assert_eq!(rep["entry_count"], 2);
    assert_eq!(fs::read(&repacked_path).unwrap(), sfcd_bin);

    // 3) Modify an extracted file to grow the SFCD archive and test `archive replace --allow-relocate`.
    fs::write(ext_dir.join("0000_STAGE01.bin"), vec![0x77u8; 512]).unwrap();
    let out_rom_path = dir.path.join("modded.gba");

    // First verify overflow without --allow-relocate is rejected.
    let blocked = run(&[
        "archive",
        "replace",
        p(&rom_path),
        "--offset",
        "0x3000",
        "--input",
        p(&ext_dir),
        "-o",
        p(&out_rom_path),
    ]);
    assert!(!blocked.status.success());
    assert!(!out_rom_path.exists());

    // Now replace with --allow-relocate and verify pointer patching + re-extract.
    let repl = run_ok_json(&[
        "archive",
        "replace",
        p(&rom_path),
        "--offset",
        "0x3000",
        "--input",
        p(&ext_dir),
        "-o",
        p(&out_rom_path),
        "--allow-relocate",
        "--json",
    ]);
    assert_eq!(repl["format"], "sfcd");
    assert_eq!(repl["relocated"], true);
    assert_eq!(repl["patched_pointers"], 1);
    let new_off = parse_hex(&repl["written_offset"]);
    assert!(new_off >= 0x8000);
    assert_eq!(fs::read(&rom_path).unwrap(), rom, "input ROM untouched");

    let modded = fs::read(&out_rom_path).unwrap();
    assert_eq!(read_u32(&modded, SFCD_PTR_OFF), 0x0800_0000 + new_off as u32);
    let parsed_new = gbakit::rom::parse_archive_at(&modded, new_off).unwrap();
    let entries_new = gbakit::rom::extract_archive_entries(&modded, &parsed_new).unwrap();
    assert_eq!(entries_new[0].0.name, "STAGE01");
    assert_eq!(entries_new[0].1, vec![0x77u8; 512].as_slice());
    assert_eq!(entries_new[1].0.name, "STAGE02");
    assert_eq!(entries_new[1].1, b"STAGE_DATA_TWO");

    // 4) Refuse overwriting input ROM in place.
    let refuse = run(&[
        "archive",
        "replace",
        p(&rom_path),
        "--offset",
        "0x3000",
        "--input",
        p(&repacked_path),
        "-o",
        p(&rom_path),
    ]);
    assert!(!refuse.status.success());
}
