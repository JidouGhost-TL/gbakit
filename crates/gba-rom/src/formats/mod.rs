//! Wave-1 unified GBA studio format parsers and ROM scanners.
//!
//! Each module consolidates a multi-magic GBA studio engine family on top of
//! `formatkit-layout` declarative primitives (`ChunkStreamSpec`,
//! `CountedOffsetArraySpec`, `AlignedStringBankSpec`, and `iff::parse_riff_form_with_options`):
//!
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

use serde::{Deserialize, Serialize};

pub mod is_asc03;
pub mod piff_rbhf;
pub mod plt_engine;
pub mod riff_scr;

/// Canonical format ID for the `PLT\0`/`IMG\0`/`PAT\0`/`SCR\0`/`TON\0` studio engine.
pub const FORMAT_ID_PLT_ENGINE: &str = "studio.plt-engine";

/// Canonical format ID for the `IS-ASC03`/`IS-ACG03`/`IS-MAP01` chunked container family.
pub const FORMAT_ID_IS_ASC03: &str = "studio.is-asc03";

/// Canonical format ID for the `PIFF`/`RBHF` relocatable module container family.
pub const FORMAT_ID_PIFF_RBHF: &str = "studio.piff-rbhf";

/// Canonical format ID for the `RIFF`/`SCR CODE` script and container family.
pub const FORMAT_ID_RIFF_SCR: &str = "studio.riff-scr";

/// A single verified Wave-1 studio format extent discovered during a ROM scan.
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

/// Scan a GBA ROM image for all 4 Wave-1 unified studio format families.
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

/// Re-validate a previously identified Wave-1 format slice by its canonical `format_id`.
pub fn validate_wave1_format_slice(format_id: &str, slice: &[u8]) -> Result<(), String> {
    match format_id {
        FORMAT_ID_PLT_ENGINE => plt_engine::parse(slice)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        FORMAT_ID_IS_ASC03 => is_asc03::parse(slice)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        FORMAT_ID_PIFF_RBHF => piff_rbhf::parse(slice)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        FORMAT_ID_RIFF_SCR => riff_scr::parse(slice)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        other => Err(format!("unknown Wave-1 studio format_id {other:?}")),
    }
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
    fn scans_all_wave1_families_in_synthetic_rom() {
        let mut rom = vec![0u8; 0x1000];

        // 1. PLT0 at 0x200 (48 bytes)
        let plt = plt_engine::tests::make_valid_plt0();
        rom[0x200..0x200 + plt.len()].copy_from_slice(&plt);

        // 2. IS-ASC03 CLRF at 0x300
        let clrf = is_asc03::tests::make_valid_clrf();
        rom[0x300..0x300 + clrf.len()].copy_from_slice(&clrf);

        // 3. PIFF container at 0x500
        let piff = piff_rbhf::tests::make_valid_piff();
        rom[0x500..0x500 + piff.len()].copy_from_slice(&piff);

        // 4. RIFF SCR container at 0x800
        let riff = riff_scr::tests::make_valid_riff_scr_inclusive();
        rom[0x800..0x800 + riff.len()].copy_from_slice(&riff);

        let found = scan_all_wave1_formats(&rom);
        assert_eq!(found.len(), 4);
        assert_eq!(found[0].format_id, FORMAT_ID_PLT_ENGINE);
        assert_eq!(found[0].offset, 0x200);
        assert_eq!(found[1].format_id, FORMAT_ID_IS_ASC03);
        assert_eq!(found[1].offset, 0x300);
        assert_eq!(found[2].format_id, FORMAT_ID_PIFF_RBHF);
        assert_eq!(found[2].offset, 0x500);
        assert_eq!(found[3].format_id, FORMAT_ID_RIFF_SCR);
        assert_eq!(found[3].offset, 0x800);

        for item in &found {
            let slice = &rom[item.offset..item.offset + item.size];
            assert!(validate_wave1_format_slice(item.format_id, slice).is_ok());
        }
    }
}
