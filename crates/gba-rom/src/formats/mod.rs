//! Unified Wave-1 & Wave-2 GBA studio format parsers, symmetric writers, and ROM scanners.
//!
//! Each module consolidates a GBA studio format family on top of `formatkit-layout` `0.1.17`
//! declarative primitives (`ChunkStreamSpec`, `CountedOffsetArraySpec`, `FixedRecordTableSpec`,
//! `AlignedStringBankSpec`, `iff::parse_riff_form_with_options`, and symmetric `pack_*` writers):
//!
//! **Wave 1 (Symmetric Read + Write):**
//! - [`plt_engine`] (`studio.plt-engine`): `PLT\0`, `IMG\0`, `PAT\0` (+ embedded `TIM\0`),
//!   `SCR\0`, and `TON\0` multi-asset studio engine.
//! - [`is_asc03`] (`studio.is-asc03`): `IS-ASC03` / `IS-ACG03` / `IS-MAP01` chunked map and
//!   graphics container (`CLRF`, `CMNT`, `VER `, `PNL `, `MAP `, `LINK`, `MODE`, `END `).
//! - [`piff_rbhf`] (`studio.piff-rbhf`): `PIFF` / `RBHF` / `RBHH` relocatable module container
//!   (`BODY`, `PACK`, `PKDW`, `PKDL`, `RELC`, `GLOB`) and `GLOBAL_SET` text manifests,
//!   explicitly disambiguated from [`crate::archive::ArchiveFormat::Pack`].
//! - [`riff_scr`] (`studio.riff-scr`): `RIFF` containers (including header-inclusive `RIFF`
//!   `SCR ` framing), standalone `SCR CODE` bytecode + `STR ` script containers, and `SCR `
//!   aligned name tables.
//!
//! **Wave 2 (Symmetric Read + Write):**
//! - [`anim_fram`] (`studio.anim-fram`): `ANIM` / `FRAM` sprite animation packages and frame pools.
//! - [`mbm_mcm`] (`studio.mbm-mcm`): paired `MBM\0` / `MCM\0` ROM asset packs.
//! - [`map0`] (`studio.map0`): `MAP\0` 4bpp packed-pixel maps.
//! - [`mto0`] (`studio.mto0`): `MTO\0` tagged key/value and compact record tables.
//! - [`brow`] (`studio.brow`): `BROWN` named record directories and message banks.
//! - [`pac3`] (`studio.pac3`): `PAC3` tagged control-word streams.

use serde::{Deserialize, Serialize};

pub mod anim_fram;
pub mod anp_engine;
pub mod brow;
pub mod is_asc03;
pub mod jcg_engine;
pub mod lv_dir;
pub mod map0;
pub mod marker_bundles;
pub mod mbm_mcm;
pub mod mto0;
pub mod nitro_gba;
pub mod nsq0;
pub mod pac3;
pub mod piff_rbhf;
pub mod plt_engine;
pub mod riff_scr;
pub mod scrp;
pub mod webfoot;

/// Canonical format ID for the `PLT\0`/`IMG\0`/`PAT\0`/`SCR\0`/`TON\0` studio engine.
pub const FORMAT_ID_PLT_ENGINE: &str = "studio.plt-engine";

/// Canonical format ID for the `IS-ASC03`/`IS-ACG03`/`IS-MAP01` chunked container family.
pub const FORMAT_ID_IS_ASC03: &str = "studio.is-asc03";

/// Canonical format ID for the `PIFF`/`RBHF` relocatable module container family.
pub const FORMAT_ID_PIFF_RBHF: &str = "studio.piff-rbhf";

/// Canonical format ID for the `RIFF`/`SCR CODE` script and container family.
pub const FORMAT_ID_RIFF_SCR: &str = "studio.riff-scr";

/// Canonical format ID for the `ANIM`/`FRAM` sprite animation package family.
pub const FORMAT_ID_ANIM_FRAM: &str = "studio.anim-fram";

/// Canonical format ID for the paired `MBM\0`/`MCM\0` ROM asset pack family.
pub const FORMAT_ID_MBM_MCM: &str = "studio.mbm-mcm";

/// Canonical format ID for `MAP\0` 4bpp packed-pixel maps.
pub const FORMAT_ID_MAP0: &str = "studio.map0";

/// Canonical format ID for `MTO\0` tagged key/value record tables.
pub const FORMAT_ID_MTO0: &str = "studio.mto0";

/// Canonical format ID for `BROWN` named record directories and message banks.
pub const FORMAT_ID_BROW: &str = "studio.brow";

/// Canonical format ID for `PAC3` tagged control-word streams.
pub const FORMAT_ID_PAC3: &str = "studio.pac3";

/// Canonical format ID for the Nitro-GBA (`RECN`/`RNAN`/`RGCN`/`KNBA`/`LBAL`) engine family.
pub const FORMAT_ID_NITRO_GBA: &str = "studio.nitro-gba";

/// Canonical format ID for the `JSC\0`/`JCL\0`/`JCG\0`/`AOB\0` 2D graphics engine family.
pub const FORMAT_ID_JCG_ENGINE: &str = "studio.jcg-engine";

/// Canonical format ID for marker-delimited multi-member bundles (`AGBJ`, `D233`, `CBA\0`, `MLXP`, `CNML`).
pub const FORMAT_ID_MARKER_BUNDLES: &str = "studio.marker-bundles";

/// Canonical format ID for the `ANP `/`HAP `/`PLA `/`PSI3` animation and script engine family.
pub const FORMAT_ID_ANP_ENGINE: &str = "studio.anp-engine";

/// Canonical format ID for `SCRP` bytecode & symbol-table script containers.
pub const FORMAT_ID_SCRP: &str = "studio.scrp";

/// Canonical format ID for `NSQ\0` 16-byte music event sequence tables.
pub const FORMAT_ID_NSQ0: &str = "studio.nsq0";

/// Canonical format ID for `LV1`..`LV6` aligned level-name & ROM-pointer directories.
pub const FORMAT_ID_LV_DIR: &str = "studio.lv-dir";

/// Canonical format ID for Webfoot GBA `AreaEntry` tables and `ByteCodeInterpreter` blobs.
pub const FORMAT_ID_WEBFOOT: &str = "studio.webfoot";

/// A single verified studio format extent discovered during a ROM scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedFormatExtent {
    /// Canonical `formatkit.asset-map.v1` format identifier.
    pub format_id: &'static str,
    /// Specific sub-format / entry-point variant within the studio family.
    pub subkind: &'static str,
    /// Byte offset in the ROM where the asset begins.
    pub offset: usize,
    /// Exact consumed byte length of the asset (`offset + size <= rom.len()`).
    pub size: usize,
}

/// Scan a GBA ROM image for all 10 unified Wave-1 and Wave-2 studio format families.
///
/// Results are sorted deterministically by `(offset, Reverse(size), format_id)`.
pub fn scan_all_studio_formats(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let mut found = Vec::new();
    // Wave 1
    found.extend(plt_engine::scan_plt_engine(rom));
    found.extend(is_asc03::scan_is_asc03(rom));
    found.extend(piff_rbhf::scan_piff_rbhf(rom));
    found.extend(riff_scr::scan_riff_scr(rom));
    // Wave 2
    found.extend(anim_fram::scan_anim_fram(rom));
    found.extend(mbm_mcm::scan_mbm_mcm(rom));
    found.extend(map0::scan_map0(rom));
    found.extend(mto0::scan_mto0(rom));
    found.extend(brow::scan_brow(rom));
    found.extend(pac3::scan_pac3(rom));
    // Wave 3
    found.extend(nitro_gba::scan_nitro_gba(rom));
    found.extend(jcg_engine::scan_jcg_engine(rom));
    found.extend(marker_bundles::scan_marker_bundles(rom));
    found.extend(anp_engine::scan_anp_engine(rom));
    found.extend(scrp::scan_scrp(rom));
    found.extend(nsq0::scan_nsq0(rom));
    found.extend(lv_dir::scan_lv_dir(rom));
    found.extend(webfoot::scan_webfoot(rom));

    found.sort_by(|a, b| {
        a.offset
            .cmp(&b.offset)
            .then_with(|| b.size.cmp(&a.size))
            .then_with(|| a.format_id.cmp(b.format_id))
    });
    found
}

/// Scan a GBA ROM image for the 4 Wave-1 unified studio format families.
///
/// Results are sorted deterministically by `(offset, Reverse(size), format_id)`.
pub fn scan_all_wave1_formats(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let mut found = Vec::new();
    found.extend(plt_engine::scan_plt_engine(rom));
    found.extend(is_asc03::scan_is_asc03(rom));
    found.extend(piff_rbhf::scan_piff_rbhf(rom));
    found.extend(riff_scr::scan_riff_scr(rom));

    found.sort_by(|a, b| {
        a.offset
            .cmp(&b.offset)
            .then_with(|| b.size.cmp(&a.size))
            .then_with(|| a.format_id.cmp(b.format_id))
    });
    found
}

fn check_roundtrip(format_id: &str, expected: &[u8], repacked: &[u8]) -> Result<(), String> {
    if repacked == expected {
        Ok(())
    } else {
        Err(format!(
            "{format_id}: byte-identical round-trip mismatch (expected {} bytes, got {} bytes)",
            expected.len(),
            repacked.len()
        ))
    }
}

/// Re-validate a previously identified studio format slice by its canonical `format_id`,
/// verifying both structural parsing and exact byte-identical `parse -> pack` round-trip.
pub fn validate_studio_format_slice(format_id: &str, slice: &[u8]) -> Result<(), String> {
    match format_id {
        FORMAT_ID_PLT_ENGINE => {
            let asset = plt_engine::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end()], &repacked)
        }
        FORMAT_ID_IS_ASC03 => {
            let asset = is_asc03::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end], &repacked)
        }
        FORMAT_ID_PIFF_RBHF => {
            let asset = piff_rbhf::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end], &repacked)
        }
        FORMAT_ID_RIFF_SCR => {
            let asset = riff_scr::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end], &repacked)
        }
        FORMAT_ID_ANIM_FRAM => {
            let asset = anim_fram::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end()], &repacked)
        }
        FORMAT_ID_MBM_MCM => {
            let asset = mbm_mcm::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end], &repacked)
        }
        FORMAT_ID_MAP0 => {
            let asset = map0::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end], &repacked)
        }
        FORMAT_ID_MTO0 => {
            let asset = mto0::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end()], &repacked)
        }
        FORMAT_ID_BROW => {
            let asset = brow::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end()], &repacked)
        }
        FORMAT_ID_PAC3 => {
            let asset = pac3::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end], &repacked)
        }
        FORMAT_ID_NITRO_GBA => {
            let asset = nitro_gba::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end()], &repacked)
        }
        FORMAT_ID_JCG_ENGINE => {
            let asset = jcg_engine::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end()], &repacked)
        }
        FORMAT_ID_MARKER_BUNDLES => {
            let asset = marker_bundles::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end], &repacked)
        }
        FORMAT_ID_ANP_ENGINE => {
            let asset = anp_engine::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end()], &repacked)
        }
        FORMAT_ID_SCRP => {
            let asset = scrp::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end], &repacked)
        }
        FORMAT_ID_NSQ0 => {
            let asset = nsq0::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end], &repacked)
        }
        FORMAT_ID_LV_DIR => {
            let asset = lv_dir::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end], &repacked)
        }
        FORMAT_ID_WEBFOOT => {
            let asset = webfoot::parse(slice).map_err(|e| e.to_string())?;
            let repacked = asset.to_bytes().map_err(|e| e.to_string())?;
            check_roundtrip(format_id, &slice[..asset.consumed_end], &repacked)
        }
        other => Err(format!("unknown studio format_id {other:?}")),
    }
}

/// Compatibility wrapper validating Wave-1 studio format slices (delegates to
/// [`validate_studio_format_slice`]).
pub fn validate_wave1_format_slice(format_id: &str, slice: &[u8]) -> Result<(), String> {
    validate_studio_format_slice(format_id, slice)
}

#[inline]
pub(crate) fn align_up_4(offset: usize) -> usize {
    let rem = offset & 3;
    if rem == 0 {
        offset
    } else {
        offset.saturating_add(4 - rem)
    }
}

#[inline]
pub(crate) fn is_printable_fourcc(tag: &[u8; 4]) -> bool {
    tag.iter().all(|&b| (0x20..=0x7E).contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_and_roundtrips_all_studio_families_in_synthetic_rom() {
        let mut rom = vec![0xEEu8; 0x3800];

        let placements: &[(usize, &str, Vec<u8>)] = &[
            (0x200, FORMAT_ID_PLT_ENGINE, plt_engine::tests::make_valid_plt0()),
            (0x300, FORMAT_ID_IS_ASC03, is_asc03::tests::make_valid_clrf()),
            (0x500, FORMAT_ID_PIFF_RBHF, piff_rbhf::tests::make_valid_piff()),
            (0x700, FORMAT_ID_RIFF_SCR, riff_scr::tests::make_valid_riff_scr_inclusive()),
            (0x900, FORMAT_ID_ANIM_FRAM, anim_fram::tests::make_valid_anim_chunked()),
            (0xB00, FORMAT_ID_MBM_MCM, mbm_mcm::tests::make_valid_mbm_mcm(0, 64, 64)),
            (0xD00, FORMAT_ID_MAP0, map0::tests::make_valid_map0(12, 12)),
            (0xF00, FORMAT_ID_MTO0, mto0::tests::make_valid_mto0_big(0xFFFF_FFFF, 9)),
            (0x1100, FORMAT_ID_BROW, brow::tests::make_valid_brow_dir()),
            (0x1300, FORMAT_ID_PAC3, pac3::tests::make_valid_pac3()),
            (0x1500, FORMAT_ID_NITRO_GBA, nitro_gba::tests::make_valid_rgcn()),
            (0x1700, FORMAT_ID_JCG_ENGINE, jcg_engine::tests::make_valid_jsc0()),
            (0x1900, FORMAT_ID_MARKER_BUNDLES, marker_bundles::tests::make_valid_agbj_v01()),
            (0x1B00, FORMAT_ID_ANP_ENGINE, anp_engine::tests::make_valid_pla()),
            (0x1D00, FORMAT_ID_SCRP, scrp::tests::make_valid_scrp()),
            (0x1F00, FORMAT_ID_NSQ0, nsq0::tests::make_valid_nsq0()),
            (0x2100, FORMAT_ID_LV_DIR, lv_dir::tests::make_valid_lv_dir()),
            (0x2300, FORMAT_ID_WEBFOOT, webfoot::tests::make_valid_webfoot_table()),
        ];

        for (off, _, bytes) in placements {
            rom[*off..*off + bytes.len()].copy_from_slice(bytes);
        }

        let found = scan_all_studio_formats(&rom);
        assert_eq!(found.len(), 18);
        for (idx, (off, expected_id, _)) in placements.iter().enumerate() {
            assert_eq!(found[idx].format_id, *expected_id);
            assert_eq!(found[idx].offset, *off);
            let slice = &rom[found[idx].offset..found[idx].offset + found[idx].size];
            assert!(
                validate_studio_format_slice(found[idx].format_id, slice).is_ok(),
                "round-trip validation failed for {}",
                found[idx].format_id
            );
        }
    }

    #[test]
    fn retail_batch200_wave3_samples_roundtrip_byte_identically() {
        let Some(base_dir) = std::env::var_os("GBA_BATCH200_CASES_DIR") else {
            return;
        };
        let base = std::path::PathBuf::from(base_dir);
        let cases: &[(&str, &str)] = &[
            ("020_recn", FORMAT_ID_NITRO_GBA),
            ("024_rnan", FORMAT_ID_NITRO_GBA),
            ("018_rgcn", FORMAT_ID_NITRO_GBA),
            ("021_knba", FORMAT_ID_NITRO_GBA),
            ("001_jsc0", FORMAT_ID_JCG_ENGINE),
            ("012_jcl0", FORMAT_ID_JCG_ENGINE),
            ("045_jcg0", FORMAT_ID_JCG_ENGINE),
            ("008_aob0", FORMAT_ID_JCG_ENGINE),
            ("016_agbj", FORMAT_ID_MARKER_BUNDLES),
            ("073_d233", FORMAT_ID_MARKER_BUNDLES),
            ("068_cba0", FORMAT_ID_MARKER_BUNDLES),
            ("092_mlxp", FORMAT_ID_MARKER_BUNDLES),
            ("100_cnml", FORMAT_ID_MARKER_BUNDLES),
            ("003_anp_spc", FORMAT_ID_ANP_ENGINE),
            ("031_hap_spc", FORMAT_ID_ANP_ENGINE),
            ("059_pla_spc", FORMAT_ID_ANP_ENGINE),
            ("067_psi3", FORMAT_ID_ANP_ENGINE),
            ("004_scrp", FORMAT_ID_SCRP),
            ("028_nsq0", FORMAT_ID_NSQ0),
            ("083_lv1", FORMAT_ID_LV_DIR),
            ("064_lv5", FORMAT_ID_LV_DIR),
            ("070_lv6", FORMAT_ID_LV_DIR),
        ];

        let mut total_checked = 0usize;
        let mut total_ok = 0usize;
        for (case_dir, fmt_id) in cases {
            let samples_dir = base.join(case_dir).join("workspace").join("samples");
            if !samples_dir.is_dir() {
                continue;
            }
            let mut case_ok = 0usize;
            let mut case_total = 0usize;
            for entry in std::fs::read_dir(&samples_dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().and_then(|e| e.to_str()) != Some("bin") {
                    continue;
                }
                let data = std::fs::read(&path).unwrap();
                case_total += 1;
                total_checked += 1;
                if validate_studio_format_slice(fmt_id, &data).is_ok() {
                    case_ok += 1;
                    total_ok += 1;
                }
            }
            assert!(
                case_ok > 0,
                "expected at least 1 retail sample round-trip for {case_dir} ({fmt_id}), got 0/{case_total}"
            );
        }
        assert!(total_checked > 0);
        assert!(
            total_ok * 100 >= total_checked * 85,
            "retail round-trip rate too low: {total_ok}/{total_checked}"
        );
    }
}
