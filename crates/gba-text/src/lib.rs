//! Game Boy Advance text localization toolkit:
//! - Fan-translation `.tbl` charmap parser, decoder, longest-match encoder, and built-in presets
//!   (`ascii`, `pokemon-gen3-en`, `shift-jis`, plus custom ASCII-offset helpers).
//! - String extraction, `0x08xxxxxx` ROM pointer-table scanning, and capacity-checked in-place
//!   or free-space-relocated string write-back with automatic ROM pointer patching.
//! - 1bpp and 2bpp bitmap/VWF font sheet PNG/PPM dump and in-place replacement.

#![deny(unsafe_code)]

pub mod charmap;
pub mod font;
pub mod string;

pub use charmap::{BuiltinCharmapInfo, Charmap, BUILTIN_CHARMAPS};
pub use font::{
    dump_font_sheet, dump_font_sheet_rgba, encode_glyph_1bpp, encode_glyph_2bpp,
    replace_font_sheet_in_rom, FontBpp, FontImageFormat, FontReplaceReport, FontSheetConfig,
};
pub use string::{
    extract_string, replace_string_in_rom, scan_string_tables, ExtractedString, ScannedStringTable,
    StringTableEntry, TextReplaceMode, TextReplaceOptions, TextReplaceReport,
    DEFAULT_MAX_STRING_LEN, DEFAULT_RELOCATE_ALIGNMENT,
};
