//! Universal structural + magic-byte format detector (`gbakit detect <path> [--json]`).
//!
//! Never trusts file extensions alone; identifies formats strictly from magic bytes,
//! checksums, and structural invariants.

use std::fs;
use std::path::Path;

use gba_ereader::dotcode::{parse_bin, raw_to_bin, LONG_STRIP_RAW_LEN, SHORT_STRIP_RAW_LEN};
use gbakit::patches::{detect_format as detect_patch_format, PatchFormat};
use gbakit::rom::{parse_archive_at, AgbHeader, ArchiveFormat};
use gbakit::save::{probe_loose_save, SaveGeometry};
use mobiclip::{MobiclipStreamView, VxgbStreamView};
use serde::Serialize;

/// A single format match produced by structural/magic detection.
#[derive(Debug, Clone, Serialize)]
pub struct DetectedFormat {
    pub id: String,
    pub category: &'static str,
    pub confidence: &'static str,
    pub detail: String,
}

/// Top-level output of `gbakit detect <path> [--json]`.
#[derive(Debug, Clone, Serialize)]
pub struct DetectReport {
    pub path: String,
    pub bytes: usize,
    pub detected: bool,
    pub primary: Option<DetectedFormat>,
    pub matches: Vec<DetectedFormat>,
}

fn is_canonical_save_size(len: usize) -> bool {
    matches!(
        len,
        512 | 8_192 | 32_768 | 65_536 | 65_552 | 131_072 | 131_088
    )
}

/// Inspect raw bytes by magic and structural invariants (never by file extension).
pub fn detect_bytes(path_label: &str, bytes: &[u8]) -> DetectReport {
    let mut matches: Vec<DetectedFormat> = Vec::new();

    // 1. Binary patches (IPS / BPS / UPS)
    if let Some(patch_fmt) = detect_patch_format(bytes) {
        let (id, detail) = match patch_fmt {
            PatchFormat::Ips => ("patch.ips", "IPS binary patch (PATCH magic)"),
            PatchFormat::Bps => ("patch.bps", "BPS binary patch (BPS1 magic)"),
            PatchFormat::Ups => ("patch.ups", "UPS binary patch (UPS1 magic)"),
        };
        matches.push(DetectedFormat {
            id: id.to_string(),
            category: "patch",
            confidence: "high",
            detail: detail.to_string(),
        });
    }

    // 2. Standalone Mobiclip video streams (MOC5 / VXGB)
    if bytes.starts_with(b"MOC5") {
        let detail = match MobiclipStreamView::parse_at(bytes, 0) {
            Ok(v) => format!(
                "Mobiclip v1 stream ({}x{}, {} video frames)",
                v.header.width, v.header.height, v.header.num_video_frames
            ),
            Err(_) => "Mobiclip v1 stream (MOC5 magic)".to_string(),
        };
        matches.push(DetectedFormat {
            id: "video.mobiclip-v1".to_string(),
            category: "video",
            confidence: "high",
            detail,
        });
    } else if bytes.starts_with(b"VXGB") {
        let detail = match VxgbStreamView::parse_at(bytes, 0) {
            Ok(v) => format!(
                "Mobiclip v2 stream ({}x{}, {} frames)",
                v.header.width, v.header.height, v.header.num_frames
            ),
            Err(_) => "Mobiclip v2 stream (VXGB magic)".to_string(),
        };
        matches.push(DetectedFormat {
            id: "video.mobiclip-v2".to_string(),
            category: "video",
            confidence: "high",
            detail,
        });
    }

    // 3. Standalone studio archives at offset 0 (PACK / WAYFORWARD / SFCD)
    if let Ok(arch) = parse_archive_at(bytes, 0) {
        let (id, label) = match arch.format {
            ArchiveFormat::Pack => ("archive.pack", "PACK"),
            ArchiveFormat::WayForward => ("archive.wayforward", "WAYFORWARD"),
            ArchiveFormat::Sfcd => ("archive.sfcd", "SFCD"),
        };
        matches.push(DetectedFormat {
            id: id.to_string(),
            category: "archive",
            confidence: "high",
            detail: format!("{label} archive ({} entries)", arch.entries.len()),
        });
    }

    // 4. e-Reader VPK0 compressed payload
    if bytes.starts_with(b"vpk0") && bytes.len() >= 8 {
        let decomp_size = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        matches.push(DetectedFormat {
            id: "ereader.vpk0".to_string(),
            category: "ereader",
            confidence: "high",
            detail: format!("e-Reader VPK0 compressed stream (declared output {decomp_size} bytes)"),
        });
    }

    // 5. e-Reader DotCode raw strip (.raw)
    if bytes.len() == SHORT_STRIP_RAW_LEN || bytes.len() == LONG_STRIP_RAW_LEN {
        if let Ok(bin) = raw_to_bin(bytes) {
            let kind = if bytes.len() == SHORT_STRIP_RAW_LEN {
                "short"
            } else {
                "long"
            };
            let detail = match parse_bin(&bin) {
                Ok((hdr, payload)) => format!(
                    "e-Reader {kind} DotCode strip (strip {}/{}, {} bytes payload)",
                    hdr.strip_index + 1,
                    hdr.strip_count,
                    payload.len()
                ),
                Err(_) => format!("e-Reader {kind} DotCode strip ({} bytes raw)", bytes.len()),
            };
            matches.push(DetectedFormat {
                id: "ereader.dotcode-raw".to_string(),
                category: "ereader",
                confidence: "high",
                detail,
            });
        }
    }

    // 6. GBA Cartridge ROM (AGB header + complement checksum + entry branch + logo region)
    if bytes.len() >= gbakit::rom::HEADER_SIZE {
        if let Ok(hdr) = AgbHeader::parse(bytes) {
            if hdr.is_valid() {
                let has_arm_branch = (hdr.entry_opcode & 0x0F00_0000) == 0x0A00_0000;
                let has_logo_data = bytes[0x04..0xA0].iter().any(|&b| b != 0);
                if has_arm_branch || has_logo_data {
                    let confidence = if has_arm_branch && has_logo_data {
                        "high"
                    } else {
                        "medium"
                    };
                    matches.push(DetectedFormat {
                        id: "rom.agb".to_string(),
                        category: "rom",
                        confidence,
                        detail: format!(
                            "GBA ROM '{}' ({}, maker {}, rev {}, checksum valid)",
                            hdr.title, hdr.game_code, hdr.maker_code, hdr.software_version
                        ),
                    });
                }
            }
        }
    }

    // 7. GBA Save file (probe_loose_save + canonical save geometry)
    // Only probe plausible save sizes (512 B ..= 256 KiB) and avoid false positives on ROMs/patches/etc.
    if matches.is_empty() && (512..=262_144).contains(&bytes.len()) {
        let geom = SaveGeometry::detect(bytes);
        let mut matched_save = false;

        if let Ok(defs) = gbakit::saves::all() {
            let probes = probe_loose_save(bytes, &defs);
            if let Some(best) = probes.first() {
                if best.magic_matched || best.checksums_valid {
                    let confidence = if best.magic_matched && best.checksums_valid {
                        "high"
                    } else {
                        "medium"
                    };
                    matches.push(DetectedFormat {
                        id: "save.gba".to_string(),
                        category: "save",
                        confidence,
                        detail: format!(
                            "GBA {} save matching {} ({}, magic={}, checksums={})",
                            best.def.media.as_str(),
                            best.def.game_code,
                            best.def.id,
                            best.magic_matched,
                            best.checksums_valid
                        ),
                    });
                    matched_save = true;
                }
            }
        }

        if !matched_save && is_canonical_save_size(bytes.len()) {
            matches.push(DetectedFormat {
                id: "save.gba".to_string(),
                category: "save",
                confidence: "low",
                detail: format!(
                    "Canonical GBA {} save size ({} bytes, no known magic signature)",
                    geom.media.as_str(),
                    bytes.len()
                ),
            });
        }
    }

    let primary = matches.first().cloned();
    DetectReport {
        path: path_label.to_string(),
        bytes: bytes.len(),
        detected: primary.is_some(),
        primary,
        matches,
    }
}

pub fn cmd_detect(path: &Path, as_json: bool) -> Result<(), String> {
    let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let report = detect_bytes(&path.display().to_string(), &bytes);

    if as_json {
        let text = serde_json::to_string_pretty(&report)
            .map_err(|e| format!("serialize detect report: {e}"))?;
        println!("{text}");
    } else if let Some(primary) = &report.primary {
        println!(
            "{}: {} [{}] ({}) — {}",
            report.path, primary.id, primary.category, primary.confidence, primary.detail
        );
        for extra in report.matches.iter().skip(1) {
            println!(
                "  also: {} [{}] ({}) — {}",
                extra.id, extra.category, extra.confidence, extra.detail
            );
        }
    } else {
        println!("{}: unknown ({} bytes)", report.path, report.bytes);
    }
    Ok(())
}
