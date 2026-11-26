//! CLI subcommands for declarative ROM asset maps (`formatkit.asset-map.v1`)
//! and map-driven asset export (`gbakit map ...`, `gbakit assets export ...`).

use std::fs;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use gbakit::asset_map::{
    export_assets_with_map, generate_asset_map, render_asset_map_markdown, verify_asset_map,
    AssetMap, GenerateOptions,
};

#[derive(Subcommand, Debug)]
pub enum MapCommands {
    /// Auto-generate (or non-destructively --merge) a formatkit.asset-map.v1 JSON map
    Generate {
        /// Path to the GBA ROM image
        rom: PathBuf,
        /// Write asset map JSON to this file instead of stdout
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
        /// Preserve curated mapped/opaque entries from an existing asset map JSON
        #[arg(long = "merge")]
        merge: Option<PathBuf>,
        /// Scan 4-byte-aligned ROM words for 0x08xxxxxx code/data references
        #[arg(long = "with-code-refs")]
        with_code_refs: bool,
        /// Overwrite an existing output file
        #[arg(long = "force")]
        force: bool,
    },
    /// Verify an asset map against a ROM (fingerprint, bounds, hashes, and test-decodes)
    Verify {
        /// Path to the GBA ROM image
        rom: PathBuf,
        /// Path to the formatkit.asset-map.v1 JSON file
        map: PathBuf,
    },
    /// Render a wiki-ready Markdown report from a formatkit.asset-map.v1 JSON file
    Markdown {
        /// Path to the formatkit.asset-map.v1 JSON file
        map: PathBuf,
        /// Optional ROM path to run verification and embed results in the Markdown
        #[arg(long = "rom")]
        rom: Option<PathBuf>,
        /// Write Markdown output to this file instead of stdout
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
        /// Overwrite an existing output file
        #[arg(long = "force")]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum AssetsCommands {
    /// Export identified and mapped assets from a ROM using a declarative asset map
    Export {
        /// Path to the GBA ROM image
        rom: PathBuf,
        /// Path to a formatkit.asset-map.v1 JSON file (auto-generates if omitted)
        #[arg(long = "asset-map")]
        asset_map: Option<PathBuf>,
        /// Output directory for exported assets and export-manifest.json
        #[arg(short = 'o', long = "output")]
        output: PathBuf,
        /// Allow writing into an existing non-empty output directory
        #[arg(long = "force")]
        force: bool,
    },
}

fn read_asset_map(path: &Path) -> Result<AssetMap, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("parse asset map {}: {e}", path.display()))
}

fn check_file_overwrite(path: &Path, force: bool) -> Result<(), String> {
    if path.exists() && !force {
        return Err(format!(
            "refusing to overwrite existing file {} without --force",
            path.display()
        ));
    }
    Ok(())
}

pub fn dispatch_map(cmd: MapCommands, as_json: bool) -> Result<(), String> {
    match cmd {
        MapCommands::Generate {
            rom,
            output,
            merge,
            with_code_refs,
            force,
        } => {
            if let Some(out_path) = &output {
                check_file_overwrite(out_path, force)?;
            }
            let rom_bytes =
                fs::read(&rom).map_err(|e| format!("read {}: {e}", rom.display()))?;
            let merge_map = match merge {
                Some(ref p) => Some(read_asset_map(p)?),
                None => None,
            };
            let map = generate_asset_map(
                &rom_bytes,
                &GenerateOptions { with_code_refs },
                merge_map.as_ref(),
            );
            let json_text = serde_json::to_string_pretty(&map)
                .map_err(|e| format!("serialize asset map: {e}"))?;

            if let Some(out_path) = output {
                if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("create dir {}: {e}", parent.display()))?;
                }
                fs::write(&out_path, &json_text)
                    .map_err(|e| format!("write {}: {e}", out_path.display()))?;
                if as_json {
                    let summary = serde_json::json!({
                        "schema": map.schema,
                        "entries": map.entries.len(),
                        "total_bytes": map.media.total_bytes,
                        "rom_sha256": map.media.fingerprint.rom_sha256,
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())?
                    );
                } else {
                    println!(
                        "Wrote {} ({} entries, {} bytes)",
                        out_path.display(),
                        map.entries.len(),
                        rom_bytes.len()
                    );
                }
            } else {
                println!("{json_text}");
            }
            Ok(())
        }
        MapCommands::Verify { rom, map } => {
            let rom_bytes =
                fs::read(&rom).map_err(|e| format!("read {}: {e}", rom.display()))?;
            let asset_map = read_asset_map(&map)?;
            let report = verify_asset_map(&rom_bytes, &asset_map);

            if as_json {
                let text = serde_json::to_string_pretty(&report)
                    .map_err(|e| format!("serialize verify report: {e}"))?;
                println!("{text}");
            } else {
                println!(
                    "Asset Map Verification: {}",
                    if report.valid { "PASS" } else { "FAIL" }
                );
                println!("  Total Bytes:    {}", report.total_bytes);
                println!(
                    "  Identified:     {} bytes ({:.2}%, {} entries)",
                    report.identified_bytes, report.identified_pct, report.identified_count
                );
                println!(
                    "  Mapped:         {} bytes ({:.2}%, {} entries)",
                    report.mapped_bytes, report.mapped_pct, report.mapped_count
                );
                println!(
                    "  Opaque:         {} bytes ({:.2}%, {} entries)",
                    report.opaque_bytes, report.opaque_pct, report.opaque_count
                );
                println!(
                    "  Unknown:        {} bytes ({:.2}%, {} entries)",
                    report.unknown_bytes, report.unknown_pct, report.unknown_count
                );
                println!("  Coverage:       {:.2}%", report.coverage_pct);
                println!(
                    "  Decoded:        {}/{} entries",
                    report.entries_decoded, report.entries_checked
                );
                if !report.errors.is_empty() {
                    println!("  Errors ({}):", report.errors.len());
                    for err in &report.errors {
                        println!("    - {err}");
                    }
                }
            }

            if report.valid {
                Ok(())
            } else {
                Err(format!(
                    "asset map verification failed with {} error(s)",
                    report.errors.len()
                ))
            }
        }
        MapCommands::Markdown {
            map,
            rom,
            output,
            force,
        } => {
            if let Some(out_path) = &output {
                check_file_overwrite(out_path, force)?;
            }
            let asset_map = read_asset_map(&map)?;
            let verify_report = match rom {
                Some(ref rom_path) => {
                    let rom_bytes = fs::read(rom_path)
                        .map_err(|e| format!("read {}: {e}", rom_path.display()))?;
                    Some(verify_asset_map(&rom_bytes, &asset_map))
                }
                None => None,
            };
            let md = render_asset_map_markdown(&asset_map, verify_report.as_ref());

            if let Some(out_path) = output {
                if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("create dir {}: {e}", parent.display()))?;
                }
                fs::write(&out_path, &md)
                    .map_err(|e| format!("write {}: {e}", out_path.display()))?;
                if as_json {
                    let summary = serde_json::json!({
                        "entries": asset_map.entries.len(),
                        "markdown_bytes": md.len(),
                    });
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())?
                    );
                } else {
                    println!("Wrote {} ({} bytes)", out_path.display(), md.len());
                }
            } else if as_json {
                let payload = serde_json::json!({
                    "markdown": md,
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
                );
            } else {
                print!("{md}");
            }
            Ok(())
        }
    }
}

pub fn dispatch_assets(cmd: AssetsCommands, as_json: bool) -> Result<(), String> {
    match cmd {
        AssetsCommands::Export {
            rom,
            asset_map,
            output,
            force,
        } => {
            let rom_bytes =
                fs::read(&rom).map_err(|e| format!("read {}: {e}", rom.display()))?;
            let map = match asset_map {
                Some(ref p) => read_asset_map(p)?,
                None => generate_asset_map(&rom_bytes, &GenerateOptions::default(), None),
            };
            let report = export_assets_with_map(&rom_bytes, &map, &output, force)?;
            if as_json {
                let text = serde_json::to_string_pretty(&report)
                    .map_err(|e| format!("serialize export report: {e}"))?;
                println!("{text}");
            } else {
                println!(
                    "Exported {} asset entries ({} files) to {}",
                    report.exported_entries,
                    report.files_written,
                    output.display()
                );
            }
            Ok(())
        }
    }
}
