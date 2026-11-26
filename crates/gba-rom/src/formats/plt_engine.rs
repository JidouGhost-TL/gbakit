//! Unified `PLT\0` / `IMG\0` / `PAT\0` (+ `TIM\0`) / `SCR\0` / `TON\0` studio asset engine.
//!
//! Consolidates five sibling asset types from the same GBA studio toolchain:
//! - **`PLT\0`**: 8-byte header (`PLT\0` + `u32 LE tag == 16`) + 20 `u16 LE` BGR555 palette words
//!   (48 bytes total).
//! - **`IMG\0`**: 8-byte header (`IMG\0` + `u16 BE width` + `u16 BE height`) followed by 4bpp pixel
//!   bytes with embedded `ECD\x01` (16-byte) and `PLT\0` palette chunks.
//! - **`PAT\0` (+ optional `TIM\0`)**: Counted `u32 LE` offset-table archive parsed via
//!   [`CountedOffsetArraySpec`] (`zero_count_implicit_blob`), optionally followed by an embedded
//!   `TIM\0` offset-table section.
//! - **`SCR\0`**: 8-byte header (`SCR\0` + `0x00` + `w: u8` + `h: u8` + `0x00`) + `w * h`
//!   little-endian `u16` GBA BG screen entries (`8 + 2 * w * h` bytes total).
//! - **`TON\0`**: 8-byte header (`TON\0` + `reserved == 0`) + `u16 LE track_count` +
//!   `u16[track_count]` offsets relative to offset `8` parsed via [`CountedOffsetArraySpec`],
//!   each pointing to a `(note_count: u16, track_id: u16, notes: [(u16, u16); note_count])` track.

use formatkit_core::{u16_at, u32_at, Endian, Error, Result};
use formatkit_layout::{
    parse_counted_offset_array, CountedOffsetArraySpec, IntWidth, OffsetBase, OffsetOrderPolicy,
    OffsetSpan,
};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_PLT_ENGINE};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_PLT: &[u8; 4] = b"PLT\0";
const MAGIC_IMG: &[u8; 4] = b"IMG\0";
const MAGIC_PAT: &[u8; 4] = b"PAT\0";
const MAGIC_TIM: &[u8; 4] = b"TIM\0";
const MAGIC_SCR: &[u8; 4] = b"SCR\0";
const MAGIC_TON: &[u8; 4] = b"TON\0";
const MAGIC_ECD: &[u8; 4] = b"ECD\x01";

const PLT0_TOTAL_LEN: usize = 48;
const PLT0_TAG_EXPECTED: u32 = 16;
const IMG0_MAX_DIM: u16 = 2048;
const IMG0_MAX_PIXELS: usize = 8_388_608;
const IMG0_SCAN_WINDOW: usize = 16 * 1024;
const PAT0_MAX_ENTRIES: usize = 8192;
const PAT0_TIM_SEARCH_LIMIT: usize = 32 * 1024;
const SCR0_MAX_DIM: u8 = 64;
const TON0_MAX_TRACKS: usize = 512;
const TON0_MAX_NOTES_PER_TRACK: usize = 8192;

/// Parsed `PLT\0` 20-color BGR555 palette table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PltAsset {
    pub tag: u32,
    pub colors_bgr555: [u16; 20],
    pub consumed_end: usize,
}

/// Embedded `PLT\0` palette chunk inside an `IMG\0` asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImgPaletteSpan {
    pub offset: usize,
    pub color_count: usize,
    pub total_size: usize,
}

/// Parsed `IMG\0` 4bpp paletted image container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImgAsset {
    pub width: u16,
    pub height: u16,
    pub ecd_chunk_count: usize,
    pub palettes: Vec<ImgPaletteSpan>,
    pub consumed_end: usize,
}

/// Parsed `PAT\0` (+ optional `TIM\0`) offset-table archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatAsset {
    pub pat_count: usize,
    pub pat_entries: Vec<OffsetSpan>,
    pub tim_offset: Option<usize>,
    pub tim_count: usize,
    pub tim_entries: Vec<OffsetSpan>,
    pub consumed_end: usize,
}

/// Parsed `SCR\0` GBA BG screenblock tilemap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrAsset {
    pub width_tiles: u8,
    pub height_tiles: u8,
    pub entry_count: usize,
    pub consumed_end: usize,
}

/// Metadata for one parsed track inside a `TON\0` sequenced-music container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TonTrackInfo {
    pub offset: usize,
    pub note_count: u16,
    pub track_id: u16,
    pub byte_size: usize,
}

/// Parsed `TON\0` multi-track note sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TonAsset {
    pub tracks: Vec<TonTrackInfo>,
    pub total_notes: usize,
    pub consumed_end: usize,
}

/// Unified parsed asset from the `studio.plt-engine` family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PltEngineAsset {
    Plt(PltAsset),
    Img(ImgAsset),
    Pat(PatAsset),
    Scr(ScrAsset),
    Ton(TonAsset),
}

impl PltEngineAsset {
    /// Exact consumed byte extent from the start of the parsed slice.
    pub const fn consumed_end(&self) -> usize {
        match self {
            Self::Plt(a) => a.consumed_end,
            Self::Img(a) => a.consumed_end,
            Self::Pat(a) => a.consumed_end,
            Self::Scr(a) => a.consumed_end,
            Self::Ton(a) => a.consumed_end,
        }
    }

    /// Sub-format identifier tag.
    pub const fn subkind(&self) -> &'static str {
        match self {
            Self::Plt(_) => "plt0",
            Self::Img(_) => "img0",
            Self::Pat(_) => "pat0",
            Self::Scr(_) => "scr0",
            Self::Ton(_) => "ton0",
        }
    }
}

/// Returns `true` if `data` begins with a valid `PLT\0`, `IMG\0`, `PAT\0`, `SCR\0`, or `TON\0` asset.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse a `PLT\0`, `IMG\0`, `PAT\0`, `SCR\0`, or `TON\0` asset from the start of `data`.
pub fn parse(data: &[u8]) -> Result<PltEngineAsset> {
    let Some(magic) = data.get(..4) else {
        return Err(Error::Truncated {
            offset: 0,
            needed: 4,
            available: data.len(),
        });
    };
    if magic == MAGIC_PLT {
        parse_plt0(data).map(PltEngineAsset::Plt)
    } else if magic == MAGIC_IMG {
        parse_img0(data).map(PltEngineAsset::Img)
    } else if magic == MAGIC_PAT {
        parse_pat0(data, false).map(PltEngineAsset::Pat)
    } else if magic == MAGIC_SCR {
        parse_scr0(data).map(PltEngineAsset::Scr)
    } else if magic == MAGIC_TON {
        parse_ton0(data).map(PltEngineAsset::Ton)
    } else {
        let found = u32::from_be_bytes([magic[0], magic[1], magic[2], magic[3]]);
        Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_PLT),
            found,
        })
    }
}

fn parse_plt0(data: &[u8]) -> Result<PltAsset> {
    if data.len() < PLT0_TOTAL_LEN {
        return Err(Error::Truncated {
            offset: 0,
            needed: PLT0_TOTAL_LEN,
            available: data.len(),
        });
    }
    let tag = u32_at(data, 4)?;
    if tag != PLT0_TAG_EXPECTED {
        return Err(Error::InvalidField {
            what: "PLT0 header tag",
            value: u64::from(tag),
        });
    }
    let mut colors_bgr555 = [0u16; 20];
    for (i, slot) in colors_bgr555.iter_mut().enumerate() {
        let off = 8usize
            .checked_add(i.checked_mul(2).ok_or(Error::InvalidField {
                what: "PLT0 color offset",
                value: i as u64,
            })?)
            .ok_or(Error::InvalidField {
                what: "PLT0 color offset",
                value: i as u64,
            })?;
        *slot = u16_at(data, off)?;
    }
    Ok(PltAsset {
        tag,
        colors_bgr555,
        consumed_end: PLT0_TOTAL_LEN,
    })
}

fn parse_img0(data: &[u8]) -> Result<ImgAsset> {
    if data.len() < 16 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 16,
            available: data.len(),
        });
    }
    let width = u16::from_be_bytes([data[4], data[5]]);
    let height = u16::from_be_bytes([data[6], data[7]]);
    if width == 0 || height == 0 || width > IMG0_MAX_DIM || height > IMG0_MAX_DIM {
        return Err(Error::InvalidField {
            what: "IMG0 dimensions",
            value: ((u64::from(width)) << 16) | u64::from(height),
        });
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .filter(|&p| p <= IMG0_MAX_PIXELS)
        .ok_or(Error::InvalidField {
            what: "IMG0 pixel count",
            value: u64::from(width),
        })?;
    let _ = pixels;

    // Scan a bounded window for embedded ECD\x01 (16B) and PLT\0 (8 + 2*count) chunks.
    // Stop before any sibling top-level magic (`IMG\0`, `PAT\0`, `SCR\0`, `TON\0`) so ROM scans
    // never bleed into adjacent assets.
    let scan_limit = data.len().min(IMG0_SCAN_WINDOW);
    let mut pos = 8usize;
    let mut ecd_chunk_count = 0usize;
    let mut palettes = Vec::new();
    let mut last_chunk_end = 8usize;

    while pos.checked_add(4).is_some_and(|e| e <= scan_limit) {
        let tag = &data[pos..pos + 4];
        if tag == MAGIC_IMG || tag == MAGIC_PAT || tag == MAGIC_SCR || tag == MAGIC_TON {
            break;
        }
        if tag == MAGIC_ECD {
            let Some(end) = pos.checked_add(16) else {
                break;
            };
            if end > data.len() {
                return Err(Error::Truncated {
                    offset: pos,
                    needed: 16,
                    available: data.len() - pos,
                });
            }
            ecd_chunk_count += 1;
            last_chunk_end = end;
            pos = end;
            continue;
        }
        if tag == MAGIC_PLT {
            if let Ok(count) = u32_at(data, pos + 4) {
                if (1..=256).contains(&count) {
                    let payload_bytes = (count as usize).checked_mul(2).ok_or(Error::InvalidField {
                        what: "IMG0 PLT chunk size",
                        value: u64::from(count),
                    })?;
                    let total_size = 8usize
                        .checked_add(payload_bytes)
                        .ok_or(Error::InvalidField {
                            what: "IMG0 PLT chunk size",
                            value: payload_bytes as u64,
                        })?;
                    let end = pos.checked_add(total_size).ok_or(Error::InvalidField {
                        what: "IMG0 PLT chunk end",
                        value: pos as u64,
                    })?;
                    if end > data.len() {
                        return Err(Error::Truncated {
                            offset: pos,
                            needed: total_size,
                            available: data.len() - pos,
                        });
                    }
                    palettes.push(ImgPaletteSpan {
                        offset: pos,
                        color_count: count as usize,
                        total_size,
                    });
                    last_chunk_end = end;
                    pos = end;
                    continue;
                }
            }
        }
        pos += 1;
    }

    if palettes.is_empty() {
        return Err(Error::InvalidField {
            what: "IMG0 palette chunk count",
            value: 0,
        });
    }

    Ok(ImgAsset {
        width,
        height,
        ecd_chunk_count,
        palettes,
        consumed_end: last_chunk_end,
    })
}

fn parse_pat0(data: &[u8], rom_scan_mode: bool) -> Result<PatAsset> {
    if data.len() < 8 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 8,
            available: data.len(),
        });
    }
    let pat_count_u32 = u32_at(data, 4)?;
    let pat_count = pat_count_u32 as usize;
    if pat_count > PAT0_MAX_ENTRIES {
        return Err(Error::InvalidField {
            what: "PAT0 entry count",
            value: u64::from(pat_count_u32),
        });
    }
    let pat_table_bytes = pat_count.checked_mul(4).ok_or(Error::InvalidField {
        what: "PAT0 table size",
        value: pat_count as u64,
    })?;
    let pat_header_end = 8usize
        .checked_add(pat_table_bytes)
        .ok_or(Error::InvalidField {
            what: "PAT0 header end",
            value: pat_table_bytes as u64,
        })?;
    if pat_header_end > data.len() {
        return Err(Error::Truncated {
            offset: 0,
            needed: pat_header_end,
            available: data.len(),
        });
    }

    // Look for an optional TIM\0 section at/after pat_header_end (bounded during ROM scan).
    let search_end = if rom_scan_mode {
        data.len().min(pat_header_end.saturating_add(PAT0_TIM_SEARCH_LIMIT))
    } else {
        data.len()
    };
    let tim_offset = find_tim_marker(data, pat_header_end, search_end);

    // During ROM scans, reject count == 0 unless paired with a valid TIM\0 section within 512 bytes
    // to keep false positives strictly zero.
    if rom_scan_mode && pat_count == 0 {
        match tim_offset {
            Some(off) if off.saturating_sub(pat_header_end) <= 512 => {}
            _ => {
                return Err(Error::InvalidField {
                    what: "PAT0 count=0 without nearby TIM0",
                    value: 0,
                });
            }
        }
    }

    let pat_section_end = tim_offset.unwrap_or_else(|| {
        if rom_scan_mode {
            // Bound standalone PAT\0 without TIM\0 to a plausible extent if scanning a full ROM.
            compute_bounded_pat_end(data, pat_header_end, pat_count)
        } else {
            data.len()
        }
    });
    if pat_section_end < pat_header_end || pat_section_end > data.len() {
        return Err(Error::InvalidField {
            what: "PAT0 section end",
            value: pat_section_end as u64,
        });
    }

    let pat_spec = CountedOffsetArraySpec::new(
        8,
        pat_count,
        IntWidth::U32,
        Endian::Little,
        PAT0_MAX_ENTRIES,
    )
    .with_offset_base(OffsetBase::Absolute)
    .with_order_policy(OffsetOrderPolicy::RequireMonotonic)
    .with_zero_count_implicit_blob(true);

    let pat_entries = parse_counted_offset_array(&data[..pat_section_end], &pat_spec)
        .map_err(|_| Error::InvalidField {
            what: "PAT0 offset table",
            value: pat_count as u64,
        })?;

    // Every PAT entry must start at or after pat_header_end.
    for entry in &pat_entries {
        if (entry.offset as usize) < pat_header_end {
            return Err(Error::InvalidField {
                what: "PAT0 offset inside header",
                value: entry.offset,
            });
        }
    }
    if pat_count > 0 && pat_entries.first().is_some_and(|e| e.offset as usize != pat_header_end) {
        return Err(Error::InvalidField {
            what: "PAT0 first offset",
            value: pat_entries.first().map_or(0, |e| e.offset),
        });
    }

    let (tim_count, tim_entries, consumed_end) = if let Some(tim_off) = tim_offset {
        let tim_slice = &data[tim_off..];
        if tim_slice.len() < 8 {
            return Err(Error::Truncated {
                offset: tim_off,
                needed: 8,
                available: tim_slice.len(),
            });
        }
        let tc_u32 = u32_at(tim_slice, 4)?;
        let tc = tc_u32 as usize;
        if tc > PAT0_MAX_ENTRIES {
            return Err(Error::InvalidField {
                what: "TIM0 entry count",
                value: u64::from(tc_u32),
            });
        }
        let tim_header_end = tc
            .checked_mul(4)
            .and_then(|b| b.checked_add(8))
            .ok_or(Error::InvalidField {
                what: "TIM0 header end",
                value: tc as u64,
            })?;
        if tim_header_end > tim_slice.len() {
            return Err(Error::Truncated {
                offset: tim_off,
                needed: tim_header_end,
                available: tim_slice.len(),
            });
        }
        let tim_local_end = if rom_scan_mode {
            compute_bounded_tim_end(tim_slice, tim_header_end, tc)
        } else {
            tim_slice.len()
        };
        let tim_spec = CountedOffsetArraySpec::new(
            8,
            tc,
            IntWidth::U32,
            Endian::Little,
            PAT0_MAX_ENTRIES,
        )
        .with_offset_base(OffsetBase::Absolute)
        .with_order_policy(OffsetOrderPolicy::RequireMonotonic)
        .with_zero_count_implicit_blob(true);
        let t_entries = parse_counted_offset_array(&tim_slice[..tim_local_end], &tim_spec)
            .map_err(|_| Error::InvalidField {
                what: "TIM0 offset table",
                value: tc as u64,
            })?;
        for entry in &t_entries {
            if (entry.offset as usize) < tim_header_end {
                return Err(Error::InvalidField {
                    what: "TIM0 offset inside header",
                    value: entry.offset,
                });
            }
        }
        if tc > 0 && t_entries.first().is_some_and(|e| e.offset as usize != tim_header_end) {
            return Err(Error::InvalidField {
                what: "TIM0 first offset",
                value: t_entries.first().map_or(0, |e| e.offset),
            });
        }
        let end = tim_off
            .checked_add(tim_local_end)
            .ok_or(Error::InvalidField {
                what: "TIM0 end offset",
                value: tim_off as u64,
            })?;
        (tc, t_entries, end)
    } else {
        (0, Vec::new(), pat_section_end)
    };

    Ok(PatAsset {
        pat_count,
        pat_entries,
        tim_offset,
        tim_count,
        tim_entries,
        consumed_end,
    })
}

fn find_tim_marker(data: &[u8], start: usize, end: usize) -> Option<usize> {
    let slice = data.get(start..end)?;
    let mut pos = 0usize;
    while pos + 4 <= slice.len() {
        let tag = &slice[pos..pos + 4];
        if tag == MAGIC_TIM {
            return Some(start + pos);
        }
        // Do not cross into a new top-level sibling asset.
        if pos > 0
            && (tag == MAGIC_PAT
                || tag == MAGIC_IMG
                || tag == MAGIC_PLT
                || tag == MAGIC_SCR
                || tag == MAGIC_TON)
        {
            return None;
        }
        pos += 4;
    }
    None
}

fn compute_bounded_pat_end(data: &[u8], pat_header_end: usize, pat_count: usize) -> usize {
    // Use the last table offset + estimated stride (or 4 bytes) when scanning a raw ROM without TIM\0.
    if pat_count == 0 {
        return pat_header_end;
    }
    let last_off_pos = 8usize.saturating_add(pat_count.saturating_sub(1).saturating_mul(4));
    let last_off = u32_at(data, last_off_pos).unwrap_or(0) as usize;
    let stride = if pat_count >= 2 {
        let prev_off = u32_at(data, last_off_pos - 4).unwrap_or(0) as usize;
        last_off.saturating_sub(prev_off).clamp(4, 4096)
    } else {
        16
    };
    last_off
        .saturating_add(stride)
        .max(pat_header_end)
        .min(data.len())
}

fn compute_bounded_tim_end(tim_slice: &[u8], tim_header_end: usize, tim_count: usize) -> usize {
    // Find the nearest sibling magic or use last entry offset + bounded tail.
    let limit = tim_slice.len().min(4096);
    let mut pos = align_up_4(tim_header_end);
    while pos + 4 <= limit {
        let tag = &tim_slice[pos..pos + 4];
        if tag == MAGIC_PAT
            || tag == MAGIC_IMG
            || tag == MAGIC_PLT
            || tag == MAGIC_SCR
            || tag == MAGIC_TON
            || tag == MAGIC_TIM
        {
            return pos;
        }
        pos += 4;
    }
    if tim_count == 0 {
        return tim_header_end.saturating_add(8).min(tim_slice.len());
    }
    let last_off_pos = 8usize.saturating_add(tim_count.saturating_sub(1).saturating_mul(4));
    let last_off = u32_at(tim_slice, last_off_pos).unwrap_or(0) as usize;
    last_off
        .saturating_add(16)
        .max(tim_header_end)
        .min(tim_slice.len())
}

fn parse_scr0(data: &[u8]) -> Result<ScrAsset> {
    if data.len() < 8 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 8,
            available: data.len(),
        });
    }
    let reserved0 = data[4];
    let width_tiles = data[5];
    let height_tiles = data[6];
    let reserved1 = data[7];
    if reserved0 != 0 || reserved1 != 0 {
        return Err(Error::InvalidField {
            what: "SCR0 reserved bytes",
            value: u64::from(reserved0) | (u64::from(reserved1) << 8),
        });
    }
    if width_tiles == 0
        || height_tiles == 0
        || width_tiles > SCR0_MAX_DIM
        || height_tiles > SCR0_MAX_DIM
    {
        return Err(Error::InvalidField {
            what: "SCR0 tile dimensions",
            value: (u64::from(width_tiles) << 8) | u64::from(height_tiles),
        });
    }
    let entry_count = (width_tiles as usize)
        .checked_mul(height_tiles as usize)
        .ok_or(Error::InvalidField {
            what: "SCR0 entry count",
            value: u64::from(width_tiles),
        })?;
    let body_bytes = entry_count.checked_mul(2).ok_or(Error::InvalidField {
        what: "SCR0 body bytes",
        value: entry_count as u64,
    })?;
    let consumed_end = 8usize.checked_add(body_bytes).ok_or(Error::InvalidField {
        what: "SCR0 total size",
        value: body_bytes as u64,
    })?;
    if consumed_end > data.len() {
        return Err(Error::Truncated {
            offset: 0,
            needed: consumed_end,
            available: data.len(),
        });
    }
    Ok(ScrAsset {
        width_tiles,
        height_tiles,
        entry_count,
        consumed_end,
    })
}

fn parse_ton0(data: &[u8]) -> Result<TonAsset> {
    if data.len() < 10 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 10,
            available: data.len(),
        });
    }
    let reserved = u32_at(data, 4)?;
    if reserved != 0 {
        return Err(Error::InvalidField {
            what: "TON0 reserved word",
            value: u64::from(reserved),
        });
    }
    let track_count_u16 = u16_at(data, 8)?;
    let track_count = track_count_u16 as usize;
    if track_count == 0 || track_count > TON0_MAX_TRACKS {
        return Err(Error::InvalidField {
            what: "TON0 track count",
            value: u64::from(track_count_u16),
        });
    }
    let table_end = track_count
        .checked_mul(2)
        .and_then(|b| b.checked_add(10))
        .ok_or(Error::InvalidField {
            what: "TON0 table end",
            value: track_count as u64,
        })?;
    if table_end > data.len() {
        return Err(Error::Truncated {
            offset: 0,
            needed: table_end,
            available: data.len(),
        });
    }

    let spec = CountedOffsetArraySpec::new(
        10,
        track_count,
        IntWidth::U16,
        Endian::Little,
        TON0_MAX_TRACKS,
    )
    .with_offset_base(OffsetBase::Fixed(8))
    .with_order_policy(OffsetOrderPolicy::RequireMonotonic);

    let spans = parse_counted_offset_array(data, &spec).map_err(|_| Error::InvalidField {
        what: "TON0 offset table",
        value: track_count as u64,
    })?;

    if spans.first().is_none_or(|s| s.offset as usize != table_end) {
        return Err(Error::InvalidField {
            what: "TON0 first track offset",
            value: spans.first().map_or(0, |s| s.offset),
        });
    }

    let mut tracks = Vec::with_capacity(track_count);
    let mut total_notes = 0usize;
    let mut consumed_end = table_end;

    for (i, span) in spans.iter().enumerate() {
        let off = span.offset as usize;
        if (off & 1) != 0 {
            return Err(Error::InvalidField {
                what: "TON0 track alignment",
                value: off as u64,
            });
        }
        if i > 0 && off <= spans[i - 1].offset as usize {
            return Err(Error::InvalidField {
                what: "TON0 strictly increasing track offsets",
                value: off as u64,
            });
        }
        if off.checked_add(4).is_none_or(|e| e > data.len()) {
            return Err(Error::Truncated {
                offset: off,
                needed: 4,
                available: data.len().saturating_sub(off),
            });
        }
        let note_count = u16_at(data, off)?;
        let track_id = u16_at(data, off + 2)?;
        if note_count as usize > TON0_MAX_NOTES_PER_TRACK {
            return Err(Error::InvalidField {
                what: "TON0 note count",
                value: u64::from(note_count),
            });
        }
        let byte_size = (note_count as usize)
            .checked_mul(4)
            .and_then(|b| b.checked_add(4))
            .ok_or(Error::InvalidField {
                what: "TON0 track byte size",
                value: u64::from(note_count),
            })?;
        let track_end = off.checked_add(byte_size).ok_or(Error::InvalidField {
            what: "TON0 track end",
            value: off as u64,
        })?;
        if track_end > data.len() {
            return Err(Error::Truncated {
                offset: off,
                needed: byte_size,
                available: data.len().saturating_sub(off),
            });
        }
        if let Some(next_span) = spans.get(i + 1) {
            if track_end > next_span.offset as usize {
                return Err(Error::InvalidField {
                    what: "TON0 overlapping track",
                    value: track_end as u64,
                });
            }
        }
        total_notes = total_notes
            .checked_add(note_count as usize)
            .ok_or(Error::InvalidField {
                what: "TON0 total notes",
                value: total_notes as u64,
            })?;
        tracks.push(TonTrackInfo {
            offset: off,
            note_count,
            track_id,
            byte_size,
        });
        consumed_end = track_end;
    }

    Ok(TonAsset {
        tracks,
        total_notes,
        consumed_end,
    })
}

/// Scan `rom` on 4-byte aligned offsets for verified `studio.plt-engine` assets.
pub fn scan_plt_engine(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective = &rom[..rom.len().min(MAX_ROM_SIZE)];
    if effective.len() < HEADER_SIZE + 8 {
        return Vec::new();
    }

    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + 8 <= effective.len() {
        let tag = &effective[off..off + 4];
        let parsed = if tag == MAGIC_PLT {
            parse_plt0(&effective[off..]).map(PltEngineAsset::Plt)
        } else if tag == MAGIC_IMG {
            parse_img0(&effective[off..]).map(PltEngineAsset::Img)
        } else if tag == MAGIC_PAT {
            parse_pat0(&effective[off..], true).map(PltEngineAsset::Pat)
        } else if tag == MAGIC_SCR {
            parse_scr0(&effective[off..]).map(PltEngineAsset::Scr)
        } else if tag == MAGIC_TON {
            parse_ton0(&effective[off..]).map(PltEngineAsset::Ton)
        } else {
            off += 4;
            continue;
        };

        if let Ok(asset) = parsed {
            let size = asset.consumed_end();
            if size > 0 && off.checked_add(size).is_some_and(|e| e <= effective.len()) {
                found.push(DetectedFormatExtent {
                    format_id: FORMAT_ID_PLT_ENGINE,
                    subkind: asset.subkind(),
                    offset: off,
                    size,
                });
                off = align_up_4(off.saturating_add(size)).max(off + 4);
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

    pub(crate) fn make_valid_plt0() -> Vec<u8> {
        let mut out = Vec::with_capacity(48);
        out.extend_from_slice(MAGIC_PLT);
        out.extend_from_slice(&16u32.to_le_bytes());
        for i in 0u16..20 {
            out.extend_from_slice(&(i * 0x0421).to_le_bytes());
        }
        out
    }

    fn make_valid_img0() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC_IMG);
        out.extend_from_slice(&32u16.to_be_bytes());
        out.extend_from_slice(&64u16.to_be_bytes());
        out.extend_from_slice(&[0x12u8; 32]);
        // Embedded ECD\x01 chunk (16 bytes)
        out.extend_from_slice(MAGIC_ECD);
        out.extend_from_slice(&[0u8; 12]);
        // Embedded PLT\0 chunk (16 colors = 40 bytes)
        out.extend_from_slice(MAGIC_PLT);
        out.extend_from_slice(&16u32.to_le_bytes());
        for i in 0u16..16 {
            out.extend_from_slice(&(i * 0x0421).to_le_bytes());
        }
        out
    }

    fn make_valid_pat0_with_tim0() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC_PAT);
        out.extend_from_slice(&2u32.to_le_bytes());
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&24u32.to_le_bytes());
        out.extend_from_slice(&[0xAA; 8]);
        out.extend_from_slice(&[0xBB; 8]);
        // TIM\0 section at offset 32
        out.extend_from_slice(MAGIC_TIM);
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&12u32.to_le_bytes());
        out.extend_from_slice(&[0xCC; 16]);
        out
    }

    fn make_valid_scr0() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC_SCR);
        out.extend_from_slice(&[0, 5, 4, 0]); // 5x4 = 20 entries = 40 bytes
        for i in 0u16..20 {
            out.extend_from_slice(&i.to_le_bytes());
        }
        out
    }

    fn make_valid_ton0() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC_TON);
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes()); // 2 tracks
        // table_end = 10 + 4 = 14 -> rel offset = 14 - 8 = 6
        out.extend_from_slice(&6u16.to_le_bytes());
        // track 0 size = 4 + 2*4 = 12 -> track 1 at 26 -> rel offset = 18
        out.extend_from_slice(&18u16.to_le_bytes());
        // Track 0: 2 notes, id=1
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&[4, 0, 0x20, 0x01, 4, 0, 0x30, 0x01]);
        // Track 1: 1 note, id=2
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&[8, 0, 0x40, 0x02]);
        // Optional terminator + trailing ROM bytes
        out.extend_from_slice(&[0, 0, 0xFF, 0xFF]);
        out
    }

    #[test]
    fn parses_all_five_plt_engine_subformats() {
        let plt = make_valid_plt0();
        assert!(detect(&plt));
        let p = parse(&plt).unwrap();
        assert_eq!(p.subkind(), "plt0");
        assert_eq!(p.consumed_end(), 48);

        let img = make_valid_img0();
        assert!(detect(&img));
        let i = parse(&img).unwrap();
        assert_eq!(i.subkind(), "img0");
        assert_eq!(i.consumed_end(), img.len());

        let pat = make_valid_pat0_with_tim0();
        assert!(detect(&pat));
        let pa = parse(&pat).unwrap();
        assert_eq!(pa.subkind(), "pat0");
        assert_eq!(pa.consumed_end(), pat.len());

        let scr = make_valid_scr0();
        assert!(detect(&scr));
        let s = parse(&scr).unwrap();
        assert_eq!(s.subkind(), "scr0");
        assert_eq!(s.consumed_end(), 48);

        let ton = make_valid_ton0();
        assert!(detect(&ton));
        let t = parse(&ton).unwrap();
        assert_eq!(t.subkind(), "ton0");
        assert_eq!(t.consumed_end(), 34);
    }

    #[test]
    fn rejects_malformed_truncated_and_random_inputs() {
        assert!(!detect(&[]));
        assert!(!detect(&make_valid_plt0()[..47]));

        let mut bad_plt = make_valid_plt0();
        bad_plt[4] = 15;
        assert!(!detect(&bad_plt));

        let mut bad_scr = make_valid_scr0();
        bad_scr[4] = 1; // non-zero reserved byte
        assert!(!detect(&bad_scr));
        assert!(!detect(&make_valid_scr0()[..20]));

        let mut bad_ton = make_valid_ton0();
        bad_ton[4] = 1; // non-zero reserved word
        assert!(!detect(&bad_ton));
        assert!(!detect(&make_valid_ton0()[..20]));

        // Deterministic pseudo-random negative probes
        let mut seed = 0x1234_5678u64;
        for _ in 0..200 {
            let mut buf = [0u8; 128];
            for b in &mut buf {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                *b = (seed >> 33) as u8;
            }
            assert!(!detect(&buf));
        }
    }
}
