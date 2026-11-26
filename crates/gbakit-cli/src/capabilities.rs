//! Code-generated build identity (`formatkit.build-identity.v1`) and
//! capability manifest (`formatkit.capabilities.v1`).

use serde::Serialize;

pub const BUILD_IDENTITY_SCHEMA: &str = "formatkit.build-identity.v1";
pub const CAPABILITIES_SCHEMA: &str = "formatkit.capabilities.v1";
pub const TOOL_NAME: &str = "gbakit";
pub const TOOL_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const FORMATKIT_VERSION: &str = env!("GBAKIT_FORMATKIT_VERSION");

/// Machine-readable build identity emitted by `gbakit --version --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BuildIdentity {
    pub schema: &'static str,
    pub tool: &'static str,
    pub version: &'static str,
    pub git_commit: Option<&'static str>,
    pub git_dirty: bool,
    pub formatkit_version: &'static str,
    pub target: &'static str,
    pub profile: &'static str,
}

impl BuildIdentity {
    pub fn current() -> Self {
        let raw_commit = env!("GBAKIT_GIT_COMMIT");
        let git_commit = if raw_commit.is_empty() {
            None
        } else {
            Some(raw_commit)
        };
        let git_dirty = env!("GBAKIT_GIT_DIRTY") == "true";

        Self {
            schema: BUILD_IDENTITY_SCHEMA,
            tool: TOOL_NAME,
            version: TOOL_VERSION,
            git_commit,
            git_dirty,
            formatkit_version: FORMATKIT_VERSION,
            target: env!("GBAKIT_TARGET"),
            profile: env!("GBAKIT_PROFILE"),
        }
    }
}

/// A single format entry in `formatkit.capabilities.v1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FormatCapability {
    pub id: &'static str,
    pub category: &'static str,
    pub capabilities: Vec<&'static str>,
    pub tier: u8,
    pub notes: &'static str,
}

/// Machine-readable capability manifest emitted by `gbakit capabilities --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilitiesManifest {
    pub schema: &'static str,
    pub tool: &'static str,
    pub version: &'static str,
    pub formatkit_version: &'static str,
    pub formats: Vec<FormatCapability>,
    pub save_engines: Vec<&'static str>,
    pub patch_formats: Vec<&'static str>,
}

impl CapabilitiesManifest {
    pub fn generate() -> Self {
        Self {
            schema: CAPABILITIES_SCHEMA,
            tool: TOOL_NAME,
            version: TOOL_VERSION,
            formatkit_version: FORMATKIT_VERSION,
            formats: vec![
                FormatCapability {
                    id: "rom.agb-header",
                    category: "rom",
                    capabilities: vec!["parse", "decode"],
                    tier: 3,
                    notes: "192-byte AGB cartridge header, complement checksum, SDK save tags, trailing padding, and peripheral signatures",
                },
                FormatCapability {
                    id: "rom.multiboot",
                    category: "rom",
                    capabilities: vec!["parse", "decode", "reproduce"],
                    tier: 3,
                    notes: "Embedded 256 KiB EWRAM MultiBoot payload scanning, extraction, and serial link server",
                },
                FormatCapability {
                    id: "code.arm-thumb-analysis",
                    category: "code",
                    capabilities: vec!["parse", "decode"],
                    tier: 2,
                    notes: "ARM/Thumb pointer-table scanning, boot reachability trace, and callgraph extraction",
                },
                FormatCapability {
                    id: "compress.bios-lz77",
                    category: "compress",
                    capabilities: vec!["parse", "decode", "reproduce"],
                    tier: 3,
                    notes: "GBA BIOS SWI 0x11/0x12 LZ77 (VRAM-safe and WRAM) and LZ11 codecs",
                },
                FormatCapability {
                    id: "compress.bios-rle",
                    category: "compress",
                    capabilities: vec!["parse", "decode", "reproduce"],
                    tier: 3,
                    notes: "GBA BIOS SWI 0x14/0x15 run-length encoding",
                },
                FormatCapability {
                    id: "compress.bios-huffman",
                    category: "compress",
                    capabilities: vec!["parse", "decode", "reproduce"],
                    tier: 3,
                    notes: "GBA BIOS SWI 0x13 4-bit and 8-bit Huffman codecs",
                },
                FormatCapability {
                    id: "compress.bios-diff",
                    category: "compress",
                    capabilities: vec!["parse", "decode", "reproduce"],
                    tier: 3,
                    notes: "GBA BIOS SWI 0x16/0x18 8-bit and 16-bit differential filters",
                },
                FormatCapability {
                    id: "compress.studio-codecs",
                    category: "compress",
                    capabilities: vec!["parse", "decode"],
                    tier: 2,
                    notes: "Third-party GBA studio codecs (Hudson LZ, Konami dictionary, Shin'en Mode 7)",
                },
                FormatCapability {
                    id: "gfx.gba-tiles-palettes",
                    category: "gfx",
                    capabilities: vec!["parse", "decode", "reproduce", "edit"],
                    tier: 3,
                    notes: "BGR555 palettes, 4bpp/8bpp 8x8 tiles, OBJ sprites, and BG screenblock tilemaps (SCR/TMX)",
                },
                FormatCapability {
                    id: "audio.m4a-sappy",
                    category: "audio",
                    capabilities: vec!["parse", "decode", "reproduce", "edit"],
                    tier: 3,
                    notes: "MP2000/M4A voicegroups, song tables, PCM/DPCM/IMA-ADPCM sample replacement, and WAV/MIDI/SF2 export",
                },
                FormatCapability {
                    id: "audio.arm-sound-engines",
                    category: "audio",
                    capabilities: vec!["parse", "decode"],
                    tier: 2,
                    notes: "13 GBA ARM audio engines (M4A, GAX, MusyX, Krawall, Konami KCE, Rare, Logik State, Southpaw, RD1 Kaede, Paragon5, EAD SMA, Engine SW, Cosmigo Sonix) with WAV/SF2 extraction",
                },
                FormatCapability {
                    id: "video.mobiclip",
                    category: "video",
                    capabilities: vec!["parse", "decode"],
                    tier: 3,
                    notes: "ActImagine Mobiclip MOC5 (GBA v1) and VXGB (GBA v2) video and audio streams",
                },
                FormatCapability {
                    id: "ereader.dotcode",
                    category: "ereader",
                    capabilities: vec!["parse", "decode", "reproduce", "author"],
                    tier: 3,
                    notes: "GBA e-Reader short/long DotCode strips, Reed-Solomon ECC, and BMP/PBM/PGM strip rendering",
                },
                FormatCapability {
                    id: "ereader.vpk0",
                    category: "ereader",
                    capabilities: vec!["parse", "decode", "reproduce", "author"],
                    tier: 3,
                    notes: "e-Reader VPK0 LZ/Huffman compression (methods 0 and 1)",
                },
                FormatCapability {
                    id: "archive.pack",
                    category: "archive",
                    capabilities: vec!["parse", "decode", "reproduce", "edit"],
                    tier: 2,
                    notes: "PACK studio container scanning, extraction, repacking, and in-ROM replacement",
                },
                FormatCapability {
                    id: "archive.wayforward",
                    category: "archive",
                    capabilities: vec!["parse", "decode", "reproduce", "edit"],
                    tier: 2,
                    notes: "WAYFORWARD container scanning, extraction, repacking, and in-ROM replacement",
                },
                FormatCapability {
                    id: "archive.sfcd",
                    category: "archive",
                    capabilities: vec!["parse", "decode", "reproduce", "edit"],
                    tier: 2,
                    notes: "SFCD container scanning, extraction, repacking, and in-ROM replacement",
                },
                FormatCapability {
                    id: "text.charmap-scripts",
                    category: "text",
                    capabilities: vec!["parse", "decode", "reproduce", "edit"],
                    tier: 2,
                    notes: "Charmap table scanning, string extraction, and pointer-aware in-ROM replacement",
                },
                FormatCapability {
                    id: "text.huffman-scripts",
                    category: "text",
                    capabilities: vec!["parse", "decode", "reproduce", "edit"],
                    tier: 2,
                    notes: "IS-style and Camelot-style Huffman compressed text extraction and replacement",
                },
                FormatCapability {
                    id: "text.vwf-fonts",
                    category: "text",
                    capabilities: vec!["parse", "decode", "reproduce", "edit"],
                    tier: 2,
                    notes: "1bpp/2bpp variable-width bitmap font dumping, replacement, and glyph width tables",
                },
                FormatCapability {
                    id: "save.gba-schema-v2",
                    category: "save",
                    capabilities: vec!["parse", "decode", "reproduce", "edit"],
                    tier: 3,
                    notes: "Declarative Schema v2 save inspection, JSON export/import, field editing, checksum verification/repair, and canonical trimming across EEPROM 512B/8KB, SRAM 32KB, and Flash 64KB/128KB",
                },
                FormatCapability {
                    id: "patch.ips-bps-ups",
                    category: "patch",
                    capabilities: vec!["parse", "decode", "reproduce", "author"],
                    tier: 3,
                    notes: "IPS, BPS, and UPS binary patch creation and CRC-verified application",
                },
                FormatCapability {
                    id: "link.gba-protocols",
                    category: "link",
                    capabilities: vec!["parse", "decode"],
                    tier: 3,
                    notes: "Multi-Play 16-bit, MultiBoot, Normal SPI, GameCube JOYBus, and Wireless Adapter (AGB-015 / RFU) capture dissection and virtual link bridge",
                },
                FormatCapability {
                    id: "map.asset-map-v1",
                    category: "map",
                    capabilities: vec!["parse", "decode", "reproduce", "author"],
                    tier: 2,
                    notes: "Declarative formatkit.asset-map.v1 ROM asset map generation, merge, verification, Markdown reporting, and map-driven export",
                },
            ],
            save_engines: vec![
                "gba-save-v2",
                "eeprom-512b",
                "eeprom-8k",
                "sram-32k",
                "flash-64k",
                "flash-128k",
            ],
            patch_formats: vec!["ips", "bps", "ups"],
        }
    }

    pub fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "{} {} (FormatKit {})\n",
            self.tool, self.version, self.formatkit_version
        ));
        out.push_str(&format!("Schema: {}\n\n", self.schema));
        out.push_str("Formats:\n");
        for fmt in &self.formats {
            out.push_str(&format!(
                "  - {} [{}] (tier {}): {}\n    {}\n",
                fmt.id,
                fmt.category,
                fmt.tier,
                fmt.capabilities.join(", "),
                fmt.notes
            ));
        }
        out.push_str(&format!(
            "\nSave Engines:  {}\n",
            self.save_engines.join(", ")
        ));
        out.push_str(&format!(
            "Patch Formats: {}\n",
            self.patch_formats.join(", ")
        ));
        out
    }

    #[allow(dead_code)]
    pub fn render_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# gbakit Capabilities\n\n");
        out.push_str(&format!(
            "Generated directly from `gbakit capabilities --json` (`{}`) for **{} {}** (FormatKit `{}`).\n\n",
            self.schema, self.tool, self.version, self.formatkit_version
        ));
        out.push_str("## Supported Formats\n\n");
        out.push_str("| ID | Category | Capabilities | Tier | Notes |\n");
        out.push_str("|---|---|---|---:|---|\n");
        for fmt in &self.formats {
            out.push_str(&format!(
                "| `{}` | `{}` | {} | {} | {} |\n",
                fmt.id,
                fmt.category,
                fmt.capabilities.join(", "),
                fmt.tier,
                fmt.notes
            ));
        }
        out.push_str("\n## Save Engines\n\n");
        for engine in &self.save_engines {
            out.push_str(&format!("- `{engine}`\n"));
        }
        out.push_str("\n## Patch Formats\n\n");
        for patch in &self.patch_formats {
            out.push_str(&format!("- `{patch}`\n"));
        }
        out
    }
}

pub fn cmd_capabilities(as_json: bool) -> Result<(), String> {
    let manifest = CapabilitiesManifest::generate();
    if as_json {
        let text = serde_json::to_string_pretty(&manifest)
            .map_err(|e| format!("serialize capabilities: {e}"))?;
        println!("{text}");
    } else {
        print!("{}", manifest.render_text());
    }
    Ok(())
}
