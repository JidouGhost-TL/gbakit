//! `studio.marker-bundles` — Marker-delimited multi-member GBA studio bundles
//! (`AGBJ`, `D233`, `CBA\0`, `MLXP`, `CNML`) backed by `formatkit-layout`
//! `MarkerScanStreamSpec::with_min_entry_size` and symmetric `pack_marker_scan_stream`.

use formatkit_core::{u32_at, Error, Result};
use formatkit_layout::{
    pack_marker_scan_stream, parse_marker_scan_stream, MarkerScanStreamSpec, PackPolicy,
};

use super::{align_up_4, DetectedFormatExtent, FORMAT_ID_MARKER_BUNDLES};
use crate::{HEADER_SIZE, MAX_ROM_SIZE};

const MAGIC_AGBJ: &[u8; 4] = b"AGBJ";
const MAGIC_D233: &[u8; 4] = b"D233";
const MAGIC_MLXP: &[u8; 4] = b"MLXP";
const MAGIC_CNML: &[u8; 4] = b"CNML";

const AGBJ_V01_VARIANT: u32 = 0x0096_3130;
const AGBJ_V01_MAX_BODY: usize = 4 * 1024 * 1024;

/// Which marker-delimited bundle dialect was parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerBundleKind {
    Agbj,
    D233,
    Cba0,
    Mlxp,
    Cnml,
}

impl MarkerBundleKind {
    /// Sub-format identifier.
    #[must_use]
    pub const fn subkind(self) -> &'static str {
        match self {
            Self::Agbj => "agbj",
            Self::D233 => "d233",
            Self::Cba0 => "cba0",
            Self::Mlxp => "mlxp",
            Self::Cnml => "cnml",
        }
    }
}

/// Parsed `studio.marker-bundles` multi-member container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkerBundleAsset {
    pub kind: MarkerBundleKind,
    /// Raw member slices (each beginning with its magic marker).
    pub members: Vec<Vec<u8>>,
    /// Exact consumed byte extent.
    pub consumed_end: usize,
}

impl MarkerBundleAsset {
    /// Serialize this marker bundle back into its exact binary representation.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        pack(self)
    }
}

/// Fast, panic-free structural detector for `studio.marker-bundles` streams.
pub fn detect(data: &[u8]) -> bool {
    parse(data).is_ok()
}

/// Parse a `studio.marker-bundles` stream (`AGBJ`, `D233`, `CBA\0`, `MLXP`, or `CNML`).
pub fn parse(data: &[u8]) -> Result<MarkerBundleAsset> {
    if data.len() < 8 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 8,
            available: data.len(),
        });
    }
    match &data[..4] {
        b"AGBJ" => parse_agbj(data),
        b"D233" => parse_d233(data),
        b"CBA\0" => parse_cba0(data),
        b"MLXP" => parse_mlxp(data),
        b"CNML" => parse_cnml(data),
        _ => Err(Error::BadMagic {
            offset: 0,
            expected: u32::from_be_bytes(*MAGIC_AGBJ),
            found: u32::from_be_bytes([data[0], data[1], data[2], data[3]]),
        }),
    }
}

fn is_valid_agbj_header(slice: &[u8]) -> Option<Option<usize>> {
    if slice.len() < 8 || &slice[..4] != MAGIC_AGBJ {
        return None;
    }
    let variant = u32_at(slice, 4).ok()?;
    match variant {
        0 => Some(None),
        AGBJ_V01_VARIANT => {
            if slice.len() < 20 || slice[8..16] != [0u8; 8] {
                return None;
            }
            let body_len = u32_at(slice, 16).ok()? as usize;
            if body_len == 0 || body_len > AGBJ_V01_MAX_BODY {
                return None;
            }
            Some(Some(20usize.saturating_add(body_len)))
        }
        _ => None,
    }
}

fn parse_agbj(data: &[u8]) -> Result<MarkerBundleAsset> {
    if data.len() < 16 || is_valid_agbj_header(data).is_none() {
        return Err(Error::InvalidField {
            what: "AGBJ header",
            value: data.len() as u64,
        });
    }
    const SPEC: MarkerScanStreamSpec = MarkerScanStreamSpec::new(0, 4, MAGIC_AGBJ, 4096)
        .with_require_first_at_start(true)
        .with_min_entry_size(8);
    let raw_spans = parse_marker_scan_stream(data, &SPEC).map_err(|_| Error::InvalidField {
        what: "AGBJ marker scan",
        value: data.len() as u64,
    })?;

    // Merge spans whose candidate marker failed AGBJ variant validation into the previous member.
    let mut valid_starts = Vec::new();
    for span in &raw_spans {
        let off = span.offset as usize;
        if let Some(v01_len) = is_valid_agbj_header(&data[off..]) {
            valid_starts.push((off, v01_len));
        }
    }
    if valid_starts.is_empty() {
        return Err(Error::InvalidField {
            what: "AGBJ valid headers",
            value: 0,
        });
    }

    let mut members = Vec::with_capacity(valid_starts.len());
    let mut consumed_end = 0usize;
    for (idx, &(start, v01_len)) in valid_starts.iter().enumerate() {
        let next_start = valid_starts.get(idx + 1).map_or(data.len(), |&(s, _)| s);
        let end = match v01_len {
            Some(declared) => start.saturating_add(declared).min(next_start).min(data.len()),
            None => next_start,
        };
        members.push(data[start..end].to_vec());
        consumed_end = end;
    }

    Ok(MarkerBundleAsset {
        kind: MarkerBundleKind::Agbj,
        members,
        consumed_end,
    })
}

fn parse_d233(data: &[u8]) -> Result<MarkerBundleAsset> {
    if data.len() < 8 {
        return Err(Error::Truncated {
            offset: 0,
            needed: 8,
            available: data.len(),
        });
    }
    const SPEC: MarkerScanStreamSpec = MarkerScanStreamSpec::new(0, 1, MAGIC_D233, 4096)
        .with_require_first_at_start(true)
        .with_min_entry_size(4);
    let spans = parse_marker_scan_stream(data, &SPEC).map_err(|_| Error::InvalidField {
        what: "D233 marker scan",
        value: data.len() as u64,
    })?;
    let mut members = Vec::with_capacity(spans.len());
    for span in &spans {
        let slice = span.slice(data).ok_or(Error::InvalidField {
            what: "D233 member span",
            value: span.offset,
        })?;
        members.push(slice.to_vec());
    }
    Ok(MarkerBundleAsset {
        kind: MarkerBundleKind::D233,
        members,
        consumed_end: data.len(),
    })
}

fn parse_cba0(data: &[u8]) -> Result<MarkerBundleAsset> {
    if data.len() < 8 || (data.len() & 3) != 0 {
        return Err(Error::InvalidField {
            what: "CBA0 length alignment",
            value: data.len() as u64,
        });
    }
    const SPEC: MarkerScanStreamSpec = MarkerScanStreamSpec::new(0, 1, b"CBA", 65536)
        .with_require_first_at_start(true)
        .with_min_entry_size(3);
    let spans = parse_marker_scan_stream(data, &SPEC).map_err(|_| Error::InvalidField {
        what: "CBA0 marker scan",
        value: data.len() as u64,
    })?;
    let mut members = Vec::with_capacity(spans.len());
    for span in &spans {
        let slice = span.slice(data).ok_or(Error::InvalidField {
            what: "CBA0 section span",
            value: span.offset,
        })?;
        members.push(slice.to_vec());
    }
    Ok(MarkerBundleAsset {
        kind: MarkerBundleKind::Cba0,
        members,
        consumed_end: data.len(),
    })
}

fn is_valid_mlxp_header(slice: &[u8]) -> bool {
    slice.len() >= 58
        && &slice[0..4] == MAGIC_MLXP
        && slice[12..18] == [0x75, 0x20, 0x40, 0xE0, 0x07, 0x54]
        && slice[24..30] == [0x20, 0x01, 0x5B, 0x52, 0x00, 0x0E]
        && slice[36..43] == [0x01, 0x32, 0x56, 0x00, 0x78, 0x11, 0x83]
        && slice[50..58] == [0x11, 0xE1, 0x10, 0x07, 0x50, 0x0F, 0x10, 0x1F]
}

fn parse_mlxp(data: &[u8]) -> Result<MarkerBundleAsset> {
    if !is_valid_mlxp_header(data) {
        return Err(Error::InvalidField {
            what: "MLXP fixed header",
            value: data.len() as u64,
        });
    }
    const SPEC: MarkerScanStreamSpec = MarkerScanStreamSpec::new(0, 1, MAGIC_MLXP, 4096)
        .with_require_first_at_start(true)
        .with_min_entry_size(58);
    let raw_spans = parse_marker_scan_stream(data, &SPEC).map_err(|_| Error::InvalidField {
        what: "MLXP marker scan",
        value: data.len() as u64,
    })?;
    let mut valid_starts = Vec::new();
    for span in &raw_spans {
        let off = span.offset as usize;
        if is_valid_mlxp_header(&data[off..]) {
            valid_starts.push(off);
        }
    }
    let mut members = Vec::with_capacity(valid_starts.len());
    for (idx, &start) in valid_starts.iter().enumerate() {
        let end = valid_starts.get(idx + 1).copied().unwrap_or(data.len());
        members.push(data[start..end].to_vec());
    }
    Ok(MarkerBundleAsset {
        kind: MarkerBundleKind::Mlxp,
        members,
        consumed_end: data.len(),
    })
}

fn has_cnml_end_code(member: &[u8]) -> bool {
    if member.len() < 16 {
        return false;
    }
    let tail_start = member.len().saturating_sub(64);
    let tail = &member[tail_start..];
    if tail.windows(2).any(|w| w == [0x21, 0xFF]) {
        return true;
    }
    if member.last() == Some(&0x4D) {
        let last6_start = member.len().saturating_sub(6);
        if member[last6_start..].contains(&0x72) {
            return true;
        }
    }
    false
}

fn parse_cnml(data: &[u8]) -> Result<MarkerBundleAsset> {
    if data.len() < 16 || &data[..4] != MAGIC_CNML {
        return Err(Error::InvalidField {
            what: "CNML header",
            value: data.len() as u64,
        });
    }
    const SPEC: MarkerScanStreamSpec = MarkerScanStreamSpec::new(0, 1, MAGIC_CNML, 4096)
        .with_require_first_at_start(true)
        .with_min_entry_size(16);
    let spans = parse_marker_scan_stream(data, &SPEC).map_err(|_| Error::InvalidField {
        what: "CNML marker scan",
        value: data.len() as u64,
    })?;

    // Longest valid prefix of CNML members carrying verified end-code signatures.
    let mut members = Vec::new();
    let mut consumed_end = 0usize;
    for span in &spans {
        let Some(slice) = span.slice(data) else {
            break;
        };
        if !has_cnml_end_code(slice) {
            break;
        }
        members.push(slice.to_vec());
        consumed_end = (span.offset as usize) + (span.size as usize);
    }
    if members.is_empty() {
        return Err(Error::InvalidField {
            what: "CNML end-code prefix",
            value: 0,
        });
    }
    Ok(MarkerBundleAsset {
        kind: MarkerBundleKind::Cnml,
        members,
        consumed_end,
    })
}

/// Symmetrically pack a [`MarkerBundleAsset`] back into its exact binary representation.
pub fn pack(asset: &MarkerBundleAsset) -> Result<Vec<u8>> {
    let payloads: Vec<&[u8]> = asset.members.iter().map(Vec::as_slice).collect();
    let spec = match asset.kind {
        MarkerBundleKind::Agbj => MarkerScanStreamSpec::new(0, 4, MAGIC_AGBJ, 4096)
            .with_require_first_at_start(true)
            .with_min_entry_size(8),
        MarkerBundleKind::D233 => MarkerScanStreamSpec::new(0, 1, MAGIC_D233, 4096)
            .with_require_first_at_start(true)
            .with_min_entry_size(4),
        MarkerBundleKind::Cba0 => MarkerScanStreamSpec::new(0, 1, b"CBA", 65536)
            .with_require_first_at_start(true)
            .with_min_entry_size(3),
        MarkerBundleKind::Mlxp => MarkerScanStreamSpec::new(0, 1, MAGIC_MLXP, 4096)
            .with_require_first_at_start(true)
            .with_min_entry_size(58),
        MarkerBundleKind::Cnml => MarkerScanStreamSpec::new(0, 1, MAGIC_CNML, 4096)
            .with_require_first_at_start(true)
            .with_min_entry_size(16),
    };
    pack_marker_scan_stream(&spec, &PackPolicy::TIGHT, &[], &payloads).map_err(|_| {
        Error::InvalidField {
            what: "marker bundle pack",
            value: payloads.len() as u64,
        }
    })
}

/// Scan a GBA ROM image for high-confidence self-delimiting `studio.marker-bundles`
/// (`AGBJ` V01 with declared length, `MLXP` with 31-byte fixed signature, and `CNML` with end-code).
pub fn scan_marker_bundles(rom: &[u8]) -> Vec<DetectedFormatExtent> {
    let effective_len = rom.len().min(MAX_ROM_SIZE);
    if effective_len < HEADER_SIZE + 20 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut off = HEADER_SIZE;
    while off + 20 <= effective_len {
        let tag = &rom[off..off + 4];
        if tag == MAGIC_AGBJ {
            if let Some(Some(declared)) = is_valid_agbj_header(&rom[off..effective_len]) {
                let size = declared.min(effective_len - off);
                found.push(DetectedFormatExtent {
                    format_id: FORMAT_ID_MARKER_BUNDLES,
                    subkind: "agbj",
                    offset: off,
                    size,
                });
                off = align_up_4(off.saturating_add(size.max(4)));
                continue;
            }
        } else if tag == MAGIC_CNML {
            if let Ok(asset) = parse_cnml(&rom[off..effective_len]) {
                let size = asset.consumed_end;
                found.push(DetectedFormatExtent {
                    format_id: FORMAT_ID_MARKER_BUNDLES,
                    subkind: "cnml",
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

    pub(crate) fn make_valid_agbj_v01() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC_AGBJ);
        out.extend_from_slice(&AGBJ_V01_VARIANT.to_le_bytes());
        out.extend_from_slice(&[0u8; 8]);
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&[0x11; 16]);
        out
    }

    pub(crate) fn make_valid_cnml() -> Vec<u8> {
        let mut m0 = vec![0x41u8; 32];
        m0[0..4].copy_from_slice(MAGIC_CNML);
        m0[26..32].copy_from_slice(&[0x72, 0x21, 0xFF, 0x80, 0x10, 0x18]);
        let mut m1 = vec![0x42u8; 24];
        m1[0..4].copy_from_slice(MAGIC_CNML);
        m1[20..24].copy_from_slice(&[0x40, 0x66, 0x72, 0x4D]);
        let mut out = m0;
        out.extend_from_slice(&m1);
        out
    }

    #[test]
    fn round_trips_all_five_marker_bundle_dialects() {
        let agbj = make_valid_agbj_v01();
        assert_eq!(parse(&agbj).unwrap().to_bytes().unwrap(), agbj);

        let d233 = b"D233abcdD233efghij".to_vec();
        assert_eq!(parse(&d233).unwrap().to_bytes().unwrap(), d233);

        let cba0 = b"CBA\0xxxxCBAyyyyy".to_vec();
        assert_eq!(parse(&cba0).unwrap().to_bytes().unwrap(), cba0);

        let mut mlxp = vec![0u8; 64];
        mlxp[0..4].copy_from_slice(MAGIC_MLXP);
        mlxp[12..18].copy_from_slice(&[0x75, 0x20, 0x40, 0xE0, 0x07, 0x54]);
        mlxp[24..30].copy_from_slice(&[0x20, 0x01, 0x5B, 0x52, 0x00, 0x0E]);
        mlxp[36..43].copy_from_slice(&[0x01, 0x32, 0x56, 0x00, 0x78, 0x11, 0x83]);
        mlxp[50..58].copy_from_slice(&[0x11, 0xE1, 0x10, 0x07, 0x50, 0x0F, 0x10, 0x1F]);
        assert_eq!(parse(&mlxp).unwrap().to_bytes().unwrap(), mlxp);

        let cnml = make_valid_cnml();
        assert_eq!(parse(&cnml).unwrap().to_bytes().unwrap(), cnml);
    }

    #[test]
    fn rejects_malformed_marker_bundles() {
        assert!(!detect(b""));
        assert!(!detect(b"AGBJ\x01\0\0\0\0\0\0\0\0\0\0\0"));
        assert!(!detect(b"CNML0123456789ABCDEF")); // no end code
    }
}
