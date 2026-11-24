#!/usr/bin/env python3
"""
Generate crates/gba-patches/src/lib.rs from data/debug-menu-patches.json.
"""

import json
import os
from pathlib import Path

GBAKIT_ROOT = Path(__file__).resolve().parent.parent
RESEARCH_ROOT = Path(os.environ.get("GBA_RESEARCH_ROOT", str(GBAKIT_ROOT.parent / "gba-research")))

p = json.loads((RESEARCH_ROOT / "data/debug-menu-patches.json").read_text(encoding="utf-8"))
verified = [r for r in p["results"] if r.get("verdict") == "VERIFIED_INTERACTIVE_DEBUG_MENU"]
verified.sort(key=lambda x: x["game_code"])
patch_dir = RESEARCH_ROOT / "wiki-site/assets/patches"

lines = [
    "//! Clean-room Game Boy Advance (AGB) verified debug-menu re-enablement patch registry.",
    "//!",
    "//! All patches in this crate are micro-patches (15–30 bytes) reverse-engineered strictly",
    "//! from clean-room static analysis and verified live in headless mGBA.",
    "",
    "#![deny(unsafe_code)]",
    "",
    "use formatkit_core::Result;",
    "use formatkit_patch::ips::{ApplyOptions, Patch};",
    "use gba_rom::AgbHeader;",
    "use serde::Serialize;",
    "",
    "/// Metadata and embedded IPS micro-patch payload for a verified GBA debug menu.",
    "#[derive(Debug, Clone, PartialEq, Eq, Serialize)]",
    "pub struct DebugMenuPatch {",
    "    pub game_code: &'static str,",
    "    pub release_key: &'static str,",
    "    pub display_name: &'static str,",
    "    pub description: &'static str,",
    "    pub hook_type: &'static str,",
    "    pub target_handler: u32,",
    "    pub input_sequence: &'static [&'static str],",
    "    #[serde(skip)]",
    "    pub ips_bytes: &'static [u8],",
    "}",
    "",
    "impl DebugMenuPatch {",
    "    /// Apply this debug menu patch to an input ROM buffer, returning the patched ROM.",
    "    pub fn apply(&self, rom_bytes: &[u8]) -> Result<Vec<u8>> {",
    "        let patch = Patch::parse(self.ips_bytes)?;",
    "        patch.apply(rom_bytes, ApplyOptions::default())",
    "    }",
    "}",
    "",
    "/// Clean-room registry of verified GBA debug menu patches.",
    f"pub const REGISTRY: &[DebugMenuPatch] = &[",
]

for r in verified:
    gc = r["game_code"]
    rk = r["release_key"]
    name = r["display_name"].replace('"', '\\"')
    sel = r["selected_attempt"]
    desc = sel["description"].replace('"', '\\"')
    hook = sel["hook_type"]
    target_str = sel["target_handler"]
    target = int(target_str, 16) if isinstance(target_str, str) else target_str
    inputs = sel.get("after_input", {}).get("input_sequence", [])

    ips_candidates = list(patch_dir.glob(f"{rk}/*-debug.ips"))
    if not ips_candidates:
        ips_candidates = list(patch_dir.glob(f"{gc}*/*-debug.ips"))
    ips_bytes = ips_candidates[0].read_bytes()
    hex_str = ", ".join(f"0x{b:02x}" for b in ips_bytes)
    inputs_str = ", ".join(f'"{inp}"' for inp in inputs)

    lines.append("    DebugMenuPatch {")
    lines.append(f'        game_code: "{gc}",')
    lines.append(f'        release_key: "{rk}",')
    lines.append(f'        display_name: "{name}",')
    lines.append(f'        description: "{desc}",')
    lines.append(f'        hook_type: "{hook}",')
    lines.append(f"        target_handler: 0x{target:08X},")
    lines.append(f"        input_sequence: &[{inputs_str}],")
    lines.append(f"        ips_bytes: &[{hex_str}],")
    lines.append("    },")

lines.extend([
    "];",
    "",
    "/// Look up a verified debug menu patch by 4-character game code (e.g., `\"A84E\"`).",
    "pub fn find_by_code(code: &str) -> Option<&'static DebugMenuPatch> {",
    "    let code_upper = code.to_ascii_uppercase();",
    "    REGISTRY.iter().find(|p| p.game_code == code_upper)",
    "}",
    "",
    "/// Look up a verified debug menu patch by exact release key (e.g., `\"A84E-r0-2e3a8e3962c9d5fb\"`).",
    "pub fn find_by_release_key(key: &str) -> Option<&'static DebugMenuPatch> {",
    "    REGISTRY.iter().find(|p| p.release_key == key)",
    "}",
    "",
    "/// Look up a verified debug menu patch for a GBA ROM slice by parsing its header.",
    "pub fn find_for_rom(rom_bytes: &[u8]) -> Option<&'static DebugMenuPatch> {",
    "    let hdr = AgbHeader::parse(rom_bytes).ok()?;",
    "    find_by_code(&hdr.game_code)",
    "}",
    "",
    "#[cfg(test)]",
    "mod tests {",
    "    use super::*;",
    "",
    "    #[test]",
    "    fn test_registry_size() {",
    f"        assert_eq!(REGISTRY.len(), {len(verified)});",
    "    }",
    "",
    "    #[test]",
    "    fn test_all_ips_patches_parse() {",
    "        for p in REGISTRY {",
    "            let patch = Patch::parse(p.ips_bytes).expect(\"valid IPS bytes\");",
    "            assert!(!patch.records.is_empty(), \"patch has records\");",
    "        }",
    "    }",
    "",
    "    #[test]",
    "    fn test_find_by_code() {",
    '        let ham = find_by_code("a84e").expect("found Hamtaro patch");',
    '        assert_eq!(ham.game_code, "A84E");',
    "        assert_eq!(ham.target_handler, 0x08084378);",
    '        assert_eq!(ham.input_sequence, &["A", "DOWN"]);',
    "    }",
    "",
    "    #[test]",
    "    fn test_apply_patch() {",
    '        let p = find_by_code("BN2E").expect("found BN2E");',
    "        // BN2E modifies ROM offset 0x0690: 0x01 -> 0x00",
    "        let mut rom = vec![0u8; 0x1000];",
    "        rom[0x690] = 0x01;",
    '        let patched = p.apply(&rom).expect("applied successfully");',
    "        assert_eq!(patched[0x690], 0x00);",
    "    }",
    "}",
])

out_dir = GBAKIT_ROOT / "crates/gba-patches/src"
out_dir.mkdir(parents=True, exist_ok=True)
(out_dir / "lib.rs").write_text("\n".join(lines) + "\n", encoding="utf-8")
print(f"Generated {out_dir / 'lib.rs'} ({len(verified)} patches, {len(lines)} lines)")
