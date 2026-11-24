//! Custom 16-bit Huffman tree text decoder, encoder, and ROM write-back.
//!
//! Supports the Intelligent Systems *Fire Emblem* GBA Huffman text convention:
//! - A binary tree stored in ROM as 4-byte little-endian nodes `(left: u16, right: u16)`.
//! - `right == 0xFFFF` indicates a leaf node emitting symbol `left: u16`:
//!   - `0x0000` is the string terminator.
//!   - `0x0001..=0x00FF` emits a single byte (`left as u8`).
//!   - `0x0100..=0xFFFF` emits two little-endian bytes (`[left as u8, (left >> 8) as u8]`).
//! - Internal nodes branch on LSB-first bitstream bits (`0` -> `left`, `1` -> `right`).

#![deny(unsafe_code)]

use std::collections::{HashMap, HashSet};

use formatkit_core::{Error, Result};
use gba_rom::rom_offset_to_bus;
use serde::{Deserialize, Serialize};

use crate::string::{write_or_relocate_bytes, TextReplaceOptions, TextReplaceReport};

/// Maximum number of tree nodes accepted when parsing a ROM Huffman table.
const MAX_TREE_NODES: usize = 65_536;

/// Maximum tree depth allowed during symbol table construction (guards against degenerate trees).
const MAX_TREE_DEPTH: usize = 256;

/// Parsed 16-bit Huffman binary tree from a GBA ROM.
#[derive(Debug, Clone)]
pub struct HuffmanTree {
    tree_offset: usize,
    root_offset: usize,
    root_index: u16,
    nodes: HashMap<u16, (u16, u16)>,
    encode_table: HashMap<u16, Vec<u8>>,
}

/// Result of decoding a Huffman-compressed string from a GBA ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HuffmanDecoded {
    /// ROM file offset where the compressed bitstream starts.
    pub offset: usize,
    /// GBA Wait State 0 bus address (`0x0800_0000 + offset`), if within 32 MiB.
    pub bus_address: Option<u32>,
    /// Raw decoded payload bytes (excluding the `0x0000` terminator).
    pub bytes: Vec<u8>,
    /// Human-readable text representation (printable ASCII + `\n`, with `<$XX>` escapes for control bytes).
    pub text: String,
    /// Exact number of compressed bytes consumed from ROM (including trailing padding bits of the last byte).
    pub compressed_bytes_len: usize,
    /// Exact number of bits consumed from the compressed bitstream.
    pub bit_len: usize,
    /// Whether the `0x0000` string terminator was reached before `max_bytes` or EOF.
    pub terminated: bool,
}

impl HuffmanTree {
    /// Parse a 16-bit Huffman tree from `rom` with node table at `tree_offset` and root node at `root_offset`.
    ///
    /// Performs bounds checking on every reachable node and rejects cyclic trees.
    pub fn from_rom(rom: &[u8], tree_offset: usize, root_offset: usize) -> Result<Self> {
        if root_offset < tree_offset || !(root_offset - tree_offset).is_multiple_of(4) {
            return Err(Error::InvalidField {
                what: "huffman_root_offset",
                value: root_offset as u64,
            });
        }
        let root_index_usize = (root_offset - tree_offset) / 4;
        let root_index = u16::try_from(root_index_usize).map_err(|_| Error::InvalidField {
            what: "huffman_root_index",
            value: root_index_usize as u64,
        })?;

        let mut nodes: HashMap<u16, (u16, u16)> = HashMap::new();
        let mut encode_table: HashMap<u16, Vec<u8>> = HashMap::new();
        let mut visiting: HashSet<u16> = HashSet::new();
        let mut path: Vec<u8> = Vec::new();

        parse_and_validate_node(
            rom,
            tree_offset,
            root_index,
            &mut nodes,
            &mut visiting,
            &mut path,
            &mut encode_table,
        )?;

        Ok(Self {
            tree_offset,
            root_offset,
            root_index,
            nodes,
            encode_table,
        })
    }

    /// ROM file offset of the node table base.
    #[must_use]
    pub const fn tree_offset(&self) -> usize {
        self.tree_offset
    }

    /// ROM file offset of the root node.
    #[must_use]
    pub const fn root_offset(&self) -> usize {
        self.root_offset
    }

    /// Node index of the root node relative to `tree_offset`.
    #[must_use]
    pub const fn root_index(&self) -> u16 {
        self.root_index
    }

    /// Total number of reachable nodes parsed from the tree.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of distinct leaf symbols in the tree.
    #[must_use]
    pub fn symbol_count(&self) -> usize {
        self.encode_table.len()
    }

    /// Decode an LSB-first Huffman bitstream starting at `offset` in `rom`.
    ///
    /// Stops when the `0x0000` terminator symbol is decoded, `max_bytes` payload bytes have
    /// been produced, or the end of `rom` is reached.
    pub fn decode_at(
        &self,
        rom: &[u8],
        offset: usize,
        max_bytes: usize,
    ) -> Result<HuffmanDecoded> {
        if offset >= rom.len() {
            return Err(Error::Truncated {
                offset,
                needed: 1,
                available: 0,
            });
        }

        let mut out = Vec::new();
        let mut byte_pos = offset;
        let mut bit_pos: u8 = 0;
        let mut bit_len: usize = 0;
        let mut node_idx = self.root_index;
        let mut terminated = false;

        loop {
            let &(left, right) = self.nodes.get(&node_idx).ok_or(Error::Malformed(format!(
                "huffman node index {node_idx} missing from parsed tree"
            )))?;

            if right == 0xFFFF {
                if left == 0x0000 {
                    terminated = true;
                    break;
                }
                if left > 0x00FF {
                    if out.len() + 2 > max_bytes {
                        break;
                    }
                    out.push((left & 0xFF) as u8);
                    out.push((left >> 8) as u8);
                } else {
                    if out.len() >= max_bytes {
                        break;
                    }
                    out.push(left as u8);
                }
                node_idx = self.root_index;
                continue;
            }

            if byte_pos >= rom.len() {
                break;
            }
            let bit = (rom[byte_pos] >> bit_pos) & 1;
            bit_pos += 1;
            bit_len += 1;
            if bit_pos == 8 {
                bit_pos = 0;
                byte_pos += 1;
            }
            node_idx = if bit == 0 { left } else { right };
        }

        let compressed_bytes_len = (byte_pos - offset) + usize::from(bit_pos > 0);
        let text = format_huffman_text(&out);

        Ok(HuffmanDecoded {
            offset,
            bus_address: rom_offset_to_bus(offset),
            bytes: out,
            text,
            compressed_bytes_len,
            bit_len,
            terminated,
        })
    }

    /// Encode raw payload bytes into an LSB-first Huffman bitstream, appending the `0x0000`
    /// terminator symbol.
    ///
    /// Returns [`Error::Malformed`] if any byte/symbol (or the `0x0000` terminator) is not
    /// present in this ROM's Huffman tree.
    pub fn encode(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let term_code = self.encode_table.get(&0x0000).ok_or_else(|| {
            Error::Malformed("huffman tree does not contain 0x0000 terminator symbol".into())
        })?;

        let mut writer = LsbBitWriter::new();
        let mut i = 0usize;
        while i < bytes.len() {
            let b0 = bytes[i];
            // For high bytes (>= 0x80), try 2-byte little-endian symbol first (e.g. Shift-JIS in JP ROMs),
            // then fall back to 1-byte symbol.
            if b0 >= 0x80 && i + 1 < bytes.len() {
                let sym16 = u16::from_le_bytes([b0, bytes[i + 1]]);
                if let Some(bits) = self.encode_table.get(&sym16) {
                    writer.write_bits(bits);
                    i += 2;
                    continue;
                }
            }
            let sym8 = u16::from(b0);
            if let Some(bits) = self.encode_table.get(&sym8) {
                writer.write_bits(bits);
                i += 1;
                continue;
            }
            // Also try 2-byte symbol even if b0 < 0x80 as a fallback.
            if i + 1 < bytes.len() {
                let sym16 = u16::from_le_bytes([b0, bytes[i + 1]]);
                if let Some(bits) = self.encode_table.get(&sym16) {
                    writer.write_bits(bits);
                    i += 2;
                    continue;
                }
            }
            return Err(Error::Malformed(format!(
                "byte 0x{b0:02X} at offset {i} is not present in this ROM's Huffman tree"
            )));
        }

        writer.write_bits(term_code);
        Ok(writer.finish())
    }
}

fn parse_and_validate_node(
    rom: &[u8],
    tree_offset: usize,
    node_idx: u16,
    nodes: &mut HashMap<u16, (u16, u16)>,
    visiting: &mut HashSet<u16>,
    path: &mut Vec<u8>,
    encode_table: &mut HashMap<u16, Vec<u8>>,
) -> Result<()> {
    if path.len() > MAX_TREE_DEPTH {
        return Err(Error::ResourceLimit {
            resource: "huffman_tree_depth",
            requested: path.len() as u64,
            limit: MAX_TREE_DEPTH as u64,
        });
    }
    if !visiting.insert(node_idx) {
        return Err(Error::Malformed(format!(
            "cycle detected in Huffman tree at node {node_idx}"
        )));
    }

    let (left, right) = if let Some(&pair) = nodes.get(&node_idx) {
        pair
    } else {
        if nodes.len() >= MAX_TREE_NODES {
            return Err(Error::ResourceLimit {
                resource: "huffman_tree_nodes",
                requested: (nodes.len() + 1) as u64,
                limit: MAX_TREE_NODES as u64,
            });
        }
        let byte_off = tree_offset
            .checked_add(usize::from(node_idx) * 4)
            .ok_or(Error::InvalidField {
                what: "huffman_node_offset",
                value: u64::from(node_idx),
            })?;
        if byte_off + 4 > rom.len() {
            return Err(Error::Truncated {
                offset: byte_off,
                needed: 4,
                available: rom.len().saturating_sub(byte_off),
            });
        }
        let left = u16::from_le_bytes([rom[byte_off], rom[byte_off + 1]]);
        let right = u16::from_le_bytes([rom[byte_off + 2], rom[byte_off + 3]]);
        nodes.insert(node_idx, (left, right));
        (left, right)
    };

    if right == 0xFFFF {
        // Leaf node: prefer shortest code if a symbol appears multiple times.
        encode_table
            .entry(left)
            .and_modify(|existing| {
                if path.len() < existing.len() {
                    *existing = path.clone();
                }
            })
            .or_insert_with(|| path.clone());
    } else {
        path.push(0);
        parse_and_validate_node(rom, tree_offset, left, nodes, visiting, path, encode_table)?;
        path.pop();

        path.push(1);
        parse_and_validate_node(rom, tree_offset, right, nodes, visiting, path, encode_table)?;
        path.pop();
    }

    visiting.remove(&node_idx);
    Ok(())
}

struct LsbBitWriter {
    bytes: Vec<u8>,
    bit_pos: u8,
}

impl LsbBitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            bit_pos: 0,
        }
    }

    fn write_bits(&mut self, bits: &[u8]) {
        for &b in bits {
            if self.bit_pos == 0 {
                self.bytes.push(0);
            }
            if b != 0 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 1u8 << self.bit_pos;
            }
            self.bit_pos = (self.bit_pos + 1) & 7;
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// Format raw decoded Huffman payload bytes into a lossless text representation
/// (printable ASCII + `\n`, with `<$XX>` escapes for control/non-ASCII bytes).
#[must_use]
pub fn format_huffman_text(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b == b'\n' {
            out.push('\n');
        } else if (0x20..=0x7E).contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("<${b:02X}>"));
        }
    }
    out
}

/// Parse a Huffman text string (supporting `<$XX>` raw hex escapes and `\n`) into raw payload bytes.
pub fn parse_huffman_text(text: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len());
    let mut pos = 0usize;
    while pos < text.len() {
        let rest = &text[pos..];
        if let Some(after) = rest.strip_prefix("<$") {
            if let Some(close) = after.find('>') {
                let hex = &after[..close];
                if !hex.is_empty()
                    && hex.len().is_multiple_of(2)
                    && hex.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    for pair in hex.as_bytes().as_chunks::<2>().0 {
                        let s = std::str::from_utf8(pair).map_err(|_| {
                            Error::Malformed(format!("non-ASCII hex escape <${hex}>"))
                        })?;
                        let byte = u8::from_str_radix(s, 16).map_err(|_| {
                            Error::Malformed(format!("invalid hex byte {s:?} in <${hex}>"))
                        })?;
                        out.push(byte);
                    }
                    pos += 2 + close + 1;
                    continue;
                }
            }
        }
        let ch = rest.chars().next().unwrap_or('\0');
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        pos += ch.len_utf8();
    }
    Ok(out)
}

/// Replace a Huffman-compressed text entry in `rom` at `entry_offset` using the tree at
/// `(tree_offset, root_offset)`.
///
/// - Measures the original compressed byte length via [`HuffmanTree::decode_at`].
/// - Encodes `new_bytes` (with trailing `0x0000` terminator) using the ROM's tree.
/// - Writes in-place if the encoded bitstream fits within the original capacity, or relocates
///   into `0xFF` free space (`0x00` fallback at `>= 0x200`) and patches `0x08xxxxxx` ROM
///   pointers when `options.allow_relocate` is `true`.
pub fn replace_huffman_text_in_rom(
    rom: &mut [u8],
    tree_offset: usize,
    root_offset: usize,
    entry_offset: usize,
    new_bytes: &[u8],
    options: &TextReplaceOptions,
) -> Result<TextReplaceReport> {
    let tree = HuffmanTree::from_rom(rom, tree_offset, root_offset)?;
    let orig = tree.decode_at(rom, entry_offset, 16_384)?;
    let encoded = tree.encode(new_bytes)?;
    let pad_byte = options.pad_byte.unwrap_or(0x00);

    write_or_relocate_bytes(
        rom,
        entry_offset,
        orig.compressed_bytes_len,
        &encoded,
        pad_byte,
        options,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small synthetic 16-bit Huffman tree in ROM for testing:
    /// Symbols:
    ///   'A' (0x0041): code 00
    ///   'B' (0x0042): code 01
    ///   'C' (0x0043): code 10
    ///   END (0x0000): code 110
    ///   ' ' (0x0020): code 111
    fn build_synthetic_huffman_rom() -> (Vec<u8>, usize, usize, usize, usize) {
        let mut rom = vec![0u8; 0x8000];
        rom[0x4000..].fill(0xFF); // free space

        let tree_off = 0x200usize;
        // Nodes:
        // 0: leaf 'A' (0x0041, 0xFFFF)
        // 1: leaf 'B' (0x0042, 0xFFFF)
        // 2: leaf 'C' (0x0043, 0xFFFF)
        // 3: leaf END (0x0000, 0xFFFF)
        // 4: leaf ' ' (0x0020, 0xFFFF)
        // 5: internal (left=0, right=1)  -> 00='A', 01='B'
        // 6: internal (left=3, right=4)  -> 110=END, 111=' '
        // 7: internal (left=2, right=6)  -> 10='C', 11=...
        // 8: root     (left=5, right=7)
        let nodes: [(u16, u16); 9] = [
            (0x0041, 0xFFFF),
            (0x0042, 0xFFFF),
            (0x0043, 0xFFFF),
            (0x0000, 0xFFFF),
            (0x0020, 0xFFFF),
            (0, 1),
            (3, 4),
            (2, 6),
            (5, 7),
        ];
        for (i, &(l, r)) in nodes.iter().enumerate() {
            let off = tree_off + i * 4;
            rom[off..off + 2].copy_from_slice(&l.to_le_bytes());
            rom[off + 2..off + 4].copy_from_slice(&r.to_le_bytes());
        }
        let root_off = tree_off + 8 * 4;

        let tree = HuffmanTree::from_rom(&rom, tree_off, root_off).unwrap();
        let encoded = tree.encode(b"AB C").unwrap();

        let entry_off = 0x1000usize;
        rom[entry_off..entry_off + encoded.len()].copy_from_slice(&encoded);

        let ptr_off = 0x300usize;
        let bus = rom_offset_to_bus(entry_off).unwrap();
        rom[ptr_off..ptr_off + 4].copy_from_slice(&bus.to_le_bytes());

        (rom, tree_off, root_off, entry_off, ptr_off)
    }

    #[test]
    fn huffman_decode_and_encode_roundtrip() {
        let (rom, tree_off, root_off, entry_off, _) = build_synthetic_huffman_rom();
        let tree = HuffmanTree::from_rom(&rom, tree_off, root_off).unwrap();
        assert_eq!(tree.node_count(), 9);
        assert_eq!(tree.symbol_count(), 5);

        let dec = tree.decode_at(&rom, entry_off, 256).unwrap();
        assert_eq!(dec.bytes, b"AB C");
        assert_eq!(dec.text, "AB C");
        assert!(dec.terminated);
        // Bits: A(00) B(01) ' '(111) C(10) END(110) = 12 bits = 2 bytes
        assert_eq!(dec.bit_len, 12);
        assert_eq!(dec.compressed_bytes_len, 2);

        let reencoded = tree.encode(&dec.bytes).unwrap();
        assert_eq!(reencoded, rom[entry_off..entry_off + dec.compressed_bytes_len]);

        // Unmapped symbol returns error
        assert!(tree.encode(b"XYZ").is_err());
    }

    #[test]
    fn huffman_replace_in_place_and_relocate() {
        let (mut rom, tree_off, root_off, entry_off, ptr_off) = build_synthetic_huffman_rom();

        // Shorter replacement fits in-place
        let rep = replace_huffman_text_in_rom(
            &mut rom,
            tree_off,
            root_off,
            entry_off,
            b"BA",
            &TextReplaceOptions::default(),
        )
        .unwrap();
        assert_eq!(rep.mode, crate::string::TextReplaceMode::InPlace);
        assert_eq!(rep.written_offset, entry_off);

        let tree = HuffmanTree::from_rom(&rom, tree_off, root_off).unwrap();
        let dec = tree.decode_at(&rom, entry_off, 256).unwrap();
        assert_eq!(dec.bytes, b"BA");

        // Longer replacement without allow_relocate fails
        let err = replace_huffman_text_in_rom(
            &mut rom,
            tree_off,
            root_off,
            entry_off,
            b"ABC ABC ABC ABC",
            &TextReplaceOptions::default(),
        );
        assert!(matches!(err, Err(Error::ResourceLimit { .. })));

        // Longer replacement with allow_relocate relocates to 0xFF free space & patches pointer
        let opts = TextReplaceOptions {
            allow_relocate: true,
            ..TextReplaceOptions::default()
        };
        let reloc = replace_huffman_text_in_rom(
            &mut rom,
            tree_off,
            root_off,
            entry_off,
            b"ABC ABC ABC ABC",
            &opts,
        )
        .unwrap();
        assert_eq!(reloc.mode, crate::string::TextReplaceMode::Relocated);
        assert!(reloc.written_offset >= 0x4000);
        assert_eq!(reloc.patched_pointer_count, 1);
        assert_eq!(reloc.patched_pointer_offsets, vec![ptr_off]);

        let new_ptr = u32::from_le_bytes(rom[ptr_off..ptr_off + 4].try_into().unwrap());
        assert_eq!(new_ptr, rom_offset_to_bus(reloc.written_offset).unwrap());

        let dec2 = tree.decode_at(&rom, reloc.written_offset, 256).unwrap();
        assert_eq!(dec2.bytes, b"ABC ABC ABC ABC");
        assert!(dec2.terminated);
    }

    #[test]
    fn huffman_rejects_cyclic_tree() {
        let mut rom = vec![0u8; 64];
        // Node 0 points to (0, 0) -> cycle!
        rom[0..4].copy_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        assert!(HuffmanTree::from_rom(&rom, 0, 0).is_err());
    }

    #[test]
    fn huffman_text_escape_roundtrip() {
        let raw = b"Weapon Level increased.\x1F";
        let formatted = format_huffman_text(raw);
        assert_eq!(formatted, "Weapon Level increased.<$1F>");
        let parsed = parse_huffman_text(&formatted).unwrap();
        assert_eq!(parsed, raw);
    }
}
