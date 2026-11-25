//! Camelot 255-tree context-sensitive bigram Huffman text codec (Read + Write).
//!
//! Supports the Camelot GBA RPG text compression format used in *Golden Sun* (`AGSE`) and
//! *Golden Sun: The Lost Age* (`AGFE`):
//!
//! ## Verified Retail Binary Layout
//! - **8-byte Header** (e.g., `0x03842C` in *Golden Sun*, `0x060C30` in *Golden Sun: The Lost Age*):
//!   - `+0x00`: `tree_data_ptr: u32` (`0x08xxxxxx` ROM pointer to packed tree buffer)
//!   - `+0x04`: `offset_table_ptr: u32` (`0x08xxxxxx` ROM pointer to `u16` relative offset table)
//! - **Offset Table (`offset_table_offset`)**:
//!   - Up to 256 little-endian `u16` byte offsets relative to `tree_data_offset`, indexed by the
//!     previous decoded character byte `prev` (`0x00` at the start of every string).
//!   - `0x8000` is the sentinel for an unused/empty context tree.
//! - **Per-Context Binary Tree (`tree_addr = tree_data_offset + offset_table[prev]`)**:
//!   - **Forward from `tree_addr`**: LSB-first pre-order bitstream describing the binary tree:
//!     - Bit `0`: internal node (left child on bitstream bit `0`, right child on bit `1`).
//!     - Bit `1`: leaf node (consumes the next 12-bit symbol going *backward* from `tree_addr`).
//!   - **Backward from `tree_addr`**: reverse-ordered packed 12-bit symbols (3 bytes per 2 leaves):
//!     - For leaf index `k` (`pair = k / 2`, `base = tree_addr - pair * 3`):
//!       - Even `k`: `((rom[base - 1] as u16) << 4) | ((rom[base - 2] as u16) >> 4)`
//!       - Odd `k`: `(((rom[base - 2] & 0x0F) as u16) << 8) | (rom[base - 3] as u16)`
//!   - Single-leaf trees (root bit = `1`, 0 bits consumed per transition, e.g. deterministic
//!     bigrams like `0x1E -> 0x00`) are valid as long as no zero-bit cycle exists.
//! - **Compressed Text Bitstream**:
//!   - LSB-first bitstream starting with `prev = 0x00`.
//!   - Repeatedly traverses `trees[prev]` to decode `sym: u16`; stops on `sym == 0x0000`
//!     (terminator), otherwise emits `sym` and updates `prev = (sym & 0xFF) as u8`.

#![deny(unsafe_code)]

use std::collections::HashMap;

use formatkit_core::{Error, Result};
use gba_rom::{bus_to_rom_offset, rom_offset_to_bus};
use serde::{Deserialize, Serialize};

use crate::huffman::{format_huffman_text, parse_huffman_text};
use crate::string::{write_or_relocate_bytes, TextReplaceOptions, TextReplaceReport};

/// Maximum number of context trees in a Camelot bigram Huffman bank (indexed by `u8`).
const MAX_CONTEXT_TREES: usize = 256;

/// Sentinel value in the `u16` offset table marking an unused context entry.
const EMPTY_TREE_SENTINEL: u16 = 0x8000;

/// Maximum binary tree depth allowed while parsing a single context tree.
const MAX_TREE_DEPTH: usize = 64;

/// Maximum number of leaf symbols allowed in a single context tree (12-bit symbol space).
const MAX_TREE_LEAVES: usize = 4096;

/// Maximum consecutive 0-bit transitions allowed during decoding (guards against zero-bit cycles).
const MAX_ZERO_BIT_STEPS: usize = 256;

/// Internal binary tree node for one context `prev`.
#[derive(Debug, Clone)]
enum CamelotNode {
    Leaf(u16),
    Branch { left: usize, right: usize },
}

/// One parsed Huffman tree for a specific preceding character `prev`.
#[derive(Debug, Clone)]
struct ContextTree {
    nodes: Vec<CamelotNode>,
    root: usize,
    encode_table: HashMap<u16, Vec<u8>>,
}

impl ContextTree {
    fn parse(rom: &[u8], tree_addr: usize) -> Result<Self> {
        if tree_addr >= rom.len() {
            return Err(Error::Truncated {
                offset: tree_addr,
                needed: 1,
                available: 0,
            });
        }

        let mut nodes = Vec::new();
        let mut bit_pos = 0usize;
        let mut leaf_idx = 0usize;

        let root = parse_preorder_node(
            rom,
            tree_addr,
            &mut bit_pos,
            &mut leaf_idx,
            0,
            &mut nodes,
        )?;

        let mut encode_table = HashMap::new();
        let mut path = Vec::new();
        build_encode_table(&nodes, root, &mut path, &mut encode_table);

        Ok(Self {
            nodes,
            root,
            encode_table,
        })
    }
}

fn parse_preorder_node(
    rom: &[u8],
    tree_addr: usize,
    bit_pos: &mut usize,
    leaf_idx: &mut usize,
    depth: usize,
    nodes: &mut Vec<CamelotNode>,
) -> Result<usize> {
    if depth > MAX_TREE_DEPTH {
        return Err(Error::ResourceLimit {
            resource: "camelot_huffman_tree_depth",
            requested: depth as u64,
            limit: MAX_TREE_DEPTH as u64,
        });
    }

    let byte_off = tree_addr
        .checked_add(*bit_pos >> 3)
        .ok_or(Error::InvalidField {
            what: "camelot_tree_bit_offset",
            value: *bit_pos as u64,
        })?;
    if byte_off >= rom.len() {
        return Err(Error::Truncated {
            offset: byte_off,
            needed: 1,
            available: 0,
        });
    }
    let bit = (rom[byte_off] >> (*bit_pos & 7)) & 1;
    *bit_pos += 1;

    if bit == 1 {
        if *leaf_idx >= MAX_TREE_LEAVES {
            return Err(Error::ResourceLimit {
                resource: "camelot_huffman_tree_leaves",
                requested: (*leaf_idx + 1) as u64,
                limit: MAX_TREE_LEAVES as u64,
            });
        }
        let sym = read_reverse_sym12(rom, tree_addr, *leaf_idx)?;
        *leaf_idx += 1;
        let idx = nodes.len();
        nodes.push(CamelotNode::Leaf(sym));
        Ok(idx)
    } else {
        let left = parse_preorder_node(rom, tree_addr, bit_pos, leaf_idx, depth + 1, nodes)?;
        let right = parse_preorder_node(rom, tree_addr, bit_pos, leaf_idx, depth + 1, nodes)?;
        let idx = nodes.len();
        nodes.push(CamelotNode::Branch { left, right });
        Ok(idx)
    }
}

fn read_reverse_sym12(rom: &[u8], tree_addr: usize, leaf_idx: usize) -> Result<u16> {
    let pair = leaf_idx / 2;
    let delta = pair
        .checked_mul(3)
        .ok_or(Error::InvalidField {
            what: "camelot_symbol_index",
            value: leaf_idx as u64,
        })?;
    let base = tree_addr
        .checked_sub(delta)
        .ok_or_else(|| Error::Malformed("Camelot Huffman symbol table underflows ROM start".into()))?;

    if leaf_idx.is_multiple_of(2) {
        if base < 2 || base > rom.len() {
            return Err(Error::Malformed(format!(
                "Camelot Huffman symbol {leaf_idx} out of bounds before tree at 0x{tree_addr:X}"
            )));
        }
        let hi = u16::from(rom[base - 1]);
        let lo = u16::from(rom[base - 2]);
        Ok((hi << 4) | (lo >> 4))
    } else {
        if base < 3 || base > rom.len() {
            return Err(Error::Malformed(format!(
                "Camelot Huffman symbol {leaf_idx} out of bounds before tree at 0x{tree_addr:X}"
            )));
        }
        let hi = u16::from(rom[base - 2] & 0x0F);
        let lo = u16::from(rom[base - 3]);
        Ok((hi << 8) | lo)
    }
}

fn build_encode_table(
    nodes: &[CamelotNode],
    node_idx: usize,
    path: &mut Vec<u8>,
    table: &mut HashMap<u16, Vec<u8>>,
) {
    match nodes[node_idx] {
        CamelotNode::Leaf(sym) => {
            table
                .entry(sym)
                .and_modify(|existing| {
                    if path.len() < existing.len() {
                        *existing = path.clone();
                    }
                })
                .or_insert_with(|| path.clone());
        }
        CamelotNode::Branch { left, right } => {
            path.push(0);
            build_encode_table(nodes, left, path, table);
            path.pop();

            path.push(1);
            build_encode_table(nodes, right, path, table);
            path.pop();
        }
    }
}

/// Parsed bank of up to 256 Camelot context-sensitive bigram Huffman trees.
#[derive(Debug, Clone)]
pub struct CamelotHuffmanBank {
    tree_data_offset: usize,
    offset_table_offset: usize,
    trees: Vec<Option<ContextTree>>,
    populated_count: usize,
}

/// Result of decoding a Camelot bigram Huffman-compressed string from a GBA ROM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CamelotHuffmanDecoded {
    /// ROM file offset where the compressed bitstream starts.
    pub offset: usize,
    /// GBA Wait State 0 bus address (`0x0800_0000 + offset`), if within 32 MiB.
    pub bus_address: Option<u32>,
    /// Raw decoded payload bytes (excluding the `0x00` terminator).
    pub bytes: Vec<u8>,
    /// Lossless human-readable text representation (printable ASCII + `\n`, `<$XX>` escapes).
    pub text: String,
    /// Exact number of compressed bytes consumed from ROM (including trailing padding bits).
    pub compressed_bytes_len: usize,
    /// Exact number of bits consumed from the compressed bitstream.
    pub bit_len: usize,
    /// Whether the `0x00` string terminator was reached before `max_bytes` or EOF.
    pub terminated: bool,
}

impl CamelotHuffmanBank {
    /// Parse a Camelot bigram Huffman bank from its 8-byte ROM header containing
    /// `[tree_data_ptr: u32, offset_table_ptr: u32]` (e.g. `0x03842C` in *Golden Sun* `AGSE`,
    /// `0x060C30` in *Golden Sun: The Lost Age* `AGFE`).
    pub fn from_header(rom: &[u8], header_offset: usize) -> Result<Self> {
        if header_offset.checked_add(8).is_none_or(|end| end > rom.len()) {
            return Err(Error::Truncated {
                offset: header_offset,
                needed: 8,
                available: rom.len().saturating_sub(header_offset),
            });
        }
        let tree_ptr = u32::from_le_bytes([
            rom[header_offset],
            rom[header_offset + 1],
            rom[header_offset + 2],
            rom[header_offset + 3],
        ]);
        let off_ptr = u32::from_le_bytes([
            rom[header_offset + 4],
            rom[header_offset + 5],
            rom[header_offset + 6],
            rom[header_offset + 7],
        ]);

        let tree_data_offset = bus_to_rom_offset(tree_ptr, rom.len()).ok_or(Error::InvalidField {
            what: "camelot_tree_data_ptr",
            value: u64::from(tree_ptr),
        })?;
        let offset_table_offset = bus_to_rom_offset(off_ptr, rom.len()).ok_or(Error::InvalidField {
            what: "camelot_offset_table_ptr",
            value: u64::from(off_ptr),
        })?;

        if offset_table_offset < header_offset {
            let span = header_offset - offset_table_offset;
            if span.is_multiple_of(2) && span / 2 <= MAX_CONTEXT_TREES {
                return Self::from_rom_with_count(
                    rom,
                    tree_data_offset,
                    offset_table_offset,
                    span / 2,
                );
            }
        }
        Self::from_rom(rom, tree_data_offset, offset_table_offset)
    }

    /// Parse a Camelot bigram Huffman bank from explicit `(tree_data_offset, offset_table_offset)`.
    ///
    /// Automatically stops at the end of the `u16` offset table (up to 256 entries), skipping
    /// `0x8000` empty-tree sentinels and trailing `0x0000` alignment padding.
    pub fn from_rom(
        rom: &[u8],
        tree_data_offset: usize,
        offset_table_offset: usize,
    ) -> Result<Self> {
        Self::parse_internal(rom, tree_data_offset, offset_table_offset, None)
    }

    /// Parse a Camelot bigram Huffman bank with an explicit `entry_count` (`1..=256`) in the
    /// `u16` offset table.
    pub fn from_rom_with_count(
        rom: &[u8],
        tree_data_offset: usize,
        offset_table_offset: usize,
        entry_count: usize,
    ) -> Result<Self> {
        if entry_count == 0 || entry_count > MAX_CONTEXT_TREES {
            return Err(Error::InvalidField {
                what: "camelot_tree_count",
                value: entry_count as u64,
            });
        }
        Self::parse_internal(rom, tree_data_offset, offset_table_offset, Some(entry_count))
    }

    fn parse_internal(
        rom: &[u8],
        tree_data_offset: usize,
        offset_table_offset: usize,
        explicit_count: Option<usize>,
    ) -> Result<Self> {
        if tree_data_offset >= rom.len() {
            return Err(Error::Truncated {
                offset: tree_data_offset,
                needed: 1,
                available: 0,
            });
        }
        if offset_table_offset.checked_add(2).is_none_or(|end| end > rom.len()) {
            return Err(Error::Truncated {
                offset: offset_table_offset,
                needed: 2,
                available: rom.len().saturating_sub(offset_table_offset),
            });
        }

        let limit = explicit_count.unwrap_or(MAX_CONTEXT_TREES);
        let mut trees: Vec<Option<ContextTree>> = vec![None; MAX_CONTEXT_TREES];
        let mut populated_count = 0usize;

        for (ch, slot) in trees.iter_mut().enumerate().take(limit) {
            let entry_off = offset_table_offset + ch * 2;
            if entry_off + 2 > rom.len() {
                if explicit_count.is_some() {
                    return Err(Error::Truncated {
                        offset: entry_off,
                        needed: 2,
                        available: rom.len().saturating_sub(entry_off),
                    });
                }
                break;
            }
            if explicit_count.is_none()
                && offset_table_offset < tree_data_offset
                && entry_off + 2 > tree_data_offset
            {
                break;
            }

            let rel = u16::from_le_bytes([rom[entry_off], rom[entry_off + 1]]);
            if rel == EMPTY_TREE_SENTINEL {
                continue;
            }
            // Trailing 2-byte alignment padding (`0x0000`) at the end of a non-4-byte-aligned
            // offset table (observed in AGSE index 123 right before the 8-byte header).
            if rel == 0 && ch > 0 && (explicit_count.is_none() || ch + 1 == limit) {
                if explicit_count.is_none() {
                    break;
                }
                continue;
            }

            let tree_addr = tree_data_offset
                .checked_add(usize::from(rel))
                .ok_or(Error::InvalidField {
                    what: "camelot_tree_rel_offset",
                    value: u64::from(rel),
                })?;

            if explicit_count.is_none()
                && tree_data_offset < offset_table_offset
                && tree_addr >= offset_table_offset
            {
                break;
            }

            let parsed = ContextTree::parse(rom, tree_addr)?;
            *slot = Some(parsed);
            populated_count += 1;
        }

        if populated_count == 0 {
            return Err(Error::Malformed(
                "Camelot Huffman bank contains zero valid context trees".into(),
            ));
        }

        Ok(Self {
            tree_data_offset,
            offset_table_offset,
            trees,
            populated_count,
        })
    }

    /// ROM file offset of the packed tree data buffer.
    #[must_use]
    pub const fn tree_data_offset(&self) -> usize {
        self.tree_data_offset
    }

    /// ROM file offset of the `u16` relative offset table.
    #[must_use]
    pub const fn offset_table_offset(&self) -> usize {
        self.offset_table_offset
    }

    /// Number of populated context trees in this bank.
    #[must_use]
    pub const fn tree_count(&self) -> usize {
        self.populated_count
    }

    /// Whether a context tree exists for preceding byte `prev`.
    #[must_use]
    pub fn has_tree(&self, prev: u8) -> bool {
        self.trees[usize::from(prev)].is_some()
    }

    /// Number of leaf symbols in the context tree for `prev` (0 if absent).
    #[must_use]
    pub fn symbol_count_for(&self, prev: u8) -> usize {
        self.trees[usize::from(prev)]
            .as_ref()
            .map_or(0, |t| t.encode_table.len())
    }

    /// Decode a Camelot bigram Huffman bitstream starting at `offset` in `rom`.
    ///
    /// Starts with `prev = 0x00` and stops when the `0x00` terminator symbol is decoded,
    /// `max_bytes` payload bytes have been produced, or EOF is reached.
    pub fn decode_at(
        &self,
        rom: &[u8],
        offset: usize,
        max_bytes: usize,
    ) -> Result<CamelotHuffmanDecoded> {
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
        let mut prev: u8 = 0;
        let mut terminated = false;
        let mut zero_bit_steps: usize = 0;

        loop {
            let tree = self.trees[usize::from(prev)].as_ref().ok_or_else(|| {
                Error::Malformed(format!(
                    "missing Camelot Huffman tree for previous byte 0x{prev:02X}"
                ))
            })?;

            let start_bits = bit_len;
            let mut node_idx = tree.root;
            let sym = loop {
                match tree.nodes[node_idx] {
                    CamelotNode::Leaf(s) => break Some(s),
                    CamelotNode::Branch { left, right } => {
                        if byte_pos >= rom.len() {
                            break None;
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
                }
            };

            let Some(sym) = sym else {
                break;
            };

            if bit_len == start_bits {
                zero_bit_steps += 1;
                if zero_bit_steps > MAX_ZERO_BIT_STEPS {
                    return Err(Error::Malformed(
                        "zero-bit cycle detected in Camelot Huffman trees".into(),
                    ));
                }
            } else {
                zero_bit_steps = 0;
            }

            if sym == 0 {
                terminated = true;
                break;
            }

            if sym > 0x00FF {
                if out.len() + 2 > max_bytes {
                    break;
                }
                out.push((sym & 0xFF) as u8);
                out.push((sym >> 8) as u8);
            } else {
                if out.len() >= max_bytes {
                    break;
                }
                out.push(sym as u8);
            }
            prev = (sym & 0xFF) as u8;
        }

        let compressed_bytes_len = bit_len.div_ceil(8);
        let text = format_huffman_text(&out);

        Ok(CamelotHuffmanDecoded {
            offset,
            bus_address: rom_offset_to_bus(offset),
            bytes: out,
            text,
            compressed_bytes_len,
            bit_len,
            terminated,
        })
    }

    /// Encode raw payload bytes into an LSB-first Camelot bigram Huffman bitstream,
    /// appending the `0x00` terminator symbol if not already present at the end of `bytes`.
    ///
    /// Returns [`Error::Malformed`] if any context tree `trees[prev]` is missing or does not
    /// contain the `(prev, next)` bigram transition.
    pub fn encode(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let mut writer = LsbBitWriter::new();
        let mut prev: u8 = 0;

        for (i, &b) in bytes.iter().enumerate() {
            let tree = self.trees[usize::from(prev)].as_ref().ok_or_else(|| {
                Error::Malformed(format!(
                    "no Camelot Huffman tree for previous byte 0x{prev:02X} at offset {i}"
                ))
            })?;
            let bits = tree.encode_table.get(&u16::from(b)).ok_or_else(|| {
                Error::Malformed(format!(
                    "bigram (0x{prev:02X}, 0x{b:02X}) at offset {i} is not present in Camelot Huffman tree 0x{prev:02X}"
                ))
            })?;
            writer.write_bits(bits);
            prev = b;
        }

        if bytes.last() != Some(&0x00) {
            let tree = self.trees[usize::from(prev)].as_ref().ok_or_else(|| {
                Error::Malformed(format!(
                    "no Camelot Huffman tree for previous byte 0x{prev:02X} when writing 0x00 terminator"
                ))
            })?;
            let term_bits = tree.encode_table.get(&0x0000).ok_or_else(|| {
                Error::Malformed(format!(
                    "bigram (0x{prev:02X}, 0x00) terminator is not present in Camelot Huffman tree 0x{prev:02X}"
                ))
            })?;
            writer.write_bits(term_bits);
        }

        Ok(writer.finish())
    }
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

/// Replace a Camelot bigram Huffman-compressed text entry in `rom` at `entry_offset` using
/// the parsed `bank`.
///
/// - Measures the original compressed byte length via [`CamelotHuffmanBank::decode_at`].
/// - Requires `orig.terminated == true` when `options.capacity` is `None`.
/// - Parses `new_text` (supporting `<$XX>` raw hex escapes) and encodes it via
///   [`CamelotHuffmanBank::encode`].
/// - Writes in-place if the encoded bitstream fits within the original capacity, or relocates
///   into `0xFF` free space (`0x00` fallback at `>= 0x200`) and patches `0x08xxxxxx` ROM
///   pointers when `options.allow_relocate` is `true`.
pub fn replace_camelot_text_in_rom(
    rom: &mut [u8],
    bank: &CamelotHuffmanBank,
    entry_offset: usize,
    new_text: &str,
    options: &TextReplaceOptions,
) -> Result<TextReplaceReport> {
    let orig = bank.decode_at(rom, entry_offset, 16_384)?;
    if options.capacity.is_none() && !orig.terminated {
        return Err(Error::Malformed(format!(
            "Camelot Huffman string at offset 0x{entry_offset:X} is not terminated within 16384 bytes; specify explicit capacity"
        )));
    }
    let new_bytes = parse_huffman_text(new_text)?;
    let encoded = bank.encode(&new_bytes)?;
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
    use crate::string::TextReplaceMode;

    /// Pack a reverse-ordered list of 12-bit symbols immediately before `tree_addr` and write
    /// the LSB-first preorder structure bits starting at `tree_addr`.
    fn write_camelot_tree(rom: &mut [u8], tree_addr: usize, syms: &[u16], tree_bits: &[u8]) {
        for (k, &sym) in syms.iter().enumerate() {
            let pair = k / 2;
            let base = tree_addr - pair * 3;
            if k % 2 == 0 {
                rom[base - 1] = ((sym >> 4) & 0xFF) as u8;
                rom[base - 2] = (rom[base - 2] & 0x0F) | (((sym & 0x0F) as u8) << 4);
            } else {
                rom[base - 2] = (rom[base - 2] & 0xF0) | (((sym >> 8) & 0x0F) as u8);
                rom[base - 3] = (sym & 0xFF) as u8;
            }
        }
        let mut writer = LsbBitWriter::new();
        writer.write_bits(tree_bits);
        let bytes = writer.finish();
        rom[tree_addr..tree_addr + bytes.len()].copy_from_slice(&bytes);
    }

    /// Build a synthetic ROM with Camelot bigram trees for:
    /// - prev=0x00: 'A' (0), 'B' (10), END=0x00 (11)
    /// - prev='A' (0x41): 'B' (0), 'C' (10), END=0x00 (11)
    /// - prev='B' (0x42): 'A' (0), ' ' (10), END=0x00 (11)
    /// - prev='C' (0x43): single-leaf deterministic 0-bit transition -> END=0x00 ()
    /// - prev=' ' (0x20): 'A' (0), 'B' (1)
    fn build_synthetic_camelot_rom() -> (Vec<u8>, usize, usize, usize, usize, usize) {
        let mut rom = vec![0u8; 0x8000];
        rom[0x4000..].fill(0xFF); // free space

        let tree_data_off = 0x200usize;
        let offset_table_off = 0x300usize;
        let header_off = 0x400usize;

        // Initialize offset table (128 u16 entries) to 0x8000 (unused).
        for i in 0..128 {
            let off = offset_table_off + i * 2;
            rom[off..off + 2].copy_from_slice(&EMPTY_TREE_SENTINEL.to_le_bytes());
        }

        // Preorder bits for 3-leaf tree: 0 (root), 1 (left), 0 (right), 1 (r.l), 1 (r.r)
        let bits3 = &[0, 1, 0, 1, 1];
        // Preorder bits for 2-leaf tree: 0, 1, 1
        let bits2 = &[0, 1, 1];
        // Preorder bits for 1-leaf tree: 1
        let bits1 = &[1];

        let specs: &[(u8, usize, &[u16], &[u8])] = &[
            (0x00, 0x10, &[b'A' as u16, b'B' as u16, 0x00], bits3),
            (b'A', 0x20, &[b'B' as u16, b'C' as u16, 0x00], bits3),
            (b'B', 0x30, &[b'A' as u16, b' ' as u16, 0x00], bits3),
            (b'C', 0x40, &[0x00], bits1),
            (b' ', 0x50, &[b'A' as u16, b'B' as u16], bits2),
        ];

        for &(ch, rel, syms, bits) in specs {
            write_camelot_tree(&mut rom, tree_data_off + rel, syms, bits);
            let slot = offset_table_off + usize::from(ch) * 2;
            rom[slot..slot + 2].copy_from_slice(&(rel as u16).to_le_bytes());
        }

        // Write 8-byte header at header_off: [tree_data_ptr, offset_table_ptr]
        let tree_bus = rom_offset_to_bus(tree_data_off).unwrap();
        let off_bus = rom_offset_to_bus(offset_table_off).unwrap();
        rom[header_off..header_off + 4].copy_from_slice(&tree_bus.to_le_bytes());
        rom[header_off + 4..header_off + 8].copy_from_slice(&off_bus.to_le_bytes());

        let bank = CamelotHuffmanBank::from_header(&rom, header_off).unwrap();
        let encoded = bank.encode(b"AB AC").unwrap();

        let entry_off = 0x1000usize;
        rom[entry_off..entry_off + encoded.len()].copy_from_slice(&encoded);

        let ptr_off = 0x500usize;
        let entry_bus = rom_offset_to_bus(entry_off).unwrap();
        rom[ptr_off..ptr_off + 4].copy_from_slice(&entry_bus.to_le_bytes());

        (rom, header_off, tree_data_off, offset_table_off, entry_off, ptr_off)
    }

    #[test]
    fn camelot_decode_encode_roundtrip_and_unmapped_bigram() {
        let (rom, header_off, tree_data_off, offset_table_off, entry_off, _) =
            build_synthetic_camelot_rom();

        let bank_hdr = CamelotHuffmanBank::from_header(&rom, header_off).unwrap();
        let bank_raw = CamelotHuffmanBank::from_rom(&rom, tree_data_off, offset_table_off).unwrap();
        assert_eq!(bank_hdr.tree_count(), 5);
        assert_eq!(bank_raw.tree_count(), 5);
        assert!(bank_hdr.has_tree(0x00));
        assert!(bank_hdr.has_tree(b'C'));
        assert!(!bank_hdr.has_tree(b'Z'));
        assert_eq!(bank_hdr.symbol_count_for(b'C'), 1);

        let dec = bank_hdr.decode_at(&rom, entry_off, 256).unwrap();
        assert_eq!(dec.bytes, b"AB AC");
        assert_eq!(dec.text, "AB AC");
        assert!(dec.terminated);
        // Bits:
        // prev=0x00 -> 'A' (0)
        // prev='A'  -> 'B' (0)
        // prev='B'  -> ' ' (10)
        // prev=' '  -> 'A' (0)
        // prev='A'  -> 'C' (10)
        // prev='C'  -> END (0 bits!)
        // Total = 7 bits = 1 byte!
        assert_eq!(dec.bit_len, 7);
        assert_eq!(dec.compressed_bytes_len, 1);

        // Bit-for-bit re-encode (both with and without explicit trailing 0x00)
        let reenc = bank_hdr.encode(&dec.bytes).unwrap();
        assert_eq!(reenc, rom[entry_off..entry_off + dec.compressed_bytes_len]);
        let reenc_term = bank_hdr.encode(b"AB AC\0").unwrap();
        assert_eq!(reenc_term, reenc);

        // Unmapped bigram ('A' -> 'A' is not in tree['A']) returns a clear error
        let err = bank_hdr.encode(b"AA");
        assert!(matches!(err, Err(Error::Malformed(_))));

        // Missing context tree ('Z' has no tree) returns a clear error
        let err2 = bank_hdr.encode(b"Z");
        assert!(matches!(err2, Err(Error::Malformed(_))));
    }

    #[test]
    fn camelot_replace_in_place_and_relocate() {
        let (mut rom, header_off, _, _, entry_off, ptr_off) = build_synthetic_camelot_rom();
        let bank = CamelotHuffmanBank::from_header(&rom, header_off).unwrap();

        // Shorter/equal replacement fits in-place ("AC" = 3 bits = 1 byte)
        let rep = replace_camelot_text_in_rom(
            &mut rom,
            &bank,
            entry_off,
            "AC",
            &TextReplaceOptions::default(),
        )
        .unwrap();
        assert_eq!(rep.mode, TextReplaceMode::InPlace);
        assert_eq!(rep.written_offset, entry_off);

        let dec = bank.decode_at(&rom, entry_off, 256).unwrap();
        assert_eq!(dec.text, "AC");

        // Longer replacement without allow_relocate fails
        let long_str = "AB AB AB AB AB AB AC";
        let err = replace_camelot_text_in_rom(
            &mut rom,
            &bank,
            entry_off,
            long_str,
            &TextReplaceOptions::default(),
        );
        assert!(matches!(err, Err(Error::ResourceLimit { .. })));

        // Longer replacement with allow_relocate relocates to 0xFF free space & patches pointer
        let opts = TextReplaceOptions {
            allow_relocate: true,
            ..TextReplaceOptions::default()
        };
        let reloc = replace_camelot_text_in_rom(&mut rom, &bank, entry_off, long_str, &opts)
            .unwrap();
        assert_eq!(reloc.mode, TextReplaceMode::Relocated);
        assert!(reloc.written_offset >= 0x4000);
        assert_eq!(reloc.patched_pointer_count, 1);
        assert_eq!(reloc.patched_pointer_offsets, vec![ptr_off]);

        let new_ptr = u32::from_le_bytes(rom[ptr_off..ptr_off + 4].try_into().unwrap());
        assert_eq!(new_ptr, rom_offset_to_bus(reloc.written_offset).unwrap());

        let dec2 = bank.decode_at(&rom, reloc.written_offset, 256).unwrap();
        assert_eq!(dec2.text, long_str);
        assert!(dec2.terminated);
    }

    #[test]
    fn camelot_rejects_zero_bit_cycle_and_unterminated_replace() {
        // 1) Zero-bit cycle: prev=0x00 -> 1-leaf 'A', prev='A' -> 1-leaf 'A'
        let mut cyc_rom = vec![0u8; 0x400];
        for i in 0..128 {
            let off = 0x200 + i * 2;
            cyc_rom[off..off + 2].copy_from_slice(&EMPTY_TREE_SENTINEL.to_le_bytes());
        }
        write_camelot_tree(&mut cyc_rom, 0x110, &[b'A' as u16], &[1]);
        write_camelot_tree(&mut cyc_rom, 0x120, &[b'A' as u16], &[1]);
        cyc_rom[0x200..0x202].copy_from_slice(&0x0010u16.to_le_bytes());
        cyc_rom[0x200 + usize::from(b'A') * 2..0x200 + usize::from(b'A') * 2 + 2]
            .copy_from_slice(&0x0020u16.to_le_bytes());

        let cyc_bank = CamelotHuffmanBank::from_rom(&cyc_rom, 0x100, 0x200).unwrap();
        assert!(matches!(
            cyc_bank.decode_at(&cyc_rom, 0x300, 512),
            Err(Error::Malformed(_))
        ));

        // 2) Unterminated bitstream rejected by replace_camelot_text_in_rom when capacity is None
        let (mut rom, header_off, _, _, entry_off, _) = build_synthetic_camelot_rom();
        let bank = CamelotHuffmanBank::from_header(&rom, header_off).unwrap();
        // All 0 bits -> 0x00->'A'(0), 'A'->'B'(0), 'B'->'A'(0), ... never terminates!
        rom[entry_off..].fill(0x00);
        let err = replace_camelot_text_in_rom(
            &mut rom,
            &bank,
            entry_off,
            "AC",
            &TextReplaceOptions::default(),
        );
        assert!(matches!(err, Err(Error::Malformed(_))));
    }

    #[test]
    fn camelot_rejects_excessive_tree_depth() {
        // Tree bitstream of all 0s -> exceeds MAX_TREE_DEPTH without overflowing stack
        let mut rom = vec![0u8; 0x400];
        for i in 0..128 {
            let off = 0x200 + i * 2;
            rom[off..off + 2].copy_from_slice(&EMPTY_TREE_SENTINEL.to_le_bytes());
        }
        rom[0x200..0x202].copy_from_slice(&0x0010u16.to_le_bytes());
        // rom[0x110..] is all zeros -> infinite internal nodes
        assert!(matches!(
            CamelotHuffmanBank::from_rom(&rom, 0x100, 0x200),
            Err(Error::ResourceLimit { .. })
        ));
    }
}
