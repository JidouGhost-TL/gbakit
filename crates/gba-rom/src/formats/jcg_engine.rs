//! `studio.jcg-engine` — `JSC\0` / `JCL\0` / `JCG\0` / `AOB\0` 2D graphics, palette, tilemap,
//! and animated-object bank parser, symmetric byte-identical writer, and ROM scanner.
//!
//! Built on `formatkit-layout` `CountedOffsetArraySpec` and symmetric `pack_counted_offset_array`.

use formatkit_core::{u16_at, u32_at, Error, Result};
use formatkit_layout::{
    pack_counted_offset_array, parse_counted_offset_array_with_count, CountedOffsetArraySpec,
    IntWidth, OffsetBase, OffsetOrderPolicy, PackPolicy,
};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_JCG_ENGINE};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_JSC0: &[u8; 4] = b"JSC\0";
const MAGIC_JCL0: &[u8; 4] = b"JCL\0";
const MAGIC_JCG0: &[u8; 4] = b"JCG\0";
const MAGIC_AOB0: &[u8; 4] = b"AOB\0";

/// Parsed `JSC\0` GBA background tilemap (`16 + width * height * 2` bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JscTilemap {
    pub reserved: u8,
    pub flags: u8,
    pub width: u8,
    pub height: u8,
    pub cells_le: Vec<u8>,
    pub consumed_end: usize,
}

/// Parsed `JCL\0` BGR555 16-color palette bank (`16 + count * 32` bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JclPaletteBank {
    pub variant: u8,
    pub count: u8,
    pub version: u32,
    pub palette_bytes: Vec<u8>,
    pub consumed_end: usize,
}

/// Parsed `JCG\0` 4bpp/8bpp tileset (`16 + count * (bpp * 8)` bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JcgTileset {
    pub aux: u16,
    pub count: u16,
    pub bpp: u16,
    pub tile_bytes: Vec<u8>,
    pub consumed_end: usize,
}

/// Parsed `AOB\0` animated-object sprite bank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AobSpriteBank {
    pub script_count: u8,
    pub sprite_count: u8,
    pub variant: u16,
    pub record_count: u8,
    pub flags: u8,
    /// Per-sprite layout records (decoded via `CountedOffsetArraySpec`).
    pub sprite_records: Vec<Vec<u8>>,
    /// Animation script region (`[A, B)`), containing `0xFF 0x02` terminator.
    pub script_bytes: Vec<u8>,
    /// BGR555 palette bank (`[B, C)`, non-zero multiple of 32 bytes).
    pub palette_bytes: Vec<u8>,
    /// 4bpp 8x8 tile bank (`[C, total)`, non-zero multiple of 32 bytes).
    pub tile_bytes: Vec<u8>,
    pub consumed_end: usize,
}

/// Unified `studio.jcg-engine` asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JcgEngineAsset {
    Jsc(JscTilemap),
    Jcl(JclPaletteBank),
    Jcg(JcgTileset),
    Aob(AobSpriteBank),
}

impl JcgEngineAsset {
    /// Exact consumed byte extent.
    #[must_use]
    pub fn consumed_end(&self) -> usize {
        match self {
            Self::Jsc(v) => v.consumed_end,
            Self::Jcl(v) => v.consumed_end,
            Self::Jcg(v) => v.consumed_end,
            Self::Aob(v) => v.consumed_end,
        }
    }

    /// Sub-format identifier.
    #[must_use]
    pub fn subkind(&self) -> &'static str {
        match self {
            Self::Jsc(_) => "jsc0",
            Self::Jcl(_) => "jcl0",
            Self::Jcg(_) => "jcg0",
            Self::Aob(_) => "aob0",
        }
    }

    /// Serialize this asset back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Fast, panic-free structural detector for `studio.jcg-engine` assets.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse any `studio.jcg-engine` asset (`JSC\0`, `JCL\0`, `JCG\0`, or `AOB\0`) at `data[0..]`.
pub fn parse(data: &[u8]) -> Result<JcgEngineAsset> {
    if data.len() < 16 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 16,
            available: data.len(),
        });
    }
    match &data[..4] {
        b"JSC\0" => parse_jsc(data).map(JcgEngineAsset::Jsc),
        b"JCL\0" => parse_jcl(data).map(JcgEngineAsset::Jcl),
        b"JCG\0" => parse_jcg(data).map(JcgEngineAsset::Jcg),
        b"AOB\0" => parse_aob(data).map(JcgEngineAsset::Aob),
        _ => Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_JSC0),
            found: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        }),
    }
}

fn parse_jsc(data: &[u8]) -> Result<JscTilemap> {
    let map_end = u32_at(data, 4)? as usize;
    let hdr_size = u32_at(data, 8)?;
    let reserved = data[12];
    let flags = data[13];
    let width = data[14];
    let height = data[15];
    if hdr_size != 16 || width == 0 || width > 128 || height == 0 || height > 128 {
        return Err(Error::InvalidField {
            what: "JSC header",
            value: u64::from(hdr_size),
        });
    }
    let expected = 16usize
        .checked_add((width as usize) * (height as usize) * 2)
        .ok_or(Error::InvalidField {
            what: "JSC map_end",
            value: map_end as u64,
        })?;
    if map_end != expected || map_end > data.len() {
        return Err(Error::InvalidField {
            what: "JSC map_end",
            value: map_end as u64,
        });
    }
    Ok(JscTilemap {
        reserved,
        flags,
        width,
        height,
        cells_le: data[16..map_end].to_vec(),
        consumed_end: map_end,
    })
}

fn parse_jcl(data: &[u8]) -> Result<JclPaletteBank> {
    let total = u32_at(data, 4)? as usize;
    let hdr_size = u16_at(data, 8)?;
    let variant = data[10];
    let count = data[11];
    let version = u32_at(data, 12)?;
    if hdr_size != 16 || count == 0 || version > 1 {
        return Err(Error::InvalidField {
            what: "JCL header",
            value: u64::from(hdr_size),
        });
    }
    let expected = 16usize + (count as usize) * 32;
    if total != expected || total > data.len() {
        return Err(Error::InvalidField {
            what: "JCL total_size",
            value: total as u64,
        });
    }
    Ok(JclPaletteBank {
        variant,
        count,
        version,
        palette_bytes: data[16..total].to_vec(),
        consumed_end: total,
    })
}

fn parse_jcg(data: &[u8]) -> Result<JcgTileset> {
    let total = u32_at(data, 4)? as usize;
    let data_off = u16_at(data, 8)?;
    let aux = u16_at(data, 10)?;
    let count = u16_at(data, 12)?;
    let bpp = u16_at(data, 14)?;
    if data_off != 16 || count == 0 || count > 8192 || !(bpp == 4 || bpp == 8) {
        return Err(Error::InvalidField {
            what: "JCG header",
            value: u64::from(bpp),
        });
    }
    let tile_size = (bpp as usize) * 8;
    let expected = 16usize
        .checked_add((count as usize) * tile_size)
        .ok_or(Error::InvalidField {
            what: "JCG total_size",
            value: total as u64,
        })?;
    if total != expected || total > data.len() {
        return Err(Error::InvalidField {
            what: "JCG total_size",
            value: total as u64,
        });
    }
    Ok(JcgTileset {
        aux,
        count,
        bpp,
        tile_bytes: data[16..total].to_vec(),
        consumed_end: total,
    })
}

fn parse_aob(data: &[u8]) -> Result<AobSpriteBank> {
    if data.len() < 24 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 24,
            available: data.len(),
        });
    }
    let total = u32_at(data, 4)? as usize;
    let tag_hdr = u16_at(data, 8)?;
    let script_count = data[10];
    let sprite_count = data[11];
    let variant = u16_at(data, 12)?;
    let record_count = data[14];
    let flags = data[15];
    let tag_sec = u16_at(data, 16)?;
    let off_a = u16_at(data, 18)? as usize;
    let off_b = u16_at(data, 20)? as usize;
    let off_c = u16_at(data, 22)? as usize;

    if tag_hdr != 0x18
        || tag_sec != 0x18
        || script_count == 0
        || sprite_count == 0
        || record_count == 0
    {
        return Err(Error::InvalidField {
            what: "AOB header",
            value: u64::from(tag_hdr),
        });
    }
    if !(24 <= off_a && off_a <= off_b && off_b <= off_c && off_c <= total && total <= data.len()) {
        return Err(Error::InvalidField {
            what: "AOB section offsets",
            value: total as u64,
        });
    }
    let pal_len = off_c - off_b;
    let tile_len = total - off_c;
    let script_len = off_b - off_a;
    if pal_len < 32
        || !pal_len.is_multiple_of(32)
        || tile_len < 32
        || !tile_len.is_multiple_of(32)
        || script_len < (script_count as usize)
    {
        return Err(Error::InvalidField {
            what: "AOB region sizes",
            value: pal_len as u64,
        });
    }
    let script_slice = &data[off_a..off_b];
    if !script_slice.windows(2).any(|w| w == [0xFF, 0x02]) {
        return Err(Error::InvalidField {
            what: "AOB script terminator",
            value: 0,
        });
    }

    let arr_spec = CountedOffsetArraySpec::new(
        24,
        sprite_count as usize,
        IntWidth::U16,
        formatkit_core::Endian::Little,
        256,
    )
    .with_offset_base(OffsetBase::Fixed(24))
    .with_order_policy(OffsetOrderPolicy::RequireMonotonic);

    let spans = parse_counted_offset_array_with_count(&data[..off_a], &arr_spec, sprite_count as usize)
        .map_err(|_| Error::InvalidField {
            what: "AOB sprite offset array",
            value: u64::from(sprite_count),
        })?;
    let dir_end = 24 + (sprite_count as usize) * 2;
    let mut sprite_records = Vec::with_capacity(spans.len());
    let mut prev_off = 0u64;
    for (idx, span) in spans.iter().enumerate() {
        if (span.offset as usize) < dir_end || (idx > 0 && span.offset <= prev_off) {
            return Err(Error::InvalidField {
                what: "AOB sprite offset strictly increasing",
                value: span.offset,
            });
        }
        prev_off = span.offset;
        let slice = span.slice(&data[..off_a]).ok_or(Error::InvalidField {
            what: "AOB sprite record slice",
            value: span.offset,
        })?;
        sprite_records.push(slice.to_vec());
    }

    Ok(AobSpriteBank {
        script_count,
        sprite_count,
        variant,
        record_count,
        flags,
        sprite_records,
        script_bytes: script_slice.to_vec(),
        palette_bytes: data[off_b..off_c].to_vec(),
        tile_bytes: data[off_c..total].to_vec(),
        consumed_end: total,
    })
}

/// Symmetrically pack a [`JcgEngineAsset`] back into its exact binary representation.
pub fn pack(asset: &JcgEngineAsset) -> Result<Vec<u8>> {
    match asset {
        JcgEngineAsset::Jsc(jsc) => {
            let total = 16usize + jsc.cells_le.len();
            let mut out = Vec::with_capacity(total);
            out.extend_from_slice(MAGIC_JSC0);
            out.extend_from_slice(&(total as u32).to_le_bytes());
            out.extend_from_slice(&16u32.to_le_bytes());
            out.push(jsc.reserved);
            out.push(jsc.flags);
            out.push(jsc.width);
            out.push(jsc.height);
            out.extend_from_slice(&jsc.cells_le);
            Ok(out)
        }
        JcgEngineAsset::Jcl(jcl) => {
            let total = 16usize + jcl.palette_bytes.len();
            let mut out = Vec::with_capacity(total);
            out.extend_from_slice(MAGIC_JCL0);
            out.extend_from_slice(&(total as u32).to_le_bytes());
            out.extend_from_slice(&16u16.to_le_bytes());
            out.push(jcl.variant);
            out.push(jcl.count);
            out.extend_from_slice(&jcl.version.to_le_bytes());
            out.extend_from_slice(&jcl.palette_bytes);
            Ok(out)
        }
        JcgEngineAsset::Jcg(jcg) => {
            let total = 16usize + jcg.tile_bytes.len();
            let mut out = Vec::with_capacity(total);
            out.extend_from_slice(MAGIC_JCG0);
            out.extend_from_slice(&(total as u32).to_le_bytes());
            out.extend_from_slice(&16u16.to_le_bytes());
            out.extend_from_slice(&jcg.aux.to_le_bytes());
            out.extend_from_slice(&jcg.count.to_le_bytes());
            out.extend_from_slice(&jcg.bpp.to_le_bytes());
            out.extend_from_slice(&jcg.tile_bytes);
            Ok(out)
        }
        JcgEngineAsset::Aob(aob) => {
            let payloads: Vec<&[u8]> = aob.sprite_records.iter().map(Vec::as_slice).collect();
            let arr_spec = CountedOffsetArraySpec::new(
                24,
                payloads.len(),
                IntWidth::U16,
                formatkit_core::Endian::Little,
                256,
            )
            .with_offset_base(OffsetBase::Fixed(24))
            .with_order_policy(OffsetOrderPolicy::RequireMonotonic);

            let prefix_placeholder = [0u8; 24];
            let mut out = pack_counted_offset_array(
                &arr_spec,
                &PackPolicy::TIGHT,
                &prefix_placeholder,
                &payloads,
            )
            .map_err(|_| Error::InvalidField {
                what: "AOB pack sprite array",
                value: payloads.len() as u64,
            })?;
            let off_a = u16::try_from(out.len()).map_err(|_| Error::InvalidField {
                what: "AOB off_a",
                value: out.len() as u64,
            })?;
            out.extend_from_slice(&aob.script_bytes);
            let off_b = u16::try_from(out.len()).map_err(|_| Error::InvalidField {
                what: "AOB off_b",
                value: out.len() as u64,
            })?;
            out.extend_from_slice(&aob.palette_bytes);
            let off_c = u16::try_from(out.len()).map_err(|_| Error::InvalidField {
                what: "AOB off_c",
                value: out.len() as u64,
            })?;
            out.extend_from_slice(&aob.tile_bytes);
            let total = u32::try_from(out.len()).map_err(|_| Error::InvalidField {
                what: "AOB total",
                value: out.len() as u64,
            })?;

            out[0..4].copy_from_slice(MAGIC_AOB0);
            out[4..8].copy_from_slice(&total.to_le_bytes());
            out[8..10].copy_from_slice(&0x0018u16.to_le_bytes());
            out[10] = aob.script_count;
            out[11] = aob.sprite_count;
            out[12..14].copy_from_slice(&aob.variant.to_le_bytes());
            out[14] = aob.record_count;
            out[15] = aob.flags;
            out[16..18].copy_from_slice(&0x0018u16.to_le_bytes());
            out[18..20].copy_from_slice(&off_a.to_le_bytes());
            out[20..22].copy_from_slice(&off_b.to_le_bytes());
            out[22..24].copy_from_slice(&off_c.to_le_bytes());
            Ok(out)
        }
    }
}

/// Scan a GBA ROM image for `studio.jcg-engine` assets (`JSC\0`, `JCL\0`, `JCG\0`, `AOB\0`).
pub fn scan_jcg_engine(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective_len = rom.len().min(MAX_ROM_SIZE);
    if effective_len < HEADER_SIZE + 16 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + 16 <= effective_len {
        let tag = &rom[off..off + 4];
        if tag == MAGIC_JSC0 || tag == MAGIC_JCL0 || tag == MAGIC_JCG0 || tag == MAGIC_AOB0 {
            if let Ok(asset) = parse(&rom[off..effective_len]) {
                let size = asset.consumed_end();
                found.push(DetectedFormatExtent {
                    format_id: FORMAT_ID_JCG_ENGINE,
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

    pub(crate) fn make_valid_jsc0() -> Vec<u8> {
        pack(&JcgEngineAsset::Jsc(JscTilemap {
            reserved: 0,
            flags: 0,
            width: 8,
            height: 4,
            cells_le: vec![0x01; 64],
            consumed_end: 80,
        }))
        .unwrap()
    }

    #[test]
    fn round_trips_jsc_jcl_jcg_and_aob() {
        let jsc = make_valid_jsc0();
        assert_eq!(parse(&jsc).unwrap().to_bytes().unwrap(), jsc);

        let jcl = pack(&JcgEngineAsset::Jcl(JclPaletteBank {
            variant: 0,
            count: 2,
            version: 1,
            palette_bytes: vec![0x1F; 64],
            consumed_end: 80,
        }))
        .unwrap();
        assert_eq!(parse(&jcl).unwrap().to_bytes().unwrap(), jcl);

        let jcg = pack(&JcgEngineAsset::Jcg(JcgTileset {
            aux: 0,
            count: 4,
            bpp: 4,
            tile_bytes: vec![0x22; 128],
            consumed_end: 144,
        }))
        .unwrap();
        assert_eq!(parse(&jcg).unwrap().to_bytes().unwrap(), jcg);

        let aob = pack(&JcgEngineAsset::Aob(AobSpriteBank {
            script_count: 1,
            sprite_count: 2,
            variant: 0x0401,
            record_count: 2,
            flags: 0,
            sprite_records: vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]],
            script_bytes: vec![0x00, 0xFF, 0x02, 0x00],
            palette_bytes: vec![0x00; 32],
            tile_bytes: vec![0x11; 64],
            consumed_end: 0,
        }))
        .unwrap();
        assert_eq!(parse(&aob).unwrap().to_bytes().unwrap(), aob);
    }

    #[test]
    fn rejects_malformed_jcg_engine() {
        assert!(!detect(b""));
        assert!(!detect(b"JSC\0\x10\0\0\0\x10\0\0\0\0\0\0\0"));
        assert!(!detect(b"JCG\0\x30\0\0\0\x10\0\0\0\x01\0\x03\0"));
    }
}
