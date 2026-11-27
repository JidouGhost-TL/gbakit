#![deny(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use gba_ereader::dotcode::{bin_to_raw, build_bin};
use gba_ereader::vpk0::vpk0_compress_with_method;
use gbakit::patches::{BpsPatch, UpsPatch};
use gbakit::rom::{compute_header_complement, pack_archive, ArchiveFormat};
use serde_json::Value;

fn gbakit_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gbakit")
}

fn run_cli(args: &[&str]) -> Output {
    Command::new(gbakit_bin())
        .args(args)
        .output()
        .expect("spawn gbakit")
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "gbakit_cli_contract_{}_{}_{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn make_synthetic_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 512];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0x04] = 0x24; // non-zero logo region marker
    rom[0xA0..0xAC].copy_from_slice(b"CONTRACTROM ");
    rom[0xAC..0xB0].copy_from_slice(b"ACTE");
    rom[0xB0..0xB2].copy_from_slice(b"01");
    rom[0xB2] = 0x96;
    rom[0xBD] = compute_header_complement(&rom[0xA0..0xBD]);
    rom
}

#[test]
fn version_human_and_build_identity_json() {
    let human = run_cli(&["--version"]);
    assert_eq!(human.status.code(), Some(0));
    let stdout = String::from_utf8(human.stdout).unwrap();
    assert_eq!(stdout.trim(), format!("gbakit {}", env!("CARGO_PKG_VERSION")));

    let short = run_cli(&["-V"]);
    assert_eq!(short.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(short.stdout).unwrap().trim(),
        format!("gbakit {}", env!("CARGO_PKG_VERSION"))
    );

    let out = run_cli(&["--version", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&out.stdout).expect("valid build-identity json");
    assert_eq!(v["schema"], "formatkit.build-identity.v1");
    assert_eq!(v["tool"], "gbakit");
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["formatkit_version"], "0.1.19");
    assert!(v["git_commit"].is_string() || v["git_commit"].is_null());
    assert!(v["git_dirty"].is_boolean());
    assert!(v["target"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(v["profile"].as_str().is_some_and(|s| !s.is_empty()));
}

#[test]
fn capabilities_schema_and_zero_drift() {
    let text_out = run_cli(&["capabilities"]);
    assert_eq!(text_out.status.code(), Some(0));
    assert!(String::from_utf8(text_out.stdout)
        .unwrap()
        .contains("formatkit.capabilities.v1"));

    let out = run_cli(&["capabilities", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let actual_text = String::from_utf8(out.stdout).unwrap();
    let v: Value = serde_json::from_str(&actual_text).expect("valid capabilities json");

    assert_eq!(v["schema"], "formatkit.capabilities.v1");
    assert_eq!(v["tool"], "gbakit");
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["formatkit_version"], "0.1.19");

    let allowed = ["parse", "decode", "reproduce", "edit", "author"];
    let formats = v["formats"].as_array().expect("formats array");
    assert!(!formats.is_empty());
    for fmt in formats {
        assert!(fmt["id"].as_str().is_some_and(|s| !s.is_empty()));
        assert!(fmt["category"].as_str().is_some_and(|s| !s.is_empty()));
        let tier = fmt["tier"].as_u64().expect("tier u64");
        assert!((1..=3).contains(&tier), "tier out of range: {tier}");
        let caps = fmt["capabilities"].as_array().expect("capabilities array");
        assert!(!caps.is_empty());
        for cap in caps {
            let cap_str = cap.as_str().expect("capability string");
            assert!(
                allowed.contains(&cap_str),
                "invalid capability {cap_str:?} in {}",
                fmt["id"]
            );
        }
    }

    // Zero-drift check against repo-root capabilities.json and CAPABILITIES.md
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let committed_json_path = repo_root.join("capabilities.json");
    let committed_json =
        fs::read_to_string(&committed_json_path).expect("read repo-root capabilities.json");
    assert_eq!(
        committed_json.trim(),
        actual_text.trim(),
        "capabilities.json drifted from `gbakit capabilities --json`"
    );

    let committed_md_path = repo_root.join("CAPABILITIES.md");
    let committed_md =
        fs::read_to_string(&committed_md_path).expect("read repo-root CAPABILITIES.md");
    for fmt in formats {
        let id = fmt["id"].as_str().unwrap();
        assert!(
            committed_md.contains(id),
            "CAPABILITIES.md missing format id `{id}`"
        );
    }
}

#[test]
fn detect_identifies_formats_without_trusting_extensions() {
    let dir = TempDir::new("detect");

    // 1. GBA ROM saved with misleading .dat extension
    let rom_path = dir.path().join("mystery_rom.dat");
    fs::write(&rom_path, make_synthetic_rom()).unwrap();
    let out = run_cli(&["detect", rom_path.to_str().unwrap(), "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["detected"], true);
    assert_eq!(v["primary"]["id"], "rom.agb");
    assert_eq!(v["primary"]["confidence"], "high");

    // 2. IPS / BPS / UPS patches with .bin extension
    let src = b"OriginalBytesForPatchDetection00112233";
    let mut dst = src.to_vec();
    dst[4..8].copy_from_slice(b"MOD!");

    let ips_path = dir.path().join("p1.bin");
    fs::write(&ips_path, b"PATCH\x00\x00\x04\x00\x04MOD!EOF").unwrap();
    let v: Value =
        serde_json::from_slice(&run_cli(&["detect", ips_path.to_str().unwrap(), "--json"]).stdout)
            .unwrap();
    assert_eq!(v["primary"]["id"], "patch.ips");

    let bps_path = dir.path().join("p2.bin");
    let bps_bytes = BpsPatch::create(src, &dst, None);
    fs::write(&bps_path, bps_bytes).unwrap();
    let v: Value =
        serde_json::from_slice(&run_cli(&["detect", bps_path.to_str().unwrap(), "--json"]).stdout)
            .unwrap();
    assert_eq!(v["primary"]["id"], "patch.bps");

    let ups_path = dir.path().join("p3.bin");
    let ups_bytes = UpsPatch::create(src, &dst);
    fs::write(&ups_path, ups_bytes).unwrap();
    let v: Value =
        serde_json::from_slice(&run_cli(&["detect", ups_path.to_str().unwrap(), "--json"]).stdout)
            .unwrap();
    assert_eq!(v["primary"]["id"], "patch.ups");

    // 3. PACK archive
    let pack_bytes = pack_archive(
        ArchiveFormat::Pack,
        &[("entry0.bin", b"hello archive".as_slice())],
    )
    .unwrap();
    let pack_path = dir.path().join("archive.unknown");
    fs::write(&pack_path, pack_bytes).unwrap();
    let v: Value =
        serde_json::from_slice(&run_cli(&["detect", pack_path.to_str().unwrap(), "--json"]).stdout)
            .unwrap();
    assert_eq!(v["primary"]["id"], "archive.pack");

    // 4. Mobiclip MOC5 & VXGB
    let moc_path = dir.path().join("clip1.unknown");
    let mut moc_bytes = vec![0u8; 64];
    moc_bytes[0..4].copy_from_slice(b"MOC5");
    fs::write(&moc_path, moc_bytes).unwrap();
    let v: Value =
        serde_json::from_slice(&run_cli(&["detect", moc_path.to_str().unwrap(), "--json"]).stdout)
            .unwrap();
    assert_eq!(v["primary"]["id"], "video.mobiclip-v1");

    let vxgb_path = dir.path().join("clip2.unknown");
    let mut vxgb_bytes = vec![0u8; 64];
    vxgb_bytes[0..4].copy_from_slice(b"VXGB");
    fs::write(&vxgb_path, vxgb_bytes).unwrap();
    let v: Value =
        serde_json::from_slice(&run_cli(&["detect", vxgb_path.to_str().unwrap(), "--json"]).stdout)
            .unwrap();
    assert_eq!(v["primary"]["id"], "video.mobiclip-v2");

    // 5. e-Reader VPK0 and raw DotCode strip
    let vpk_bytes = vpk0_compress_with_method(b"cleanroom ereader vpk0 payload", 0).unwrap();
    let vpk_path = dir.path().join("payload.unknown");
    fs::write(&vpk_path, &vpk_bytes).unwrap();
    let v: Value =
        serde_json::from_slice(&run_cli(&["detect", vpk_path.to_str().unwrap(), "--json"]).stdout)
            .unwrap();
    assert_eq!(v["primary"]["id"], "ereader.vpk0");

    let bin = build_bin(0x01, 0x00, 0, 1, &vpk_bytes).unwrap();
    let raw = bin_to_raw(&bin).unwrap();
    let raw_path = dir.path().join("strip.unknown");
    fs::write(&raw_path, raw).unwrap();
    let v: Value =
        serde_json::from_slice(&run_cli(&["detect", raw_path.to_str().unwrap(), "--json"]).stdout)
            .unwrap();
    assert_eq!(v["primary"]["id"], "ereader.dotcode-raw");

    // 6. GBA Save (known magic + valid checksum)
    let def = gbakit::saves::find("a4fe").unwrap().unwrap();
    let mut sav = vec![0u8; def.size_bytes];
    sav[0..4].copy_from_slice(b"INTI");
    gbakit::save::fix_checksums(&def, &mut sav).unwrap();
    let sav_path = dir.path().join("savefile.unknown");
    fs::write(&sav_path, sav).unwrap();
    let v: Value =
        serde_json::from_slice(&run_cli(&["detect", sav_path.to_str().unwrap(), "--json"]).stdout)
            .unwrap();
    assert_eq!(v["primary"]["id"], "save.gba");
    assert_eq!(v["primary"]["confidence"], "high");

    // 7. Unknown blob
    let unk_path = dir.path().join("random.gba");
    fs::write(&unk_path, b"not a gba rom or save").unwrap();
    let v: Value =
        serde_json::from_slice(&run_cli(&["detect", unk_path.to_str().unwrap(), "--json"]).stdout)
            .unwrap();
    assert_eq!(v["detected"], false);
    assert!(v["primary"].is_null());
}

#[test]
fn exit_codes_and_json_error_envelopes() {
    // Exit 0 on --help
    let help = run_cli(&["--help"]);
    assert_eq!(help.status.code(), Some(0));

    // Exit 1 on I/O failure with --json: stdout empty, stderr structured JSON
    let io_fail = run_cli(&["detect", "/nonexistent/gbakit_missing_file.bin", "--json"]);
    assert_eq!(io_fail.status.code(), Some(1));
    assert!(
        io_fail.stdout.is_empty(),
        "stdout must stay clean on --json error"
    );
    let err_v: Value = serde_json::from_slice(&io_fail.stderr).expect("valid json error on stderr");
    assert_eq!(err_v["ok"], false);
    assert_eq!(err_v["error"]["code"], "io_error");
    assert_eq!(err_v["error"]["exit_code"], 1);
    assert!(err_v["error"]["message"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));

    // Exit 2 on clap CLI usage error with --json
    let usage_fail = run_cli(&["--definitely-invalid-flag", "--json"]);
    assert_eq!(usage_fail.status.code(), Some(2));
    assert!(usage_fail.stdout.is_empty());
    let err_v: Value =
        serde_json::from_slice(&usage_fail.stderr).expect("valid json usage error on stderr");
    assert_eq!(err_v["ok"], false);
    assert_eq!(err_v["error"]["code"], "cli_usage");
    assert_eq!(err_v["error"]["exit_code"], 2);

    // Exit 2 on legacy subcommand usage error with --json
    let sub_usage = run_cli(&["patch", "--json"]);
    assert_eq!(sub_usage.status.code(), Some(2));
    assert!(sub_usage.stdout.is_empty());
    let err_v: Value =
        serde_json::from_slice(&sub_usage.stderr).expect("valid json subcommand usage error");
    assert_eq!(err_v["ok"], false);
    assert_eq!(err_v["error"]["code"], "cli_usage");
    assert_eq!(err_v["error"]["exit_code"], 2);
}

#[test]
fn asset_map_generate_merge_verify_markdown_and_export_cli() {
    let dir = TempDir::new("asset_map_e2e");

    // Build synthetic GBA ROM with header, pointer table, palette, LZ77 stream, PACK archive, raw 4bpp tile, trailing 0xFF
    let mut rom = vec![0u8; 4096];
    rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
    rom[0xA0..0xAC].copy_from_slice(b"MAPCLITEST  ");
    rom[0xAC..0xB0].copy_from_slice(b"AMCE");
    rom[0xB0..0xB2].copy_from_slice(b"01");
    rom[0xB2] = 0x96;
    rom[0xBD] = compute_header_complement(&rom[0xA0..0xBD]);

    // Pointer table at 0x100 -> palette at 0x200, LZ77 at 0x300
    rom[0x100..0x104].copy_from_slice(&(0x0800_0200u32).to_le_bytes());
    rom[0x104..0x108].copy_from_slice(&(0x0800_0300u32).to_le_bytes());

    // 16-color BGR555 palette at 0x200
    for i in 0..16 {
        let c = gbakit::gfx::rgb8_to_bgr555((i * 16) as u8, ((15 - i) * 16) as u8, 0x40);
        rom[0x200 + i * 2..0x200 + i * 2 + 2].copy_from_slice(&c.to_le_bytes());
    }

    // BIOS LZ77 at 0x300
    let lz = gbakit::compress::encode_lz77(&vec![0x5Au8; 256]).unwrap();
    rom[0x300..0x300 + lz.len()].copy_from_slice(&lz);

    // PACK archive at 0x500
    let pack = pack_archive(
        ArchiveFormat::Pack,
        &[("m0", b"member-zero".as_slice()), ("m1", b"member-one".as_slice())],
    )
    .unwrap();
    rom[0x500..0x500 + pack.len()].copy_from_slice(&pack);

    // Raw 4bpp tile at 0x700 (to be mapped manually)
    for i in 0..32 {
        rom[0x700 + i] = ((i % 16) as u8) | ((((i + 3) % 16) as u8) << 4);
    }

    // Trailing 0xFF padding
    rom[0xC00..0x1000].fill(0xFF);

    let rom_path = dir.path().join("test.gba");
    fs::write(&rom_path, &rom).unwrap();
    let map_path = dir.path().join("map.json");

    // 1. map generate --with-code-refs -o map.json
    let gen = run_cli(&[
        "map",
        "generate",
        rom_path.to_str().unwrap(),
        "--with-code-refs",
        "-o",
        map_path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(gen.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&gen.stderr));
    let mut map_v: Value =
        serde_json::from_str(&fs::read_to_string(&map_path).unwrap()).expect("valid map json");
    assert_eq!(map_v["schema"], "formatkit.asset-map.v1");
    assert_eq!(map_v["schema_version"], 1);
    assert_eq!(map_v["kit"], "gbakit");
    assert_eq!(map_v["media"]["kind"], "flat-rom");
    assert_eq!(map_v["media"]["total_bytes"], 4096);

    // Overwrite guard on map generate -o without --force -> exit 2
    let gen_refused = run_cli(&[
        "map",
        "generate",
        rom_path.to_str().unwrap(),
        "-o",
        map_path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(gen_refused.status.code(), Some(2));

    // 2. Inject a manual `mapped` raw-image entry paired with the detected palette, then --merge
    let pal_id = map_v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["offset"] == 0x200)
        .expect("palette entry at 0x200")["id"]
        .as_str()
        .unwrap()
        .to_string();

    map_v["entries"].as_array_mut().unwrap().push(serde_json::json!({
        "id": "manual_sprite_tile",
        "offset": 0x700,
        "size": 32,
        "status": "mapped",
        "mapping": {
            "kind": "raw-image",
            "params": { "bpp": 4, "tiles_per_row": 1 },
            "paired_with": pal_id,
            "evidence": "synthetic contract test"
        }
    }));
    let curated_path = dir.path().join("curated.json");
    fs::write(&curated_path, serde_json::to_string_pretty(&map_v).unwrap()).unwrap();

    let merged_path = dir.path().join("merged.json");
    let merge_out = run_cli(&[
        "map",
        "generate",
        rom_path.to_str().unwrap(),
        "--merge",
        curated_path.to_str().unwrap(),
        "-o",
        merged_path.to_str().unwrap(),
    ]);
    assert_eq!(merge_out.status.code(), Some(0));
    let merged_v: Value =
        serde_json::from_str(&fs::read_to_string(&merged_path).unwrap()).unwrap();
    assert!(merged_v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["id"] == "manual_sprite_tile" && e["status"] == "mapped"));

    // 3. map verify --json
    let ver = run_cli(&[
        "map",
        "verify",
        rom_path.to_str().unwrap(),
        merged_path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(ver.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&ver.stderr));
    let ver_v: Value = serde_json::from_slice(&ver.stdout).unwrap();
    assert_eq!(ver_v["valid"], true);
    assert_eq!(ver_v["accounted_bytes"], 4096);
    assert_eq!(ver_v["mapped_bytes"], 32);

    // 4. map markdown --rom
    let md_path = dir.path().join("map.md");
    let md_out = run_cli(&[
        "map",
        "markdown",
        merged_path.to_str().unwrap(),
        "--rom",
        rom_path.to_str().unwrap(),
        "-o",
        md_path.to_str().unwrap(),
    ]);
    assert_eq!(md_out.status.code(), Some(0));
    let md_text = fs::read_to_string(&md_path).unwrap();
    assert!(md_text.contains("manual_sprite_tile"));
    assert!(md_text.contains("## Verification"));

    // 5. assets export --asset-map + --force guard
    let export_dir = dir.path().join("exported");
    let exp = run_cli(&[
        "assets",
        "export",
        rom_path.to_str().unwrap(),
        "--asset-map",
        merged_path.to_str().unwrap(),
        "-o",
        export_dir.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(exp.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&exp.stderr));
    assert!(export_dir.join("manual_sprite_tile.png").is_file());
    assert!(export_dir.join("export-manifest.json").is_file());

    // Without --force on non-empty directory -> exit 2
    let exp_refused = run_cli(&[
        "assets",
        "export",
        rom_path.to_str().unwrap(),
        "--asset-map",
        merged_path.to_str().unwrap(),
        "-o",
        export_dir.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(exp_refused.status.code(), Some(2));
    assert!(exp_refused.stdout.is_empty());

    // With --force -> exit 0
    let exp_forced = run_cli(&[
        "assets",
        "export",
        rom_path.to_str().unwrap(),
        "--asset-map",
        merged_path.to_str().unwrap(),
        "-o",
        export_dir.to_str().unwrap(),
        "--force",
    ]);
    assert_eq!(exp_forced.status.code(), Some(0));
}
