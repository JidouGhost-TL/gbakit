//! Checked Game Boy Advance (AGB) 192-byte cartridge header, bus mapping, and SDK backup tags.

#![deny(unsafe_code)]

use formatkit_core::{Error, Reader, Result};

/// Minimum valid GBA cartridge size (192-byte header).
pub const HEADER_SIZE: usize = 0xC0;
/// Maximum standard GBA cartridge ROM bus window (32 MiB).
pub const MAX_ROM_SIZE: usize = 32 * 1024 * 1024;
/// Base bus address for Wait State 0 ROM mapping.
pub const ROM_BUS_BASE: u32 = 0x0800_0000;
/// Base bus address for GBA Multiboot EWRAM images.
pub const MULTIBOOT_BUS_BASE: u32 = 0x0200_0000;
/// Offset of the ARM branch entry instruction inside a GBA Multiboot header.
pub const MULTIBOOT_ENTRY_OFFSET: usize = 0xC0;
/// Maximum size of a GBA Multiboot image (256 KiB EWRAM).
pub const MAX_MULTIBOOT_SIZE: usize = 256 * 1024;
/// Expected fixed value at header offset 0xB2.
pub const FIXED_VALUE: u8 = 0x96;
/// Byte offset of the 156-byte GBA boot logo inside the 192-byte cartridge header.
pub const AGB_LOGO_OFFSET: usize = 0x04;
/// Byte length of the GBA boot logo (`0x04..0xA0`).
pub const AGB_LOGO_LEN: usize = 156;
/// Bank alignment (64 KiB) for multi-cart compilation sub-ROM boundaries.
pub const MULTICART_BANK_ALIGN: usize = 0x10000;
/// Canonical format ID for a bank-aligned multi-cart sub-ROM header extent.
pub const FORMAT_ID_MULTICART_SUBROM: &str = "rom.multicart.subrom";

/// Canonical 156-byte GBA cartridge boot logo stored at header offsets `0x04..0xA0`.
pub const AGB_NINTENDO_LOGO: [u8; AGB_LOGO_LEN] = [
    0x24, 0xFF, 0xAE, 0x51, 0x69, 0x9A, 0xA2, 0x21, 0x3D, 0x84, 0x82, 0x0A,
    0x84, 0xE4, 0x09, 0xAD, 0x11, 0x24, 0x8B, 0x98, 0xC0, 0x81, 0x7F, 0x21,
    0xA3, 0x52, 0xBE, 0x19, 0x93, 0x09, 0xCE, 0x20, 0x10, 0x46, 0x4A, 0x4A,
    0xF8, 0x27, 0x31, 0xEC, 0x58, 0xC7, 0xE8, 0x33, 0x82, 0xE3, 0xCE, 0xBF,
    0x85, 0xF4, 0xDF, 0x94, 0xCE, 0x4B, 0x09, 0xC1, 0x94, 0x56, 0x8A, 0xC0,
    0x13, 0x72, 0xA7, 0xFC, 0x9F, 0x84, 0x4D, 0x73, 0xA3, 0xCA, 0x9A, 0x61,
    0x58, 0x97, 0xA3, 0x27, 0xFC, 0x03, 0x98, 0x76, 0x23, 0x1D, 0xC7, 0x61,
    0x03, 0x04, 0xAE, 0x56, 0xBF, 0x38, 0x84, 0x00, 0x40, 0xA7, 0x0E, 0xFD,
    0xFF, 0x52, 0xFE, 0x03, 0x6F, 0x95, 0x30, 0xF1, 0x97, 0xFB, 0xC0, 0x85,
    0x60, 0xD6, 0x80, 0x25, 0xA9, 0x63, 0xBE, 0x03, 0x01, 0x4E, 0x38, 0xE2,
    0xF9, 0xA2, 0x34, 0xFF, 0xBB, 0x3E, 0x03, 0x44, 0x78, 0x00, 0x90, 0xCB,
    0x88, 0x11, 0x3A, 0x94, 0x65, 0xC0, 0x7C, 0x63, 0x87, 0xF0, 0x3C, 0xAF,
    0xD6, 0x25, 0xE4, 0x8B, 0x38, 0x0A, 0xAC, 0x72, 0x21, 0xD4, 0xF8, 0x07,
];

/// Returns `true` if `rom_slice` starts with the exact 156-byte GBA boot logo at `0x04..0xA0`.
pub fn has_agb_nintendo_logo(rom_slice: &[u8]) -> bool {
    rom_slice
        .get(AGB_LOGO_OFFSET..AGB_LOGO_OFFSET + AGB_LOGO_LEN)
        .is_some_and(|logo| logo == AGB_NINTENDO_LOGO)
}

/// A bank-aligned sub-ROM discovered inside a multi-cart compilation cartridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MulticartSubRom {
    /// Byte offset where the sub-ROM begins (64 KiB aligned, `>= 0x10000`).
    pub offset: usize,
    /// Byte length of the sub-ROM slice `[offset .. next_sub_offset_or_end]`.
    pub length: usize,
    /// Header title (ASCII, up to 12 chars).
    pub title: String,
    /// Header 4-character game code.
    pub game_code: String,
    /// Header 2-character maker code.
    pub maker_code: String,
}

/// Scan `rom` at `>= 0x10000` on 64 KiB (`0x10000`) bank-aligned boundaries for embedded
/// full GBA sub-ROMs matching the exact 156-byte boot logo (`+0x04..+0xA0`), fixed `0x96`
/// byte (`+0xB2`), and valid header complement checksum (`+0xBD`).
///
/// Each discovered sub-ROM spans `[sub_base .. next_sub_base_or_end]`.
pub fn scan_multicart_subroms(rom: &[u8]) -> Vec<MulticartSubRom> {
    let effective_len = rom.len().min(MAX_ROM_SIZE);
    if effective_len < MULTICART_BANK_ALIGN + HEADER_SIZE {
        return Vec::new();
    }

    let mut found: Vec<(usize, AgbHeader)> = Vec::new();
    let mut off = MULTICART_BANK_ALIGN;
    while off.checked_add(HEADER_SIZE).is_some_and(|end| end <= effective_len) {
        if rom[off + 0xB2] == FIXED_VALUE && has_agb_nintendo_logo(&rom[off..]) {
            if let Ok(hdr) = AgbHeader::parse(&rom[off..]) {
                if hdr.is_valid() {
                    found.push((off, hdr));
                }
            }
        }
        off = off.saturating_add(MULTICART_BANK_ALIGN);
    }

    let mut subroms = Vec::with_capacity(found.len());
    for (idx, (base, hdr)) in found.iter().enumerate() {
        let next_base = found
            .get(idx + 1)
            .map_or(effective_len, |&(next_off, _)| next_off);
        let length = next_base.saturating_sub(*base);
        if length >= HEADER_SIZE {
            subroms.push(MulticartSubRom {
                offset: *base,
                length,
                title: hdr.title.clone(),
                game_code: hdr.game_code.clone(),
                maker_code: hdr.maker_code.clone(),
            });
        }
    }
    subroms
}

/// Parsed 192-byte GBA cartridge header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgbHeader {
    pub entry_opcode: u32,
    pub title: String,
    pub game_code: String,
    pub maker_code: String,
    pub fixed_value: u8,
    pub unit_code: u8,
    pub device_type: u8,
    pub software_version: u8,
    pub header_checksum: u8,
    pub computed_checksum: u8,
}

impl AgbHeader {
    /// Parse a 192-byte GBA cartridge header from the start of a ROM slice.
    pub fn parse(rom: &[u8]) -> Result<Self> {
        if rom.len() < HEADER_SIZE {
            return Err(Error::Truncated {
                offset: 0,
                needed: HEADER_SIZE,
                available: rom.len(),
            });
        }
        let mut r = Reader::new(&rom[..HEADER_SIZE]);
        let entry_opcode = r.u32_le()?;
        r.skip(0xA0 - 4)?;
        let title_bytes = r.bytes(12)?;
        let code_bytes = r.bytes(4)?;
        let maker_bytes = r.bytes(2)?;
        let fixed_value = r.u8()?;
        let unit_code = r.u8()?;
        let device_type = r.u8()?;
        r.skip(0xBC - 0xB5)?;
        let software_version = r.u8()?;
        let header_checksum = r.u8()?;

        let computed_checksum = compute_header_complement(&rom[0xA0..0xBD]);

        Ok(Self {
            entry_opcode,
            title: clean_ascii(title_bytes),
            game_code: clean_ascii(code_bytes),
            maker_code: clean_ascii(maker_bytes),
            fixed_value,
            unit_code,
            device_type,
            software_version,
            header_checksum,
            computed_checksum,
        })
    }

    /// Returns true if the complement checksum and fixed 0x96 marker are both valid.
    pub fn is_valid(&self) -> bool {
        self.fixed_value == FIXED_VALUE && self.header_checksum == self.computed_checksum
    }

    /// 3-character header family code (e.g., `"BPE"` for `"BPEE"`).
    pub fn header_family(&self) -> &str {
        self.game_code.get(..3).unwrap_or(&self.game_code)
    }
}

/// Compute the GBA header complement checksum over bytes `[0xA0..=0xBC]`.
pub fn compute_header_complement(slice_a0_to_bc: &[u8]) -> u8 {
    let mut chk: u8 = 0;
    for &b in slice_a0_to_bc {
        chk = chk.wrapping_sub(b);
    }
    chk.wrapping_sub(0x19)
}

/// Map a GBA Wait State 0 ROM bus address (`0x08000000..`) to a ROM file offset.
pub fn bus_to_rom_offset(bus_addr: u32, rom_len: usize) -> Option<usize> {
    if bus_addr < ROM_BUS_BASE {
        return None;
    }
    let off = (bus_addr - ROM_BUS_BASE) as usize;
    if off < rom_len {
        Some(off)
    } else {
        None
    }
}

/// Map a ROM file offset to a GBA Wait State 0 bus address (`0x08000000..`).
pub fn rom_offset_to_bus(offset: usize) -> Option<u32> {
    if offset >= MAX_ROM_SIZE {
        return None;
    }
    Some(ROM_BUS_BASE + offset as u32)
}

/// Standard GBA SDK backup-media signature strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SdkSaveTag {
    Eeprom,
    Sram,
    SramF,
    Flash,
    Flash512,
    Flash1M,
}

impl SdkSaveTag {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eeprom => "EEPROM_V",
            Self::Sram => "SRAM_V",
            Self::SramF => "SRAM_F_V",
            Self::Flash => "FLASH_V",
            Self::Flash512 => "FLASH512_V",
            Self::Flash1M => "FLASH1M_V",
        }
    }
}

/// Scan a ROM for standard GBA SDK backup-media ASCII signature tags.
pub fn scan_sdk_save_tags(rom: &[u8]) -> Vec<(SdkSaveTag, usize)> {
    const PATTERNS: &[(SdkSaveTag, &[u8])] = &[
        (SdkSaveTag::Eeprom, b"EEPROM_V"),
        (SdkSaveTag::SramF, b"SRAM_F_V"),
        (SdkSaveTag::Sram, b"SRAM_V"),
        (SdkSaveTag::Flash1M, b"FLASH1M_V"),
        (SdkSaveTag::Flash512, b"FLASH512_V"),
        (SdkSaveTag::Flash, b"FLASH_V"),
    ];
    let mut found = Vec::new();
    for &(tag, pat) in PATTERNS {
        if let Some(pos) = find_subslice(rom, pat) {
            found.push((tag, pos));
        }
    }
    found.sort_by_key(|&(_, pos)| pos);
    found
}

pub use formatkit_core::padding::{classify_padding, PaddingKind, PaddingSummary};
pub use formatkit_core::NamespaceEntry;
pub use formatkit_core::PointerTable;
pub mod trace;
pub use trace::{
    analyze_trace, DeveloperJumpTable, RuntimeMode, TraceAnalysisReport, TraceAnalysisSummary,
    UnreachedEntryPoint,
};
pub mod relocate;
pub use relocate::{
    find_free_space, find_free_space_prefer_ff, patch_rom_pointers, SAFE_ZERO_FREE_SPACE_START,
};
pub mod archive;
pub use archive::{
    extract_archive_entries, pack_archive, parse_archive, parse_archive_at,
    replace_archive_in_rom, scan_archives, ArchiveEntry, ArchiveFormat, ArchiveManifest,
    ArchiveManifestEntry, ArchiveReplaceOptions, ArchiveReplaceReport, ParsedArchive,
    ARCHIVE_FREE_SPACE_START, DEFAULT_ARCHIVE_ALIGN, MAX_ARCHIVE_ENTRIES,
};
pub mod callgraph;
pub use callgraph::{
    analyze_callgraph, CallgraphEdge, CallgraphEdgeKind, CallgraphJumpTable, CallgraphNode,
    CallgraphNodeKind, CallgraphNodeStatus, CallgraphReport, CallgraphSummary,
};
pub mod peripherals;
pub use peripherals::{detect_peripherals, DetectedPeripheral, PeripheralKind};
pub mod formats;
pub use formats::{
    scan_all_studio_formats, scan_all_wave1_formats, validate_studio_format_slice,
    validate_wave1_format_slice, DetectedFormatExtent, FORMAT_ID_ANIM_FRAM, FORMAT_ID_BROW,
    FORMAT_ID_IS_ASC03, FORMAT_ID_MAP0, FORMAT_ID_MBM_MCM, FORMAT_ID_MTO0, FORMAT_ID_PAC3,
    FORMAT_ID_PIFF_RBHF, FORMAT_ID_PLT_ENGINE, FORMAT_ID_RIFF_SCR,
};
pub use formatkit_layout::tables::{FixedStrideOffsetTable, TableSizeMode};

/// Summary of trailing hardware padding at the end of a GBA cartridge ROM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RomPadding {
    /// Classified padding kind (e.g. all `0xFF` or all `0x00`).
    pub kind: PaddingKind,
    /// Number of trailing padding bytes.
    pub padding_bytes: usize,
    /// Offset where trailing padding begins (i.e. the true payload size).
    pub payload_size: usize,
}

impl RomPadding {
    /// Returns true if the padding region aligns to `alignment` bytes.
    pub const fn is_aligned_to(&self, alignment: usize) -> bool {
        alignment != 0 && self.padding_bytes.is_multiple_of(alignment)
    }
}

/// Analyze `rom` for uniform trailing hardware padding (e.g. erased NOR Flash `0xFF`
/// or zeroed ROM fill).
///
/// Returns `Some(RomPadding)` if the ROM ends with at least `min_padding_bytes` of
/// uniform padding, identifying the exact payload boundary.
pub fn detect_rom_padding(rom: &[u8], min_padding_bytes: usize) -> Option<RomPadding> {
    let trailing = formatkit_core::detect_trailing_padding(rom, min_padding_bytes.max(1))?;
    Some(RomPadding {
        kind: trailing.summary.kind,
        padding_bytes: trailing.summary.byte_len as usize,
        payload_size: trailing.start_offset,
    })
}

/// Parse a fixed-stride table of offsets using `formatkit_layout::tables::fixed_stride_offset`.
pub fn parse_fixed_stride_offset_table(
    table_bytes: &[u8],
    data_size: u64,
    descriptor: &FixedStrideOffsetTable,
) -> Result<Vec<NamespaceEntry>> {
    descriptor.parse_entries(table_bytes, data_size)
}

/// Scan `rom` for non-overlapping, 4-byte aligned contiguous arrays of at least
/// `min_entries` valid Wait State 0 ROM bus pointers (`0x08xxxxxx`..`0x09xxxxxx`)
/// pointing within `rom.len()`, delegating to `formatkit_core::scan_u32_pointer_tables`.
pub fn scan_pointer_tables(rom: &[u8], min_entries: usize) -> Vec<PointerTable> {
    let effective = &rom[..rom.len().min(MAX_ROM_SIZE)];
    formatkit_core::scan_u32_pointer_tables(
        effective,
        ROM_BUS_BASE,
        formatkit_core::Endian::Little,
        min_entries,
    )
}

/// Decode the GBA Multiboot EWRAM (`0x0200_0000`) entry target offset from the ARM
/// unconditional branch (`B`) instruction at header offset `0xC0`.
///
/// Requires a valid standard 192-byte GBA header (fixed `0x96` byte and complement
/// checksum), total image size within the 256 KiB EWRAM limit, an unconditional ARM
/// `B` (`0xEAxxxxxx`) instruction at `0xC0`, and a resolved target within `bytes.len()`.
pub fn multiboot_entry_offset(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < MULTIBOOT_ENTRY_OFFSET + 4 || bytes.len() > MAX_MULTIBOOT_SIZE {
        return None;
    }
    let hdr = AgbHeader::parse(bytes).ok()?;
    if !hdr.is_valid() {
        return None;
    }
    let opcode = u32::from_le_bytes([
        bytes[MULTIBOOT_ENTRY_OFFSET],
        bytes[MULTIBOOT_ENTRY_OFFSET + 1],
        bytes[MULTIBOOT_ENTRY_OFFSET + 2],
        bytes[MULTIBOOT_ENTRY_OFFSET + 3],
    ]);
    // Unconditional ARM B instruction: bits 31..24 == 0xEA.
    if (opcode >> 24) != 0xEA {
        return None;
    }
    let imm24 = (opcode & 0x00FF_FFFF) as i32;
    let signed_words = (imm24 << 8) >> 8;
    let pc = (MULTIBOOT_ENTRY_OFFSET + 8) as isize;
    let target = pc.checked_add((signed_words as isize).checked_mul(4)?)?;
    if target < 0 {
        return None;
    }
    let target_usize = target as usize;
    if target_usize < bytes.len() {
        Some(target_usize)
    } else {
        None
    }
}

/// Returns true if `bytes` has a valid GBA header and a valid Multiboot EWRAM
/// (`0x0200_0000`) entry branch at offset `0xC0`.
pub fn is_multiboot_image(bytes: &[u8]) -> bool {
    multiboot_entry_offset(bytes).is_some()
}

/// An embedded MultiBoot (single-cartridge download play) payload detected inside a parent cartridge ROM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedMultiBootPayload {
    /// Byte offset where the MultiBoot image begins in the parent ROM.
    pub offset: usize,
    /// Header title (ASCII, up to 12 chars).
    pub title: String,
    /// Header 4-character game code.
    pub game_code: String,
    /// Header 2-character maker code.
    pub maker_code: String,
    /// ARM opcode word at header offset `0xC0`.
    pub entry_branch_opcode: u32,
    /// Resolved entry point target offset relative to payload start (usually `0x10C`).
    pub entry_target_offset: usize,
    /// Bounded byte length of the payload (at least 192 bytes, max 256 KiB).
    pub length: usize,
    /// Exact length confirmed via `MultiBootParam.boot_endp` pointer table reference, if found.
    pub confirmed_exact_length: bool,
}

impl EmbeddedMultiBootPayload {
    /// Extract the payload slice from `rom`.
    pub fn extract<'a>(&self, rom: &'a [u8]) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(self.length)?;
        if end <= rom.len() {
            Some(&rom[self.offset..end])
        } else {
            None
        }
    }
}

/// Scans `rom` for embedded MultiBoot download-play images.
///
/// Searches on 4-byte aligned boundaries at or past offset `0x200` (beyond the primary
/// cartridge header) for valid GBA headers (fixed `0x96` byte at `+0xB2`, valid complement
/// checksum at `+0xBD`, and unconditional ARM `B` at `+0xC0`).
pub fn scan_embedded_multiboot(rom: &[u8]) -> Vec<EmbeddedMultiBootPayload> {
    let rom = &rom[..rom.len().min(MAX_ROM_SIZE)];
    if rom.len() < 0x200 + MULTIBOOT_ENTRY_OFFSET + 4 {
        return Vec::new();
    }
    let mut payloads = Vec::new();
    let max_search = rom.len().saturating_sub(MULTIBOOT_ENTRY_OFFSET + 4);

    let mut off = 0x200;
    while off <= max_search {
        // Fast pre-filter: check fixed 0x96 at off + 0xB2
        if rom[off + 0xB2] == 0x96 {
            if let Ok(hdr) = AgbHeader::parse(&rom[off..]) {
                if hdr.is_valid() {
                    let op = u32::from_le_bytes([
                        rom[off + MULTIBOOT_ENTRY_OFFSET],
                        rom[off + MULTIBOOT_ENTRY_OFFSET + 1],
                        rom[off + MULTIBOOT_ENTRY_OFFSET + 2],
                        rom[off + MULTIBOOT_ENTRY_OFFSET + 3],
                    ]);
                    if (op >> 24) == 0xEA {
                        let imm24 = (op & 0x00FF_FFFF) as i32;
                        let signed_words = (imm24 << 8) >> 8;
                        let pc = (MULTIBOOT_ENTRY_OFFSET + 8) as isize;
                        if let Some(target) = pc.checked_add(signed_words as isize * 4) {
                            if target >= 0 && (target as usize) <= MAX_MULTIBOOT_SIZE {
                                let default_len = (rom.len() - off).min(MAX_MULTIBOOT_SIZE);
                                payloads.push(EmbeddedMultiBootPayload {
                                    offset: off,
                                    title: hdr.title.clone(),
                                    game_code: hdr.game_code.clone(),
                                    maker_code: hdr.maker_code.clone(),
                                    entry_branch_opcode: op,
                                    entry_target_offset: target as usize,
                                    length: default_len,
                                    confirmed_exact_length: false,
                                });
                            }
                        }
                    }
                }
            }
        }
        off += 4;
    }

    // Attempt to refine lengths via MultiBootParam boot_endp search or next payload offset
    let word_scan_end = rom.len() & !3;
    for i in 0..payloads.len() {
        let cur_off = payloads[i].offset;
        let Some(bus_addr) = rom_offset_to_bus(cur_off) else {
            continue;
        };
        let Some(max_end_addr) = bus_addr.checked_add(MAX_MULTIBOOT_SIZE as u32) else {
            continue;
        };
        let bus_bytes = bus_addr.to_le_bytes();

        let mut found_exact_end = None;
        let mut abs_pos = 0usize;
        while abs_pos < word_scan_end {
            if rom[abs_pos..abs_pos + 4] == bus_bytes {
                let check_start = abs_pos.saturating_sub(32);
                let check_end = (abs_pos + 36).min(word_scan_end.saturating_sub(4));
                let mut ptr_pos = check_start;
                while ptr_pos <= check_end {
                    let val = u32::from_le_bytes([
                        rom[ptr_pos],
                        rom[ptr_pos + 1],
                        rom[ptr_pos + 2],
                        rom[ptr_pos + 3],
                    ]);
                    if val > bus_addr && val <= max_end_addr {
                        let end_dist = (val - bus_addr) as usize;
                        if end_dist >= HEADER_SIZE && end_dist.is_multiple_of(4) {
                            found_exact_end = Some(end_dist);
                            break;
                        }
                    }
                    ptr_pos += 4;
                }
                if found_exact_end.is_some() {
                    break;
                }
            }
            abs_pos += 4;
        }

        if let Some(exact_len) = found_exact_end {
            payloads[i].length = exact_len;
            payloads[i].confirmed_exact_length = true;
        } else if i + 1 < payloads.len() {
            let next_off = payloads[i + 1].offset;
            if next_off > cur_off {
                payloads[i].length = payloads[i].length.min(next_off - cur_off);
            }
        }
    }

    payloads
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn clean_ascii(bytes: &[u8]) -> String {
    let field = formatkit_core::fixed_field(bytes, bytes.len()).unwrap_or(bytes);
    field
        .iter()
        .map(|&b| if (0x20..=0x7E).contains(&b) { b as char } else { '?' })
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 512];
        rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
        rom[0xA0..0xAC].copy_from_slice(b"TEST GAME   ");
        rom[0xAC..0xB0].copy_from_slice(b"BTSE");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xB2] = FIXED_VALUE;
        rom[0xBC] = 1;
        rom[0xBD] = compute_header_complement(&rom[0xA0..0xBD]);
        rom[0x120..0x12B].copy_from_slice(b"EEPROM_V124");
        rom
    }

    #[test]
    fn parses_valid_header_and_tags() {
        let rom = synthetic_rom();
        let hdr = AgbHeader::parse(&rom).unwrap();
        assert!(hdr.is_valid());
        assert_eq!(hdr.title, "TEST GAME");
        assert_eq!(hdr.game_code, "BTSE");
        assert_eq!(hdr.header_family(), "BTS");
        assert_eq!(hdr.maker_code, "01");
        assert_eq!(hdr.software_version, 1);

        let tags = scan_sdk_save_tags(&rom);
        assert_eq!(tags, vec![(SdkSaveTag::Eeprom, 0x120)]);
    }

    #[test]
    fn bus_mapping_bounds() {
        assert_eq!(bus_to_rom_offset(0x0800_0100, 512), Some(0x100));
        assert_eq!(bus_to_rom_offset(0x0800_0200, 512), None);
        assert_eq!(bus_to_rom_offset(0x07FF_FFFF, 512), None);
        assert_eq!(rom_offset_to_bus(0x100), Some(0x0800_0100));
    }

    #[test]
    fn scans_non_overlapping_pointer_tables() {
        let mut rom = vec![0u8; 512];
        // Table 1 at offset 0x20: 3 valid pointers
        let t1 = [0x0800_0040u32, 0x0800_0080, 0x0800_01F0];
        for (i, ptr) in t1.iter().enumerate() {
            let off = 0x20 + i * 4;
            rom[off..off + 4].copy_from_slice(&ptr.to_le_bytes());
        }
        // Short run of 2 valid pointers at 0x50 (below min_entries=3)
        rom[0x50..0x54].copy_from_slice(&0x0800_0010u32.to_le_bytes());
        rom[0x54..0x58].copy_from_slice(&0x0800_0020u32.to_le_bytes());
        // Out-of-bounds ROM pointer breaks a run
        rom[0x58..0x5C].copy_from_slice(&0x0800_0200u32.to_le_bytes());

        // Table 2 at end of ROM (0x1F0..0x200): 4 valid pointers
        let t2 = [0x0800_0000u32, 0x0800_0004, 0x0800_0008, 0x0800_000C];
        for (i, ptr) in t2.iter().enumerate() {
            let off = 0x1F0 + i * 4;
            rom[off..off + 4].copy_from_slice(&ptr.to_le_bytes());
        }

        let tables = scan_pointer_tables(&rom, 3);
        assert_eq!(
            tables,
            vec![
                PointerTable {
                    offset: 0x20,
                    entries: t1.to_vec(),
                },
                PointerTable {
                    offset: 0x1F0,
                    entries: t2.to_vec(),
                },
            ]
        );
        assert!(scan_pointer_tables(&rom, 0).is_empty());
        assert!(scan_pointer_tables(&rom, 5).is_empty());
    }

    #[test]
    fn detects_multiboot_image_and_decodes_entry_offset() {
        let mut img = synthetic_rom();
        // Standard cartridge ROM has zeros at 0xC0 -> not multiboot
        assert!(!is_multiboot_image(&img));
        assert_eq!(multiboot_entry_offset(&img), None);

        // Place ARM `B 0xE4` at 0xC0: PC = 0xC8, delta = 0xE4 - 0xC8 = 0x1C = 7 words
        img[0xC0..0xC4].copy_from_slice(&0xEA00_0007u32.to_le_bytes());
        assert!(is_multiboot_image(&img));
        assert_eq!(multiboot_entry_offset(&img), Some(0xE4));

        // Corrupt header checksum -> rejected
        let mut bad_chk = img.clone();
        bad_chk[0xBD] ^= 0xFF;
        assert!(!is_multiboot_image(&bad_chk));
        assert_eq!(multiboot_entry_offset(&bad_chk), None);

        // Branch target past end of image -> rejected
        let mut bad_target = img.clone();
        bad_target[0xC0..0xC4].copy_from_slice(&0xEA00_1000u32.to_le_bytes());
        assert!(!is_multiboot_image(&bad_target));
        assert_eq!(multiboot_entry_offset(&bad_target), None);

        // Oversized (> 256 KiB EWRAM) -> rejected
        let mut oversized = img.clone();
        oversized.resize(MAX_MULTIBOOT_SIZE + 4, 0);
        assert!(!is_multiboot_image(&oversized));
        assert_eq!(multiboot_entry_offset(&oversized), None);
    }

    #[test]
    fn detects_rom_trailing_padding_ff_and_zero() {
        let mut rom = synthetic_rom();
        rom.truncate(256);
        // Pad with 256 bytes of 0xFF (standard unwritten NOR flash)
        rom.extend_from_slice(&[0xFF; 256]);
        let pad = detect_rom_padding(&rom, 64).expect("should detect 0xFF padding");
        assert_eq!(pad.kind, PaddingKind::RepeatedByte(0xFF));
        assert_eq!(pad.padding_bytes, 256);
        assert_eq!(pad.payload_size, 256);
        assert!(pad.is_aligned_to(16));
        assert!(pad.is_aligned_to(256));

        // Pad with 128 bytes of 0x00
        let mut zero_rom = vec![0x42u8; 128];
        zero_rom.extend_from_slice(&[0x00; 128]);
        let pad0 = detect_rom_padding(&zero_rom, 32).expect("should detect 0x00 padding");
        assert_eq!(pad0.kind, PaddingKind::AllZero);
        assert_eq!(pad0.padding_bytes, 128);
        assert_eq!(pad0.payload_size, 128);

        // No padding (ends with arbitrary payload)
        let unpadded = vec![0x12, 0x34, 0x56, 0x78];
        assert_eq!(detect_rom_padding(&unpadded, 4), None);
    }

    #[test]
    fn parses_fixed_stride_offset_table_entries() {
        // Table of two 8-byte entries: offset (u32), size (u32)
        let mut table = Vec::new();
        table.extend_from_slice(&100u32.to_le_bytes());
        table.extend_from_slice(&50u32.to_le_bytes());
        table.extend_from_slice(&200u32.to_le_bytes());
        table.extend_from_slice(&80u32.to_le_bytes());

        let desc = FixedStrideOffsetTable {
            entry_count: 2,
            entry_stride: 8,
            offset_field: 0,
            offset_unit: 1,
            size_mode: TableSizeMode::ExplicitU32 {
                field_offset: 4,
                unit: 1,
            },
            name_prefix: "asset_",
        };

        let entries = parse_fixed_stride_offset_table(&table, 512, &desc).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name.as_deref(), Some("asset_0000"));
        assert_eq!(entries[0].offset, 100);
        assert_eq!(entries[0].size, 50);
        assert_eq!(entries[1].name.as_deref(), Some("asset_0001"));
        assert_eq!(entries[1].offset, 200);
        assert_eq!(entries[1].size, 80);
    }

    #[test]
    fn scans_embedded_multiboot_payloads() {
        let mut rom = vec![0xEEu8; 2048];
        rom[..512].copy_from_slice(&synthetic_rom());

        // Embed a MultiBoot payload at offset 1024 (0x400)
        let mut mb = synthetic_rom();
        mb[0xA0..0xAC].copy_from_slice(b"MB DEMO     ");
        mb[0xAC..0xB0].copy_from_slice(b"AMBE");
        mb[0xBD] = compute_header_complement(&mb[0xA0..0xBD]);
        mb[0xC0..0xC4].copy_from_slice(&0xEA00_0011u32.to_le_bytes());

        rom[1024..1024 + 512].copy_from_slice(&mb);

        // Put a pointer table referencing this multiboot payload at offset 0x100:
        // boot_srcp = 0x08000400, boot_endp = 0x08000600 (512 bytes length)
        rom[0x100..0x104].copy_from_slice(&0x0800_0400u32.to_le_bytes());
        rom[0x108..0x10C].copy_from_slice(&0x0800_0600u32.to_le_bytes());

        let found = scan_embedded_multiboot(&rom);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].offset, 1024);
        assert_eq!(found[0].title, "MB DEMO");
        assert_eq!(found[0].game_code, "AMBE");
        assert_eq!(found[0].entry_target_offset, 0x10C);
        assert_eq!(found[0].length, 512);
        assert!(found[0].confirmed_exact_length);

        let extracted = found[0].extract(&rom).unwrap();
        assert_eq!(extracted.len(), 512);
        assert!(is_multiboot_image(extracted));
    }

    #[test]
    fn scans_multicart_subroms_on_64k_boundaries() {
        let mut rom = vec![0xFFu8; 0x50000];
        // Outer header at 0x00000
        let mut outer = synthetic_rom();
        outer[AGB_LOGO_OFFSET..AGB_LOGO_OFFSET + AGB_LOGO_LEN]
            .copy_from_slice(&AGB_NINTENDO_LOGO);
        rom[..outer.len()].copy_from_slice(&outer);

        // Valid Sub-ROM #1 at 0x10000
        let mut sub1 = outer.clone();
        sub1[0xA0..0xAC].copy_from_slice(b"SUB GAME 1  ");
        sub1[0xAC..0xB0].copy_from_slice(b"BS1E");
        sub1[0xBD] = compute_header_complement(&sub1[0xA0..0xBD]);
        rom[0x10000..0x10000 + sub1.len()].copy_from_slice(&sub1);

        // Unaligned header at 0x20004 (must be ignored — not 64 KiB aligned)
        rom[0x20004..0x20004 + sub1.len()].copy_from_slice(&sub1);

        // Corrupted logo at 0x20000 (must be ignored)
        let mut bad_logo = sub1.clone();
        bad_logo[AGB_LOGO_OFFSET] ^= 0xFF;
        rom[0x20000..0x20000 + bad_logo.len()].copy_from_slice(&bad_logo);

        // Valid Sub-ROM #2 at 0x30000
        let mut sub2 = outer;
        sub2[0xA0..0xAC].copy_from_slice(b"SUB GAME 2  ");
        sub2[0xAC..0xB0].copy_from_slice(b"BS2E");
        sub2[0xBD] = compute_header_complement(&sub2[0xA0..0xBD]);
        rom[0x30000..0x30000 + sub2.len()].copy_from_slice(&sub2);

        // Corrupted checksum at 0x40000 (must be ignored)
        let mut bad_chk = sub2;
        bad_chk[0xBD] ^= 0x01;
        rom[0x40000..0x40000 + bad_chk.len()].copy_from_slice(&bad_chk);

        let subs = scan_multicart_subroms(&rom);
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].offset, 0x10000);
        assert_eq!(subs[0].length, 0x20000);
        assert_eq!(subs[0].game_code, "BS1E");
        assert_eq!(subs[1].offset, 0x30000);
        assert_eq!(subs[1].length, 0x20000);
        assert_eq!(subs[1].game_code, "BS2E");
    }
}
