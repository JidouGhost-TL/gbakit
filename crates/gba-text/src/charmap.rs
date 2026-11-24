//! Fan-translation `.tbl` charmap parser, decoder, longest-match encoder, and built-in presets.

#![deny(unsafe_code)]

use std::collections::HashMap;

use formatkit_core::{Error, Result};
use serde::{Deserialize, Serialize};

/// Metadata describing a built-in charmap preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltinCharmapInfo {
    /// Canonical preset identifier passed to [`Charmap::builtin`].
    pub name: &'static str,
    /// Human-readable description of the encoding.
    pub description: &'static str,
    /// Default string terminator byte(s) in hex, if defined.
    pub default_terminator_hex: &'static str,
}

/// Built-in charmap presets available via [`Charmap::builtin`].
pub const BUILTIN_CHARMAPS: &[BuiltinCharmapInfo] = &[
    BuiltinCharmapInfo {
        name: "ascii",
        description: "Standard printable ASCII (0x20..=0x7E, 0x0A=\\n, 0x00={END})",
        default_terminator_hex: "00",
    },
    BuiltinCharmapInfo {
        name: "pokemon-gen3-en",
        description: "Pokemon Ruby/Sapphire/FireRed/LeafGreen/Emerald English table (0xFF={END})",
        default_terminator_hex: "FF",
    },
    BuiltinCharmapInfo {
        name: "shift-jis",
        description: "Shift-JIS (ASCII, half-width Katakana, JIS X 0208 Hiragana/Katakana/full-width, 0x00={END})",
        default_terminator_hex: "00",
    },
    BuiltinCharmapInfo {
        name: "capcom-megaman-en",
        description: "Capcom Mega Man Zero / Battle Network English table (0x00=' ', 0x01='0', 0x0B='A', 0x25='a', 0xFF={END})",
        default_terminator_hex: "FF",
    },
    BuiltinCharmapInfo {
        name: "breath-of-fire-en",
        description: "Capcom Breath of Fire 1/2 English table (0x02=' ', 0x2A='A', 0x48='a', 0x00={END})",
        default_terminator_hex: "00",
    },
    BuiltinCharmapInfo {
        name: "tactics-ogre-en",
        description: "Tactics Ogre: The Knight of Lodis English table (0x00='A', 0x1A='a', 0x89=' ', 0xFF={END})",
        default_terminator_hex: "FF",
    },
    BuiltinCharmapInfo {
        name: "metroid-fusion-en",
        description: "Metroid Fusion 16-bit LE English table (0x0040=' ', 0x0081='A', 0x00C1='a', 0xFF00={END})",
        default_terminator_hex: "00FF",
    },
    BuiltinCharmapInfo {
        name: "utf16-le",
        description: "Standard UTF-16LE null-terminated Unicode (full BMP decode/encode, 0x0000={END})",
        default_terminator_hex: "0000",
    },
];

/// A bidirectional byte-sequence <-> UTF-8 text mapping table (`.tbl`).
#[derive(Debug, Clone)]
pub struct Charmap {
    name: String,
    description: String,
    byte_to_text: HashMap<Vec<u8>, String>,
    single_byte_to_text: [Option<Box<str>>; 256],
    has_multibyte_prefix: [bool; 256],
    text_to_bytes: HashMap<String, Vec<u8>>,
    text_first_byte_max_len: [usize; 256],
    max_byte_len: usize,
    max_text_len: usize,
    default_terminator: Option<Vec<u8>>,
    shift_jis_fallback: bool,
    utf16_le: bool,
}

impl Charmap {
    /// Create an empty charmap with the given name and description.
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            byte_to_text: HashMap::new(),
            single_byte_to_text: std::array::from_fn(|_| None),
            has_multibyte_prefix: [false; 256],
            text_to_bytes: HashMap::new(),
            text_first_byte_max_len: [0; 256],
            max_byte_len: 0,
            max_text_len: 0,
            default_terminator: None,
            shift_jis_fallback: false,
            utf16_le: false,
        }
    }

    /// Name of this charmap.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Description of this charmap.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Number of byte-sequence -> text mappings in this charmap.
    #[must_use]
    pub fn len(&self) -> usize {
        self.byte_to_text.len()
    }

    /// Returns `true` if this charmap has no mappings.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.byte_to_text.is_empty()
    }

    /// Default string terminator byte sequence, if defined (e.g. `[0x00]` or `[0xFF]`).
    #[must_use]
    pub fn default_terminator(&self) -> Option<&[u8]> {
        self.default_terminator.as_deref()
    }

    /// Explicitly set the default terminator byte sequence.
    pub fn set_default_terminator(&mut self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            self.default_terminator = Some(bytes);
        }
    }

    /// Insert a byte-sequence <-> text mapping.
    ///
    /// If `text == "{END}"` and no default terminator has been set yet, `bytes` becomes
    /// the default terminator.
    pub fn insert(&mut self, bytes: Vec<u8>, text: String) {
        if bytes.is_empty() || text.is_empty() {
            return;
        }
        if text == "{END}" && self.default_terminator.is_none() {
            self.default_terminator = Some(bytes.clone());
        }
        self.max_byte_len = self.max_byte_len.max(bytes.len());
        self.max_text_len = self.max_text_len.max(text.len());
        if bytes.len() == 1 {
            self.single_byte_to_text[bytes[0] as usize] = Some(text.clone().into_boxed_str());
        } else {
            self.has_multibyte_prefix[bytes[0] as usize] = true;
        }
        let first_text_byte = text.as_bytes()[0] as usize;
        self.text_first_byte_max_len[first_text_byte] =
            self.text_first_byte_max_len[first_text_byte].max(text.len());
        self.text_to_bytes
            .entry(text.clone())
            .or_insert_with(|| bytes.clone());
        self.byte_to_text.insert(bytes, text);
    }

    /// Load a built-in charmap by name (`ascii`, `pokemon-gen3-en`, `shift-jis`,
    /// `capcom-megaman-en`, `breath-of-fire-en`, `tactics-ogre-en`, `metroid-fusion-en`,
    /// `utf16-le`, or `ascii-offset:<delta>` such as `ascii-offset:-0x20` or `ascii-offset:0x80`).
    pub fn builtin(name: &str) -> Result<Self> {
        let lower = name.trim().to_ascii_lowercase();
        match lower.as_str() {
            "ascii" => Ok(Self::ascii()),
            "pokemon-gen3-en" | "poke-gen3-en" | "gen3-en" => Ok(Self::pokemon_gen3_en()),
            "shift-jis" | "shift_jis" | "sjis" => Ok(Self::shift_jis()),
            "capcom-megaman-en" | "megaman-en" | "mmz-en" => Ok(Self::capcom_megaman_en()),
            "breath-of-fire-en" | "bof-en" => Ok(Self::breath_of_fire_en()),
            "tactics-ogre-en" | "to-en" => Ok(Self::tactics_ogre_en()),
            "metroid-fusion-en" | "metroid-en" => Ok(Self::metroid_fusion_en()),
            "utf16-le" | "utf-16le" | "utf16le" => Ok(Self::utf16_le()),
            _ => {
                if let Some(rest) = lower.strip_prefix("ascii-offset:") {
                    let delta = parse_signed_offset(rest)?;
                    return Ok(Self::ascii_offset(delta, 0x00));
                }
                Err(Error::Unsupported(format!(
                    "unknown built-in charmap {name:?}; expected one of: ascii, pokemon-gen3-en, shift-jis, capcom-megaman-en, breath-of-fire-en, tactics-ogre-en, metroid-fusion-en, utf16-le, or ascii-offset:<delta>"
                )))
            }
        }
    }

    /// Built-in standard ASCII charmap (`0x20..=0x7E`, `0x0A` = `\n`, `0x00` = `{END}`).
    #[must_use]
    pub fn ascii() -> Self {
        let mut cm = Self::new(
            "ascii",
            "Standard printable ASCII (0x20..=0x7E, 0x0A=\\n, 0x00={END})",
        );
        cm.insert(vec![0x00], "{END}".to_string());
        cm.insert(vec![0x0A], "\n".to_string());
        for b in 0x20u8..=0x7Eu8 {
            cm.insert(vec![b], (b as char).to_string());
        }
        cm
    }

    /// Build a custom ASCII-offset charmap where printable ASCII bytes (`0x20..=0x7E`) are
    /// shifted by `delta` (e.g. `-0x20` maps `' '` -> `0x00`, `'A'` -> `0x21`).
    ///
    /// Characters whose shifted byte falls outside `0x00..=0xFF` or collides with `terminator`
    /// are skipped.
    #[must_use]
    pub fn ascii_offset(delta: i16, terminator: u8) -> Self {
        let mut cm = Self::new(
            format!("ascii-offset:{delta}"),
            format!("Printable ASCII shifted by {delta} (terminator 0x{terminator:02X})"),
        );
        cm.insert(vec![terminator], "{END}".to_string());
        for b in 0x20u8..=0x7Eu8 {
            let shifted = i16::from(b) + delta;
            if (0..=0xFF).contains(&shifted) {
                let sb = shifted as u8;
                if sb != terminator {
                    cm.insert(vec![sb], (b as char).to_string());
                }
            }
        }
        if 0x0Au8 != terminator && !cm.byte_to_text.contains_key(&vec![0x0A]) {
            cm.insert(vec![0x0A], "\n".to_string());
        }
        cm
    }

    /// Built-in Pokemon Gen 3 (Ruby/Sapphire/FireRed/LeafGreen/Emerald) English charmap.
    #[must_use]
    pub fn pokemon_gen3_en() -> Self {
        let mut cm = Self::new(
            "pokemon-gen3-en",
            "Pokemon Ruby/Sapphire/FireRed/LeafGreen/Emerald English table (0xFF={END})",
        );
        cm.insert(vec![0x00], " ".to_string());
        cm.insert(vec![0x1B], "é".to_string());
        for (i, ch) in ('0'..='9').enumerate() {
            cm.insert(vec![0xA1 + i as u8], ch.to_string());
        }
        cm.insert(vec![0xAB], "!".to_string());
        cm.insert(vec![0xAC], "?".to_string());
        cm.insert(vec![0xAD], ".".to_string());
        cm.insert(vec![0xAE], "-".to_string());
        cm.insert(vec![0xB0], "…".to_string());
        cm.insert(vec![0xB1], "“".to_string());
        cm.insert(vec![0xB2], "”".to_string());
        cm.insert(vec![0xB3], "‘".to_string());
        cm.insert(vec![0xB4], "’".to_string());
        cm.insert(vec![0xB5], "♂".to_string());
        cm.insert(vec![0xB6], "♀".to_string());
        cm.insert(vec![0xB7], "¥".to_string());
        cm.insert(vec![0xB8], ",".to_string());
        cm.insert(vec![0xB9], "×".to_string());
        cm.insert(vec![0xBA], "/".to_string());
        for (i, ch) in ('A'..='Z').enumerate() {
            cm.insert(vec![0xBB + i as u8], ch.to_string());
        }
        for (i, ch) in ('a'..='z').enumerate() {
            cm.insert(vec![0xD5 + i as u8], ch.to_string());
        }
        cm.insert(vec![0xF0], ":".to_string());
        cm.insert(vec![0xFA], "{SCROLL}".to_string());
        cm.insert(vec![0xFB], "{PAGE}".to_string());
        cm.insert(vec![0xFC], "{CTRL}".to_string());
        cm.insert(vec![0xFD], "{VAR}".to_string());
        cm.insert(vec![0xFE], "\n".to_string());
        cm.insert(vec![0xFF], "{END}".to_string());
        cm
    }

    /// Built-in Shift-JIS charmap covering ASCII, half-width Katakana, common JIS X 0208
    /// punctuation/full-width alphanumerics/Hiragana/Katakana (`0x8140..=0x8396`), with
    /// 2-byte `<$XXXX>` fallback for rarer kanji.
    #[must_use]
    pub fn shift_jis() -> Self {
        let mut cm = Self::new(
            "shift-jis",
            "Shift-JIS (ASCII, half-width Katakana, JIS X 0208 Hiragana/Katakana/full-width, 0x00={END})",
        );
        cm.shift_jis_fallback = true;
        cm.insert(vec![0x00], "{END}".to_string());
        cm.insert(vec![0x0A], "\n".to_string());

        // Printable ASCII
        for b in 0x20u8..=0x7Eu8 {
            cm.insert(vec![b], (b as char).to_string());
        }

        // Half-width Katakana (0xA1..=0xDF -> U+FF61..=U+FF9F)
        for b in 0xA1u8..=0xDFu8 {
            if let Some(ch) = char::from_u32(0xFF61 + u32::from(b - 0xA1)) {
                cm.insert(vec![b], ch.to_string());
            }
        }

        // Common 2-byte JIS X 0208 punctuation & symbols (0x8140..=0x81AB)
        const PUNCT: &[(u16, char)] = &[
            (0x8140, '　'),
            (0x8141, '、'),
            (0x8142, '。'),
            (0x8143, '，'),
            (0x8144, '．'),
            (0x8145, '・'),
            (0x8146, '：'),
            (0x8147, '；'),
            (0x8148, '？'),
            (0x8149, '！'),
            (0x815B, 'ー'),
            (0x815C, '―'),
            (0x815E, '／'),
            (0x8160, '〜'),
            (0x8163, '…'),
            (0x8164, '‥'),
            (0x8165, '‘'),
            (0x8166, '’'),
            (0x8167, '“'),
            (0x8168, '”'),
            (0x8169, '（'),
            (0x816A, '）'),
            (0x816D, '［'),
            (0x816E, '］'),
            (0x816F, '｛'),
            (0x8170, '｝'),
            (0x8175, '「'),
            (0x8176, '」'),
            (0x8177, '『'),
            (0x8178, '』'),
            (0x8179, '【'),
            (0x817A, '】'),
            (0x817B, '＋'),
            (0x817C, '－'),
            (0x8181, '＝'),
            (0x818F, '￥'),
            (0x8193, '％'),
            (0x8194, '＃'),
            (0x8195, '＆'),
            (0x8196, '＊'),
            (0x8197, '＠'),
            (0x8199, '☆'),
            (0x819A, '★'),
            (0x81A8, '→'),
            (0x81A9, '←'),
            (0x81AA, '↑'),
            (0x81AB, '↓'),
        ];
        for &(code, ch) in PUNCT {
            cm.insert(code.to_be_bytes().to_vec(), ch.to_string());
        }

        // Full-width digits 0-9 (0x824F..=0x8258 -> U+FF10..=U+FF19)
        for i in 0u8..10 {
            if let Some(ch) = char::from_u32(0xFF10 + u32::from(i)) {
                cm.insert(vec![0x82, 0x4F + i], ch.to_string());
            }
        }
        // Full-width uppercase A-Z (0x8260..=0x8279 -> U+FF21..=U+FF3A)
        for i in 0u8..26 {
            if let Some(ch) = char::from_u32(0xFF21 + u32::from(i)) {
                cm.insert(vec![0x82, 0x60 + i], ch.to_string());
            }
        }
        // Full-width lowercase a-z (0x8281..=0x829A -> U+FF41..=U+FF5A)
        for i in 0u8..26 {
            if let Some(ch) = char::from_u32(0xFF41 + u32::from(i)) {
                cm.insert(vec![0x82, 0x81 + i], ch.to_string());
            }
        }
        // Hiragana ぁ..=ん (0x829F..=0x82F1 -> U+3041..=U+3093)
        for i in 0u8..=0x52 {
            if let Some(ch) = char::from_u32(0x3041 + u32::from(i)) {
                cm.insert(vec![0x82, 0x9F + i], ch.to_string());
            }
        }
        // Katakana ァ..=ミ (0x8340..=0x837E -> U+30A1..=U+30DF)
        for i in 0u8..=0x3E {
            if let Some(ch) = char::from_u32(0x30A1 + u32::from(i)) {
                cm.insert(vec![0x83, 0x40 + i], ch.to_string());
            }
        }
        // Katakana ム..=ヶ (0x8380..=0x8396 -> U+30E0..=U+30F6)
        for i in 0u8..=0x16 {
            if let Some(ch) = char::from_u32(0x30E0 + u32::from(i)) {
                cm.insert(vec![0x83, 0x80 + i], ch.to_string());
            }
        }

        cm
    }

    /// Built-in Capcom Mega Man Zero / Battle Network English charmap.
    ///
    /// Verified on retail *Mega Man Zero* (`AZCE`) and *Mega Man Zero 2* (`A62E`):
    /// `' '=0x00`, `'0'..='9'=0x01..=0x0A`, `'A'..='Z'=0x0B..=0x24`,
    /// `'a'..='z'=0x25..=0x3E`, `'!'=0xE5`, `'.'=0xE9`, `'\''=0xEB`, `','=0xEC`,
    /// `'\n'=0xFC`, `{END}=0xFF`.
    #[must_use]
    pub fn capcom_megaman_en() -> Self {
        let mut cm = Self::new(
            "capcom-megaman-en",
            "Capcom Mega Man Zero / Battle Network English table (0x00=' ', 0x01='0', 0x0B='A', 0x25='a', 0xFF={END})",
        );
        cm.insert(vec![0x00], " ".to_string());
        for (i, ch) in ('0'..='9').enumerate() {
            cm.insert(vec![0x01 + i as u8], ch.to_string());
        }
        for (i, ch) in ('A'..='Z').enumerate() {
            cm.insert(vec![0x0B + i as u8], ch.to_string());
        }
        for (i, ch) in ('a'..='z').enumerate() {
            cm.insert(vec![0x25 + i as u8], ch.to_string());
        }
        cm.insert(vec![0xE5], "!".to_string());
        cm.insert(vec![0xE6], "?".to_string());
        cm.insert(vec![0xE9], ".".to_string());
        cm.insert(vec![0xEB], "'".to_string());
        cm.insert(vec![0xEC], ",".to_string());
        cm.insert(vec![0xFC], "\n".to_string());
        cm.insert(vec![0xFF], "{END}".to_string());
        cm
    }

    /// Built-in Capcom *Breath of Fire* 1/2 English charmap.
    ///
    /// Verified on retail *Breath of Fire* (`ABFE`) and *Breath of Fire II* (`AB2E`):
    /// `{END}=0x00`, `' '=0x02`, `'\n'=0x03`, `'0'..='9'=0x20..=0x29`,
    /// `'A'..='Z'=0x2A..=0x43`, `'.'=0x45`, `'a'..='z'=0x48..=0x61`, `','=0x63`.
    #[must_use]
    pub fn breath_of_fire_en() -> Self {
        let mut cm = Self::new(
            "breath-of-fire-en",
            "Capcom Breath of Fire 1/2 English table (0x02=' ', 0x2A='A', 0x48='a', 0x00={END})",
        );
        cm.insert(vec![0x00], "{END}".to_string());
        cm.insert(vec![0x02], " ".to_string());
        cm.insert(vec![0x03], "\n".to_string());
        for (i, ch) in ('0'..='9').enumerate() {
            cm.insert(vec![0x20 + i as u8], ch.to_string());
        }
        for (i, ch) in ('A'..='Z').enumerate() {
            cm.insert(vec![0x2A + i as u8], ch.to_string());
        }
        cm.insert(vec![0x45], ".".to_string());
        for (i, ch) in ('a'..='z').enumerate() {
            cm.insert(vec![0x48 + i as u8], ch.to_string());
        }
        cm.insert(vec![0x63], ",".to_string());
        cm
    }

    /// Built-in *Tactics Ogre: The Knight of Lodis* English charmap.
    ///
    /// Verified on retail *Tactics Ogre: The Knight of Lodis* (`ATOE`):
    /// `'A'..='Z'=0x00..=0x19`, `'a'..='z'=0x1A..=0x33`, `'0'..='9'=0x34..=0x3D`,
    /// `' '=0x89`, `{END}=0xFF`.
    #[must_use]
    pub fn tactics_ogre_en() -> Self {
        let mut cm = Self::new(
            "tactics-ogre-en",
            "Tactics Ogre: The Knight of Lodis English table (0x00='A', 0x1A='a', 0x89=' ', 0xFF={END})",
        );
        for (i, ch) in ('A'..='Z').enumerate() {
            cm.insert(vec![i as u8], ch.to_string());
        }
        for (i, ch) in ('a'..='z').enumerate() {
            cm.insert(vec![0x1A + i as u8], ch.to_string());
        }
        for (i, ch) in ('0'..='9').enumerate() {
            cm.insert(vec![0x34 + i as u8], ch.to_string());
        }
        cm.insert(vec![0x89], " ".to_string());
        cm.insert(vec![0xFF], "{END}".to_string());
        cm
    }

    /// Built-in *Metroid Fusion* 16-bit little-endian English charmap.
    ///
    /// Verified on retail *Metroid Fusion* (`AMTE`):
    /// `' '=0x0040`, `'!'=0x0041`, `'\''=0x0047`, `','=0x004C`, `'-'=0x004D`, `'.'=0x004E`,
    /// `'0'..='9'=0x0050..=0x0059`, `':'=0x005A`, `'?'=0x005F`,
    /// `'A'..='Z'=0x0081..=0x009A`, `'a'..='z'=0x00C1..=0x00DA`,
    /// `{PAGE}=0xFD00`, `'\n'=0xFE00`, `{END}=0xFF00`.
    #[must_use]
    pub fn metroid_fusion_en() -> Self {
        let mut cm = Self::new(
            "metroid-fusion-en",
            "Metroid Fusion 16-bit LE English table (0x0040=' ', 0x0081='A', 0x00C1='a', 0xFF00={END})",
        );
        cm.insert(0x0040u16.to_le_bytes().to_vec(), " ".to_string());
        cm.insert(0x0041u16.to_le_bytes().to_vec(), "!".to_string());
        cm.insert(0x0047u16.to_le_bytes().to_vec(), "'".to_string());
        cm.insert(0x004Cu16.to_le_bytes().to_vec(), ",".to_string());
        cm.insert(0x004Du16.to_le_bytes().to_vec(), "-".to_string());
        cm.insert(0x004Eu16.to_le_bytes().to_vec(), ".".to_string());
        for (i, ch) in ('0'..='9').enumerate() {
            cm.insert((0x0050u16 + i as u16).to_le_bytes().to_vec(), ch.to_string());
        }
        cm.insert(0x005Au16.to_le_bytes().to_vec(), ":".to_string());
        cm.insert(0x005Fu16.to_le_bytes().to_vec(), "?".to_string());
        for (i, ch) in ('A'..='Z').enumerate() {
            cm.insert((0x0081u16 + i as u16).to_le_bytes().to_vec(), ch.to_string());
        }
        for (i, ch) in ('a'..='z').enumerate() {
            cm.insert((0x00C1u16 + i as u16).to_le_bytes().to_vec(), ch.to_string());
        }
        cm.insert(0xFD00u16.to_le_bytes().to_vec(), "{PAGE}".to_string());
        cm.insert(0xFE00u16.to_le_bytes().to_vec(), "\n".to_string());
        cm.insert(0xFF00u16.to_le_bytes().to_vec(), "{END}".to_string());
        cm
    }

    /// Built-in standard UTF-16LE null-terminated Unicode charmap (`0x0000` = `{END}`).
    ///
    /// Supports full BMP (and surrogate-pair) UTF-16LE decoding and encoding, as used by
    /// *Gunstar Super Heroes* (`BHGE`) and other 16-bit localized GBA titles.
    #[must_use]
    pub fn utf16_le() -> Self {
        let mut cm = Self::new(
            "utf16-le",
            "Standard UTF-16LE null-terminated Unicode (full BMP decode/encode, 0x0000={END})",
        );
        cm.utf16_le = true;
        cm.insert(vec![0x00, 0x00], "{END}".to_string());
        cm
    }

    /// Parse a standard fan-translation `.tbl` file (`HEX=TEXT`, multi-byte sequences,
    /// `#` and `//` comments, blank lines, `\n`/`\r`/`\t`/`\\` escapes).
    pub fn parse_tbl(name: impl Into<String>, input: &str) -> Result<Self> {
        let name = name.into();
        let mut cm = Self::new(name.clone(), format!("Custom .tbl charmap ({name})"));
        let input = input.strip_prefix('\u{FEFF}').unwrap_or(input);

        for (line_idx, raw_line) in input.lines().enumerate() {
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            let trimmed = line.trim_start();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
                continue;
            }
            let Some((hex_part, val_part)) = line.split_once('=') else {
                return Err(Error::Malformed(format!(
                    ".tbl line {}: expected HEX=TEXT entry",
                    line_idx + 1
                )));
            };
            let hex_clean = hex_part.trim();
            let hex_clean = hex_clean
                .strip_prefix("0x")
                .or_else(|| hex_clean.strip_prefix("0X"))
                .unwrap_or(hex_clean);
            let bytes = parse_hex_bytes(hex_clean).map_err(|msg| {
                Error::Malformed(format!(".tbl line {}: {msg}", line_idx + 1))
            })?;
            let text = unescape_tbl_value(val_part);
            if text.is_empty() {
                return Err(Error::Malformed(format!(
                    ".tbl line {}: empty mapped text after '='",
                    line_idx + 1
                )));
            }
            cm.insert(bytes, text);
        }

        if cm.is_empty() {
            return Err(Error::Malformed(
                ".tbl file contains no valid HEX=TEXT entries".into(),
            ));
        }

        Ok(cm)
    }

    /// Check if `bytes` starts with this charmap's string terminator, returning its byte length.
    #[must_use]
    pub fn terminator_len_at(&self, bytes: &[u8]) -> Option<usize> {
        if let Some(term) = &self.default_terminator {
            if bytes.starts_with(term) {
                return Some(term.len());
            }
        }
        None
    }

    /// Decode a single token from the start of `bytes`, returning `(decoded_text, bytes_consumed, is_mapped)`.
    #[must_use]
    pub fn decode_next(&self, bytes: &[u8]) -> Option<(String, usize, bool)> {
        if bytes.is_empty() {
            return None;
        }
        let b0 = bytes[0] as usize;
        if self.max_byte_len > 1 && self.has_multibyte_prefix[b0] {
            let max_try = self.max_byte_len.min(bytes.len());
            for len in (2..=max_try).rev() {
                if let Some(text) = self.byte_to_text.get(&bytes[..len]) {
                    return Some((text.clone(), len, true));
                }
            }
        }
        if let Some(ref text) = self.single_byte_to_text[b0] {
            return Some((text.to_string(), 1, true));
        }
        if self.utf16_le {
            if bytes.len() >= 2 {
                let code = u16::from_le_bytes([bytes[0], bytes[1]]);
                if code == 0x0000 {
                    return Some(("{END}".to_string(), 2, true));
                }
                if (0xD800..=0xDBFF).contains(&code) && bytes.len() >= 4 {
                    let low = u16::from_le_bytes([bytes[2], bytes[3]]);
                    if (0xDC00..=0xDFFF).contains(&low) {
                        let cp = 0x1_0000
                            + ((u32::from(code) - 0xD800) << 10)
                            + (u32::from(low) - 0xDC00);
                        if let Some(ch) = char::from_u32(cp) {
                            return Some((ch.to_string(), 4, true));
                        }
                    }
                }
                if let Some(ch) = char::from_u32(u32::from(code)) {
                    return Some((ch.to_string(), 2, true));
                }
                return Some((format!("<${:02X}{:02X}>", bytes[0], bytes[1]), 2, false));
            }
            return Some((format!("<${:02X}>", bytes[0]), 1, false));
        }
        if self.shift_jis_fallback
            && bytes.len() >= 2
            && is_sjis_lead(bytes[0])
            && is_sjis_trail(bytes[1])
        {
            return Some((format!("<${:02X}{:02X}>", bytes[0], bytes[1]), 2, false));
        }
        Some((format!("<${:02X}>", bytes[0]), 1, false))
    }

    /// Decode a byte slice into a UTF-8 string using longest-match greedy decoding.
    ///
    /// Unmapped bytes are emitted as `<$XX>` (or `<$XXXX>` for unmapped 2-byte Shift-JIS kanji).
    #[must_use]
    pub fn decode(&self, bytes: &[u8]) -> String {
        let mut out = String::new();
        let mut pos = 0usize;
        while pos < bytes.len() {
            if let Some((tok, consumed, _)) = self.decode_next(&bytes[pos..]) {
                out.push_str(&tok);
                pos += consumed;
            } else {
                break;
            }
        }
        out
    }

    /// Encode a UTF-8 string back into ROM bytes using longest-match greedy encoding.
    ///
    /// Supports raw hex escapes of the form `<$XX>` or `<$XXXX>` (any even number of hex digits).
    pub fn encode(&self, text: &str) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(if self.utf16_le {
            text.len() * 2
        } else {
            text.len()
        });
        let mut pos = 0usize;

        while pos < text.len() {
            let rest = &text[pos..];

            // Raw hex escape: <$XX> or <$XXXX> ...
            if let Some(after_prefix) = rest.strip_prefix("<$") {
                if let Some(close_idx) = after_prefix.find('>') {
                    let hex_str = &after_prefix[..close_idx];
                    if !hex_str.is_empty()
                        && hex_str.len().is_multiple_of(2)
                        && hex_str.bytes().all(|b| b.is_ascii_hexdigit())
                    {
                        let raw = parse_hex_bytes(hex_str).map_err(Error::Malformed)?;
                        out.extend_from_slice(&raw);
                        pos += 2 + close_idx + 1;
                        continue;
                    }
                }
            }

            // Longest-match greedy lookup in text_to_bytes, bounded by first-byte max length.
            let first_byte_max = self.text_first_byte_max_len[rest.as_bytes()[0] as usize];
            let max_end = rest.len().min(first_byte_max);
            let mut matched = None;
            for end in (1..=max_end).rev() {
                if !rest.is_char_boundary(end) {
                    continue;
                }
                if let Some(bytes) = self.text_to_bytes.get(&rest[..end]) {
                    matched = Some((bytes, end));
                    break;
                }
            }

            if let Some((bytes, consumed)) = matched {
                out.extend_from_slice(bytes);
                pos += consumed;
            } else if self.utf16_le {
                let ch = rest.chars().next().unwrap_or('\0');
                let mut units = [0u16; 2];
                for &u in ch.encode_utf16(&mut units).iter() {
                    out.extend_from_slice(&u.to_le_bytes());
                }
                pos += ch.len_utf8();
            } else {
                let ch = rest.chars().next().unwrap_or('\0');
                return Err(Error::Malformed(format!(
                    "unmapped character {ch:?} at byte offset {pos} in charmap '{}'",
                    self.name
                )));
            }
        }

        Ok(out)
    }
}

fn is_sjis_lead(b: u8) -> bool {
    (0x81..=0x9F).contains(&b) || (0xE0..=0xEF).contains(&b)
}

fn is_sjis_trail(b: u8) -> bool {
    (0x40..=0x7E).contains(&b) || (0x80..=0xFC).contains(&b)
}

fn parse_signed_offset(s: &str) -> Result<i16> {
    let s = s.trim();
    let (neg, body) = if let Some(rest) = s.strip_prefix('-') {
        (true, rest)
    } else if let Some(rest) = s.strip_prefix('+') {
        (false, rest)
    } else {
        (false, s)
    };
    let mag: i16 = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i16::from_str_radix(hex, 16)
            .map_err(|_| Error::Malformed(format!("invalid hex offset {s:?}")))?
    } else {
        body.parse::<i16>()
            .map_err(|_| Error::Malformed(format!("invalid integer offset {s:?}")))?
    };
    Ok(if neg { -mag } else { mag })
}

fn parse_hex_bytes(hex: &str) -> std::result::Result<Vec<u8>, String> {
    if hex.is_empty() || !hex.len().is_multiple_of(2) {
        return Err(format!(
            "hex key {hex:?} must be a non-empty even-length hex string"
        ));
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().as_chunks::<2>().0 {
        let s = std::str::from_utf8(pair).map_err(|_| format!("non-ASCII hex key {hex:?}"))?;
        let byte =
            u8::from_str_radix(s, 16).map_err(|_| format!("invalid hex byte {s:?} in {hex:?}"))?;
        out.push(byte);
    }
    Ok(out)
}

fn unescape_tbl_value(val: &str) -> String {
    let mut out = String::with_capacity(val.len());
    let mut chars = val.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tbl_handles_comments_multibyte_and_escapes() {
        let tbl = "\
# Pokemon-style custom table
// Another comment

20= 
BB=A
BC=B
FE=\\n
FF={END}
FC01={COLOR:01}
FC02={COLOR:02}
";
        let cm = Charmap::parse_tbl("custom", tbl).unwrap();
        assert_eq!(cm.default_terminator(), Some(&[0xFF][..]));

        let bytes = [0xFC, 0x01, 0xBB, 0x20, 0xBC, 0xFE, 0xFC, 0x02, 0x99, 0xFF];
        let decoded = cm.decode(&bytes);
        assert_eq!(decoded, "{COLOR:01}A B\n{COLOR:02}<$99>{END}");

        let reencoded = cm.encode(&decoded).unwrap();
        assert_eq!(reencoded, bytes);
    }

    #[test]
    fn pokemon_gen3_en_roundtrip() {
        let cm = Charmap::builtin("pokemon-gen3-en").unwrap();
        assert_eq!(cm.default_terminator(), Some(&[0xFF][..]));

        let text = "Hello, Trainer! “Pikachu” Lv.50♂/♀…\n{SCROLL}{PAGE}{CTRL}{VAR}{END}";
        let encoded = cm.encode(text).unwrap();
        assert_eq!(encoded[0], 0xBB + (b'H' - b'A'));
        assert_eq!(*encoded.last().unwrap(), 0xFF);

        let decoded = cm.decode(&encoded);
        assert_eq!(decoded, text);
    }

    #[test]
    fn ascii_and_ascii_offset_roundtrip() {
        let ascii = Charmap::builtin("ascii").unwrap();
        let bytes = ascii.encode("GBA Text 123!\n{END}").unwrap();
        assert_eq!(bytes, b"GBA Text 123!\n\0");
        assert_eq!(ascii.decode(&bytes), "GBA Text 123!\n{END}");

        let shifted = Charmap::builtin("ascii-offset:-0x20").unwrap();
        let enc = shifted.encode("ABC").unwrap();
        assert_eq!(enc, vec![0x21, 0x22, 0x23]);
        assert_eq!(shifted.decode(&enc), "ABC");
    }

    #[test]
    fn shift_jis_roundtrip_and_kanji_fallback() {
        let sjis = Charmap::builtin("shift-jis").unwrap();
        assert_eq!(sjis.default_terminator(), Some(&[0x00][..]));

        let text = "GBAゲーム「テスト」ｱｲｳ！Ａ１\n<$93FA><$967B>{END}";
        let encoded = sjis.encode(text).unwrap();
        let decoded = sjis.decode(&encoded);
        assert_eq!(decoded, text);
    }

    #[test]
    fn unmapped_char_returns_error() {
        let ascii = Charmap::ascii();
        assert!(ascii.encode("Hello 🌟").is_err());
    }

    #[test]
    fn capcom_megaman_en_roundtrip() {
        let cm = Charmap::builtin("capcom-megaman-en").unwrap();
        assert_eq!(cm.default_terminator(), Some(&[0xFF][..]));
        let text = "Thank you,\nZero! 09{END}";
        let enc = cm.encode(text).unwrap();
        assert_eq!(enc[0], 0x0B + (b'T' - b'A'));
        assert_eq!(*enc.last().unwrap(), 0xFF);
        assert_eq!(cm.decode(&enc), text);
    }

    #[test]
    fn breath_of_fire_en_roundtrip() {
        let cm = Charmap::builtin("breath-of-fire-en").unwrap();
        assert_eq!(cm.default_terminator(), Some(&[0x00][..]));
        let text = "Dark Dragon King,\nvowed 123.{END}";
        let enc = cm.encode(text).unwrap();
        assert_eq!(enc[0], 0x2A + (b'D' - b'A'));
        assert_eq!(enc[1], 0x48); // 'a'
        assert_eq!(*enc.last().unwrap(), 0x00);
        assert_eq!(cm.decode(&enc), text);
    }

    #[test]
    fn tactics_ogre_en_roundtrip() {
        let cm = Charmap::builtin("tactics-ogre-en").unwrap();
        assert_eq!(cm.default_terminator(), Some(&[0xFF][..]));
        let text = "Alphonse Knight 42{END}";
        let enc = cm.encode(text).unwrap();
        assert_eq!(enc[0], 0x00); // 'A'
        assert_eq!(enc[8], 0x89); // ' '
        assert_eq!(*enc.last().unwrap(), 0xFF);
        assert_eq!(cm.decode(&enc), text);
    }

    #[test]
    fn metroid_fusion_en_roundtrip() {
        let cm = Charmap::builtin("metroid-fusion-en").unwrap();
        assert_eq!(cm.default_terminator(), Some(&[0x00, 0xFF][..]));
        let text = "Samus, Sector 1!\nStop.{PAGE}{END}";
        let enc = cm.encode(text).unwrap();
        assert_eq!(&enc[0..2], &0x0093u16.to_le_bytes()); // 'S'
        assert_eq!(&enc[enc.len() - 2..], &0xFF00u16.to_le_bytes());
        assert_eq!(cm.decode(&enc), text);
    }

    #[test]
    fn utf16_le_full_bmp_roundtrip() {
        let cm = Charmap::builtin("utf16-le").unwrap();
        assert_eq!(cm.default_terminator(), Some(&[0x00, 0x00][..]));
        let text = "Gunstar Super Heroes! 日本語 Café ★{END}";
        let enc = cm.encode(text).unwrap();
        // 'G' = 0x0047 LE
        assert_eq!(&enc[0..2], &[0x47, 0x00]);
        // {END} = 0x0000
        assert_eq!(&enc[enc.len() - 2..], &[0x00, 0x00]);
        assert_eq!(cm.decode(&enc), text);
    }

    #[test]
    fn parse_tbl_strips_utf8_bom() {
        let tbl = "\u{FEFF}41=A\n42=B\n00={END}\n";
        let cm = Charmap::parse_tbl("bom-test", tbl).unwrap();
        assert_eq!(cm.decode(&[0x41, 0x42, 0x00]), "AB{END}");
        assert_eq!(cm.encode("AB{END}").unwrap(), vec![0x41, 0x42, 0x00]);
    }
}
