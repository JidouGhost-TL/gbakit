//! End-to-end CLI integration tests for `gbakit text ...` commands.
//!
//! All ROMs, `.tbl` files, and font sheets are synthetic and generated in-test.

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
        let path = std::env::temp_dir().join(format!("gbakit_text_{tag}_{pid}_{id}"));
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

fn parse_hex(v: &serde_json::Value) -> usize {
    let s = v.as_str().expect("hex string");
    usize::from_str_radix(s.trim_start_matches("0x"), 16).expect("hex")
}

fn read_u32(rom: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([rom[off], rom[off + 1], rom[off + 2], rom[off + 3]])
}

const ROM_SIZE: usize = 0x1_0000;
const FREE_START: usize = 0x8000;

fn build_string_rom(cm: &gbakit::text::Charmap, strings: &[&str]) -> (Vec<u8>, usize, Vec<usize>) {
    let mut rom = vec![0u8; ROM_SIZE];
    rom[FREE_START..].fill(0xFF);
    let table_off = 0x200usize;
    let mut cursor = 0x1000usize;
    let mut offsets = Vec::new();

    for (i, &s) in strings.iter().enumerate() {
        let mut enc = cm.encode(s).unwrap();
        if let Some(term) = cm.default_terminator() {
            if !enc.ends_with(term) {
                enc.extend_from_slice(term);
            }
        }
        rom[cursor..cursor + enc.len()].copy_from_slice(&enc);
        let bus = 0x0800_0000u32 + cursor as u32;
        rom[table_off + i * 4..table_off + i * 4 + 4].copy_from_slice(&bus.to_le_bytes());
        offsets.push(cursor);
        cursor = (cursor + enc.len() + 3) & !3;
    }

    (rom, table_off, offsets)
}

#[test]
fn cli_text_tables_lists_builtins() {
    let json = run_ok_json(&["text", "tables", "--json"]);
    let tables = json["tables"].as_array().expect("tables array");
    let names: Vec<&str> = tables
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"ascii"));
    assert!(names.contains(&"pokemon-gen3-en"));
    assert!(names.contains(&"shift-jis"));
    assert!(names.contains(&"capcom-megaman-en"));
    assert!(names.contains(&"breath-of-fire-en"));
    assert!(names.contains(&"tactics-ogre-en"));
    assert!(names.contains(&"metroid-fusion-en"));
    assert!(names.contains(&"utf16-le"));

    let human = run(&["text", "tables"]);
    assert!(human.status.success());
    let stdout = String::from_utf8_lossy(&human.stdout);
    assert!(stdout.contains("pokemon-gen3-en"));
}

#[test]
fn cli_text_scan_extract_and_replace_pokemon_gen3() {
    let dir = TempDir::new("poke_gen3");
    let cm = gbakit::text::Charmap::pokemon_gen3_en();
    let (rom, table_off, offsets) = build_string_rom(
        &cm,
        &["BULBASAUR", "CHARMANDER", "SQUIRTLE", "PIKACHU", "MEWTWO"],
    );
    let rom_path = dir.path.join("poke.gba");
    fs::write(&rom_path, &rom).unwrap();

    // Scan string tables
    let scan_json = run_ok_json(&[
        "text",
        "scan",
        p(&rom_path),
        "--table",
        "pokemon-gen3-en",
        "--min-entries",
        "4",
        "--json",
    ]);
    assert_eq!(scan_json["total_tables_detected"], 1);
    assert_eq!(parse_hex(&scan_json["tables"][0]["table_offset"]), table_off);
    assert_eq!(scan_json["tables"][0]["valid_string_count"], 5);
    assert_eq!(scan_json["tables"][0]["entries"][3]["preview"], "PIKACHU");

    // Extract single string
    let off_str = format!("0x{:X}", offsets[1]);
    let ext_json = run_ok_json(&[
        "text",
        "extract",
        p(&rom_path),
        "--offset",
        &off_str,
        "--table",
        "pokemon-gen3-en",
        "--json",
    ]);
    assert_eq!(ext_json["text"], "CHARMANDER");
    assert_eq!(ext_json["terminated"], true);
    assert_eq!(ext_json["terminator_hex"], "FF");

    // In-place replace (shorter)
    let inplace_path = dir.path.join("inplace.gba");
    let rep_json = run_ok_json(&[
        "text",
        "replace",
        p(&rom_path),
        "--offset",
        &off_str,
        "--text",
        "CHARIZARD",
        "-o",
        p(&inplace_path),
        "--table",
        "pokemon-gen3-en",
        "--json",
    ]);
    assert_eq!(rep_json["mode"], "in_place");
    assert_eq!(parse_hex(&rep_json["written_offset"]), offsets[1]);
    assert_eq!(fs::read(&rom_path).unwrap(), rom, "input ROM unchanged");

    // Verify extracted string from modified ROM
    let verify_json = run_ok_json(&[
        "text",
        "extract",
        p(&inplace_path),
        "--offset",
        &off_str,
        "--table",
        "pokemon-gen3-en",
        "--json",
    ]);
    assert_eq!(verify_json["text"], "CHARIZARD");

    // Refuse overwriting input ROM in place
    let refuse = run(&[
        "text",
        "replace",
        p(&rom_path),
        "--offset",
        &off_str,
        "--text",
        "CHAR",
        "-o",
        p(&rom_path),
        "--table",
        "pokemon-gen3-en",
    ]);
    assert!(!refuse.status.success());

    // Oversized replacement without --allow-relocate fails
    let blocked_path = dir.path.join("blocked.gba");
    let blocked = run(&[
        "text",
        "replace",
        p(&rom_path),
        "--offset",
        &off_str,
        "--text",
        "MEGA CHARIZARD X SUPREME",
        "-o",
        p(&blocked_path),
        "--table",
        "pokemon-gen3-en",
    ]);
    assert!(!blocked.status.success());
    assert!(!blocked_path.exists());

    // Relocation with --allow-relocate patches pointer table
    let reloc_path = dir.path.join("reloc.gba");
    let reloc_json = run_ok_json(&[
        "text",
        "replace",
        p(&rom_path),
        "--offset",
        &off_str,
        "--text",
        "MEGA CHARIZARD X SUPREME",
        "-o",
        p(&reloc_path),
        "--table",
        "pokemon-gen3-en",
        "--allow-relocate",
        "--json",
    ]);
    assert_eq!(reloc_json["mode"], "relocated");
    assert_eq!(reloc_json["patched_pointers"], 1);
    let new_off = parse_hex(&reloc_json["written_offset"]);
    assert!(new_off >= FREE_START);

    let reloc_rom = fs::read(&reloc_path).unwrap();
    assert_eq!(
        read_u32(&reloc_rom, table_off + 4),
        0x0800_0000 + new_off as u32
    );

    let new_off_str = format!("0x{new_off:X}");
    let verify_reloc = run_ok_json(&[
        "text",
        "extract",
        p(&reloc_path),
        "--offset",
        &new_off_str,
        "--table",
        "pokemon-gen3-en",
        "--json",
    ]);
    assert_eq!(verify_reloc["text"], "MEGA CHARIZARD X SUPREME");
}

#[test]
fn cli_text_custom_tbl_file_extract_and_replace() {
    let dir = TempDir::new("custom_tbl");
    let tbl_path = dir.path.join("custom.tbl");
    fs::write(
        &tbl_path,
        "# Custom game table\n00={END}\n10=H\n11=E\n12=L\n13=O\n14=!\n",
    )
    .unwrap();

    let mut rom = vec![0u8; 0x1000];
    // "HELLO!" + 0x00 at 0x200
    rom[0x200..0x207].copy_from_slice(&[0x10, 0x11, 0x12, 0x12, 0x13, 0x14, 0x00]);
    let rom_path = dir.path.join("custom.gba");
    fs::write(&rom_path, &rom).unwrap();

    let ext = run_ok_json(&[
        "text",
        "extract",
        p(&rom_path),
        "--offset",
        "0x200",
        "--table",
        p(&tbl_path),
        "--json",
    ]);
    assert_eq!(ext["text"], "HELLO!");

    let out_path = dir.path.join("custom_out.gba");
    let rep = run_ok_json(&[
        "text",
        "replace",
        p(&rom_path),
        "--offset",
        "0x200",
        "--text",
        "HELO!",
        "-o",
        p(&out_path),
        "--table",
        p(&tbl_path),
        "--json",
    ]);
    assert_eq!(rep["mode"], "in_place");
    let out_rom = fs::read(&out_path).unwrap();
    assert_eq!(&out_rom[0x200..0x207], &[0x10, 0x11, 0x12, 0x13, 0x14, 0x00, 0x00]);
}

#[test]
fn cli_text_font_dump_and_replace_roundtrip() {
    let dir = TempDir::new("font_roundtrip");
    let mut rom = vec![0u8; 0x1000];
    let font_off = 0x200usize;
    // 4 glyphs of 8x8 2bpp (16 bytes each = 64 bytes)
    let font_bytes: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(29).wrapping_add(7)).collect();
    rom[font_off..font_off + 64].copy_from_slice(&font_bytes);

    let rom_path = dir.path.join("font.gba");
    fs::write(&rom_path, &rom).unwrap();

    // Dump to PNG
    let png_path = dir.path.join("sheet.png");
    let dump_json = run_ok_json(&[
        "text",
        "font-dump",
        p(&rom_path),
        "--offset",
        "0x200",
        "--count",
        "4",
        "--width",
        "8",
        "--height",
        "8",
        "--bpp",
        "2",
        "--cols",
        "2",
        "-o",
        p(&png_path),
        "--json",
    ]);
    assert_eq!(dump_json["format"], "png");
    assert_eq!(dump_json["sheet_width"], 16);
    assert_eq!(dump_json["sheet_height"], 16);
    assert!(png_path.is_file());

    // Replace into a blank target ROM and verify exact byte recovery
    let blank_path = dir.path.join("blank.gba");
    fs::write(&blank_path, vec![0u8; 0x1000]).unwrap();
    let out_path = dir.path.join("replaced.gba");

    let rep_json = run_ok_json(&[
        "text",
        "font-replace",
        p(&blank_path),
        "--offset",
        "0x200",
        "--image",
        p(&png_path),
        "--count",
        "4",
        "--width",
        "8",
        "--height",
        "8",
        "--bpp",
        "2",
        "--cols",
        "2",
        "-o",
        p(&out_path),
        "--json",
    ]);
    assert_eq!(rep_json["bytes_written"], 64);
    assert!(rep_json["bytes_changed"].as_u64().unwrap() > 0);

    let replaced = fs::read(&out_path).unwrap();
    assert_eq!(&replaced[font_off..font_off + 64], font_bytes.as_slice());

    // Also test 1bpp LSB-first PPM round-trip
    let ppm_path = dir.path.join("sheet1.ppm");
    let dump1 = run(&[
        "text",
        "font-dump",
        p(&rom_path),
        "--offset",
        "0x200",
        "--count",
        "4",
        "--width",
        "8",
        "--height",
        "8",
        "--bpp",
        "1",
        "--lsb",
        "--cols",
        "4",
        "-o",
        p(&ppm_path),
    ]);
    assert!(dump1.status.success());

    let out1_path = dir.path.join("replaced1.gba");
    let rep1 = run_ok_json(&[
        "text",
        "font-replace",
        p(&blank_path),
        "--offset",
        "0x200",
        "--image",
        p(&ppm_path),
        "--count",
        "4",
        "--width",
        "8",
        "--height",
        "8",
        "--bpp",
        "1",
        "--lsb",
        "--cols",
        "4",
        "-o",
        p(&out1_path),
        "--json",
    ]);
    assert_eq!(rep1["bytes_written"], 32);
    let replaced1 = fs::read(&out1_path).unwrap();
    assert_eq!(&replaced1[font_off..font_off + 32], &font_bytes[..32]);
}

#[test]
fn cli_text_lz77_extract_replace_and_relocate() {
    let dir = TempDir::new("lz77_text");
    let mut rom = vec![0u8; ROM_SIZE];
    rom[FREE_START..].fill(0xFF);

    let orig = b"WELCOME TO GBAKIT LZ77 TEXT!\0";
    let comp = gbakit::compress::encode_lz77(orig).unwrap();
    let entry_off = 0x1000usize;
    rom[entry_off..entry_off + comp.len()].copy_from_slice(&comp);

    let ptr_off = 0x200usize;
    let bus = 0x0800_0000u32 + entry_off as u32;
    rom[ptr_off..ptr_off + 4].copy_from_slice(&bus.to_le_bytes());

    let rom_path = dir.path.join("lz77.gba");
    fs::write(&rom_path, &rom).unwrap();

    let ext = run_ok_json(&[
        "text",
        "extract",
        p(&rom_path),
        "--offset",
        "0x1000",
        "--lz77",
        "--json",
    ]);
    assert_eq!(ext["lz77"], true);
    assert_eq!(ext["text"], "WELCOME TO GBAKIT LZ77 TEXT!");
    assert_eq!(ext["compressed_bytes_len"], comp.len());

    let reloc_path = dir.path.join("lz77_reloc.gba");
    let long_str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!@#$%^&*()-_=+";
    let rep = run_ok_json(&[
        "text",
        "replace",
        p(&rom_path),
        "--offset",
        "0x1000",
        "--text",
        long_str,
        "-o",
        p(&reloc_path),
        "--lz77",
        "--allow-relocate",
        "--json",
    ]);
    assert_eq!(rep["lz77"], true);
    assert_eq!(rep["mode"], "relocated");
    assert_eq!(rep["patched_pointers"], 1);
    let new_off = parse_hex(&rep["written_offset"]);
    assert!(new_off >= FREE_START);

    let new_off_str = format!("0x{new_off:X}");
    let verify = run_ok_json(&[
        "text",
        "extract",
        p(&reloc_path),
        "--offset",
        &new_off_str,
        "--lz77",
        "--json",
    ]);
    assert_eq!(verify["text"], long_str);
}

#[test]
fn cli_text_huffman_extract_and_replace() {
    let dir = TempDir::new("huffman_cli");
    let mut rom = vec![0u8; ROM_SIZE];
    rom[FREE_START..].fill(0xFF);

    let tree_off = 0x200usize;
    let nodes: [(u16, u16); 9] = [
        (0x0041, 0xFFFF), // 'A'
        (0x0042, 0xFFFF), // 'B'
        (0x0043, 0xFFFF), // 'C'
        (0x0000, 0xFFFF), // END
        (0x0020, 0xFFFF), // ' '
        (0, 1),
        (3, 4),
        (2, 6),
        (5, 7),
    ];
    for (i, &(l, r)) in nodes.iter().enumerate() {
        let off = tree_off + i * 4;
        rom[off..off + 2].copy_from_slice(&l.to_le_bytes());
        rom[off + 2..off + 4].copy_from_slice(&r.to_le_bytes());
    }
    let root_off = tree_off + 8 * 4;
    let tree = gbakit::text::HuffmanTree::from_rom(&rom, tree_off, root_off).unwrap();
    let encoded = tree.encode(b"AB C").unwrap();

    let entry_off = 0x1000usize;
    rom[entry_off..entry_off + encoded.len()].copy_from_slice(&encoded);
    let ptr_off = 0x300usize;
    let bus = 0x0800_0000u32 + entry_off as u32;
    rom[ptr_off..ptr_off + 4].copy_from_slice(&bus.to_le_bytes());

    let rom_path = dir.path.join("fe.gba");
    fs::write(&rom_path, &rom).unwrap();
    let root_str = format!("0x{root_off:X}");

    let ext = run_ok_json(&[
        "text",
        "huffman-extract",
        p(&rom_path),
        "--tree",
        "0x200",
        "--root",
        &root_str,
        "--offset",
        "0x1000",
        "--json",
    ]);
    assert_eq!(ext["text"], "AB C");
    assert_eq!(ext["terminated"], true);

    let reloc_path = dir.path.join("fe_reloc.gba");
    let rep = run_ok_json(&[
        "text",
        "huffman-replace",
        p(&rom_path),
        "--tree",
        "0x200",
        "--root",
        &root_str,
        "--offset",
        "0x1000",
        "--text",
        "CBA CBA CBA CBA",
        "-o",
        p(&reloc_path),
        "--allow-relocate",
        "--json",
    ]);
    assert_eq!(rep["mode"], "relocated");
    assert_eq!(rep["patched_pointers"], 1);
    let new_off = parse_hex(&rep["written_offset"]);
    let new_off_str = format!("0x{new_off:X}");

    let verify = run_ok_json(&[
        "text",
        "huffman-extract",
        p(&reloc_path),
        "--tree",
        "0x200",
        "--root",
        &root_str,
        "--offset",
        &new_off_str,
        "--json",
    ]);
    assert_eq!(verify["text"], "CBA CBA CBA CBA");
}

#[test]
fn cli_text_font_widths_read_and_update_from_image() {
    let dir = TempDir::new("font_widths_cli");
    let mut rom = vec![0u8; 0x1000];
    let font_off = 0x100usize;
    let widths_off = 0x200usize;
    let mut glyphs = [0u8; 32];
    glyphs[8..16].fill(0xC0); // 2px wide
    glyphs[16..24].fill(0xF8); // 5px wide
    glyphs[24..32].fill(0xFF); // 8px wide
    rom[font_off..font_off + 32].copy_from_slice(&glyphs);
    rom[widths_off..widths_off + 4].copy_from_slice(&[1, 2, 3, 4]);

    let rom_path = dir.path.join("vwf.gba");
    fs::write(&rom_path, &rom).unwrap();

    // Read existing widths
    let read_json = run_ok_json(&[
        "text",
        "font-widths",
        p(&rom_path),
        "--offset",
        "0x200",
        "--count",
        "4",
        "--json",
    ]);
    assert_eq!(read_json["widths"], serde_json::json!([1, 2, 3, 4]));

    // Dump font sheet to PNG
    let png_path = dir.path.join("vwf.png");
    let dump = run(&[
        "text",
        "font-dump",
        p(&rom_path),
        "--offset",
        "0x100",
        "--count",
        "4",
        "--width",
        "8",
        "--height",
        "8",
        "--bpp",
        "1",
        "--cols",
        "4",
        "-o",
        p(&png_path),
    ]);
    assert!(dump.status.success());

    // Compute and write updated widths from sheet
    let out_path = dir.path.join("vwf_out.gba");
    let upd_json = run_ok_json(&[
        "text",
        "font-widths",
        p(&rom_path),
        "--offset",
        "0x200",
        "--count",
        "4",
        "--from-image",
        p(&png_path),
        "--width",
        "8",
        "--height",
        "8",
        "--cols",
        "4",
        "--pad",
        "1",
        "-o",
        p(&out_path),
        "--json",
    ]);
    assert_eq!(upd_json["widths"], serde_json::json!([0, 3, 6, 9]));
    assert_eq!(upd_json["bytes_changed"], 4);

    let verify_json = run_ok_json(&[
        "text",
        "font-widths",
        p(&out_path),
        "--offset",
        "0x200",
        "--count",
        "4",
        "--json",
    ]);
    assert_eq!(verify_json["widths"], serde_json::json!([0, 3, 6, 9]));
}

fn write_camelot_test_tree(rom: &mut [u8], tree_addr: usize, syms: &[u16], bits: &[u8]) {
    for (k, &sym) in syms.iter().enumerate() {
        let pair = k / 2;
        let base = tree_addr - pair * 3;
        if k % 2 == 0 {
            rom[base - 1] = ((sym >> 4) & 0xFF) as u8;
            rom[base - 2] = (rom[base - 2] & 0x0F) | (((sym & 0x0F) as u8) << 4);
        } else {
            rom[base - 2] = (rom[base - 2] & 0xF0) | (((sym >> 8) & 0x0F) as u8);
            rom[base - 3] = (sym & 0xFF) as u8;
        }
    }
    let mut byte_idx = 0usize;
    let mut bit_pos = 0u8;
    for &b in bits {
        if bit_pos == 0 {
            rom[tree_addr + byte_idx] = 0;
        }
        if b != 0 {
            rom[tree_addr + byte_idx] |= 1 << bit_pos;
        }
        bit_pos += 1;
        if bit_pos == 8 {
            bit_pos = 0;
            byte_idx += 1;
        }
    }
}

#[test]
fn cli_text_camelot_extract_and_replace() {
    let dir = TempDir::new("camelot_cli");
    let mut rom = vec![0u8; ROM_SIZE];
    rom[FREE_START..].fill(0xFF);

    let tree_data_off = 0x200usize;
    let offset_table_off = 0x300usize;
    let header_off = 0x400usize;

    for i in 0..128 {
        let off = offset_table_off + i * 2;
        rom[off..off + 2].copy_from_slice(&0x8000u16.to_le_bytes());
    }

    let bits3 = &[0u8, 1, 0, 1, 1];
    let bits2 = &[0u8, 1, 1];
    let bits1 = &[1u8];
    let specs: &[(u8, usize, &[u16], &[u8])] = &[
        (0x00, 0x10, &[b'A' as u16, b'B' as u16, 0x00], bits3),
        (b'A', 0x20, &[b'B' as u16, b'C' as u16, 0x00], bits3),
        (b'B', 0x30, &[b'A' as u16, b' ' as u16, 0x00], bits3),
        (b'C', 0x40, &[0x00], bits1),
        (b' ', 0x50, &[b'A' as u16, b'B' as u16], bits2),
    ];
    for &(ch, rel, syms, bits) in specs {
        write_camelot_test_tree(&mut rom, tree_data_off + rel, syms, bits);
        let slot = offset_table_off + usize::from(ch) * 2;
        rom[slot..slot + 2].copy_from_slice(&(rel as u16).to_le_bytes());
    }

    rom[header_off..header_off + 4]
        .copy_from_slice(&(0x0800_0000u32 + tree_data_off as u32).to_le_bytes());
    rom[header_off + 4..header_off + 8]
        .copy_from_slice(&(0x0800_0000u32 + offset_table_off as u32).to_le_bytes());

    let bank = gbakit::text::CamelotHuffmanBank::from_header(&rom, header_off).unwrap();
    let encoded = bank.encode(b"AB AC").unwrap();

    let entry_off = 0x1000usize;
    rom[entry_off..entry_off + encoded.len()].copy_from_slice(&encoded);
    let ptr_off = 0x500usize;
    rom[ptr_off..ptr_off + 4].copy_from_slice(&(0x0800_0000u32 + entry_off as u32).to_le_bytes());

    let rom_path = dir.path.join("camelot.gba");
    fs::write(&rom_path, &rom).unwrap();

    // 1. Extract via 8-byte header (--trees 0x400)
    let ext_hdr = run_ok_json(&[
        "text",
        "camelot-extract",
        p(&rom_path),
        "--trees",
        "0x400",
        "--offset",
        "0x1000",
        "--json",
    ]);
    assert_eq!(ext_hdr["text"], "AB AC");
    assert_eq!(ext_hdr["tree_count"], 5);
    assert_eq!(ext_hdr["terminated"], true);

    // 2. Extract via explicit --trees + --offsets
    let ext_raw = run_ok_json(&[
        "text",
        "camelot-extract",
        p(&rom_path),
        "--trees",
        "0x200",
        "--offsets",
        "0x300",
        "--offset",
        "0x1000",
        "--json",
    ]);
    assert_eq!(ext_raw["text"], "AB AC");

    // 3. Refuse overwriting input ROM in place
    let refuse = run(&[
        "text",
        "camelot-replace",
        p(&rom_path),
        "--trees",
        "0x400",
        "--offset",
        "0x1000",
        "--text",
        "AC",
        "-o",
        p(&rom_path),
    ]);
    assert!(!refuse.status.success());

    // 4. In-place replace ("AC" fits within 1-byte original capacity)
    let inplace_path = dir.path.join("camelot_inplace.gba");
    let rep_in = run_ok_json(&[
        "text",
        "camelot-replace",
        p(&rom_path),
        "--trees",
        "0x400",
        "--offset",
        "0x1000",
        "--text",
        "AC",
        "-o",
        p(&inplace_path),
        "--json",
    ]);
    assert_eq!(rep_in["mode"], "in_place");
    assert_eq!(fs::read(&rom_path).unwrap(), rom, "input ROM unchanged");

    let verify_in = run_ok_json(&[
        "text",
        "camelot-extract",
        p(&inplace_path),
        "--trees",
        "0x400",
        "--offset",
        "0x1000",
        "--json",
    ]);
    assert_eq!(verify_in["text"], "AC");

    // 5. Oversized replacement with --allow-relocate relocates to 0xFF free space & patches pointer
    let reloc_path = dir.path.join("camelot_reloc.gba");
    let long_str = "AB AB AB AB AB AB AC";
    let rep_reloc = run_ok_json(&[
        "text",
        "camelot-replace",
        p(&rom_path),
        "--trees",
        "0x400",
        "--offset",
        "0x1000",
        "--text",
        long_str,
        "-o",
        p(&reloc_path),
        "--allow-relocate",
        "--json",
    ]);
    assert_eq!(rep_reloc["mode"], "relocated");
    assert_eq!(rep_reloc["patched_pointers"], 1);
    let new_off = parse_hex(&rep_reloc["written_offset"]);
    assert!(new_off >= FREE_START);

    let reloc_rom = fs::read(&reloc_path).unwrap();
    assert_eq!(read_u32(&reloc_rom, ptr_off), 0x0800_0000 + new_off as u32);

    let new_off_str = format!("0x{new_off:X}");
    let verify_reloc = run_ok_json(&[
        "text",
        "camelot-extract",
        p(&reloc_path),
        "--trees",
        "0x400",
        "--offset",
        &new_off_str,
        "--json",
    ]);
    assert_eq!(verify_reloc["text"], long_str);
}
