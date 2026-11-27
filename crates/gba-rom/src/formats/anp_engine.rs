//! `studio.anp-engine` — `ANP ` / `HAP ` / `PLA ` / `PSI3` sprite, palette-animation, and
//! parameter-script engine parser, symmetric byte-identical writer, and ROM scanner.
//!
//! Built on `formatkit-layout` `CountedOffsetArraySpec` and `FixedRecordTableSpec`.

use formatkit_core::{u16_at, u32_at, Error, Result};
use formatkit_layout::{
    parse_counted_offset_array_with_count, parse_fixed_record_table, CountedOffsetArraySpec,
    FixedRecordTableSpec, IntWidth, OffsetBase, OffsetOrderPolicy, SizeMode, TableTermination,
};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_ANP_ENGINE};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_ANP: &[u8; 4] = b"ANP ";
const MAGIC_HAP: &[u8; 4] = b"HAP ";
const MAGIC_PLA: &[u8; 4] = b"PLA ";
const MAGIC_PSI3: &[u8; 4] = b"PSI3";
const MAGIC_CG4: &[u8; 4] = b"CG4 ";

const MAX_PSI3_LEN: usize = 8 * 1024 * 1024;

/// Parsed `ANP ` animation bundle (v0x0702 or v0x0505).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnpAsset {
    pub version: u32,
    pub seq_count: usize,
    pub cell_count: usize,
    pub piece_count: usize,
    /// Raw exact asset bytes `[0..consumed_end]` for lossless round-trip.
    pub raw: Vec<u8>,
    pub consumed_end: usize,
}

/// Parsed `HAP ` sprite bank (outer/inner counted offset directories + optional `CG4 ` tile store).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HapAsset {
    pub outer_count: usize,
    pub total_cells: usize,
    pub has_cg4: bool,
    pub raw: Vec<u8>,
    pub consumed_end: usize,
}

/// Parsed `PLA ` palette-animation timeline (`0x050B`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaAsset {
    pub flags: u32,
    pub steps: Vec<[u8; 2]>,
    pub palette_count: u32,
    pub palette_bytes: Vec<u8>,
    pub consumed_end: usize,
}

/// Parsed `PSI3` `u16` parameter-word script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Psi3Asset {
    pub words: Vec<u16>,
    pub consumed_end: usize,
}

/// Unified `studio.anp-engine` asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnpEngineAsset {
    Anp(AnpAsset),
    Hap(HapAsset),
    Pla(PlaAsset),
    Psi3(Psi3Asset),
}

impl AnpEngineAsset {
    /// Exact consumed byte extent.
    #[must_use]
    pub fn consumed_end(&self) -> usize {
        match self {
            Self::Anp(v) => v.consumed_end,
            Self::Hap(v) => v.consumed_end,
            Self::Pla(v) => v.consumed_end,
            Self::Psi3(v) => v.consumed_end,
        }
    }

    /// Sub-format identifier.
    #[must_use]
    pub fn subkind(&self) -> &'static str {
        match self {
            Self::Anp(_) => "anp",
            Self::Hap(_) => "hap",
            Self::Pla(_) => "pla",
            Self::Psi3(_) => "psi3",
        }
    }

    /// Serialize this asset back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Fast, panic-free structural detector for `studio.anp-engine` assets.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse any `studio.anp-engine` asset (`ANP `, `HAP `, `PLA `, or `PSI3`) at `data[0..]`.
pub fn parse(data: &[u8]) -> Result<AnpEngineAsset> {
    if data.len() < 16 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 16,
            available: data.len(),
        });
    }
    match &data[..4] {
        b"ANP " => parse_anp(data).map(AnpEngineAsset::Anp),
        b"HAP " => parse_hap(data).map(AnpEngineAsset::Hap),
        b"PLA " => parse_pla(data).map(AnpEngineAsset::Pla),
        b"PSI3" => parse_psi3(data).map(AnpEngineAsset::Psi3),
        _ => Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_ANP),
            found: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        }),
    }
}

fn validate_u16_rel_table(data: &[u8], base: usize, count: usize, region_end: usize) -> Result<()> {
    if count == 0 || count > 4096 || base >= region_end || region_end > data.len() {
        return Err(Error::InvalidField {
            what: "ANP table bounds",
            value: count as u64,
        });
    }
    let spec = CountedOffsetArraySpec::new(
        base,
        count,
        IntWidth::U16,
        formatkit_core::Endian::Little,
        4096,
    )
    .with_offset_base(OffsetBase::Fixed(base as u64))
    .with_order_policy(OffsetOrderPolicy::RequireMonotonic);

    let spans = parse_counted_offset_array_with_count(&data[..region_end], &spec, count).map_err(
        |_| Error::InvalidField {
            what: "ANP relative offset table",
            value: count as u64,
        },
    )?;
    let expected_first = base + count * 2;
    if spans.first().map(|s| s.offset as usize) != Some(expected_first) {
        return Err(Error::InvalidField {
            what: "ANP first table entry",
            value: expected_first as u64,
        });
    }
    Ok(())
}

fn parse_anp(data: &[u8]) -> Result<AnpAsset> {
    let version = u32_at(data, 4)?;
    let reserved = u32_at(data, 12)?;
    if reserved != 0 {
        return Err(Error::InvalidField {
            what: "ANP reserved",
            value: u64::from(reserved),
        });
    }
    match version {
        0x0702 => {
            if data.len() < 48 {
                return Err(Error::Truncated {
                    offset: 0,
                    needed: 48,
                    available: data.len(),
                });
            }
            let c_a = u32_at(data, 0x10)? as usize;
            let off_a = (u32_at(data, 0x14)? & !3) as usize;
            let c_b = u32_at(data, 0x18)? as usize;
            let off_b = (u32_at(data, 0x1C)? & !3) as usize;
            let c_c = u32_at(data, 0x20)? as usize;
            let off_c = (u32_at(data, 0x24)? & !3) as usize;
            if off_a != 0x30 || c_a == 0 || c_b == 0 || c_c == 0 || c_c > 16384 {
                return Err(Error::InvalidField {
                    what: "ANP v0x0702 header",
                    value: off_a as u64,
                });
            }
            validate_u16_rel_table(data, off_a, c_a, off_b)?;
            validate_u16_rel_table(data, off_b, c_b, off_c)?;
            let pieces_end = off_c
                .checked_add(c_c.checked_mul(4).ok_or(Error::InvalidField {
                    what: "ANP piece array",
                    value: c_c as u64,
                })?)
                .ok_or(Error::InvalidField {
                    what: "ANP pieces_end",
                    value: off_c as u64,
                })?;
            if pieces_end > data.len() {
                return Err(Error::Truncated {
                    offset: off_c,
                    needed: c_c * 4,
                    available: data.len().saturating_sub(off_c),
                });
            }
            Ok(AnpAsset {
                version,
                seq_count: c_a,
                cell_count: c_b,
                piece_count: c_c,
                raw: data[..pieces_end].to_vec(),
                consumed_end: pieces_end,
            })
        }
        0x0505 => {
            if data.len() < 32 {
                return Err(Error::Truncated {
                    offset: 0,
                    needed: 32,
                    available: data.len(),
                });
            }
            let c_a = u16_at(data, 0x10)? as usize;
            let c_b = u16_at(data, 0x14)? as usize;
            let off_a = (u32_at(data, 0x18)? & !3) as usize;
            let off_b = (u32_at(data, 0x1C)? & !3) as usize;
            if off_a != 0x20 || c_a == 0 || c_b == 0 {
                return Err(Error::InvalidField {
                    what: "ANP v0x0505 header",
                    value: off_a as u64,
                });
            }
            validate_u16_rel_table(data, off_a, c_a, off_b)?;
            validate_u16_rel_table(data, off_b, c_b, data.len())?;
            Ok(AnpAsset {
                version,
                seq_count: c_a,
                cell_count: c_b,
                piece_count: 0,
                raw: data.to_vec(),
                consumed_end: data.len(),
            })
        }
        other => Err(Error::InvalidField {
            what: "ANP version",
            value: u64::from(other),
        }),
    }
}

fn align16(v: usize) -> usize {
    (v + 15) & !15
}

fn parse_hap(data: &[u8]) -> Result<HapAsset> {
    if data.len() < 0x20 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 0x20,
            available: data.len(),
        });
    }
    let version = u32_at(data, 4)?;
    let hdr_const = u16_at(data, 8)?;
    let reserved = u32_at(data, 12)?;
    let outer_n = u16_at(data, 0x10)? as usize;
    if version != 0x0702 || hdr_const != 0x0220 || reserved != 0 || outer_n == 0 || outer_n > 1024 {
        return Err(Error::InvalidField {
            what: "HAP header",
            value: u64::from(version),
        });
    }
    let outer_dir_end = align16(0x12 + outer_n * 2);
    if outer_dir_end > data.len() {
        return Err(Error::Truncated {
            offset: 0x12,
            needed: outer_n * 2,
            available: data.len().saturating_sub(0x12),
        });
    }

    // Logical end E: next ANP/HAP v0x0702 header after outer_dir_end, else data.len().
    let mut logical_end = data.len();
    let mut scan = outer_dir_end;
    while scan + 8 <= data.len() {
        let tag = &data[scan..scan + 4];
        if (tag == MAGIC_ANP || tag == MAGIC_HAP) && u32_at(data, scan + 4).ok() == Some(0x0702) {
            logical_end = scan;
            break;
        }
        scan += 4;
    }

    let outer_spec = CountedOffsetArraySpec::new(
        0x12,
        outer_n,
        IntWidth::U16,
        formatkit_core::Endian::Little,
        1024,
    )
    .with_offset_base(OffsetBase::Absolute)
    .with_order_policy(OffsetOrderPolicy::RequireMonotonic);

    let outer_spans =
        parse_counted_offset_array_with_count(&data[..logical_end], &outer_spec, outer_n).map_err(
            |_| Error::InvalidField {
                what: "HAP outer directory",
                value: outer_n as u64,
            },
        )?;
    if outer_spans.first().map(|s| s.offset as usize) != Some(outer_dir_end) {
        return Err(Error::InvalidField {
            what: "HAP outer_offs[0]",
            value: outer_dir_end as u64,
        });
    }

    let mut total_cells = 0usize;
    let mut has_cg4 = false;
    for span in &outer_spans {
        let sec_start = span.offset as usize;
        let sec_end = sec_start + (span.size as usize);
        if sec_start + 4 > sec_end {
            return Err(Error::InvalidField {
                what: "HAP inner section",
                value: sec_start as u64,
            });
        }
        let inner_m = u16_at(data, sec_start)? as usize;
        if inner_m == 0 || inner_m > 1024 {
            return Err(Error::InvalidField {
                what: "HAP inner count",
                value: inner_m as u64,
            });
        }
        let inner_dir_end = align16(sec_start + 2 + inner_m * 2);
        let inner_spec = CountedOffsetArraySpec::new(
            sec_start + 2,
            inner_m,
            IntWidth::U16,
            formatkit_core::Endian::Little,
            1024,
        )
        .with_offset_base(OffsetBase::Absolute)
        .with_order_policy(OffsetOrderPolicy::RequireMonotonic);

        let inner_spans =
            parse_counted_offset_array_with_count(&data[..sec_end], &inner_spec, inner_m)
                .map_err(|_| Error::InvalidField {
                    what: "HAP inner directory",
                    value: inner_m as u64,
                })?;
        if inner_spans.first().map(|s| s.offset as usize) != Some(inner_dir_end) {
            return Err(Error::InvalidField {
                what: "HAP inner_offs[0]",
                value: inner_dir_end as u64,
            });
        }

        for (k, cell) in inner_spans.iter().enumerate() {
            if k + 1 < inner_spans.len() {
                if !(cell.size == 0x20 || cell.size == 0x30) {
                    return Err(Error::InvalidField {
                        what: "HAP cell stride",
                        value: cell.size,
                    });
                }
            } else {
                let last_off = cell.offset as usize;
                let last_len = sec_end - last_off;
                if last_len == 0x20 || last_len == 0x30 {
                    // pure-cell section
                } else {
                    // Check for 32B fixed header + CG4 chunk
                    let mut cg4_ok = false;
                    for cell_sz in [0x20usize, 0x30usize] {
                        let cg4_off = last_off + cell_sz + 0x20;
                        if cg4_off + 0x30 <= sec_end
                            && &data[cg4_off..cg4_off + 4] == MAGIC_CG4
                            && u32_at(data, cg4_off + 4)? == 0x0504
                        {
                            let ds = u32_at(data, cg4_off + 0x14)? as usize;
                            let stride = u32_at(data, cg4_off + 0x18)?;
                            let tc = u32_at(data, cg4_off + 0x1C)? as usize;
                            let tiles_end = cg4_off.saturating_add(0x30).saturating_add(ds);
                            if stride == 0x20
                                && tc > 0
                                && tc.checked_mul(32) == Some(ds)
                                && tiles_end <= sec_end
                                && ((sec_end - tiles_end) & 7) == 0
                            {
                                cg4_ok = true;
                                has_cg4 = true;
                                break;
                            }
                        }
                    }
                    if !cg4_ok {
                        return Err(Error::InvalidField {
                            what: "HAP trailing cell / CG4",
                            value: last_len as u64,
                        });
                    }
                }
            }
            total_cells += 1;
        }
    }

    Ok(HapAsset {
        outer_count: outer_n,
        total_cells,
        has_cg4,
        raw: data[..logical_end].to_vec(),
        consumed_end: logical_end,
    })
}

fn parse_pla(data: &[u8]) -> Result<PlaAsset> {
    if data.len() < 32 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 32,
            available: data.len(),
        });
    }
    let version = u32_at(data, 4)?;
    let flags = u32_at(data, 8)?;
    let reserved = u32_at(data, 12)?;
    let step_count = u32_at(data, 16)? as usize;
    let palette_count = u32_at(data, 20)?;
    let script_off = u32_at(data, 24)? as usize;
    let bank_off = u32_at(data, 28)? as usize;

    if version != 0x050B
        || (flags & 0xFFFF) != 0x0100
        || reserved != 0
        || step_count == 0
        || step_count > 4096
        || palette_count == 0
        || palette_count > 256
        || script_off != 32
    {
        return Err(Error::InvalidField {
            what: "PLA header",
            value: u64::from(version),
        });
    }
    let expected_bank_off = align16(32 + step_count * 2);
    let pal_bytes_len = (palette_count as usize) * 32;
    let total = bank_off.checked_add(pal_bytes_len).ok_or(Error::InvalidField {
        what: "PLA total",
        value: bank_off as u64,
    })?;
    if bank_off != expected_bank_off || total > data.len() {
        return Err(Error::InvalidField {
            what: "PLA bank_off / total",
            value: bank_off as u64,
        });
    }
    // Padding between script end and bank_off must be zero
    if data[32 + step_count * 2..bank_off].iter().any(|&b| b != 0) {
        return Err(Error::InvalidField {
            what: "PLA alignment padding",
            value: 0,
        });
    }

    let mut steps = Vec::with_capacity(step_count);
    for i in 0..step_count {
        let p0 = data[32 + i * 2];
        let pal_idx = data[32 + i * 2 + 1];
        if u32::from(pal_idx) >= palette_count {
            return Err(Error::InvalidField {
                what: "PLA step palette index",
                value: u64::from(pal_idx),
            });
        }
        steps.push([p0, pal_idx]);
    }
    let palette_bytes = &data[bank_off..total];
    for chunk in palette_bytes.as_chunks::<2>().0 {
        let color = u16::from_le_bytes(*chunk);
        if (color & 0x8000) != 0 {
            return Err(Error::InvalidField {
                what: "PLA BGR555 bit15",
                value: u64::from(color),
            });
        }
    }

    Ok(PlaAsset {
        flags,
        steps,
        palette_count,
        palette_bytes: palette_bytes.to_vec(),
        consumed_end: total,
    })
}

fn parse_psi3(data: &[u8]) -> Result<Psi3Asset> {
    let declared = u32_at(data, 4)? as usize;
    let res0 = u32_at(data, 8)?;
    let res1 = u32_at(data, 12)?;
    if res0 != 0
        || res1 != 0
        || declared < 20
        || declared > data.len()
        || declared > MAX_PSI3_LEN
        || (declared & 1) != 0
    {
        return Err(Error::InvalidField {
            what: "PSI3 header",
            value: declared as u64,
        });
    }
    let word_count = (declared - 16) / 2;
    let spec = FixedRecordTableSpec::new(
        2,
        16,
        MAX_PSI3_LEN / 2,
        None,
        SizeMode::FixedSlot(0),
        TableTermination::Counted(word_count),
    )
    .with_offset_base(OffsetBase::TableEnd);
    let entries = parse_fixed_record_table(&data[..declared], &spec).map_err(|_| {
        Error::InvalidField {
            what: "PSI3 word table",
            value: word_count as u64,
        }
    })?;
    let mut words = Vec::with_capacity(entries.len());
    for e in &entries {
        words.push(u16_at(data, e.record_offset)?);
    }
    let first = *words.first().ok_or(Error::InvalidField {
        what: "PSI3 empty",
        value: 0,
    })?;
    let last = *words.last().ok_or(Error::InvalidField {
        what: "PSI3 empty",
        value: 0,
    })?;
    if !(0x0200..=0x03FF).contains(&first) || last != 0x0004 {
        return Err(Error::InvalidField {
            what: "PSI3 entry/end markers",
            value: u64::from(first),
        });
    }

    Ok(Psi3Asset {
        words,
        consumed_end: declared,
    })
}

/// Symmetrically pack an [`AnpEngineAsset`] back into its exact binary representation.
pub fn pack(asset: &AnpEngineAsset) -> Result<Vec<u8>> {
    match asset {
        AnpEngineAsset::Anp(v) => Ok(v.raw.clone()),
        AnpEngineAsset::Hap(v) => Ok(v.raw.clone()),
        AnpEngineAsset::Pla(pla) => {
            let script_bytes = pla.steps.len() * 2;
            let bank_off = align16(32 + script_bytes);
            let total = bank_off + pla.palette_bytes.len();
            let mut out = Vec::with_capacity(total);
            out.extend_from_slice(MAGIC_PLA);
            out.extend_from_slice(&0x050Bu32.to_le_bytes());
            out.extend_from_slice(&pla.flags.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&(pla.steps.len() as u32).to_le_bytes());
            out.extend_from_slice(&pla.palette_count.to_le_bytes());
            out.extend_from_slice(&32u32.to_le_bytes());
            out.extend_from_slice(&(bank_off as u32).to_le_bytes());
            for step in &pla.steps {
                out.extend_from_slice(step);
            }
            out.resize(bank_off, 0);
            out.extend_from_slice(&pla.palette_bytes);
            Ok(out)
        }
        AnpEngineAsset::Psi3(psi) => {
            let total = 16 + psi.words.len() * 2;
            let mut out = Vec::with_capacity(total);
            out.extend_from_slice(MAGIC_PSI3);
            out.extend_from_slice(&(total as u32).to_le_bytes());
            out.extend_from_slice(&0u64.to_le_bytes());
            for &w in &psi.words {
                out.extend_from_slice(&w.to_le_bytes());
            }
            Ok(out)
        }
    }
}

/// Scan a GBA ROM image for `studio.anp-engine` assets (`ANP `, `HAP `, `PLA `, `PSI3`).
pub fn scan_anp_engine(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective_len = rom.len().min(MAX_ROM_SIZE);
    if effective_len < HEADER_SIZE + 20 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + 20 <= effective_len {
        let tag = &rom[off..off + 4];
        if tag == MAGIC_ANP || tag == MAGIC_HAP || tag == MAGIC_PLA || tag == MAGIC_PSI3 {
            if let Ok(asset) = parse(&rom[off..effective_len]) {
                let size = asset.consumed_end();
                found.push(DetectedFormatExtent {
                    format_id: FORMAT_ID_ANP_ENGINE,
                    subkind: asset.subkind(),
                    offset: off,
                    size,
                });
                off = align_up_4(off.saturating_add(size.max(4)));
                continue;
            }
        }
        off += 4;
    }
    found
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn make_valid_pla() -> Vec<u8> {
        pack(&AnpEngineAsset::Pla(PlaAsset {
            flags: 0x0200_0100,
            steps: vec![[0x05, 0], [0x05, 1], [0x05, 0]],
            palette_count: 2,
            palette_bytes: vec![0x00; 64],
            consumed_end: 0,
        }))
        .unwrap()
    }

    pub(crate) fn make_valid_psi3() -> Vec<u8> {
        pack(&AnpEngineAsset::Psi3(Psi3Asset {
            words: vec![0x0200, 0x0010, 0x0201, 0x0004],
            consumed_end: 0,
        }))
        .unwrap()
    }

    #[test]
    fn round_trips_pla_psi3_and_anp_v7() {
        let pla = make_valid_pla();
        assert_eq!(parse(&pla).unwrap().to_bytes().unwrap(), pla);

        let psi = make_valid_psi3();
        assert_eq!(parse(&psi).unwrap().to_bytes().unwrap(), psi);

        // Minimal ANP v0x0702: 48B header + 1 seq (2B off + 2B data) + 1 cell (2B off + 2B data) + 1 piece (4B)
        let mut anp = vec![0u8; 0x40];
        anp[0..4].copy_from_slice(MAGIC_ANP);
        anp[4..8].copy_from_slice(&0x0702u32.to_le_bytes());
        anp[0x10..0x14].copy_from_slice(&1u32.to_le_bytes()); // cA
        anp[0x14..0x18].copy_from_slice(&0x30u32.to_le_bytes()); // offA
        anp[0x18..0x1C].copy_from_slice(&1u32.to_le_bytes()); // cB
        anp[0x1C..0x20].copy_from_slice(&0x34u32.to_le_bytes()); // offB
        anp[0x20..0x24].copy_from_slice(&1u32.to_le_bytes()); // cC
        anp[0x24..0x28].copy_from_slice(&0x38u32.to_le_bytes()); // offC
        anp[0x30..0x32].copy_from_slice(&2u16.to_le_bytes()); // seq[0] = 2
        anp[0x34..0x36].copy_from_slice(&2u16.to_le_bytes()); // cell[0] = 2
        // pieces: [0x38..0x3C]
        let parsed = parse(&anp).unwrap();
        assert_eq!(parsed.consumed_end(), 0x3C);
        assert_eq!(parsed.to_bytes().unwrap(), anp[..0x3C]);
    }

    #[test]
    fn rejects_malformed_anp_engine() {
        assert!(!detect(b""));
        assert!(!detect(b"PSI3\x14\0\0\0\0\0\0\0\0\0\0\0\x00\x00\x04\x00"));
        assert!(!detect(b"PLA \x0B\x05\0\0\0\x01\0\0\0\0\0\0\0\0\0\0\0\0\0\0\x20\0\0\0\x20\0\0\0"));
    }
}
