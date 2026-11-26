//! Clean-room Game Boy Advance (AGB) verified debug-menu re-enablement patch registry.
//!
//! All patches in this crate are micro-patches (15–45 bytes) reverse-engineered strictly
//! from clean-room static analysis and verified live in headless mGBA.

#![deny(unsafe_code)]

use formatkit_core::Result;
use formatkit_patch::ips::{ApplyOptions, Patch};
use gba_rom::AgbHeader;
use serde::Serialize;

pub mod cheats;

pub use cheats::{
    find_button_combo, find_button_combo_by_release_key, list_button_combos, ButtonComboCheat,
    BUTTON_COMBO_REGISTRY,
};
pub use formatkit_core::checksum::crc32_iso_hdlc as crc32;
pub use formatkit_patch::bps::{self, BpsAction, BpsPatch};
pub use formatkit_patch::ups::{self, UpsChunk, UpsPatch};
pub use formatkit_patch::vli::{
    self, decode_vli, decode_vli_from_slice, encode_vli, encode_vli_vec,
};

/// BPS errors are reported through the shared FormatKit error type.
pub type BpsError = formatkit_core::Error;
/// UPS errors are reported through the shared FormatKit error type.
pub type UpsError = formatkit_core::Error;

/// Supported ROM patch formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchFormat {
    /// International Patching System (IPS).
    Ips,
    /// Universal Patching System (UPS).
    Ups,
    /// Beat Patching System (BPS).
    Bps,
}

impl std::fmt::Display for PatchFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ips => write!(f, "IPS"),
            Self::Ups => write!(f, "UPS"),
            Self::Bps => write!(f, "BPS"),
        }
    }
}

/// Detects the patch format from header magic bytes.
pub fn detect_format(bytes: &[u8]) -> Option<PatchFormat> {
    if bytes.starts_with(b"PATCH") {
        Some(PatchFormat::Ips)
    } else if bytes.starts_with(b"UPS1") {
        Some(PatchFormat::Ups)
    } else if bytes.starts_with(b"BPS1") {
        Some(PatchFormat::Bps)
    } else {
        None
    }
}

/// Error type for unified patch application.
#[derive(Debug)]
pub enum UnifiedPatchError {
    UnknownFormat,
    Patch {
        format: PatchFormat,
        error: formatkit_core::Error,
    },
}

impl std::fmt::Display for UnifiedPatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownFormat => write!(
                f,
                "Unknown or unsupported patch format (expected IPS, UPS, or BPS header)"
            ),
            Self::Patch { format, error } => write!(f, "{format} error: {error}"),
        }
    }
}

impl std::error::Error for UnifiedPatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Patch { error, .. } => Some(error),
            Self::UnknownFormat => None,
        }
    }
}

/// Applies a patch (IPS, UPS, or BPS) to source bytes with automatic format detection.
pub fn apply_patch_unified(
    patch_bytes: &[u8],
    source_bytes: &[u8],
) -> std::result::Result<Vec<u8>, UnifiedPatchError> {
    let format = detect_format(patch_bytes).ok_or(UnifiedPatchError::UnknownFormat)?;
    match format {
        PatchFormat::Ips => Patch::parse(patch_bytes)
            .and_then(|patch| patch.apply(source_bytes, ApplyOptions::default())),
        PatchFormat::Ups => UpsPatch::parse(patch_bytes).and_then(|patch| patch.apply(source_bytes)),
        PatchFormat::Bps => BpsPatch::parse(patch_bytes).and_then(|patch| patch.apply(source_bytes)),
    }
    .map_err(|error| UnifiedPatchError::Patch { format, error })
}

/// Metadata and embedded IPS micro-patch payload for a verified GBA debug menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DebugMenuPatch {
    pub game_code: &'static str,
    pub release_key: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
    pub hook_type: &'static str,
    pub target_handler: u32,
    pub input_sequence: &'static [&'static str],
    #[serde(skip)]
    pub ips_bytes: &'static [u8],
}

impl DebugMenuPatch {
    /// Apply this debug menu patch to an input ROM buffer, returning the patched ROM.
    pub fn apply(&self, rom_bytes: &[u8]) -> Result<Vec<u8>> {
        let patch = Patch::parse(self.ips_bytes)?;
        patch.apply(rom_bytes, ApplyOptions::default())
    }
}

/// Clean-room registry of verified GBA debug menu patches.
pub const REGISTRY: &[DebugMenuPatch] = &[
    DebugMenuPatch {
        game_code: "A29J",
        release_key: "A29J-r0-b96b203aecf90146",
        display_name: "Mickey to Minnie no Magical Quest 2 (Japan)",
        description: "Patch scene dispatcher at 0x0800698c (movs r0, #0 -> movs r0, #20) to select Capcom test mode menu slot 10 (0x0801996d)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0801996C,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x69, 0x8c, 0x00, 0x02, 0x14, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "A6SJ",
        release_key: "A6SJ-r0-12b706b637a6504c",
        display_name: "Super Robot Taisen D (Japan)",
        description: "Patch scene dispatcher at 0x08004cfa (ldr r0,[r4] -> movs r0,#49) to select jump-table index 48 (AGB SYSTEM DEBUG MENU at 0x08004d88)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08004D88,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x4c, 0xfa, 0x00, 0x02, 0x31, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "A84E",
        release_key: "A84E-r0-2e3a8e3962c9d5fb",
        display_name: "Hamtaro - Rainbow Rescue (USA) (Proto) (2003-07-29)",
        description: "Patch conditional branch at 0x080819dc (beq -> nop) to force fall-through into debug menu branch (0x08084378)",
        hook_type: "thumb_branch_nop_override",
        target_handler: 0x08084378,
        input_sequence: &["A", "DOWN"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x08, 0x19, 0xdc, 0x00, 0x02, 0xc0, 0x46, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "A8DJ",
        release_key: "A8DJ-r0-756a546899a1fdf4",
        display_name: "Doubutsu-jima no Chobigurumi (Japan)",
        description: "Patch scene dispatcher at 0x0800599a (ldr r0,[r4] -> movs r0,#49) to select AGB SYSTEM DEBUG MENU",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x080059BC,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x59, 0x9a, 0x00, 0x02, 0x31, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AABE",
        release_key: "AABE-r0-294e7c34a9d2ee37",
        display_name: "American Bass Challenge (USA)",
        description: "Patch branch at 0x08000262 (beq -> b #0x8000276) to branch directly to debug menu launcher and nop spinloop at 0x08000baa",
        hook_type: "thumb_branch_override",
        target_handler: 0x08000276,
        input_sequence: &["DOWN", "RIGHT", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x02, 0x62, 0x00, 0x02, 0x08, 0xe0, 0x00, 0x0b, 0xaa, 0x00, 0x02, 0xc0, 0x46, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "ABRE",
        release_key: "ABRE-r0-5d9a18f14392d823",
        display_name: "Blender Bros. (USA)",
        description: "Patch scene dispatcher at 0x080048c2 (ldr r0,[r4] -> movs r0,#44) after boot init to select jump-table index 43 (0x080049a4 -> AGB SYSTEM DEBUG MENU 0x08008fd9)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08008FD8,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x48, 0xc2, 0x00, 0x02, 0x2c, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "ACBE",
        release_key: "ACBE-r0-e9d2ea6862408043",
        display_name: "Car Battler Joe (USA)",
        description: "Replace real scene descriptor table slot 0 (0x08375050, 12 bytes) with debug scene triplet {0x08039251, 0x080392d1, 0x080392e9}",
        hook_type: "func_ptr_table_triplet",
        target_handler: 0x08039250,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x37, 0x50, 0x50, 0x00, 0x0c, 0x51, 0x92, 0x03, 0x08, 0xd1, 0x92, 0x03, 0x08, 0xe9, 0x92, 0x03, 0x08, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AEKJ",
        release_key: "AEKJ-r0-647e501388b5216f",
        display_name: "Elemix! (Japan)",
        description: "Patch scene dispatcher at 0x080004ca (movs r0, #12 -> movs r0, #16) to select AGB SYSTEM DEBUG MENU",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08000520,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x04, 0xca, 0x00, 0x02, 0x10, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AF5E",
        release_key: "AF5E-r0-33bcb4f2b3591187",
        display_name: "Shining Force - Resurrection of the Dark Dragon (USA)",
        description: "Replace scene-factory jump-table boot slots (0x0800b420 & 0x0800b424 -> bl 0x08002700) with DebugMenuScene constructor stub (0x0800b4cc -> bl 0x08000778)",
        hook_type: "state_jump_table_ptr",
        target_handler: 0x08000778,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0xb4, 0x20, 0x00, 0x04, 0xcc, 0xb4, 0x00, 0x08, 0x00, 0xb4, 0x24, 0x00, 0x04, 0xcc, 0xb4, 0x00, 0x08, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AJ9J",
        release_key: "AJ9J-r0-1a341f6f246ede62",
        display_name: "Super Robot Taisen R (Japan)",
        description: "Patch scene dispatcher at 0x08004d72 (ldr r0,[r4] -> movs r0,#49) to select jump-table index 48 (AGB SYSTEM DEBUG MENU at 0x08004e60)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08004E60,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x4d, 0x72, 0x00, 0x02, 0x31, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AOIE",
        release_key: "AOIE-r0-55158d0a74d6ec54",
        display_name: "Shrek - Reekin' Havoc (USA) (En,Fr,De,Es,It,Nl)",
        description: "Replace boot task callback literal at 0x08000d30 (0x08012c89 -> 0x08013aa5) with Stage Select task constructor",
        hook_type: "task_constructor_ptr_redirect",
        target_handler: 0x08013AA4,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x0d, 0x30, 0x00, 0x04, 0xa5, 0x3a, 0x01, 0x08, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "ASRJ",
        release_key: "ASRJ-r0-df99974c58a67ec2",
        display_name: "Super Robot Taisen A (Japan)",
        description: "Patch scene dispatcher at 0x08003832 (ldr r0,[r4] -> movs r0,#40) after boot init to select jump-table index 39 (0x080038f8 -> AGB SYSTEM DEBUG MENU 0x08006369)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08006368,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x38, 0x32, 0x00, 0x02, 0x28, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BFPJ",
        release_key: "BFPJ-r0-d11a800f7730d559",
        display_name: "Futari wa Pretty Cure - Arienaai! Yume no Sono wa Daimeikyuu (Japan)",
        description: "Patch initial scene descriptor literal at 0x08003d70 (0x0802ac3c -> 0x0802a714) to boot precure debug menu",
        hook_type: "scene_descriptor_ptr_redirect",
        target_handler: 0x0802A714,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x3d, 0x70, 0x00, 0x04, 0x14, 0xa7, 0x02, 0x08, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BH2J",
        release_key: "BH2J-r0-f7eba3b4bbd6e0e3",
        display_name: "Hagane no Renkinjutsushi - Omoide no Sonata (Japan)",
        description: "Patch boot scene selector at 0x0801d7d0 (movs r1, #68 -> movs r1, #0) to boot into scene 0 [DEBUG MENU]",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08019318,
        input_sequence: &["RIGHT", "RIGHT", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x01, 0xd7, 0xd0, 0x00, 0x02, 0x00, 0x21, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BHRJ",
        release_key: "BHRJ-r0-49df5e9e12830273",
        display_name: "Hagane no Renkinjutsushi - Meisou no Rondo (Japan)",
        description: "Patch scene table at 0x0817D8A8 (slot 1) with slot 0 debug menu descriptor [DEBUG MENU] (16 bytes)",
        hook_type: "scene_descriptor_table_override",
        target_handler: 0x08014100,
        input_sequence: &["RIGHT", "RIGHT", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x17, 0xd8, 0xa8, 0x00, 0x10, 0x01, 0x41, 0x01, 0x08, 0xe1, 0x41, 0x01, 0x08, 0x1d, 0x47, 0x01, 0x08, 0xe4, 0x81, 0x08, 0x08, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BK6J",
        release_key: "BK6J-r0-44e5793ffd95dd17",
        display_name: "Kouchuu Ouja Mushiking - Greatest Champion e no Michi (Japan)",
        description: "Patch initial task spawn at 0x0800033a (movs r0, #21 -> movs r0, #6) to spawn debug menu process",
        hook_type: "thumb_imm_task_override",
        target_handler: 0x08001090,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x03, 0x3a, 0x00, 0x02, 0x06, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BN2E",
        release_key: "BN2E-r0-9faf9865da9f5d98",
        display_name: "Naruto - Ninja Council 2 (USA)",
        description: "Patch scene dispatcher at 0x08000690 (movs r0, #1 -> movs r0, #0) to select debug dispatcher scene 0 (--- DEBUG MENU ---)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08037DB4,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x06, 0x90, 0x00, 0x02, 0x00, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BPSJ",
        release_key: "BPSJ-r0-9a677452e232f7e8",
        display_name: "Cinnamoroll - Koko ni Iru yo (Japan)",
        description: "Patch scene dispatcher at 0x08005e7a (ldr r0,[r4] -> movs r0,#48) to select AGB SYSTEM DEBUG MENU",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08005E9C,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x5e, 0x7a, 0x00, 0x02, 0x30, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BUTJ",
        release_key: "BUTJ-r0-0e291011ac3b60d2",
        display_name: "Ultra Keibitai - Monster Attack (Japan)",
        description: "Patch scene dispatcher at 0x08006bce (ldr r0,[r4] -> movs r0,#48) to select AGB SYSTEM DEBUG MENU",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08006BF0,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x6b, 0xce, 0x00, 0x02, 0x30, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BWXJ",
        release_key: "BWXJ-r0-952fa87ed26a954a",
        display_name: "Wanko Mix Chiwanko World (Japan)",
        description: "Patch scene table at 0x082B5098 (slot 10 [LOGO]) with slot 0 debug menu descriptor [DEBUG MENU] (16 bytes)",
        hook_type: "scene_descriptor_table_override",
        target_handler: 0x08006368,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x2b, 0x50, 0x98, 0x00, 0x10, 0x69, 0x63, 0x00, 0x08, 0x11, 0x64, 0x00, 0x08, 0xed, 0x67, 0x00, 0x08, 0xcc, 0x64, 0x04, 0x08, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BLTE",
        release_key: "BLTE-r0-05be7940195ba5c2",
        display_name: "Looney Tunes - Back in Action (USA, Europe)",
        description: "Thumb branch at Scene 0 dispatcher entry (0x080005d0) redirecting to developer debug level select handler (0x0801B98C)",
        hook_type: "thumb_branch_override",
        target_handler: 0x0801B98C,
        input_sequence: &["DOWN", "DOWN"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x05, 0xd0, 0x00, 0x04, 0x1b, 0xf0, 0xdc, 0xf9, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "ANNJ",
        release_key: "ANNJ-r0-94233e3dddcdb1bb",
        display_name: "Gekitou Densetsu Noah - Dream Management (Japan)",
        description: "Thumb branch at 0x08035c12 redirecting boot task initialization to Hudson Soft Sound Test developer menu (0x0803356C)",
        hook_type: "thumb_branch_override",
        target_handler: 0x0803356C,
        input_sequence: &["DOWN", "DOWN"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x03, 0x5c, 0x12, 0x00, 0x04, 0xfd, 0xf7, 0xab, 0xfc, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "ASMJ",
        release_key: "ASMJ-r0-9b7e8e8506fb3b18",
        display_name: "Saibara Rieko no Dendou Mahjong (Japan)",
        description: "Patch boot task constructor pointer literal at 0x080004a4 to point to AGB TEST MENU launcher (0x0800279C)",
        hook_type: "pointer_literal_override",
        target_handler: 0x0800279C,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x04, 0xa4, 0x00, 0x04, 0x9d, 0x27, 0x00, 0x08, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AEAJ",
        release_key: "AEAJ-r0-2147033af285ca33",
        display_name: "Snap Kid's (Japan)",
        description: "Thumb branch in master hardware init at 0x08000672 replacing title scene launcher with test mode init (0x08003FD4)",
        hook_type: "thumb_branch_override",
        target_handler: 0x08003FD4,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x06, 0x72, 0x00, 0x04, 0x03, 0xf0, 0xaf, 0xfc, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BMOJ",
        release_key: "BMOJ-r0-57381312afaad22b",
        display_name: "Minna no Ouji-sama (Japan)",
        description: "Patch reset vector entry pointer literal at 0x08000104 to boot directly into Konami developer debug menu (< TP2004:DEBUG MENU > at 0x09B00E80)",
        hook_type: "reset_vector_literal_override",
        target_handler: 0x09B00E80,
        input_sequence: &["DOWN", "DOWN"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x01, 0x04, 0x00, 0x04, 0x81, 0x0e, 0xb0, 0x09, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "ACHE",
        release_key: "ACHE-r0-85e0a40de22783ba",
        display_name: "Castlevania - Harmony of Dissonance (USA)",
        description: "Konami scene dispatcher override at 0x08000522: replace scene byte load with movs r0, #0x10 to boot directly into developer test menu mode 16",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0800C348,
        input_sequence: &["DOWN", "DOWN"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x05, 0x22, 0x00, 0x02, 0x10, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BE8E",
        release_key: "BE8E-r0-638cda9d9b726572",
        display_name: "Fire Emblem - The Sacred Stones (USA, Australia)",
        description: "Intelligent Systems GAMECTRL proc developer check bypass at 0x08009a10: nop conditional branch bne to force Proc_Goto(proc, 0x12) into developer debug SIO/multiplayer/battle test menu",
        hook_type: "thumb_branch_bypass",
        target_handler: 0x08048850,
        input_sequence: &["DOWN", "DOWN"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x9a, 0x10, 0x00, 0x02, 0xc0, 0x46, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AE2E",
        release_key: "AE2E-r0-61a8324c5179f0f8",
        display_name: "Megaman - Battle Network 2 (USA)",
        description: "Capcom scene initializer override at 0x0800029c: change boot scene from title screen (20) to debug mode selector (16)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x080244A0,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x02, 0x9c, 0x00, 0x02, 0x10, 0x21, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AX4E",
        release_key: "AX4E-r0-d99f8e5288b9ae17",
        display_name: "Super Mario Advance 4 - Super Mario Bros. 3 (USA)",
        description: "Nintendo main mode dispatcher override at 0x08000708: replace ldrb r0, [r0] with movs r0, #1 to boot directly into Mode 1 stage selector / world map handler, bypassing title/intro",
        hook_type: "thumb_imm_mode_override",
        target_handler: 0x08003888,
        input_sequence: &["RIGHT", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x07, 0x08, 0x00, 0x02, 0x01, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "ASYE",
        release_key: "ASYE-r0-d331abe19e3f02e2",
        display_name: "Spyro - Season of Ice (USA)",
        description: "Digital Eclipse task launcher override at 0x0805153e: replace ldr r0, [r5] with ldr r0, [r5, #0x1c] to launch Task 7 (options / level select menu dispatcher) instead of Task 0 (copyright screen)",
        hook_type: "task_launcher_override",
        target_handler: 0x0800F54C,
        input_sequence: &["DOWN", "DOWN"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x05, 0x15, 0x3e, 0x00, 0x02, 0xe8, 0x69, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BGWJ",
        release_key: "BGWJ-r0-a8ad7c7d2a48b4ce",
        display_name: "Game Boy Wars Advance 1+2 (Japan)",
        description: "Intelligent Systems mode selector override at 0x0000dd72: replace mode mask with movs r0, #4 to boot directly into Game Boy Wars Advance 2 map editor / campaign mode",
        hook_type: "thumb_imm_mode_override",
        target_handler: 0x0800AC30,
        input_sequence: &["A", "A", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0xdd, 0x72, 0x00, 0x02, 0x04, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BZME",
        release_key: "BZME-r0-bedc74df62755f70",
        display_name: "Legend of Zelda, The - The Minish Cap (USA)",
        description: "Flagship scene dispatcher override at 0x00055ec0: replace movs r0, #0 with movs r0, #1 to boot directly into game debug startup handler",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08050450,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x05, 0x5e, 0xc0, 0x00, 0x02, 0x01, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "A7KE",
        release_key: "A7KE-r0-caa4e11b61022579",
        display_name: "Kirby - Nightmare in Dream Land (USA)",
        description: "HAL Laboratory scene dispatcher override at 0x0000730e: replace scene index load with movs r1, #0x0c to boot into sound test / level warp handler",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08007584,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x73, 0x0e, 0x00, 0x02, 0x0c, 0x21, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BR5E",
        release_key: "BR5E-r0-572e113eeb53bb29",
        display_name: "Megaman - Battle Network 6 - Cybeast Gregar (USA)",
        description: "Capcom scene dispatcher override at 0x00000314: replace scene byte load with movs r1, #0x28 to boot directly into battle test & chip tester",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08121550,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x03, 0x14, 0x00, 0x02, 0x28, 0x21, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BZ4E",
        release_key: "BZ4E-r0-fdc7a48c4cf22746",
        display_name: "Final Fantasy IV Advance (USA)",
        description: "Matrix/TOSE task descriptor pointer override at 0x00001a58: redirect initial scene descriptor to 0x086db2fc for debug room / sound test scene",
        hook_type: "task_descriptor_override",
        target_handler: 0x0805DA25,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x1a, 0x58, 0x00, 0x02, 0xfc, 0xb2, 0x45, 0x4f, 0x46],
    },
    // Wave 8 (35 -> 40)
    DebugMenuPatch {
        game_code: "A2CE",
        release_key: "A2CE-r0-910f88d3701e6c9f",
        display_name: "Castlevania - Aria of Sorrow (USA)",
        description: "Patch scene dispatcher at 0x0800050a (ldrb r0,[r0,#0x10] -> movs r0,#0x12) to select Boss Rush / Debug Room handler (0x08036670)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x080005DE,
        input_sequence: &["DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x05, 0x0a, 0x00, 0x02, 0x12, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "A4XJ",
        release_key: "A4XJ-r0-e745c6b3dd7b5365",
        display_name: "Gachasute! Dino Device 2 - Dragon (Japan)",
        description: "Patch scene dispatcher at 0x08005b50 (subs r0,#1 -> movs r0,#0x2f) to select Mode 48 debug menu task (0x0800a565)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0800A565,
        input_sequence: &["DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x5b, 0x50, 0x00, 0x02, 0x2f, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AGRE",
        release_key: "AGRE-r0-9f3d1c31958c19a8",
        display_name: "ESPN Final Round Golf 2002 (USA)",
        description: "Patch mode selector at 0x080001d8 (movs r0,#1 -> movs r0,#0x1f) to select Mode 31 debug test menu (0x08001a6c)",
        hook_type: "thumb_imm_mode_override",
        target_handler: 0x08001A6C,
        input_sequence: &["DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x01, 0xd8, 0x00, 0x02, 0x1f, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AJ8J",
        release_key: "AJ8J-r0-429545548d89d9ff",
        display_name: "Jurassic Park Institute Tour - Dinosaur Rescue (Japan)",
        description: "Patch mode selector at 0x080025c8 (movs r0,#2 -> movs r0,#0) to select Mode 0 debug menu (0x08002790)",
        hook_type: "thumb_imm_mode_override",
        target_handler: 0x08002790,
        input_sequence: &["DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x25, 0xc8, 0x00, 0x02, 0x00, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BTDE",
        release_key: "BTDE-r0-e69c1fa888129f01",
        display_name: "Pocket Dogs (USA)",
        description: "Patch scene dispatcher at 0x08000a30 (movs r0,#6 -> movs r0,#2) to select Scene 2 debug menu (0x08025891)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08025891,
        input_sequence: &["DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x0a, 0x30, 0x00, 0x02, 0x02, 0x20, 0x45, 0x4f, 0x46],
    },
    // Wave 9 (40 -> 45)
    DebugMenuPatch {
        game_code: "BMXE",
        release_key: "BMXE-r0-fc94f65380b65b87",
        display_name: "Metroid - Zero Mission (USA)",
        description: "Patch scene mode selector at 0x08000260 (ldrsh r0,[r0,r1] -> movs r0,#3) to select Mode 3 Map Screen (0x08000358)",
        hook_type: "thumb_imm_mode_override",
        target_handler: 0x08000358,
        input_sequence: &["DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x02, 0x60, 0x00, 0x02, 0x03, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "U3IE",
        release_key: "U3IE-r0-aad4eaf0b3ab9f80",
        display_name: "Boktai - The Sun Is in Your Hand (USA)",
        description: "Patch solar sensor check branch at 0x081d1c34 (beq -> b #0x81d1c3a) to bypass sensor check into RTC setup (0x081D1C3A)",
        hook_type: "thumb_branch_override",
        target_handler: 0x081D1C3A,
        input_sequence: &["DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x1d, 0x1c, 0x34, 0x00, 0x02, 0x01, 0xe0, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BZ3E",
        release_key: "BZ3E-r0-067f420fbd4aabc1",
        display_name: "Megaman Zero 3 (USA)",
        description: "Patch task scene selector at 0x080eb3e2 (ldrb r0,[r4,#4] -> movs r0,#4) to dispatch Scene 4 In-Game Action / Stage Runner (0x080EBA1D)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x080EBA1D,
        input_sequence: &["DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x0e, 0xb3, 0xe2, 0x00, 0x02, 0x04, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BPEE",
        release_key: "BPEE-r0-a9dec84dfe7f62ab",
        display_name: "Pokemon - Emerald Version (USA, Europe)",
        description: "Patch copyright state machine at 0x0816ccd4 (ldrb r4,[r5] -> movs r4,#0x8d) to fast-track transition to Title Screen (0x0816CE2D)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0816CE2D,
        input_sequence: &["START", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x16, 0xcc, 0xd4, 0x00, 0x02, 0x8d, 0x24, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AGSE",
        release_key: "AGSE-r0-c14f1151897e8d73",
        display_name: "Golden Sun (USA, Europe)",
        description: "Patch event branch at 0x0808a8fc (beq -> b #0x808a902) to boot directly into Mode 1 interactive map / scene dispatcher (0x0808A902)",
        hook_type: "thumb_branch_override",
        target_handler: 0x0808A902,
        input_sequence: &["A", "DOWN"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x08, 0xa8, 0xfc, 0x00, 0x02, 0x01, 0xe0, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AZLE",
        release_key: "AZLE-r0-f328f8f07d736288",
        display_name: "The Legend of Zelda - A Link to the Past & Four Swords (USA)",
        description: "Patch scene dispatcher at 0x0800b1be (ldrb r0,[r0] -> movs r0,#3) to boot into Mode 3 Game Select / Mode Selection screen (0x080310B5)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x080310B5,
        input_sequence: &["DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0xb1, 0xbe, 0x00, 0x02, 0x03, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AW2E",
        release_key: "AW2E-r0-ef3cc89273f9df88",
        display_name: "Advance Wars 2 - Black Hole Rising (USA)",
        description: "Patch boot button check at 0x08036db2 (beq -> nop) to force developer boot routine into test mode (0x08036E54)",
        hook_type: "thumb_branch_override",
        target_handler: 0x08036E54,
        input_sequence: &["DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x03, 0x6d, 0xb2, 0x00, 0x02, 0x00, 0x00, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AZWE",
        release_key: "AZWE-r0-c09be39a09b37f6a",
        display_name: "WarioWare, Inc. - Mega Microgame$! (USA)",
        description: "Patch mode dispatcher at 0x0800032a (ldrsh r0,[r5,r1] -> movs r0,#6) to boot into Mode 6 Developer Debug / Test Menu (0x08016D88)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08016D88,
        input_sequence: &["DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x03, 0x2a, 0x00, 0x02, 0x06, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AE7E",
        release_key: "AE7E-r0-f1200d19771b366b",
        display_name: "Fire Emblem (USA)",
        description: "Patch GAMECTRL developer check at 0x080125fc (bne -> nop) to force Proc_Goto(proc, 0x0F) into developer debug/test proc (0x08012602)",
        hook_type: "thumb_branch_override",
        target_handler: 0x08012602,
        input_sequence: &["DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x01, 0x25, 0xfc, 0x00, 0x02, 0xc0, 0x46, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AMAE",
        release_key: "AMAE-r0-d29dec02caacbf44",
        display_name: "Super Mario Advance (USA)",
        description: "Patch scene selector at 0x08001c78 (ldrb r1,[r2] -> movs r1,#1) to boot into Mode 1 Game Select Menu (0x0801AB45)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0801AB45,
        input_sequence: &["DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x1c, 0x78, 0x00, 0x02, 0x01, 0x21, 0x45, 0x4f, 0x46],
    },
    // Wave 11 (50 -> 66)
    DebugMenuPatch {
        game_code: "A2JJ",
        release_key: "A2JJ-r0-49115d02cb64ae7c",
        display_name: "J.League Winning Eleven Advance 2002 (Japan)",
        description: "Override initial main-loop state store at 0x000312 (ldr r0,[pc,#0x3c]; strh r4,[r0] -> movs r1,#5; strb r1,[r0]) to boot directly into Konami TEST MODE (ANIME NO / AUDIO TEST / CABLE TEST / MEMORY TEST / COLOR TEST / BACKUP TEST / BIG TESTMODE)",
        hook_type: "thumb_state_init_override",
        target_handler: 0x08004110,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x03, 0x12, 0x00, 0x04, 0x05, 0x21, 0x01, 0x70, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "A3OJ",
        release_key: "A3OJ-r0-a5e2549851554af9",
        display_name: "Di Gi Charat - DigiCommunication (Japan)",
        description: "Override boot scene push at 0x001e86 (movs r0, #1 -> movs r0, #0) to boot directly into Di Gi Charat Scene 0 Test Menu (Sound Test / Script Test / Chain Test)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08002798,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x1e, 0x86, 0x00, 0x02, 0x00, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "A4WJ",
        release_key: "A4WJ-r0-ada4eb8d385673d4",
        display_name: "Gachasute! Dino Device 2 - Phoenix (Japan)",
        description: "Override initial scene at 0x005b2c (movs r0, #0x1e -> movs r0, #0x30) and bypass retail 0x30->0x1e redirect at 0x005b40 (bne -> b) to boot into DEBUG MENU ----- DINODEVICE2",
        hook_type: "thumb_retail_guard_bypass",
        target_handler: 0x080095D0,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x5b, 0x2c, 0x00, 0x02, 0x30, 0x20, 0x00, 0x5b, 0x40, 0x00, 0x02, 0x03, 0xe0, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "A8LJ",
        release_key: "A8LJ-r0-a0f2ec393d391ab7",
        display_name: "BB Ball (Japan)",
        description: "Override initial mode selector at 0x00038e (movs r0, #1 -> movs r0, #0) to boot directly into Mode 0 DEBUG MENU (MAP 0800 / OBJ TEST / BGM / SE / BATTLE)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x080149A0,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x03, 0x8e, 0x00, 0x02, 0x00, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AC2J",
        release_key: "AC2J-r0-bede08138e841421",
        display_name: "J.League Pro Soccer Club o Tsukurou! Advance (Japan)",
        description: "Redirect boot initializer call at 0x003aa8 (bl 0x08014acc -> bl 0x080026ec) to launch S29 AGB Developer Test Menu (Test menu / Scene Viewer / sound test / Score Anim / montage / Tobi menu2 / sgame test)",
        hook_type: "thumb_bl_redirect",
        target_handler: 0x080026EC,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x3a, 0xa8, 0x00, 0x04, 0xfe, 0xf7, 0x20, 0xfe, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AENE",
        release_key: "AENE-r0-e35b4ec9b8df2631",
        display_name: "Serious Sam Advance (USA) (En,Fr,De)",
        description: "Override initial scene index at 0x002de8 (movs r0, #6 -> movs r0, #8) in main scene table 0x0844a5e4 to boot directly into Scene 8 Multiplayer/Link Test screen",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0801B3FC,
        input_sequence: &["B", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x2d, 0xe8, 0x00, 0x02, 0x08, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AJ3E",
        release_key: "AJ3E-r0-9b073efb65342a1e",
        display_name: "Jurassic Park III - Park Builder (USA)",
        description: "Replace boot scene table entry 4 at 0x397ad0 (0x080011a5 -> 0x08008eb1) to boot directly into Konami Park Builder TEST MENU",
        hook_type: "scene_table_ptr_swap",
        target_handler: 0x08008EB0,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x39, 0x7a, 0xd0, 0x00, 0x04, 0xb1, 0x8e, 0x00, 0x08, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AO3E",
        release_key: "AO3E-r0-a50aff4c7478692e",
        display_name: "Terminator 3 - Rise of the Machines (USA)",
        description: "Bypass intro sequence guard at 0x0074f4 (beq -> nop) and swap menu[0] update/draw pointers at 0x0074cc/0x0074d0 to menu[5] SOUND TEST (0x080078b5 / 0x08007e99)",
        hook_type: "menu_handler_swap",
        target_handler: 0x080078B4,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x74, 0xcc, 0x00, 0x04, 0xb5, 0x78, 0x00, 0x08, 0x00, 0x74, 0xd0, 0x00, 0x04, 0x99, 0x7e, 0x00, 0x08, 0x00, 0x74, 0xf4, 0x00, 0x02, 0x00, 0x00, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "APNJ",
        release_key: "APNJ-r0-d8b60a1934e34dcc",
        display_name: "Pinky Monkey Town (Japan)",
        description: "Bypass intro wait at 0x00012a (beq -> b) and 0xA5 developer flag guard at 0x000156 (bne -> nop) to boot directly into DEBUG MODE",
        hook_type: "thumb_flag_check_bypass",
        target_handler: 0x08000158,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x01, 0x2a, 0x00, 0x02, 0x0a, 0xe0, 0x00, 0x01, 0x56, 0x00, 0x02, 0x00, 0x00, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AWIE",
        release_key: "AWIE-r0-843b4ea112c13399",
        display_name: "ESPN International Winter Sports 2002 (USA)",
        description: "Replace initial Konami scene struct pointer at 0x000448 (0x082fe93c bumper -> 0x0830436c sys_mode) to boot directly into MODE SELECT with developer font overlay",
        hook_type: "scene_struct_ptr_swap",
        target_handler: 0x0805E040,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x04, 0x48, 0x00, 0x04, 0x6c, 0x43, 0x30, 0x08, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AXSE",
        release_key: "AXSE-r0-dda490a021f2e083",
        display_name: "ESPN X-Games Skateboarding (USA)",
        description: "Redirect main state jump-table slot 2 at 0x00053c (0x08000598 -> 0x080005d4) to boot directly into state 4 Developer Character/Trick Test mode (0x080173cc)",
        hook_type: "jump_table_slot_swap",
        target_handler: 0x080173CC,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x05, 0x3c, 0x00, 0x02, 0xd4, 0x05, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BK8J",
        release_key: "BK8J-r0-82c70e1a3bf5ed78",
        display_name: "Kappa no Kai-kata - Kaatan Daibouken! (Japan)",
        description: "Override boot scene argument at 0x000346 (movs r0, #0x1c -> movs r0, #3) to boot directly into Scene 3 Developer <L/R> Sprite/Animation Viewer (0x0800ede1)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0800EDE0,
        input_sequence: &["L", "R", "DOWN"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x03, 0x46, 0x00, 0x02, 0x03, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BLYE",
        release_key: "BLYE-r0-eb0e2af0dffe04e1",
        display_name: "Lemony Snicket's A Series of Unfortunate Events (USA, Europe)",
        description: "Override post-logo scene transition at 0x021a62 (movs r0, #1 -> movs r0, #9) to boot directly into Scene 9 Debug Menu (Map Select / Map Select Replay / Test Sound and Music / Cheat To Book 1-3 / Toggle Invincibility)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0801CDE8,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x02, 0x1a, 0x62, 0x00, 0x02, 0x09, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BM8J",
        release_key: "BM8J-r0-53bde3ab41323a35",
        display_name: "Mermaid Melody - Pichi Pichi Pitch - Pichi Pichi Party (Japan)",
        description: "Replace boot Konami scene struct pointer at 0x00f598 (0x0829b118 klogo -> 0x0829af40 bg_test) to boot directly into interactive BG TEST background viewer",
        hook_type: "scene_struct_ptr_swap",
        target_handler: 0x08002B74,
        input_sequence: &["RIGHT", "RIGHT", "L", "R"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0xf5, 0x98, 0x00, 0x04, 0x40, 0xaf, 0x29, 0x08, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BSRE",
        release_key: "BSRE-r0-98887216a00c5e99",
        display_name: "Wade Hixton's Counter Punch (USA, Europe)",
        description: "Redirect boot intro call at 0x00e688 (bl 0x0800d890 -> bl 0x08011ddc) to boot directly into Stats / Options / Music & Sound Test menu",
        hook_type: "thumb_bl_redirect",
        target_handler: 0x08011DDC,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0xe6, 0x88, 0x00, 0x04, 0x03, 0xf0, 0xa8, 0xfb, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BXKE",
        release_key: "BXKE-r0-0471c34b0e30321e",
        display_name: "Castlevania Double Pack (USA)",
        description: "Redirect Double Pack boot entry at 0x0001c8 to Aria of Sorrow (0x08800000) and swap Aria jump-table slot 0 at 0x800464 (0x088004b8 -> 0x0880051e) to boot directly into Aria of Sorrow Test/Boss-Rush stage mode (0x088365b0)",
        hook_type: "embedded_rom_jump_table_override",
        target_handler: 0x088365B0,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x01, 0xc8, 0x00, 0x04, 0x00, 0x00, 0x80, 0x08, 0x80, 0x04, 0x64, 0x00, 0x02, 0x1e, 0x05, 0x45, 0x4f, 0x46],
    },
    // Wave 12 (66 -> 82)
    DebugMenuPatch {
        game_code: "AP6J",
        release_key: "AP6J-r0-5e89bcd1d9a3d58c",
        display_name: "Pinobee & Phoebee (Japan)",
        description: "Redirect boot scene creator call at 0x080044b2 (bl 0x08004780 -> bl 0x0804b5a8) to launch PINOBEE2 DEBUG MENU",
        hook_type: "thumb_branch_override",
        target_handler: 0x0804B5A8,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x44, 0xb2, 0x00, 0x04, 0x47, 0xf0, 0x79, 0xf8, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AHEJ",
        release_key: "AHEJ-r0-740dcfdaaa4f39f0",
        display_name: "Bakuten Shoot Beyblade - Gekitou! Saikyou Blader (Japan)",
        description: "Patch scene dispatcher at 0x080047fe (ldr r0,[r4] -> movs r0,#43) to select jump-table index 42 (AGB SYSTEM DEBUG MENU at 0x08009c3d)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08009C3C,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x47, 0xfe, 0x00, 0x02, 0x2b, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "A3EJ",
        release_key: "A3EJ-r0-f8f50e0e49438f4b",
        display_name: "Bakuten Shoot Beyblade 2002 - Gekisen! Team Battle!! Kouryuu no Shou - Daichi Hen (Japan)",
        description: "Patch scene dispatcher at 0x0800534e (ldr r0,[r4] -> movs r0,#49) to select jump-table index 48 (AGB SYSTEM DEBUG MENU at 0x0800bfa9)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0800BFA8,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x53, 0x4e, 0x00, 0x02, 0x31, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "A3WJ",
        release_key: "A3WJ-r0-2df731e763871ed3",
        display_name: "Bakuten Shoot Beyblade 2002 - Gekisen! Team Battle!! Seiryuu no Shou - Takao Hen (Japan)",
        description: "Patch scene dispatcher at 0x0800534e (ldr r0,[r4] -> movs r0,#49) to select jump-table index 48 (AGB SYSTEM DEBUG MENU at 0x0800bfa9)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0800BFA8,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x53, 0x4e, 0x00, 0x02, 0x31, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AB8J",
        release_key: "AB8J-r0-10c8a203b6d3fe7c",
        display_name: "Bakuten Shoot Beyblade 2002 - Ikuze! Bakutou! Chou Jiryoku Battle!! (Japan)",
        description: "Patch scene dispatcher at 0x08004f96 (ldr r0,[r4] -> movs r0,#43) to select jump-table index 42 (AGB SYSTEM DEBUG MENU at 0x0800a8e5)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0800A8E4,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x4f, 0x96, 0x00, 0x02, 0x2b, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "ABIJ",
        release_key: "ABIJ-r0-ad808c271dcb9596",
        display_name: "Gachasute! Dino Device - Blue (Japan)",
        description: "Patch scene dispatcher at 0x08004dd2 (ldr r0,[r4] -> movs r0,#50) to select jump-table index 49 (AGB SYSTEM DEBUG MENU at 0x0800ad9d)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0800AD9C,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x4d, 0xd2, 0x00, 0x02, 0x32, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AAIJ",
        release_key: "AAIJ-r0-73c8e9f4399930b8",
        display_name: "Gachasute! Dino Device - Red (Japan)",
        description: "Patch scene dispatcher at 0x08004dd2 (ldr r0,[r4] -> movs r0,#50) to select jump-table index 49 (AGB SYSTEM DEBUG MENU at 0x0800ad9d)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0800AD9C,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x4d, 0xd2, 0x00, 0x02, 0x32, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "B24E",
        release_key: "B24E-r0-ad316814c77ed083",
        display_name: "Pokemon Mystery Dungeon - Red Rescue Team (USA, Australia)",
        description: "Skip health-and-safety/intro loops (0x080003d2, 0x080004d8) and set initial MainMenu state at 0x08035796 (movs r2,#1 -> movs r2,#16) to boot developer Debug Mode menu (0x0803a200)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0803A200,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x03, 0xd2, 0x00, 0x02, 0x14, 0xe0, 0x00, 0x04, 0xd8, 0x00, 0x02, 0x23, 0xe0, 0x03, 0x57, 0x96, 0x00, 0x02, 0x10, 0x22, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "B9JE",
        release_key: "B9JE-r0-1ae520cfcf8e64e1",
        display_name: "Pokemon Mystery Dungeon - Red Rescue Team (USA) (Demo) (Kiosk)",
        description: "Skip kiosk demo title/intro loops (0x080003bc, 0x080003e0, 0x08000492, 0x080004aa) and set initial MainMenu state at 0x080355f2 (movs r2,#1 -> movs r2,#16) to boot developer Debug Mode menu",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0803A040,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x03, 0xbc, 0x00, 0x02, 0x07, 0xe0, 0x00, 0x03, 0xe0, 0x00, 0x02, 0x11, 0xe0, 0x00, 0x04, 0x92, 0x00, 0x04, 0xc0, 0x46, 0xc0, 0x46, 0x00, 0x04, 0xaa, 0x00, 0x02, 0x34, 0xe0, 0x03, 0x55, 0xf2, 0x00, 0x02, 0x10, 0x22, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BOSJ",
        release_key: "BOSJ-r0-5ca196c5265ae19f",
        display_name: "Boboboubo Boubobo - Bakutou Hajike Taisen (Japan)",
        description: "Patch main scene dispatcher at 0x08001904 (ldrb r1,[r5] -> movs r1,#4) to select scene table slot 4 (0x080216f1 -> developer character select / debug menu)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x080216F0,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x19, 0x04, 0x00, 0x02, 0x04, 0x21, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "A8GJ",
        release_key: "A8GJ-r0-47c81371c11ca645",
        display_name: "GetBackers Dakkanya - Metropolis Dakkan Sakusen! (Japan)",
        description: "Patch state-0 branch in scene dispatcher at 0x0800226a (beq 0x080022b0 -> b 0x08002354) to launch developer SOUND TEST menu (0x0800e870)",
        hook_type: "thumb_branch_override",
        target_handler: 0x0800E870,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x22, 0x6a, 0x00, 0x02, 0x73, 0xe0, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "B6JJ",
        release_key: "B6JJ-r0-a6d5e22f0e937865",
        display_name: "Super Robot Taisen J (Japan)",
        description: "Patch scene dispatcher at 0x0800718c (ldr r0,[r4] -> movs r0,#49) to select jump-table index 48 (DEBUG MAIN PAGE at 0x080098b9)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x080098B8,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x71, 0x8c, 0x00, 0x02, 0x31, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "ABSE",
        release_key: "ABSE-r0-e02e6cc3e91f4f4c",
        display_name: "Bomberman Tournament (USA, Europe)",
        description: "Patch Hudson scene dispatcher at 0x08000cc2 (ldr r0,[r4] -> movs r0,#90) to select scene 0x5a (TEST SELECT debug menu at 0x08042981)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08042980,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x0c, 0xc2, 0x00, 0x02, 0x5a, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AJZJ",
        release_key: "AJZJ-r0-8627870dbe38e1fb",
        display_name: "Bomberman Jetters - Densetsu no Bomberman (Japan)",
        description: "Patch Hudson scene dispatcher at 0x0800039a (ldr r0,[r4] -> movs r0,#90) to select scene 0x5a (TEST SELECT debug menu)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x0800039A,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x03, 0x9a, 0x00, 0x02, 0x5a, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BD2J",
        release_key: "BD2J-r0-915fada793849b6b",
        display_name: "Duel Masters 2 - Invincible Advance (Japan)",
        description: "Patch Hudson scene dispatcher at 0x0800039e (ldr r0,[r4] -> movs r0,#90) to select scene 0x5a (TEST SELECT 2.0 debug menu at 0x08007009)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08007008,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x03, 0x9e, 0x00, 0x02, 0x5a, 0x20, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BD5J",
        release_key: "BD5J-r0-938d3c96df6d8493",
        display_name: "Duel Masters 2 - Kirifuda Shoubu Ver. (Japan)",
        description: "Patch Hudson scene dispatcher at 0x0800039e (ldr r0,[r4] -> movs r0,#90) to select scene 0x5a (TEST SELECT 2.5 debug menu at 0x08007151)",
        hook_type: "thumb_imm_scene_override",
        target_handler: 0x08007150,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x03, 0x9e, 0x00, 0x02, 0x5a, 0x20, 0x45, 0x4f, 0x46],
    },

    // Wave 13 (82 -> 85)
    DebugMenuPatch {
        game_code: "A3JJ",
        release_key: "A3JJ-r0-f45f94483c8cb8e5",
        display_name: "Gyakuten Saiban 3 (Japan)",
        description: "Redirect main state table entry 0x0C at 0x081610b8 (0x0800cc09 -> 0x08001a29) to launch 7-entry DEBUG MENU (VRAM VIEW, SOUND TEST, OBJ TEST, MOVIE TEST, FLAG EDIT, SCENARIO, EXIT)",
        hook_type: "state_jump_table_ptr",
        target_handler: 0x08001A28,
        input_sequence: &["A", "DOWN", "DOWN", "DOWN", "A", "RIGHT", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x16, 0x10, 0xb8, 0x00, 0x04, 0x29, 0x1a, 0x00, 0x08, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "ALFE",
        release_key: "ALFE-r0-6e6c370c339ec917",
        display_name: "Dragon Ball Z - The Legacy of Goku II (USA)",
        description: "Redirect post-copyright screen constructor call at 0x080001e6 (bl 0x0801d378 -> bl 0x080056b0) to boot directly into interactive DEBUG menu (MUSIC TEST, SAMPLE TEST, MAP TEST, ZONE, AREA, VARIATION)",
        hook_type: "thumb_bl_redirect",
        target_handler: 0x080056B0,
        input_sequence: &["RIGHT", "DOWN"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x01, 0xe6, 0x00, 0x04, 0x05, 0xf0, 0x63, 0xfa, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BKTP",
        release_key: "BKTP-r0-1613cf4a76a01dba",
        display_name: "Steel Empire (Europe)",
        description: "Patch boot debug-flag initializer at 0x08000410 (movs r0,#0 -> movs r0,#0xa5) to arm debug mode flag *(u8*)0x03003be4 at boot; press L+SELECT+START to open 6-mode VRAM VIEW debugger",
        hook_type: "thumb_imm_mode_override",
        target_handler: 0x08000A64,
        input_sequence: &["L+SELECT+START", "R"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x04, 0x10, 0x00, 0x01, 0xa5, 0x45, 0x4f, 0x46],
    },
    // Wave 14 (live-verified interactive debug menus)
    DebugMenuPatch {
        game_code: "AZQE",
        release_key: "AZQE-r0-c5e7ac2ffdaa46ab",
        display_name: "Treasure Planet (USA)",
        description: "Redirect 16-byte game-state table[0] at 0x080ea2e8 to unreachable state[3] (0x080ea318) to boot into hidden 3-tier Theme Select -> Level Select -> Room Select debug menu (FUN_0801c3e4)",
        hook_type: "state_table_redirect",
        target_handler: 0x0801C3E4,
        input_sequence: &["START", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x0e, 0xa2, 0xe8, 0x00, 0x10, 0xed, 0xc7, 0x01, 0x08, 0xd5, 0xc8, 0x01, 0x08, 0x85, 0xc8, 0x01, 0x08, 0x45, 0xc9, 0x01, 0x08, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "BYUE",
        release_key: "BYUE-r0-27cf19eaced550f3",
        display_name: "Yggdra Union - We'll Never Fight Alone (USA)",
        description: "Redirect 12-byte main mode-table triplet at 0x080ad76c to unreachable Debug Test Mode triplet at 0x080ad8c8 (FUN_08005c04/FUN_08005e68/FUN_08006d28) for live Character/BGM/Effect/Event debug tester",
        hook_type: "mode_table_redirect",
        target_handler: 0x08005C04,
        input_sequence: &["START", "DOWN", "RIGHT"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x0a, 0xd7, 0x6c, 0x00, 0x0c, 0x05, 0x5c, 0x00, 0x08, 0x69, 0x5e, 0x00, 0x08, 0x29, 0x6d, 0x00, 0x08, 0x45, 0x4f, 0x46],
    },
    DebugMenuPatch {
        game_code: "AORJ",
        release_key: "AORJ-r0-0e9997636409c477",
        display_name: "Oriental Blue - Ao no Tengai (Japan)",
        description: "Redirect boot thread BL at 0x08000b92 to FUN_08074fec (5-entry AGB Sample DEBUG MENU), NOP debug-text guard at 0x0807517e, and NOP AgbMain DISPCNT/PRAM overrides at 0x0807361a/0x08073626",
        hook_type: "thumb_bl_redirect",
        target_handler: 0x08074FEC,
        input_sequence: &["DOWN", "DOWN", "A"],
        ips_bytes: &[0x50, 0x41, 0x54, 0x43, 0x48, 0x00, 0x0b, 0x92, 0x00, 0x04, 0x74, 0xf0, 0x2b, 0xfa, 0x07, 0x36, 0x1a, 0x00, 0x04, 0xc0, 0x46, 0xc0, 0x46, 0x07, 0x36, 0x26, 0x00, 0x04, 0xc0, 0x46, 0xc0, 0x46, 0x07, 0x51, 0x7e, 0x00, 0x02, 0xc0, 0x46, 0x45, 0x4f, 0x46],
    },

];

/// Look up a verified debug menu patch by 4-character game code (e.g., `"A84E"`).
///
/// Supports aliases for known multi-region/development codes:
/// - `"AICE"` -> `"ASYE"` (Spyro: Season of Ice)
/// - `"BLFE"` -> `"BZME"` (The Legend of Zelda: The Minish Cap)
/// - `"AGBE"` -> `"A7KE"` (Kirby: Nightmare in Dream Land)
/// - `"B88E"` -> `"BR5E"` (Mega Man Battle Network 6: Cybeast Gregar)
/// - `"A4FE"` -> `"BZ4E"` (Final Fantasy IV Advance)
/// - `"AFEE"` -> `"AE7E"` (Fire Emblem)
pub fn find_by_code(code: &str) -> Option<&'static DebugMenuPatch> {
    if let Some(exact) = REGISTRY
        .iter()
        .find(|p| p.game_code.eq_ignore_ascii_case(code))
    {
        return Some(exact);
    }
    const ALIASES: &[(&str, &str)] = &[
        ("AICE", "ASYE"),
        ("BLFE", "BZME"),
        ("AGBE", "A7KE"),
        ("B88E", "BR5E"),
        ("A4FE", "BZ4E"),
        ("AFEE", "AE7E"),
    ];
    let alias = ALIASES
        .iter()
        .find(|&&(from, _)| from.eq_ignore_ascii_case(code))
        .map(|&(_, to)| to)?;
    REGISTRY.iter().find(|p| p.game_code == alias)
}

/// Look up a verified debug menu patch by exact release key (e.g., `"A84E-r0-2e3a8e3962c9d5fb"`).
pub fn find_by_release_key(key: &str) -> Option<&'static DebugMenuPatch> {
    REGISTRY.iter().find(|p| p.release_key == key)
}

/// Look up a verified debug menu patch for a GBA ROM slice by parsing its header.
pub fn find_for_rom(rom_bytes: &[u8]) -> Option<&'static DebugMenuPatch> {
    let hdr = AgbHeader::parse(rom_bytes).ok()?;
    find_by_code(&hdr.game_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_size() {
        assert_eq!(REGISTRY.len(), 88);
    }

    #[test]
    fn test_unique_game_codes_and_release_keys() {
        let mut codes = std::collections::HashSet::new();
        let mut keys = std::collections::HashSet::new();
        for p in REGISTRY {
            assert!(
                codes.insert(p.game_code),
                "duplicate game_code: {}",
                p.game_code
            );
            assert!(
                keys.insert(p.release_key),
                "duplicate release_key: {}",
                p.release_key
            );
        }
    }

    #[test]
    fn test_all_ips_patches_parse() {
        for p in REGISTRY {
            let patch = Patch::parse(p.ips_bytes).expect("valid IPS bytes");
            assert!(!patch.records.is_empty(), "patch has records");
        }
    }

    #[test]
    fn test_find_by_code() {
        let ham = find_by_code("a84e").expect("found Hamtaro patch");
        assert_eq!(ham.game_code, "A84E");
        assert_eq!(ham.target_handler, 0x08084378);
        assert_eq!(ham.input_sequence, &["A", "DOWN"]);

        let spyro = find_by_code("ASYE").expect("found Spyro patch by ASYE");
        assert_eq!(spyro.game_code, "ASYE");

        let spyro_alias = find_by_code("aice").expect("found Spyro patch by AICE alias");
        assert_eq!(spyro_alias.game_code, "ASYE");

        // Wave 7 entries and aliases
        let bgwj = find_by_code("BGWJ").expect("found BGWJ");
        assert_eq!(bgwj.game_code, "BGWJ");
        assert_eq!(bgwj.target_handler, 0x0800AC30);

        // Wave 11 & 12 entries
        let a2jj = find_by_code("A2JJ").expect("found A2JJ (Wave 11)");
        assert_eq!(a2jj.game_code, "A2JJ");
        assert_eq!(a2jj.target_handler, 0x08004110);

        let b24e = find_by_code("B24E").expect("found B24E (Wave 12)");
        assert_eq!(b24e.game_code, "B24E");
        assert_eq!(b24e.target_handler, 0x0803A200);

        let bzme = find_by_code("BLFE").expect("found BZME via BLFE alias");
        assert_eq!(bzme.game_code, "BZME");
        assert_eq!(bzme.target_handler, 0x08050450);

        let a7ke = find_by_code("AGBE").expect("found A7KE via AGBE alias");
        assert_eq!(a7ke.game_code, "A7KE");
        assert_eq!(a7ke.target_handler, 0x08007584);

        let br5e = find_by_code("B88E").expect("found BR5E via B88E alias");
        assert_eq!(br5e.game_code, "BR5E");
        assert_eq!(br5e.target_handler, 0x08121550);

        let bz4e = find_by_code("A4FE").expect("found BZ4E via A4FE alias");
        assert_eq!(bz4e.game_code, "BZ4E");
        assert_eq!(bz4e.target_handler, 0x0805DA25);

        let afee = find_by_code("AFEE").expect("found AE7E via AFEE alias");
        assert_eq!(afee.game_code, "AE7E");
        assert_eq!(afee.target_handler, 0x08012602);

        // Wave 13 entries
        let a3jj = find_by_code("A3JJ").expect("found A3JJ (Wave 13)");
        assert_eq!(a3jj.game_code, "A3JJ");
        assert_eq!(a3jj.target_handler, 0x08001A28);

        let alfe = find_by_code("ALFE").expect("found ALFE (Wave 13)");
        assert_eq!(alfe.game_code, "ALFE");
        assert_eq!(alfe.target_handler, 0x080056B0);

        let bktp = find_by_code("BKTP").expect("found BKTP (Wave 13)");
        assert_eq!(bktp.game_code, "BKTP");
        assert_eq!(bktp.target_handler, 0x08000A64);

        // Wave 14 entries
        let azqe = find_by_code("AZQE").expect("found AZQE (Wave 14)");
        assert_eq!(azqe.game_code, "AZQE");
        assert_eq!(azqe.target_handler, 0x0801C3E4);

        let byue = find_by_code("BYUE").expect("found BYUE (Wave 14)");
        assert_eq!(byue.game_code, "BYUE");
        assert_eq!(byue.target_handler, 0x08005C04);

        let aorj = find_by_code("AORJ").expect("found AORJ (Wave 14)");
        assert_eq!(aorj.game_code, "AORJ");
        assert_eq!(aorj.target_handler, 0x08074FEC);
    }

    #[test]
    fn test_apply_patch() {
        let p = find_by_code("BN2E").expect("found BN2E");
        // BN2E modifies ROM offset 0x0690: 0x01 -> 0x00
        let mut rom = vec![0u8; 0x1000];
        rom[0x690] = 0x01;
        let patched = p.apply(&rom).expect("applied successfully");
        assert_eq!(patched[0x690], 0x00);

        let blte = find_by_code("BLTE").expect("found BLTE");
        let blte_rom = vec![0u8; 0x1000];
        let blte_patched = blte.apply(&blte_rom).expect("applied BLTE");
        assert_eq!(&blte_patched[0x5d0..0x5d4], &[0x1b, 0xf0, 0xdc, 0xf9]);

        let ache = find_by_code("ACHE").expect("found ACHE");
        let ache_rom = vec![0u8; 0x1000];
        let ache_patched = ache.apply(&ache_rom).expect("applied ACHE");
        assert_eq!(&ache_patched[0x522..0x524], &[0x10, 0x20]);

        let be8e = find_by_code("BE8E").expect("found BE8E");
        let be8e_rom = vec![0u8; 0x10000];
        let be8e_patched = be8e.apply(&be8e_rom).expect("applied BE8E");
        assert_eq!(&be8e_patched[0x9a10..0x9a12], &[0xc0, 0x46]);

        let ae2e = find_by_code("AE2E").expect("found AE2E");
        let ae2e_rom = vec![0u8; 0x1000];
        let ae2e_patched = ae2e.apply(&ae2e_rom).expect("applied AE2E");
        assert_eq!(&ae2e_patched[0x29c..0x29e], &[0x10, 0x21]);

        let ax4e = find_by_code("AX4E").expect("found AX4E");
        let ax4e_rom = vec![0u8; 0x1000];
        let ax4e_patched = ax4e.apply(&ax4e_rom).expect("applied AX4E");
        assert_eq!(&ax4e_patched[0x708..0x70a], &[0x01, 0x20]);

        let asye = find_by_code("AICE").expect("found ASYE via alias");
        let asye_rom = vec![0u8; 0x60000];
        let asye_patched = asye.apply(&asye_rom).expect("applied ASYE");
        assert_eq!(&asye_patched[0x5153e..0x51540], &[0xe8, 0x69]);

        // Wave 7 patch application tests
        let bgwj = find_by_code("BGWJ").expect("found BGWJ");
        let mut bgwj_rom = vec![0u8; 0x10000];
        bgwj_rom[0xdd72..0xdd74].copy_from_slice(&[0x08, 0x40]);
        let bgwj_patched = bgwj.apply(&bgwj_rom).expect("applied BGWJ");
        assert_eq!(&bgwj_patched[0xdd72..0xdd74], &[0x04, 0x20]);

        let bzme = find_by_code("BLFE").expect("found BZME");
        let mut bzme_rom = vec![0u8; 0x60000];
        bzme_rom[0x55ec0..0x55ec2].copy_from_slice(&[0x00, 0x20]);
        let bzme_patched = bzme.apply(&bzme_rom).expect("applied BZME");
        assert_eq!(&bzme_patched[0x55ec0..0x55ec2], &[0x01, 0x20]);

        let a7ke = find_by_code("AGBE").expect("found A7KE");
        let mut a7ke_rom = vec![0u8; 0x10000];
        a7ke_rom[0x730e..0x7310].copy_from_slice(&[0x01, 0x88]);
        let a7ke_patched = a7ke.apply(&a7ke_rom).expect("applied A7KE");
        assert_eq!(&a7ke_patched[0x730e..0x7310], &[0x0c, 0x21]);

        let br5e = find_by_code("B88E").expect("found BR5E");
        let mut br5e_rom = vec![0u8; 0x1000];
        br5e_rom[0x314..0x316].copy_from_slice(&[0x09, 0x78]);
        let br5e_patched = br5e.apply(&br5e_rom).expect("applied BR5E");
        assert_eq!(&br5e_patched[0x314..0x316], &[0x28, 0x21]);

        let bz4e = find_by_code("A4FE").expect("found BZ4E");
        let mut bz4e_rom = vec![0u8; 0x10000];
        bz4e_rom[0x1a58..0x1a5a].copy_from_slice(&[0xb4, 0x9e]);
        let bz4e_patched = bz4e.apply(&bz4e_rom).expect("applied BZ4E");
        assert_eq!(&bz4e_patched[0x1a58..0x1a5a], &[0xfc, 0xb2]);

        // Wave 8 patch application tests
        let a2ce = find_by_code("A2CE").expect("found A2CE");
        let mut a2ce_rom = vec![0u8; 0x1000];
        a2ce_rom[0x50a..0x50c].copy_from_slice(&[0x00, 0x7c]);
        let a2ce_patched = a2ce.apply(&a2ce_rom).expect("applied A2CE");
        assert_eq!(&a2ce_patched[0x50a..0x50c], &[0x12, 0x20]);

        let a4xj = find_by_code("A4XJ").expect("found A4XJ");
        let mut a4xj_rom = vec![0u8; 0x10000];
        a4xj_rom[0x5b50..0x5b52].copy_from_slice(&[0x01, 0x38]);
        let a4xj_patched = a4xj.apply(&a4xj_rom).expect("applied A4XJ");
        assert_eq!(&a4xj_patched[0x5b50..0x5b52], &[0x2f, 0x20]);

        let agre = find_by_code("AGRE").expect("found AGRE");
        let mut agre_rom = vec![0u8; 0x1000];
        agre_rom[0x1d8..0x1da].copy_from_slice(&[0x01, 0x20]);
        let agre_patched = agre.apply(&agre_rom).expect("applied AGRE");
        assert_eq!(&agre_patched[0x1d8..0x1da], &[0x1f, 0x20]);

        let aj8j = find_by_code("AJ8J").expect("found AJ8J");
        let mut aj8j_rom = vec![0u8; 0x10000];
        aj8j_rom[0x25c8..0x25ca].copy_from_slice(&[0x02, 0x20]);
        let aj8j_patched = aj8j.apply(&aj8j_rom).expect("applied AJ8J");
        assert_eq!(&aj8j_patched[0x25c8..0x25ca], &[0x00, 0x20]);

        let btde = find_by_code("BTDE").expect("found BTDE");
        let mut btde_rom = vec![0u8; 0x1000];
        btde_rom[0xa30..0xa32].copy_from_slice(&[0x06, 0x20]);
        let btde_patched = btde.apply(&btde_rom).expect("applied BTDE");
        assert_eq!(&btde_patched[0xa30..0xa32], &[0x02, 0x20]);

        // Wave 9 patch application tests
        let bmxe = find_by_code("BMXE").expect("found BMXE");
        let mut bmxe_rom = vec![0u8; 0x1000];
        bmxe_rom[0x260..0x262].copy_from_slice(&[0x40, 0x5e]);
        let bmxe_patched = bmxe.apply(&bmxe_rom).expect("applied BMXE");
        assert_eq!(&bmxe_patched[0x260..0x262], &[0x03, 0x20]);

        let u3ie = find_by_code("U3IE").expect("found U3IE");
        let mut u3ie_rom = vec![0u8; 0x200000];
        u3ie_rom[0x1d1c34..0x1d1c36].copy_from_slice(&[0x01, 0xd0]);
        let u3ie_patched = u3ie.apply(&u3ie_rom).expect("applied U3IE");
        assert_eq!(&u3ie_patched[0x1d1c34..0x1d1c36], &[0x01, 0xe0]);

        let bz3e = find_by_code("BZ3E").expect("found BZ3E");
        let mut bz3e_rom = vec![0u8; 0x100000];
        bz3e_rom[0xeb3e2..0xeb3e4].copy_from_slice(&[0x20, 0x79]);
        let bz3e_patched = bz3e.apply(&bz3e_rom).expect("applied BZ3E");
        assert_eq!(&bz3e_patched[0xeb3e2..0xeb3e4], &[0x04, 0x20]);

        let bpee = find_by_code("BPEE").expect("found BPEE");
        let mut bpee_rom = vec![0u8; 0x200000];
        bpee_rom[0x16ccd4..0x16ccd6].copy_from_slice(&[0x2c, 0x78]);
        let bpee_patched = bpee.apply(&bpee_rom).expect("applied BPEE");
        assert_eq!(&bpee_patched[0x16ccd4..0x16ccd6], &[0x8d, 0x24]);

        let agse = find_by_code("AGSE").expect("found AGSE");
        let mut agse_rom = vec![0u8; 0x100000];
        agse_rom[0x8a8fc..0x8a8fe].copy_from_slice(&[0x19, 0xd0]);
        let agse_patched = agse.apply(&agse_rom).expect("applied AGSE");
        assert_eq!(&agse_patched[0x8a8fc..0x8a8fe], &[0x01, 0xe0]);

        // Wave 10 patch application tests
        let azle = find_by_code("AZLE").expect("found AZLE");
        let mut azle_rom = vec![0u8; 0x20000];
        azle_rom[0xb1be..0xb1c0].copy_from_slice(&[0x00, 0x78]);
        let azle_patched = azle.apply(&azle_rom).expect("applied AZLE");
        assert_eq!(&azle_patched[0xb1be..0xb1c0], &[0x03, 0x20]);

        let aw2e = find_by_code("AW2E").expect("found AW2E");
        let mut aw2e_rom = vec![0u8; 0x50000];
        aw2e_rom[0x36db2..0x36db4].copy_from_slice(&[0x1d, 0xd0]);
        let aw2e_patched = aw2e.apply(&aw2e_rom).expect("applied AW2E");
        assert_eq!(&aw2e_patched[0x36db2..0x36db4], &[0x00, 0x00]);

        let azwe = find_by_code("AZWE").expect("found AZWE");
        let mut azwe_rom = vec![0u8; 0x1000];
        azwe_rom[0x32a..0x32c].copy_from_slice(&[0x68, 0x5e]);
        let azwe_patched = azwe.apply(&azwe_rom).expect("applied AZWE");
        assert_eq!(&azwe_patched[0x32a..0x32c], &[0x06, 0x20]);

        let ae7e = find_by_code("AE7E").expect("found AE7E");
        let mut ae7e_rom = vec![0u8; 0x20000];
        ae7e_rom[0x125fc..0x125fe].copy_from_slice(&[0x03, 0xd1]);
        let ae7e_patched = ae7e.apply(&ae7e_rom).expect("applied AE7E");
        assert_eq!(&ae7e_patched[0x125fc..0x125fe], &[0xc0, 0x46]);

        let amae = find_by_code("AMAE").expect("found AMAE");
        let mut amae_rom = vec![0u8; 0x10000];
        amae_rom[0x1c78..0x1c7a].copy_from_slice(&[0x11, 0x78]);
        let amae_patched = amae.apply(&amae_rom).expect("applied AMAE");
        assert_eq!(&amae_patched[0x1c78..0x1c7a], &[0x01, 0x21]);

        // Wave 11 patch application tests
        let a2jj = find_by_code("A2JJ").expect("found A2JJ");
        let mut a2jj_rom = vec![0u8; 0x1000];
        a2jj_rom[0x312..0x316].copy_from_slice(&[0x0f, 0x48, 0x04, 0x80]);
        let a2jj_patched = a2jj.apply(&a2jj_rom).expect("applied A2JJ");
        assert_eq!(&a2jj_patched[0x312..0x316], &[0x05, 0x21, 0x01, 0x70]);

        let bxke = find_by_code("BXKE").expect("found BXKE");
        let mut bxke_rom = vec![0u8; 0x810000];
        bxke_rom[0x1c8..0x1cc].copy_from_slice(&[0xd1, 0x01, 0x00, 0x08]);
        bxke_rom[0x800464..0x800466].copy_from_slice(&[0xb8, 0x04]);
        let bxke_patched = bxke.apply(&bxke_rom).expect("applied BXKE");
        assert_eq!(&bxke_patched[0x1c8..0x1cc], &[0x00, 0x00, 0x80, 0x08]);
        assert_eq!(&bxke_patched[0x800464..0x800466], &[0x1e, 0x05]);

        // Wave 12 patch application tests
        let b24e = find_by_code("B24E").expect("found B24E");
        let mut b24e_rom = vec![0u8; 0x40000];
        b24e_rom[0x3d2..0x3d4].copy_from_slice(&[0x0e, 0x20]);
        b24e_rom[0x4d8..0x4da].copy_from_slice(&[0x35, 0xf0]);
        b24e_rom[0x35796..0x35798].copy_from_slice(&[0x01, 0x22]);
        let b24e_patched = b24e.apply(&b24e_rom).expect("applied B24E");
        assert_eq!(&b24e_patched[0x3d2..0x3d4], &[0x14, 0xe0]);
        assert_eq!(&b24e_patched[0x4d8..0x4da], &[0x23, 0xe0]);
        assert_eq!(&b24e_patched[0x35796..0x35798], &[0x10, 0x22]);

        let bd5j = find_by_code("BD5J").expect("found BD5J");
        let mut bd5j_rom = vec![0u8; 0x1000];
        bd5j_rom[0x39e..0x3a0].copy_from_slice(&[0x20, 0x68]);
        let bd5j_patched = bd5j.apply(&bd5j_rom).expect("applied BD5J");
        assert_eq!(&bd5j_patched[0x39e..0x3a0], &[0x5a, 0x20]);

        // Wave 13 patch application tests
        let a3jj = find_by_code("A3JJ").expect("found A3JJ");
        let mut a3jj_rom = vec![0u8; 0x170000];
        a3jj_rom[0x1610b8..0x1610bc].copy_from_slice(&[0x09, 0xcc, 0x00, 0x08]);
        let a3jj_patched = a3jj.apply(&a3jj_rom).expect("applied A3JJ");
        assert_eq!(&a3jj_patched[0x1610b8..0x1610bc], &[0x29, 0x1a, 0x00, 0x08]);

        let alfe = find_by_code("ALFE").expect("found ALFE");
        let mut alfe_rom = vec![0u8; 0x1000];
        alfe_rom[0x1e6..0x1ea].copy_from_slice(&[0x1d, 0xf0, 0xc7, 0xf8]);
        let alfe_patched = alfe.apply(&alfe_rom).expect("applied ALFE");
        assert_eq!(&alfe_patched[0x1e6..0x1ea], &[0x05, 0xf0, 0x63, 0xfa]);

        let bktp = find_by_code("BKTP").expect("found BKTP");
        let mut bktp_rom = vec![0u8; 0x1000];
        bktp_rom[0x410] = 0x00;
        let bktp_patched = bktp.apply(&bktp_rom).expect("applied BKTP");
        assert_eq!(bktp_patched[0x410], 0xa5);

        // Wave 14 patch application tests
        let azqe = find_by_code("AZQE").expect("found AZQE");
        let mut azqe_rom = vec![0u8; 0x100000];
        azqe_rom[0x0ea2e8..0x0ea2f8].copy_from_slice(&[
            0x49, 0xd9, 0x00, 0x08, 0x6d, 0xda, 0x00, 0x08, 0xfd, 0xea, 0x00, 0x08, 0xe9, 0xe6,
            0x00, 0x08,
        ]);
        let azqe_patched = azqe.apply(&azqe_rom).expect("applied AZQE");
        assert_eq!(
            &azqe_patched[0x0ea2e8..0x0ea2f8],
            &[
                0xed, 0xc7, 0x01, 0x08, 0xd5, 0xc8, 0x01, 0x08, 0x85, 0xc8, 0x01, 0x08, 0x45,
                0xc9, 0x01, 0x08
            ]
        );

        let byue = find_by_code("BYUE").expect("found BYUE");
        let mut byue_rom = vec![0u8; 0x100000];
        byue_rom[0x0ad76c..0x0ad778].copy_from_slice(&[
            0x89, 0xec, 0x06, 0x08, 0x85, 0xee, 0x06, 0x08, 0x5d, 0xf0, 0x06, 0x08,
        ]);
        let byue_patched = byue.apply(&byue_rom).expect("applied BYUE");
        assert_eq!(
            &byue_patched[0x0ad76c..0x0ad778],
            &[
                0x05, 0x5c, 0x00, 0x08, 0x69, 0x5e, 0x00, 0x08, 0x29, 0x6d, 0x00, 0x08
            ]
        );

        let aorj = find_by_code("AORJ").expect("found AORJ");
        let mut aorj_rom = vec![0u8; 0x80000];
        aorj_rom[0x0b92..0x0b96].copy_from_slice(&[0xff, 0xf7, 0xe5, 0xfe]);
        aorj_rom[0x7361a..0x7361e].copy_from_slice(&[0x08, 0xf0, 0xed, 0xfe]);
        aorj_rom[0x73626..0x7362a].copy_from_slice(&[0x05, 0xf0, 0x59, 0xfc]);
        aorj_rom[0x7517e..0x75180].copy_from_slice(&[0x0c, 0xd0]);
        let aorj_patched = aorj.apply(&aorj_rom).expect("applied AORJ");
        assert_eq!(&aorj_patched[0x0b92..0x0b96], &[0x74, 0xf0, 0x2b, 0xfa]);
        assert_eq!(&aorj_patched[0x7361a..0x7361e], &[0xc0, 0x46, 0xc0, 0x46]);
        assert_eq!(&aorj_patched[0x73626..0x7362a], &[0xc0, 0x46, 0xc0, 0x46]);
        assert_eq!(&aorj_patched[0x7517e..0x75180], &[0xc0, 0x46]);
    }

    #[test]
    fn test_unified_format_detection_and_apply() {
        let source = b"CleanRoomGbaHeaderAndPayload0123456789";
        let target = b"CleanRoomGbaHeaderAndHACKED_0123456789";

        // IPS
        let ips_bytes = formatkit_patch::ips::create(source, target).expect("create ips");
        assert_eq!(detect_format(&ips_bytes), Some(PatchFormat::Ips));
        let res_ips = apply_patch_unified(&ips_bytes, source).expect("apply unified ips");
        assert_eq!(res_ips, target);

        // UPS
        let ups_bytes = UpsPatch::create(source, target);
        assert_eq!(detect_format(&ups_bytes), Some(PatchFormat::Ups));
        let res_ups = apply_patch_unified(&ups_bytes, source).expect("apply unified ups");
        assert_eq!(res_ups, target);

        // BPS
        let bps_bytes = BpsPatch::create(source, target, None);
        assert_eq!(detect_format(&bps_bytes), Some(PatchFormat::Bps));
        let res_bps = apply_patch_unified(&bps_bytes, source).expect("apply unified bps");
        assert_eq!(res_bps, target);

        // Unknown format
        let invalid = b"NOPE_HEADER_DATA";
        assert_eq!(detect_format(invalid), None);
        assert!(matches!(
            apply_patch_unified(invalid, source),
            Err(UnifiedPatchError::UnknownFormat)
        ));
    }
}
