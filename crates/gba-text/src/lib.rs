//! Game Boy Advance text localization toolkit:
//! - Fan-translation `.tbl` charmap parser, decoder, longest-match encoder, and built-in presets
//!   (`ascii`, `pokemon-gen3-en`, `shift-jis`, `capcom-megaman-en`, `breath-of-fire-en`,
//!   `tactics-ogre-en`, `metroid-fusion-en`, `utf16-le`, plus custom ASCII-offset helpers).
//! - String extraction (raw and BIOS LZ77 `0x10`), `0x08xxxxxx` ROM pointer-table scanning, and
//!   capacity-checked in-place or free-space-relocated string write-back with automatic ROM
//!   pointer patching.
//! - Custom 16-bit Huffman tree text decode/encode/replace (*Fire Emblem* GBA lineage).
//! - 1bpp and 2bpp bitmap/VWF font sheet PNG/PPM dump/replace and VWF width table tools.

#![deny(unsafe_code)]

pub mod charmap;
pub mod font;
pub mod huffman;
pub mod string;

pub use charmap::{BuiltinCharmapInfo, Charmap, BUILTIN_CHARMAPS};
pub use font::{
    compute_vwf_widths_from_sheet, dump_font_sheet, dump_font_sheet_rgba, encode_glyph_1bpp,
    encode_glyph_2bpp, read_vwf_widths, replace_font_sheet_in_rom, replace_vwf_widths_in_rom,
    FontBpp, FontImageFormat, FontReplaceReport, FontSheetConfig, VwfWidthsReplaceReport,
};
pub use huffman::{
    format_huffman_text, parse_huffman_text, replace_huffman_text_in_rom, HuffmanDecoded,
    HuffmanTree,
};
pub use string::{
    extract_lz77_string, extract_string, replace_lz77_string_in_rom, replace_string_in_rom,
    scan_string_tables, ExtractedLz77String, ExtractedString, ScannedStringTable,
    StringTableEntry, TextReplaceMode, TextReplaceOptions, TextReplaceReport,
    DEFAULT_MAX_STRING_LEN, DEFAULT_RELOCATE_ALIGNMENT,
};
