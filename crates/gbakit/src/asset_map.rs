//! Declarative ROM asset maps (`formatkit.asset-map.v1`) for Game Boy Advance ROMs.
//!
//! Provides schema-exact types, fast multi-scanner auto-generation, non-destructive
//! `--merge` upgrades, verification (bounds, SHA-256, and test-decoding), Markdown
//! reporting, and map-driven asset export.

#![deny(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use gba_compress::{
    decode_diff_filter, decode_hudson_lz, decode_huffman, decode_konami_dict, decode_lz11,
    decode_lz77, decode_rle, decode_shinen_mode7, DEFAULT_MAX_OUTPUT,
};
use gba_gfx::{
    decode_palette, decode_tile_4bpp, decode_tile_8bpp, render_palette_swatch_rgba, RawPngEncoder,
};
use gba_rom::{
    detect_rom_padding, extract_archive_entries, parse_archive_at, scan_archives,
    scan_embedded_multiboot, AgbHeader, ArchiveFormat, HEADER_SIZE, ROM_BUS_BASE,
};
use gba_sound::m4a::{rom_ptr_to_offset, M4aSampleView};
use gba_sound::{
    detect_sound_engine, encode_wav_i16_mono, extract_pcm_i8, scan_ead_sma_soundpool,
    scan_engine_sw_soundpool, scan_gax_soundpool, scan_konami_soundpool, scan_krawall_soundpool,
    scan_logik_soundpool, scan_musyx_soundpool, scan_paragon5_soundpool, scan_rare_soundpool,
    scan_rd1_soundpool, scan_sonix_soundpool, scan_southpaw_soundpool, DetectedSoundEngine,
};
use mobiclip::{scan_mobiclip_streams, scan_vxgb_streams, MobiclipStreamView, VxgbStreamView};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Canonical schema identifier for declarative asset maps.
pub const ASSET_MAP_SCHEMA: &str = "formatkit.asset-map.v1";

/// Canonical schema version for declarative asset maps.
pub const ASSET_MAP_SCHEMA_VERSION: u32 = 1;

/// Kit name written into generated asset maps.
pub const ASSET_MAP_KIT: &str = "gbakit";

/// Maximum number of code references attached to a single entry.
const MAX_CODE_REFS_PER_ENTRY: usize = 8;

/// Maximum decompressed size accepted when auto-probing BIOS compressed streams.
const AUTO_SCAN_MAX_DECOMPRESSED: usize = 256 * 1024;

/// Maximum number of pointer-probed graphics/compressed entries emitted during auto-generation.
const MAX_AUTO_PROBED_ENTRIES: usize = 4096;

/// Root document conforming to `formatkit.asset-map.v1`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetMap {
    pub schema: String,
    pub schema_version: u32,
    pub kit: String,
    pub media: MediaDescriptor,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub free_space: Vec<FreeSpaceRegion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<serde_json::Map<String, serde_json::Value>>,
    pub entries: Vec<AssetEntry>,
}

/// Declared safe relocation region when a re-encoded asset grows beyond its original allocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreeSpaceRegion {
    pub offset: u64,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Single companion entry ID or list of companion entry IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StringOrVec {
    One(String),
    Many(Vec<String>),
}

impl StringOrVec {
    /// Return the first entry ID, if any.
    pub fn first(&self) -> Option<&str> {
        match self {
            Self::One(s) => Some(s.as_str()),
            Self::Many(v) => v.first().map(String::as_str),
        }
    }

    /// Iterate over all referenced entry IDs.
    pub fn as_slice_iter(&self) -> std::slice::Iter<'_, String> {
        match self {
            Self::One(s) => std::slice::from_ref(s).iter(),
            Self::Many(v) => v.iter(),
        }
    }
}

/// Byte order for structured pointer/offset references.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Endian {
    Le,
    Be,
}

/// Structured pointer/offset reference descriptor for automated relocation patching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointerReference {
    pub at: u64,
    pub width: u32,
    pub endian: Endian,
    pub form: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shift: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub add: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bank: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Media classification for `formatkit.asset-map.v1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaKind {
    FlatRom,
    FilesystemDisc,
    FilesystemRom,
}

/// Media descriptor block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaDescriptor {
    pub kind: MediaKind,
    pub fingerprint: MediaFingerprint,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<u64>,
}

/// Neutral cryptographic fingerprint identifying the target media.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaFingerprint {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rom_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pvd_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_exe_sha256: Option<String>,
}

/// Classification status of an asset map entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssetStatus {
    Identified,
    Mapped,
    Opaque,
    Unknown,
}

/// Single extent or file entry in `formatkit.asset-map.v1`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetEntry {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lba: Option<u64>,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_capacity: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub status: AssetStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entropy: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub code_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<PointerReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapping: Option<AssetMapping>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
}

/// Mapping kind for `mapped` or `opaque` entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MappingKind {
    RawImage,
    RawAudio,
    Compressed,
    ExeIndexed,
    Companion,
    Opaque,
}

/// Sub-classification for `opaque` entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpaqueClass {
    Script,
    Collision,
    Table,
    Padding,
    Bytecode,
    Other,
}

/// Declarative mapping definition attached to `mapped` or `opaque` entries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssetMapping {
    pub kind: MappingKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paired_with: Option<StringOrVec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub relations: BTreeMap<String, StringOrVec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inner_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opaque_class: Option<OpaqueClass>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

/// Options controlling [`generate_asset_map`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GenerateOptions {
    /// Scan 4-byte-aligned ROM words for `0x08xxxxxx` references and attach up to 8 per entry.
    pub with_code_refs: bool,
}

/// Verification summary returned by [`verify_asset_map`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub valid: bool,
    pub total_bytes: u64,
    pub accounted_bytes: u64,
    pub identified_bytes: u64,
    pub mapped_bytes: u64,
    pub opaque_bytes: u64,
    pub unknown_bytes: u64,
    pub identified_count: usize,
    pub mapped_count: usize,
    pub opaque_count: usize,
    pub unknown_count: usize,
    pub identified_pct: f64,
    pub mapped_pct: f64,
    pub opaque_pct: f64,
    pub unknown_pct: f64,
    pub coverage_pct: f64,
    pub entries_checked: usize,
    pub entries_decoded: usize,
    pub errors: Vec<String>,
}

/// Single exported artifact record written into `export-manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedAsset {
    pub id: String,
    pub status: AssetStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format_id: Option<String>,
    pub offset: u64,
    pub size: u64,
    pub output_files: Vec<String>,
}

/// Summary returned by [`export_assets_with_map`] and persisted to `export-manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportReport {
    pub schema: String,
    pub rom_sha256: String,
    pub exported_entries: usize,
    pub files_written: usize,
    pub assets: Vec<ExportedAsset>,
}

// -----------------------------------------------------------------------------
// Hashing & Entropy Helpers
// -----------------------------------------------------------------------------

/// Compute lowercase hexadecimal SHA-256 digest of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Compute Shannon entropy of `data` in bits per byte (`0.0..=8.0`), rounded to 3 decimal places.
pub fn shannon_entropy(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let len = data.len() as f64;
    let mut h = 0.0f64;
    for &c in &counts {
        if c > 0 {
            let p = (c as f64) / len;
            h -= p * p.log2();
        }
    }
    let rounded = (h * 1000.0).round() / 1000.0;
    rounded.clamp(0.0, 8.0)
}

// -----------------------------------------------------------------------------
// Auto-Generation & Non-Destructive Merge
// -----------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct CandidateExtent {
    priority: u8,
    offset: usize,
    size: usize,
    id: String,
    status: AssetStatus,
    format_id: Option<String>,
    reserved_capacity: Option<u64>,
    references: Vec<PointerReference>,
    mapping: Option<AssetMapping>,
    verified: Option<bool>,
}

/// Generate a complete, 100% byte-accounted `formatkit.asset-map.v1` map for `rom`.
///
/// When `merge_from` is supplied, curated `mapped` and `opaque` entries,
/// `free_space`, `shared`, `reserved_capacity`, and `references` from the
/// existing map are preserved. Uncovered gaps are emitted as `unknown` entries
/// so `sum(entry.size) == rom.len()`.
pub fn generate_asset_map(
    rom: &[u8],
    opts: &GenerateOptions,
    merge_from: Option<&AssetMap>,
) -> AssetMap {
    let mut candidates: Vec<CandidateExtent> = Vec::new();
    let mut free_space: Vec<FreeSpaceRegion> = merge_from
        .map(|prev| prev.free_space.clone())
        .unwrap_or_default();
    let shared = merge_from.and_then(|prev| prev.shared.clone());

    // 0. Curated entries from --merge (highest priority: 250)
    if let Some(prev) = merge_from {
        for e in &prev.entries {
            if matches!(e.status, AssetStatus::Mapped | AssetStatus::Opaque) {
                if let Some(off_u64) = e.offset {
                    let off = off_u64 as usize;
                    let sz = e.size as usize;
                    if sz > 0 && off.checked_add(sz).is_some_and(|end| end <= rom.len()) {
                        candidates.push(CandidateExtent {
                            priority: 250,
                            offset: off,
                            size: sz,
                            id: e.id.clone(),
                            status: e.status,
                            format_id: e.format_id.clone(),
                            reserved_capacity: e.reserved_capacity,
                            references: e.references.clone(),
                            mapping: e.mapping.clone(),
                            verified: e.verified,
                        });
                    }
                }
            }
        }
    }

    // 1. GBA ROM header (0x000..0x0C0)
    if rom.len() >= HEADER_SIZE && AgbHeader::parse(rom).is_ok() {
        candidates.push(CandidateExtent {
            priority: 100,
            offset: 0,
            size: HEADER_SIZE,
            id: "rom_header".to_string(),
            status: AssetStatus::Identified,
            format_id: Some("gba.rom-header".to_string()),
            reserved_capacity: None,
            references: Vec::new(),
            mapping: None,
            verified: Some(true),
        });
    }

    // 2. Trailing ROM padding (and auto-populated free_space region when >= 64 bytes)
    if let Some(pad) = detect_rom_padding(rom, 64) {
        if pad.padding_bytes > 0 && pad.payload_size < rom.len() {
            if pad.padding_bytes >= 64 {
                let pad_off = pad.payload_size as u64;
                let pad_sz = pad.padding_bytes as u64;
                if !free_space
                    .iter()
                    .any(|r| r.offset == pad_off && r.size == pad_sz)
                {
                    free_space.push(FreeSpaceRegion {
                        offset: pad_off,
                        size: pad_sz,
                        bank: None,
                        note: Some("trailing ROM padding".to_string()),
                    });
                }
            }
            candidates.push(CandidateExtent {
                priority: 95,
                offset: pad.payload_size,
                size: pad.padding_bytes,
                id: format!("trailing_padding_{:08x}", pad.payload_size),
                status: AssetStatus::Opaque,
                format_id: None,
                reserved_capacity: None,
                references: Vec::new(),
                mapping: Some(AssetMapping {
                    kind: MappingKind::Opaque,
                    params: None,
                    paired_with: None,
                    relations: BTreeMap::new(),
                    inner_format: None,
                    opaque_class: Some(OpaqueClass::Padding),
                    evidence: Some(format!("uniform trailing ROM padding ({:?})", pad.kind)),
                }),
                verified: Some(true),
            });
        }
    }

    // 3. Embedded archives (PACK, WAYFORWARD, SFCD)
    for arch in scan_archives(rom) {
        if arch.total_size > 0 && arch.rom_offset + arch.total_size <= rom.len() {
            let (fmt_id, prefix) = match arch.format {
                ArchiveFormat::Pack => ("archive.pack", "archive_pack"),
                ArchiveFormat::WayForward => ("archive.wayforward", "archive_wayforward"),
                ArchiveFormat::Sfcd => ("archive.sfcd", "archive_sfcd"),
            };
            candidates.push(CandidateExtent {
                priority: 88,
                offset: arch.rom_offset,
                size: arch.total_size,
                id: format!("{prefix}_{:08x}", arch.rom_offset),
                status: AssetStatus::Identified,
                format_id: Some(fmt_id.to_string()),
                reserved_capacity: None,
                references: Vec::new(),
                mapping: None,
                verified: Some(true),
            });
        }
    }

    // 4. Embedded MultiBoot payloads
    for mb in scan_embedded_multiboot(rom) {
        if mb.length > 0 && mb.offset + mb.length <= rom.len() {
            candidates.push(CandidateExtent {
                priority: 86,
                offset: mb.offset,
                size: mb.length,
                id: format!("multiboot_{:08x}", mb.offset),
                status: AssetStatus::Identified,
                format_id: Some("rom.multiboot".to_string()),
                reserved_capacity: None,
                references: Vec::new(),
                mapping: None,
                verified: Some(true),
            });
        }
    }

    // 5. Mobiclip video streams (MOC5 / VXGB)
    for v1 in scan_mobiclip_streams(rom) {
        if v1.total_size > 0 && v1.offset + v1.total_size <= rom.len() {
            candidates.push(CandidateExtent {
                priority: 85,
                offset: v1.offset,
                size: v1.total_size,
                id: format!("mobiclip_v1_{:08x}", v1.offset),
                status: AssetStatus::Identified,
                format_id: Some("video.mobiclip-v1".to_string()),
                reserved_capacity: None,
                references: Vec::new(),
                mapping: None,
                verified: Some(true),
            });
        }
    }
    for v2 in scan_vxgb_streams(rom) {
        if v2.offset < rom.len() {
            let size = rom.len().saturating_sub(v2.offset).min(64);
            if size >= 16 {
                candidates.push(CandidateExtent {
                    priority: 85,
                    offset: v2.offset,
                    size,
                    id: format!("mobiclip_v2_{:08x}", v2.offset),
                    status: AssetStatus::Identified,
                    format_id: Some("video.mobiclip-v2".to_string()),
                    reserved_capacity: None,
                    references: Vec::new(),
                    mapping: None,
                    verified: Some(true),
                });
            }
        }
    }

    // 6. Audio engine sound tables & sample pools (all 13 engines)
    collect_audio_candidates(rom, &mut candidates);

    // 7. Single-pass 4-byte-aligned ROM pointer scan (for fast BIOS compression & palette probes + code_refs)
    let mut ptr_refs: HashMap<usize, Vec<u32>> = HashMap::new();
    let mut probe_targets: Vec<usize> = Vec::new();
    scan_rom_pointers(rom, opts.with_code_refs, &mut ptr_refs, &mut probe_targets);

    // Probe pointer targets (plus bounded linear scan on small ROMs <= 256 KiB) for BIOS streams & palettes
    collect_probed_gfx_and_compress_candidates(rom, &probe_targets, &mut candidates);

    // Deterministic overlap resolution: higher priority wins, then earlier offset, then larger size.
    candidates.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.offset.cmp(&b.offset))
            .then_with(|| b.size.cmp(&a.size))
            .then_with(|| a.id.cmp(&b.id))
    });

    // BTreeMap keyed by start offset -> (end offset, CandidateExtent)
    let mut accepted: BTreeMap<usize, (usize, CandidateExtent)> = BTreeMap::new();
    for cand in candidates {
        let start = cand.offset;
        let end = match start.checked_add(cand.size) {
            Some(e) if e <= rom.len() && cand.size > 0 => e,
            _ => continue,
        };
        if overlaps_existing(&accepted, start, end) {
            continue;
        }
        accepted.insert(start, (end, cand));
    }

    // Assemble 100% byte-accounted entry list with unknown gaps filled
    let mut entries: Vec<AssetEntry> = Vec::with_capacity(accepted.len() * 2 + 1);
    let mut used_ids: HashMap<String, usize> = HashMap::new();
    let mut cursor = 0usize;

    for (start, (end, cand)) in accepted {
        if start > cursor {
            let gap_slice = &rom[cursor..start];
            let gap_id = unique_id(&mut used_ids, format!("unknown_{cursor:08x}"), cursor);
            entries.push(AssetEntry {
                id: gap_id,
                path: None,
                offset: Some(cursor as u64),
                lba: None,
                size: (start - cursor) as u64,
                reserved_capacity: None,
                sha256: Some(sha256_hex(gap_slice)),
                status: AssetStatus::Unknown,
                format_id: None,
                entropy: Some(shannon_entropy(gap_slice)),
                code_refs: Vec::new(),
                references: Vec::new(),
                mapping: None,
                verified: None,
            });
        }

        let slice = &rom[start..end];
        let id = unique_id(&mut used_ids, cand.id, start);
        entries.push(AssetEntry {
            id,
            path: None,
            offset: Some(start as u64),
            lba: None,
            size: (end - start) as u64,
            reserved_capacity: cand.reserved_capacity,
            sha256: Some(sha256_hex(slice)),
            status: cand.status,
            format_id: cand.format_id,
            entropy: Some(shannon_entropy(slice)),
            code_refs: Vec::new(),
            references: cand.references,
            mapping: cand.mapping,
            verified: cand.verified,
        });
        cursor = end;
    }

    if cursor < rom.len() {
        let gap_slice = &rom[cursor..];
        let gap_id = unique_id(&mut used_ids, format!("unknown_{cursor:08x}"), cursor);
        entries.push(AssetEntry {
            id: gap_id,
            path: None,
            offset: Some(cursor as u64),
            lba: None,
            size: (rom.len() - cursor) as u64,
            reserved_capacity: None,
            sha256: Some(sha256_hex(gap_slice)),
            status: AssetStatus::Unknown,
            format_id: None,
            entropy: Some(shannon_entropy(gap_slice)),
            code_refs: Vec::new(),
            references: Vec::new(),
            mapping: None,
            verified: None,
        });
    }

    // Preserve curated reserved_capacity and references on re-detected entries across --merge
    if let Some(prev) = merge_from {
        let prev_by_span: HashMap<(u64, u64), &AssetEntry> = prev
            .entries
            .iter()
            .filter_map(|e| e.offset.map(|off| ((off, e.size), e)))
            .collect();
        for entry in &mut entries {
            if let Some(off) = entry.offset {
                if let Some(prev_entry) = prev_by_span.get(&(off, entry.size)) {
                    if entry.reserved_capacity.is_none() {
                        entry.reserved_capacity = prev_entry.reserved_capacity;
                    }
                    if entry.references.is_empty() && !prev_entry.references.is_empty() {
                        entry.references = prev_entry.references.clone();
                    }
                }
            }
        }
    }

    // Attach code_refs and structured GBA ROM pointer references when requested
    if opts.with_code_refs && !ptr_refs.is_empty() {
        for entry in &mut entries {
            if let Some(off) = entry.offset {
                if let Some(refs) = ptr_refs.get(&(off as usize)) {
                    entry.code_refs = refs
                        .iter()
                        .take(MAX_CODE_REFS_PER_ENTRY)
                        .map(|addr| format!("0x{addr:08x}"))
                        .collect();
                    for &addr in refs.iter().take(MAX_CODE_REFS_PER_ENTRY) {
                        let at = u64::from(addr.saturating_sub(ROM_BUS_BASE));
                        if !entry.references.iter().any(|r| r.at == at) {
                            entry.references.push(PointerReference {
                                at,
                                width: 4,
                                endian: Endian::Le,
                                form: "gba_rom_ptr".to_string(),
                                shift: None,
                                add: Some(i64::from(ROM_BUS_BASE)),
                                bank: None,
                                note: None,
                            });
                        }
                    }
                }
            }
        }
    }

    AssetMap {
        schema: ASSET_MAP_SCHEMA.to_string(),
        schema_version: ASSET_MAP_SCHEMA_VERSION,
        kit: ASSET_MAP_KIT.to_string(),
        media: MediaDescriptor {
            kind: MediaKind::FlatRom,
            fingerprint: MediaFingerprint {
                rom_sha256: Some(sha256_hex(rom)),
                pvd_sha256: None,
                main_exe_sha256: None,
            },
            total_bytes: Some(rom.len() as u64),
            file_count: Some(entries.len() as u64),
        },
        free_space,
        shared,
        entries,
    }
}

fn overlaps_existing(
    accepted: &BTreeMap<usize, (usize, CandidateExtent)>,
    start: usize,
    end: usize,
) -> bool {
    if let Some((&_prev_start, &(prev_end, _))) = accepted.range(..=start).next_back() {
        if prev_end > start {
            return true;
        }
    }
    if let Some((&next_start, _)) = accepted.range(start..).next() {
        if next_start < end {
            return true;
        }
    }
    false
}

fn unique_id(used: &mut HashMap<String, usize>, base: String, offset: usize) -> String {
    if !used.contains_key(&base) {
        used.insert(base.clone(), 1);
        return base;
    }
    let with_off = format!("{base}_{offset:08x}");
    if !used.contains_key(&with_off) {
        used.insert(with_off.clone(), 1);
        return with_off;
    }
    let count = used.entry(base.clone()).or_insert(1);
    *count += 1;
    format!("{base}_{}", *count)
}

fn collect_audio_candidates(rom: &[u8], out: &mut Vec<CandidateExtent>) {
    match detect_sound_engine(rom) {
        DetectedSoundEngine::M4a(scan) => {
            let tbl_size = scan
                .song_count
                .saturating_mul(8)
                .min(rom.len().saturating_sub(scan.song_table_offset));
            if tbl_size > 0 {
                out.push(CandidateExtent {
                    priority: 82,
                    offset: scan.song_table_offset,
                    size: tbl_size,
                    id: format!("m4a_song_table_{:08x}", scan.song_table_offset),
                    status: AssetStatus::Identified,
                    format_id: Some("audio.m4a-song-table".to_string()),
                    reserved_capacity: None,
                    references: Vec::new(),
                    mapping: None,
                    verified: Some(true),
                });
            }
            for &ptr in &scan.sample_pointers {
                if let Some(off) = rom_ptr_to_offset(ptr, rom.len()) {
                    if let Ok(view) = M4aSampleView::from_wavdata_bytes(&rom[off..]) {
                        let total = gba_sound::WAVE_HEADER_SIZE + view.pcm_bytes.len();
                        if off + total <= rom.len() {
                            out.push(CandidateExtent {
                                priority: 80,
                                offset: off,
                                size: total,
                                id: format!("m4a_sample_{off:08x}"),
                                status: AssetStatus::Identified,
                                format_id: Some("audio.m4a-wavedata".to_string()),
                                reserved_capacity: None,
                                references: Vec::new(),
                                mapping: None,
                                verified: Some(true),
                            });
                        }
                    }
                }
            }
        }
        DetectedSoundEngine::Gax(_) => {
            for s in scan_gax_soundpool(rom) {
                push_audio_sample(out, rom.len(), s.rom_offset, s.size, "gax", "audio.gax-sample");
            }
        }
        DetectedSoundEngine::Musyx(_) => {
            for s in scan_musyx_soundpool(rom) {
                push_audio_sample(
                    out,
                    rom.len(),
                    s.rom_offset,
                    16usize.saturating_add(s.size),
                    "musyx",
                    "audio.musyx-sample",
                );
            }
        }
        DetectedSoundEngine::EadSma(_) => {
            for s in scan_ead_sma_soundpool(rom) {
                push_audio_sample(
                    out,
                    rom.len(),
                    s.rom_offset,
                    s.size,
                    "ead_sma",
                    "audio.ead-sma-sample",
                );
            }
        }
        DetectedSoundEngine::EngineSw(_) => {
            for s in scan_engine_sw_soundpool(rom) {
                push_audio_sample(
                    out,
                    rom.len(),
                    s.rom_offset,
                    s.size,
                    "engine_sw",
                    "audio.engine-sw-sample",
                );
            }
        }
        DetectedSoundEngine::CosmigoSonix(_) => {
            for s in scan_sonix_soundpool(rom) {
                push_audio_sample(
                    out,
                    rom.len(),
                    s.rom_offset,
                    s.size,
                    "sonix",
                    "audio.sonix-sample",
                );
            }
        }
        DetectedSoundEngine::Krawall(_) => {
            for s in scan_krawall_soundpool(rom) {
                push_audio_sample(
                    out,
                    rom.len(),
                    s.rom_offset,
                    s.size,
                    "krawall",
                    "audio.krawall-sample",
                );
            }
        }
        DetectedSoundEngine::Rare(_) => {
            for s in scan_rare_soundpool(rom) {
                push_audio_sample(
                    out,
                    rom.len(),
                    s.rom_offset,
                    s.size,
                    "rare",
                    "audio.rare-sample",
                );
            }
        }
        DetectedSoundEngine::Konami(_, _) => {
            for s in scan_konami_soundpool(rom) {
                push_audio_sample(
                    out,
                    rom.len(),
                    s.rom_offset,
                    s.size,
                    "konami",
                    "audio.konami-sample",
                );
            }
        }
        DetectedSoundEngine::LogikState(_) => {
            for s in scan_logik_soundpool(rom) {
                push_audio_sample(
                    out,
                    rom.len(),
                    s.data_offset,
                    s.size,
                    "logik",
                    "audio.logik-sample",
                );
            }
        }
        DetectedSoundEngine::Paragon5(_) => {
            for s in scan_paragon5_soundpool(rom) {
                push_audio_sample(
                    out,
                    rom.len(),
                    s.rom_offset,
                    s.size,
                    "paragon5",
                    "audio.paragon5-sample",
                );
            }
        }
        DetectedSoundEngine::Rd1(_) => {
            for s in scan_rd1_soundpool(rom) {
                push_audio_sample(out, rom.len(), s.rom_offset, s.size, "rd1", "audio.rd1-sample");
            }
        }
        DetectedSoundEngine::Southpaw(_) => {
            for s in scan_southpaw_soundpool(rom) {
                push_audio_sample(
                    out,
                    rom.len(),
                    s.rom_offset,
                    s.size,
                    "southpaw",
                    "audio.southpaw-sample",
                );
            }
        }
        DetectedSoundEngine::Custom => {}
    }
}

fn push_audio_sample(
    out: &mut Vec<CandidateExtent>,
    rom_len: usize,
    offset: usize,
    size: usize,
    prefix: &str,
    format_id: &str,
) {
    if size > 0 && offset.checked_add(size).is_some_and(|end| end <= rom_len) {
        out.push(CandidateExtent {
            priority: 80,
            offset,
            size,
            id: format!("{prefix}_sample_{offset:08x}"),
            status: AssetStatus::Identified,
            format_id: Some(format_id.to_string()),
            reserved_capacity: None,
            references: Vec::new(),
            mapping: None,
            verified: Some(true),
        });
    }
}

/// Single-pass 4-byte-aligned ROM scan collecting valid `0x08xxxxxx` pointer targets.
fn scan_rom_pointers(
    rom: &[u8],
    collect_refs: bool,
    ptr_refs: &mut HashMap<usize, Vec<u32>>,
    probe_targets: &mut Vec<usize>,
) {
    let rom_len_u32 = u32::try_from(rom.len().min(gba_rom::MAX_ROM_SIZE)).unwrap_or(0);
    let bus_end = ROM_BUS_BASE.saturating_add(rom_len_u32);
    let mut seen_targets: HashMap<usize, ()> = HashMap::new();

    for (idx, chunk) in rom.as_chunks::<4>().0.iter().enumerate() {
        let word = u32::from_le_bytes(*chunk);
        if word >= ROM_BUS_BASE && word < bus_end {
            let target = (word - ROM_BUS_BASE) as usize;
            if target >= HEADER_SIZE
                && target.is_multiple_of(4)
                && seen_targets.insert(target, ()).is_none()
            {
                probe_targets.push(target);
            }
            if collect_refs {
                let src_addr = ROM_BUS_BASE.saturating_add((idx * 4) as u32);
                let refs = ptr_refs.entry(target).or_default();
                if refs.len() < MAX_CODE_REFS_PER_ENTRY && !refs.contains(&src_addr) {
                    refs.push(src_addr);
                }
            }
        }
    }
    probe_targets.sort_unstable();
}

fn collect_probed_gfx_and_compress_candidates(
    rom: &[u8],
    probe_targets: &[usize],
    out: &mut Vec<CandidateExtent>,
) {
    let mut probed = 0usize;

    for &off in probe_targets {
        if probed >= MAX_AUTO_PROBED_ENTRIES {
            break;
        }
        if off < HEADER_SIZE || off + 8 > rom.len() {
            continue;
        }
        if let Some(cand) = probe_bios_stream_at(rom, off) {
            out.push(cand);
            probed += 1;
            continue;
        }
        if let Some(cand) = probe_palette_at(rom, off) {
            out.push(cand);
            probed += 1;
        }
    }

    // For small ROMs (<= 256 KiB, e.g., synthetic fixtures or tiny homebrew without pointer tables),
    // also do a quick 4-byte-aligned linear pass for BIOS LZ77/RLE/Huffman streams.
    if rom.len() <= 256 * 1024 && probed < MAX_AUTO_PROBED_ENTRIES {
        let mut off = HEADER_SIZE;
        while off + 8 <= rom.len() && probed < MAX_AUTO_PROBED_ENTRIES {
            if let Some(cand) = probe_bios_stream_at(rom, off) {
                let advance = ((cand.size + 3) & !3).max(4);
                out.push(cand);
                probed += 1;
                off = off.saturating_add(advance);
            } else {
                off += 4;
            }
        }
    }
}

fn probe_bios_stream_at(rom: &[u8], off: usize) -> Option<CandidateExtent> {
    let slice = rom.get(off..)?;
    if slice.len() < 8 {
        return None;
    }
    let tag = slice[0];
    let dec_len = (slice[1] as usize) | ((slice[2] as usize) << 8) | ((slice[3] as usize) << 16);
    if !(32..=AUTO_SCAN_MAX_DECOMPRESSED).contains(&dec_len) {
        return None;
    }

    match tag {
        0x10 => {
            let outcome = decode_lz77(slice, AUTO_SCAN_MAX_DECOMPRESSED).ok()?;
            if outcome.consumed >= 16
                && outcome.bytes.len() == dec_len
                && outcome.consumed < dec_len
            {
                return Some(CandidateExtent {
                    priority: 72,
                    offset: off,
                    size: outcome.consumed,
                    id: format!("lz77_{off:08x}"),
                    status: AssetStatus::Identified,
                    format_id: Some("compress.bios-lz77".to_string()),
                    reserved_capacity: None,
                    references: Vec::new(),
                    mapping: None,
                    verified: Some(true),
                });
            }
        }
        0x30 => {
            let outcome = decode_rle(slice, AUTO_SCAN_MAX_DECOMPRESSED).ok()?;
            if outcome.consumed >= 16
                && outcome.bytes.len() == dec_len
                && outcome.consumed < dec_len
            {
                return Some(CandidateExtent {
                    priority: 71,
                    offset: off,
                    size: outcome.consumed,
                    id: format!("rle_{off:08x}"),
                    status: AssetStatus::Identified,
                    format_id: Some("compress.bios-rle".to_string()),
                    reserved_capacity: None,
                    references: Vec::new(),
                    mapping: None,
                    verified: Some(true),
                });
            }
        }
        0x24 | 0x28 => {
            let outcome = decode_huffman(slice, AUTO_SCAN_MAX_DECOMPRESSED).ok()?;
            if outcome.consumed >= 16
                && outcome.bytes.len() == dec_len
                && outcome.consumed < dec_len
            {
                return Some(CandidateExtent {
                    priority: 71,
                    offset: off,
                    size: outcome.consumed,
                    id: format!("huffman_{off:08x}"),
                    status: AssetStatus::Identified,
                    format_id: Some("compress.bios-huffman".to_string()),
                    reserved_capacity: None,
                    references: Vec::new(),
                    mapping: None,
                    verified: Some(true),
                });
            }
        }
        _ => {}
    }
    None
}

fn probe_palette_at(rom: &[u8], off: usize) -> Option<CandidateExtent> {
    // Check 256-color (512B) then 16-color (32B) BGR555 palette with bit 15 clear and good color variety
    if off + 512 <= rom.len() && is_plausible_bgr555_palette(&rom[off..off + 512], 32) {
        return Some(CandidateExtent {
            priority: 62,
            offset: off,
            size: 512,
            id: format!("pal256_{off:08x}"),
            status: AssetStatus::Identified,
            format_id: Some("gfx.gba-palette".to_string()),
            reserved_capacity: None,
            references: Vec::new(),
            mapping: None,
            verified: Some(true),
        });
    }
    if off + 32 <= rom.len() && is_plausible_bgr555_palette(&rom[off..off + 32], 6) {
        return Some(CandidateExtent {
            priority: 60,
            offset: off,
            size: 32,
            id: format!("pal16_{off:08x}"),
            status: AssetStatus::Identified,
            format_id: Some("gfx.gba-palette".to_string()),
            reserved_capacity: None,
            references: Vec::new(),
            mapping: None,
            verified: Some(true),
        });
    }
    None
}

fn is_plausible_bgr555_palette(slice: &[u8], min_distinct: usize) -> bool {
    let mut seen = Vec::with_capacity(min_distinct);
    let mut non_zero = 0usize;
    for &pair in slice.as_chunks::<2>().0 {
        let w = u16::from_le_bytes(pair);
        if (w & 0x8000) != 0 {
            return false;
        }
        if w != 0 {
            non_zero += 1;
        }
        if seen.len() < min_distinct && !seen.contains(&w) {
            seen.push(w);
        }
    }
    non_zero >= min_distinct && seen.len() >= min_distinct
}

// -----------------------------------------------------------------------------
// Verification
// -----------------------------------------------------------------------------

/// Verify an [`AssetMap`] against `rom`, checking schema metadata, fingerprint,
/// bounds, SHA-256 digests, and test-decoding every `identified` and `mapped` entry.
pub fn verify_asset_map(rom: &[u8], map: &AssetMap) -> VerifyReport {
    let mut errors = Vec::new();
    let total_bytes = rom.len() as u64;

    if map.schema != ASSET_MAP_SCHEMA {
        errors.push(format!(
            "unexpected schema {:?} (expected {:?})",
            map.schema, ASSET_MAP_SCHEMA
        ));
    }
    if map.schema_version != ASSET_MAP_SCHEMA_VERSION {
        errors.push(format!(
            "unexpected schema_version {} (expected {})",
            map.schema_version, ASSET_MAP_SCHEMA_VERSION
        ));
    }
    if let Some(expected_sha) = &map.media.fingerprint.rom_sha256 {
        let actual_sha = sha256_hex(rom);
        if !expected_sha.eq_ignore_ascii_case(&actual_sha) {
            errors.push(format!(
                "rom_sha256 mismatch: map has {expected_sha}, ROM has {actual_sha}"
            ));
        }
    }
    if let Some(expected_total) = map.media.total_bytes {
        if expected_total != total_bytes {
            errors.push(format!(
                "media.total_bytes mismatch: map has {expected_total}, ROM has {total_bytes}"
            ));
        }
    }

    for (idx, region) in map.free_space.iter().enumerate() {
        if region
            .offset
            .checked_add(region.size)
            .is_none_or(|end| end > total_bytes)
        {
            errors.push(format!(
                "free_space[{idx}] range 0x{:X}..+{} exceeds ROM length {total_bytes}",
                region.offset, region.size
            ));
        }
    }

    let entry_by_id: HashMap<&str, &AssetEntry> =
        map.entries.iter().map(|e| (e.id.as_str(), e)).collect();

    let mut identified_bytes = 0u64;
    let mut mapped_bytes = 0u64;
    let mut opaque_bytes = 0u64;
    let mut unknown_bytes = 0u64;
    let mut identified_count = 0usize;
    let mut mapped_count = 0usize;
    let mut opaque_count = 0usize;
    let mut unknown_count = 0usize;
    let mut entries_decoded = 0usize;

    let mut intervals: Vec<(u64, u64, &str)> = Vec::with_capacity(map.entries.len());

    for entry in &map.entries {
        match entry.status {
            AssetStatus::Identified => {
                identified_bytes = identified_bytes.saturating_add(entry.size);
                identified_count += 1;
            }
            AssetStatus::Mapped => {
                mapped_bytes = mapped_bytes.saturating_add(entry.size);
                mapped_count += 1;
            }
            AssetStatus::Opaque => {
                opaque_bytes = opaque_bytes.saturating_add(entry.size);
                opaque_count += 1;
            }
            AssetStatus::Unknown => {
                unknown_bytes = unknown_bytes.saturating_add(entry.size);
                unknown_count += 1;
            }
        }

        let Some(off_u64) = entry.offset else {
            errors.push(format!("entry {:?} is missing `offset`", entry.id));
            continue;
        };
        let end_u64 = match off_u64.checked_add(entry.size) {
            Some(e) if e <= total_bytes => e,
            _ => {
                errors.push(format!(
                    "entry {:?} range 0x{:X}..+{} exceeds ROM length {}",
                    entry.id, off_u64, entry.size, total_bytes
                ));
                continue;
            }
        };
        intervals.push((off_u64, end_u64, entry.id.as_str()));

        let off = off_u64 as usize;
        let end = end_u64 as usize;
        let slice = &rom[off..end];

        if let Some(expected_sha) = &entry.sha256 {
            let actual = sha256_hex(slice);
            if !expected_sha.eq_ignore_ascii_case(&actual) {
                errors.push(format!(
                    "entry {:?} sha256 mismatch: expected {expected_sha}, got {actual}",
                    entry.id
                ));
                continue;
            }
        }

        for (idx, pref) in entry.references.iter().enumerate() {
            if pref.width == 0 {
                errors.push(format!(
                    "entry {:?} references[{idx}] has invalid width 0",
                    entry.id
                ));
            } else if pref
                .at
                .checked_add(u64::from(pref.width))
                .is_none_or(|end| end > total_bytes)
            {
                errors.push(format!(
                    "entry {:?} references[{idx}] range 0x{:X}..+{} exceeds ROM length {total_bytes}",
                    entry.id, pref.at, pref.width
                ));
            }
        }

        if matches!(entry.status, AssetStatus::Identified | AssetStatus::Mapped) {
            match test_decode_entry(rom, entry, slice, &entry_by_id) {
                Ok(()) => entries_decoded += 1,
                Err(err) => errors.push(format!("entry {:?} decode failed: {err}", entry.id)),
            }
        }
    }

    // Check for overlapping extents
    intervals.sort_by_key(|&(s, e, _)| (s, e));
    for pair in intervals.windows(2) {
        let (s1, e1, id1) = pair[0];
        let (s2, _e2, id2) = pair[1];
        if s2 < e1 {
            errors.push(format!(
                "overlapping entries {id1:?} (0x{s1:X}..0x{e1:X}) and {id2:?} (starts at 0x{s2:X})"
            ));
        }
    }

    let accounted_bytes = identified_bytes
        .saturating_add(mapped_bytes)
        .saturating_add(opaque_bytes)
        .saturating_add(unknown_bytes);
    let pct = |bytes: u64| -> f64 {
        if total_bytes == 0 {
            0.0
        } else {
            ((bytes as f64 / total_bytes as f64) * 100.0 * 100.0).round() / 100.0
        }
    };
    let covered_bytes = identified_bytes
        .saturating_add(mapped_bytes)
        .saturating_add(opaque_bytes);

    VerifyReport {
        valid: errors.is_empty(),
        total_bytes,
        accounted_bytes,
        identified_bytes,
        mapped_bytes,
        opaque_bytes,
        unknown_bytes,
        identified_count,
        mapped_count,
        opaque_count,
        unknown_count,
        identified_pct: pct(identified_bytes),
        mapped_pct: pct(mapped_bytes),
        opaque_pct: pct(opaque_bytes),
        unknown_pct: pct(unknown_bytes),
        coverage_pct: pct(covered_bytes),
        entries_checked: map.entries.len(),
        entries_decoded,
        errors,
    }
}

fn test_decode_entry(
    rom: &[u8],
    entry: &AssetEntry,
    slice: &[u8],
    entry_by_id: &HashMap<&str, &AssetEntry>,
) -> Result<(), String> {
    let off = entry.offset.unwrap_or(0) as usize;

    if let Some(mapping) = &entry.mapping {
        return match mapping.kind {
            MappingKind::RawImage => {
                let bpp = param_u64(mapping, "bpp").unwrap_or(4);
                let tile_bytes = match bpp {
                    4 => 32usize,
                    8 => 64usize,
                    other => return Err(format!("unsupported raw-image bpp {other} (expected 4 or 8)")),
                };
                if slice.len() < tile_bytes {
                    return Err(format!(
                        "raw-image slice {} bytes smaller than one {bpp}bpp tile ({tile_bytes} bytes)",
                        slice.len()
                    ));
                }
                if bpp == 4 {
                    decode_tile_4bpp(&slice[..32]).map_err(|e| e.to_string())?;
                } else {
                    decode_tile_8bpp(&slice[..64]).map_err(|e| e.to_string())?;
                }
                if let Some(pal_id) = resolve_palette_id(mapping) {
                    let pal_entry = entry_by_id
                        .get(pal_id)
                        .ok_or_else(|| format!("paired palette {pal_id:?} not found in map"))?;
                    let pal_slice = entry_slice(rom, pal_entry)?;
                    decode_palette(pal_slice).map_err(|e| format!("paired palette {pal_id:?}: {e}"))?;
                }
                Ok(())
            }
            MappingKind::Compressed => {
                let codec = param_str(mapping, "codec").unwrap_or("lz77");
                decode_compressed_slice(codec, slice).map(|_| ())
            }
            MappingKind::RawAudio => {
                let encoding = param_str(mapping, "encoding").unwrap_or("pcm8");
                let rate = param_u64(mapping, "sample_rate").unwrap_or(13_379) as u32;
                decode_raw_audio_slice(encoding, rate, slice).map(|_| ())
            }
            MappingKind::ExeIndexed | MappingKind::Companion | MappingKind::Opaque => Ok(()),
        };
    }

    if let Some(fmt) = entry.format_id.as_deref() {
        match fmt {
            "gba.rom-header" => {
                AgbHeader::parse(slice).map_err(|e| e.to_string())?;
            }
            "rom.multiboot" => {
                AgbHeader::parse(slice).map_err(|e| e.to_string())?;
            }
            "archive.pack" | "archive.wayforward" | "archive.sfcd" => {
                parse_archive_at(rom, off).map_err(|e| e.to_string())?;
            }
            "video.mobiclip-v1" => {
                MobiclipStreamView::parse_at(rom, off).map_err(|e| e.to_string())?;
            }
            "video.mobiclip-v2" => {
                VxgbStreamView::parse_at(rom, off).map_err(|e| e.to_string())?;
            }
            "compress.bios-lz77" => {
                decode_lz77(slice, DEFAULT_MAX_OUTPUT).map_err(|e| e.to_string())?;
            }
            "compress.bios-lz11" => {
                decode_lz11(slice, DEFAULT_MAX_OUTPUT).map_err(|e| e.to_string())?;
            }
            "compress.bios-rle" => {
                decode_rle(slice, DEFAULT_MAX_OUTPUT).map_err(|e| e.to_string())?;
            }
            "compress.bios-huffman" => {
                decode_huffman(slice, DEFAULT_MAX_OUTPUT).map_err(|e| e.to_string())?;
            }
            "compress.bios-diff" => {
                decode_diff_filter(slice, DEFAULT_MAX_OUTPUT).map_err(|e| e.to_string())?;
            }
            "gfx.gba-palette" => {
                decode_palette(slice).map_err(|e| e.to_string())?;
            }
            "audio.m4a-wavedata" => {
                extract_pcm_i8(slice, 16 * 1024 * 1024).map_err(|e| e.to_string())?;
            }
            _ => {}
        }
    }

    Ok(())
}

fn entry_slice<'a>(rom: &'a [u8], entry: &AssetEntry) -> Result<&'a [u8], String> {
    let off = entry
        .offset
        .ok_or_else(|| format!("entry {:?} missing offset", entry.id))? as usize;
    let end = off
        .checked_add(entry.size as usize)
        .filter(|&e| e <= rom.len())
        .ok_or_else(|| format!("entry {:?} out of bounds", entry.id))?;
    Ok(&rom[off..end])
}

fn resolve_palette_id(mapping: &AssetMapping) -> Option<&str> {
    mapping
        .relations
        .get("palette")
        .and_then(StringOrVec::first)
        .or_else(|| mapping.paired_with.as_ref().and_then(StringOrVec::first))
}

fn param_u64(mapping: &AssetMapping, key: &str) -> Option<u64> {
    mapping.params.as_ref()?.get(key)?.as_u64()
}

fn param_str<'a>(mapping: &'a AssetMapping, key: &str) -> Option<&'a str> {
    mapping.params.as_ref()?.get(key)?.as_str()
}

fn decode_compressed_slice(codec: &str, slice: &[u8]) -> Result<Vec<u8>, String> {
    let outcome = match codec.to_ascii_lowercase().as_str() {
        "lz77" | "bios-lz77" | "compress.bios-lz77" => decode_lz77(slice, DEFAULT_MAX_OUTPUT),
        "lz11" | "bios-lz11" | "compress.bios-lz11" => decode_lz11(slice, DEFAULT_MAX_OUTPUT),
        "rle" | "bios-rle" | "compress.bios-rle" => decode_rle(slice, DEFAULT_MAX_OUTPUT),
        "huffman" | "bios-huffman" | "compress.bios-huffman" => {
            decode_huffman(slice, DEFAULT_MAX_OUTPUT)
        }
        "diff" | "bios-diff" | "compress.bios-diff" => {
            decode_diff_filter(slice, DEFAULT_MAX_OUTPUT)
        }
        "hudson" | "hudson-lz" | "hudson_lz" => decode_hudson_lz(slice, DEFAULT_MAX_OUTPUT),
        "konami" | "konami-dict" | "konami_dict" => decode_konami_dict(slice, DEFAULT_MAX_OUTPUT),
        "shinen" | "shinen-mode7" | "shinen_mode7" => {
            decode_shinen_mode7(slice, DEFAULT_MAX_OUTPUT)
        }
        other => return Err(format!("unsupported compression codec {other:?}")),
    }
    .map_err(|e| e.to_string())?;
    Ok(outcome.bytes)
}

fn decode_raw_audio_slice(encoding: &str, sample_rate: u32, slice: &[u8]) -> Result<Vec<u8>, String> {
    if slice.is_empty() {
        return Err("empty raw-audio slice".into());
    }
    let (rate, pcm_i8): (u32, Vec<i8>) = match encoding.to_ascii_lowercase().as_str() {
        "wavedata" | "m4a" | "dpcm" => {
            let (hdr, pcm) = extract_pcm_i8(slice, 16 * 1024 * 1024).map_err(|e| e.to_string())?;
            (hdr.base_sample_rate_hz().max(1), pcm)
        }
        "pcm8" | "s8" | "signed8" => {
            (sample_rate.max(1), slice.iter().map(|&b| b as i8).collect())
        }
        "pcm8_unsigned" | "u8" | "unsigned8" => (
            sample_rate.max(1),
            slice.iter().map(|&b| b.wrapping_sub(0x80) as i8).collect(),
        ),
        other => return Err(format!("unsupported raw-audio encoding {other:?}")),
    };
    encode_wav_i16_mono(rate, &pcm_i8).map_err(|e| e.to_string())
}

// -----------------------------------------------------------------------------
// Markdown Rendering
// -----------------------------------------------------------------------------

/// Render a wiki-ready Markdown summary of an [`AssetMap`] and optional [`VerifyReport`].
pub fn render_asset_map_markdown(map: &AssetMap, verify: Option<&VerifyReport>) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let total_bytes = map
        .media
        .total_bytes
        .unwrap_or_else(|| map.entries.iter().map(|e| e.size).sum());
    let rom_sha = map
        .media
        .fingerprint
        .rom_sha256
        .as_deref()
        .unwrap_or("unknown");

    let _ = writeln!(out, "# GBA Asset Map (`{}`)\n", map.schema);
    let _ = writeln!(out, "- **Kit:** `{}`", map.kit);
    let _ = writeln!(out, "- **Media Kind:** `{:?}`", map.media.kind);
    let _ = writeln!(out, "- **ROM SHA-256:** `{rom_sha}`");
    let _ = writeln!(out, "- **Total Bytes:** `{total_bytes}`");
    let _ = writeln!(out, "- **Entries:** `{}`\n", map.entries.len());

    // Coverage breakdown
    let mut id_bytes = 0u64;
    let mut map_bytes = 0u64;
    let mut opq_bytes = 0u64;
    let mut unk_bytes = 0u64;
    let mut id_cnt = 0usize;
    let mut map_cnt = 0usize;
    let mut opq_cnt = 0usize;
    let mut unk_cnt = 0usize;
    for e in &map.entries {
        match e.status {
            AssetStatus::Identified => {
                id_bytes += e.size;
                id_cnt += 1;
            }
            AssetStatus::Mapped => {
                map_bytes += e.size;
                map_cnt += 1;
            }
            AssetStatus::Opaque => {
                opq_bytes += e.size;
                opq_cnt += 1;
            }
            AssetStatus::Unknown => {
                unk_bytes += e.size;
                unk_cnt += 1;
            }
        }
    }
    let pct = |b: u64| -> f64 {
        if total_bytes == 0 {
            0.0
        } else {
            (b as f64 / total_bytes as f64) * 100.0
        }
    };
    let cov_bytes = id_bytes + map_bytes + opq_bytes;

    let _ = writeln!(out, "## Coverage Summary\n");
    let _ = writeln!(out, "| Status | Entries | Bytes | % of ROM |");
    let _ = writeln!(out, "|---|---:|---:|---:|");
    let _ = writeln!(out, "| `identified` | {id_cnt} | {id_bytes} | {:.2}% |", pct(id_bytes));
    let _ = writeln!(out, "| `mapped` | {map_cnt} | {map_bytes} | {:.2}% |", pct(map_bytes));
    let _ = writeln!(out, "| `opaque` | {opq_cnt} | {opq_bytes} | {:.2}% |", pct(opq_bytes));
    let _ = writeln!(out, "| `unknown` | {unk_cnt} | {unk_bytes} | {:.2}% |", pct(unk_bytes));
    let _ = writeln!(
        out,
        "| **Total Accounted Coverage** | **{}** | **{cov_bytes}** | **{:.2}%** |\n",
        id_cnt + map_cnt + opq_cnt,
        pct(cov_bytes)
    );

    if let Some(v) = verify {
        let _ = writeln!(out, "## Verification\n");
        let _ = writeln!(
            out,
            "- **Valid:** `{}` ({} checked, {} decoded, {} error(s))",
            v.valid,
            v.entries_checked,
            v.entries_decoded,
            v.errors.len()
        );
        for err in &v.errors {
            let _ = writeln!(out, "  - {err}");
        }
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "## Entries\n");
    let _ = writeln!(
        out,
        "| ID | Offset | Size | Status | Format / Mapping | Entropy | Code Refs |"
    );
    let _ = writeln!(out, "|---|---|---:|---|---|---:|---|");

    for e in &map.entries {
        let off_str = e
            .offset
            .map(|o| format!("`0x{o:08X}`"))
            .unwrap_or_else(|| "-".to_string());
        let status_str = match e.status {
            AssetStatus::Identified => "identified",
            AssetStatus::Mapped => "mapped",
            AssetStatus::Opaque => "opaque",
            AssetStatus::Unknown => "unknown",
        };
        let fmt_str = if let Some(f) = &e.format_id {
            format!("`{f}`")
        } else if let Some(m) = &e.mapping {
            if let Some(oc) = m.opaque_class {
                format!("`{:?}/{:?}`", m.kind, oc)
            } else {
                format!("`{:?}`", m.kind)
            }
        } else {
            "-".to_string()
        };
        let ent_str = e
            .entropy
            .map(|h| format!("{h:.3}"))
            .unwrap_or_else(|| "-".to_string());
        let refs_str = if e.code_refs.is_empty() {
            "-".to_string()
        } else {
            e.code_refs
                .iter()
                .take(4)
                .map(|r| format!("`{r}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | `{}` | {} | {} | {} |",
            e.id, off_str, e.size, status_str, fmt_str, ent_str, refs_str
        );
    }

    out
}

// -----------------------------------------------------------------------------
// Map-Driven Export
// -----------------------------------------------------------------------------

/// Export every `identified` and `mapped` entry from `rom` using `map` into `out_dir`.
///
/// Refuses to write into a non-empty `out_dir` unless `force` is `true`.
/// Writes `<out_dir>/export-manifest.json` summarizing all exported files.
pub fn export_assets_with_map(
    rom: &[u8],
    map: &AssetMap,
    out_dir: &Path,
    force: bool,
) -> Result<ExportReport, String> {
    if out_dir.exists() {
        let is_non_empty = fs::read_dir(out_dir)
            .map_err(|e| format!("read dir {}: {e}", out_dir.display()))?
            .next()
            .is_some();
        if is_non_empty && !force {
            return Err(format!(
                "refusing to overwrite non-empty output directory {} without --force",
                out_dir.display()
            ));
        }
    }

    fs::create_dir_all(out_dir).map_err(|e| format!("create dir {}: {e}", out_dir.display()))?;

    let entry_by_id: HashMap<&str, &AssetEntry> =
        map.entries.iter().map(|e| (e.id.as_str(), e)).collect();

    let mut exported = Vec::new();
    let mut files_written = 0usize;

    for entry in &map.entries {
        if !matches!(entry.status, AssetStatus::Identified | AssetStatus::Mapped) {
            continue;
        }
        let off = entry
            .offset
            .ok_or_else(|| format!("entry {:?} is missing offset", entry.id))?
            as usize;
        let slice = entry_slice(rom, entry)?;
        let safe_id = sanitize_filename(&entry.id);
        let mut output_files = Vec::new();

        // 1. Archives -> extract members into <out_dir>/<id>/
        if matches!(
            entry.format_id.as_deref(),
            Some("archive.pack" | "archive.wayforward" | "archive.sfcd")
        ) {
            let parsed = parse_archive_at(rom, off)
                .map_err(|e| format!("parse archive {:?} at 0x{off:X}: {e}", entry.id))?;
            let members = extract_archive_entries(rom, &parsed)
                .map_err(|e| format!("extract archive {:?}: {e}", entry.id))?;
            let arch_dir = out_dir.join(&safe_id);
            fs::create_dir_all(&arch_dir)
                .map_err(|e| format!("create dir {}: {e}", arch_dir.display()))?;
            for (meta, payload) in members {
                let member_name = sanitize_filename(&meta.name);
                let rel = format!("{safe_id}/{member_name}.bin");
                let dst = arch_dir.join(format!("{member_name}.bin"));
                fs::write(&dst, payload).map_err(|e| format!("write {}: {e}", dst.display()))?;
                output_files.push(rel);
                files_written += 1;
            }
        }
        // 2. Mapped raw-image (4bpp/8bpp tiles with optional relations["palette"] or paired_with palette)
        else if let Some(mapping) = entry
            .mapping
            .as_ref()
            .filter(|m| m.kind == MappingKind::RawImage)
        {
            let bpp = param_u64(mapping, "bpp").unwrap_or(4);
            let cols = param_u64(mapping, "tiles_per_row").unwrap_or(16).max(1) as usize;
            if let Some(pal_id) = resolve_palette_id(mapping) {
                let pal_entry = entry_by_id
                    .get(pal_id)
                    .ok_or_else(|| format!("paired palette {pal_id:?} not found"))?;
                let pal_slice = entry_slice(rom, pal_entry)?;
                let palette =
                    decode_palette(pal_slice).map_err(|e| format!("decode palette {pal_id:?}: {e}"))?;
                let (w, h, rgba) = render_raw_tiles_rgba(slice, bpp, cols, &palette)?;
                let png = RawPngEncoder::encode_rgba8(w, h, &rgba)
                    .map_err(|e| format!("encode png for {:?}: {e}", entry.id))?;
                let rel = format!("{safe_id}.png");
                let dst = out_dir.join(&rel);
                fs::write(&dst, &png).map_err(|e| format!("write {}: {e}", dst.display()))?;
                output_files.push(rel);
                files_written += 1;
            } else {
                let rel = format!("{safe_id}.bin");
                let dst = out_dir.join(&rel);
                fs::write(&dst, slice).map_err(|e| format!("write {}: {e}", dst.display()))?;
                output_files.push(rel);
                files_written += 1;
            }
        }
        // 3. Mapped compressed or auto-identified BIOS compressed streams
        else if entry
            .mapping
            .as_ref()
            .is_some_and(|m| m.kind == MappingKind::Compressed)
            || entry
                .format_id
                .as_deref()
                .is_some_and(|f| f.starts_with("compress."))
        {
            let codec = entry
                .mapping
                .as_ref()
                .and_then(|m| param_str(m, "codec"))
                .or(entry.format_id.as_deref())
                .unwrap_or("lz77");
            let decoded = decode_compressed_slice(codec, slice)?;
            let rel = format!("{safe_id}.bin");
            let dst = out_dir.join(&rel);
            fs::write(&dst, &decoded).map_err(|e| format!("write {}: {e}", dst.display()))?;
            output_files.push(rel);
            files_written += 1;
        }
        // 4. Raw audio or M4A WaveData -> .wav
        else if entry
            .mapping
            .as_ref()
            .is_some_and(|m| m.kind == MappingKind::RawAudio)
            || entry.format_id.as_deref() == Some("audio.m4a-wavedata")
        {
            let encoding = entry
                .mapping
                .as_ref()
                .and_then(|m| param_str(m, "encoding"))
                .unwrap_or("wavedata");
            let rate = entry
                .mapping
                .as_ref()
                .and_then(|m| param_u64(m, "sample_rate"))
                .unwrap_or(13_379) as u32;
            let wav = decode_raw_audio_slice(encoding, rate, slice)?;
            let rel = format!("{safe_id}.wav");
            let dst = out_dir.join(&rel);
            fs::write(&dst, &wav).map_err(|e| format!("write {}: {e}", dst.display()))?;
            output_files.push(rel);
            files_written += 1;
        }
        // 5. Identified BGR555 palettes -> swatch .png
        else if entry.format_id.as_deref() == Some("gfx.gba-palette") {
            let palette = decode_palette(slice)
                .map_err(|e| format!("decode palette {:?}: {e}", entry.id))?;
            let (w, h, rgba) = render_palette_swatch_rgba(&palette);
            let png = RawPngEncoder::encode_rgba8(w, h, &rgba)
                .map_err(|e| format!("encode palette png {:?}: {e}", entry.id))?;
            let rel = format!("{safe_id}.png");
            let dst = out_dir.join(&rel);
            fs::write(&dst, &png).map_err(|e| format!("write {}: {e}", dst.display()))?;
            output_files.push(rel);
            files_written += 1;
        }
        // 6. All other identified/mapped entries -> raw .bin slice
        else {
            let rel = format!("{safe_id}.bin");
            let dst = out_dir.join(&rel);
            fs::write(&dst, slice).map_err(|e| format!("write {}: {e}", dst.display()))?;
            output_files.push(rel);
            files_written += 1;
        }

        exported.push(ExportedAsset {
            id: entry.id.clone(),
            status: entry.status,
            format_id: entry.format_id.clone(),
            offset: off as u64,
            size: entry.size,
            output_files,
        });
    }

    let report = ExportReport {
        schema: "gbakit.asset-export-manifest.v1".to_string(),
        rom_sha256: sha256_hex(rom),
        exported_entries: exported.len(),
        files_written,
        assets: exported,
    };

    let manifest_path = out_dir.join("export-manifest.json");
    let manifest_json = serde_json::to_string_pretty(&report)
        .map_err(|e| format!("serialize export manifest: {e}"))?;
    fs::write(&manifest_path, manifest_json)
        .map_err(|e| format!("write {}: {e}", manifest_path.display()))?;

    Ok(report)
}

fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "asset".to_string()
    } else {
        cleaned
    }
}

fn render_raw_tiles_rgba(
    data: &[u8],
    bpp: u64,
    tiles_per_row: usize,
    palette: &[[u8; 4]],
) -> Result<(u32, u32, Vec<u8>), String> {
    let tile_bytes = if bpp == 8 { 64usize } else { 32usize };
    let tile_count = data.len() / tile_bytes;
    if tile_count == 0 {
        return Err("no complete tiles in raw-image slice".into());
    }
    let cols = tiles_per_row.min(tile_count).max(1);
    let rows = tile_count.div_ceil(cols);
    let width = (cols * 8) as u32;
    let height = (rows * 8) as u32;
    let mut rgba = vec![0u8; (width as usize) * (height as usize) * 4];

    for t in 0..tile_count {
        let tile_slice = &data[t * tile_bytes..(t + 1) * tile_bytes];
        let indices = if bpp == 8 {
            decode_tile_8bpp(tile_slice).map_err(|e| e.to_string())?
        } else {
            decode_tile_4bpp(tile_slice).map_err(|e| e.to_string())?
        };
        let tx = (t % cols) * 8;
        let ty = (t / cols) * 8;
        for py in 0..8 {
            for px in 0..8 {
                let idx = indices[py * 8 + px] as usize;
                let col = palette.get(idx).copied().unwrap_or([0, 0, 0, 255]);
                let dst = ((ty + py) * (width as usize) + (tx + px)) * 4;
                rgba[dst..dst + 4].copy_from_slice(&col);
            }
        }
    }

    Ok((width, height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gba_compress::encode_lz77;
    use gba_gfx::rgb8_to_bgr555;
    use gba_rom::{compute_header_complement, pack_archive};

    fn make_synthetic_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 4096];
        rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
        rom[0xA0..0xAC].copy_from_slice(b"ASSETMAPTEST");
        rom[0xAC..0xB0].copy_from_slice(b"AMTE");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xB2] = 0x96;
        rom[0xBD] = compute_header_complement(&rom[0xA0..0xBD]);

        // Pointer table at 0x100 referencing palette (0x200) and LZ77 blob (0x300)
        rom[0x100..0x104].copy_from_slice(&(ROM_BUS_BASE + 0x200).to_le_bytes());
        rom[0x104..0x108].copy_from_slice(&(ROM_BUS_BASE + 0x300).to_le_bytes());

        // 16-color BGR555 palette at 0x200
        for i in 0..16 {
            let c = rgb8_to_bgr555((i * 16) as u8, ((15 - i) * 16) as u8, (i * 8) as u8);
            rom[0x200 + i * 2..0x200 + i * 2 + 2].copy_from_slice(&c.to_le_bytes());
        }

        // LZ77 compressed payload at 0x300
        let raw_payload = vec![0x42u8; 256];
        let lz = encode_lz77(&raw_payload).expect("encode lz77");
        rom[0x300..0x300 + lz.len()].copy_from_slice(&lz);

        // PACK archive at 0x500
        let pack = pack_archive(
            ArchiveFormat::Pack,
            &[("a", b"hello-pack-a".as_slice()), ("b", b"hello-pack-b".as_slice())],
        )
        .expect("pack archive");
        rom[0x500..0x500 + pack.len()].copy_from_slice(&pack);

        // Raw 4bpp tile at 0x700 (for manual mapped test)
        for i in 0..32 {
            rom[0x700 + i] = ((i % 16) as u8) | ((((i + 1) % 16) as u8) << 4);
        }

        // Trailing 0xFF padding at 0xC00..0x1000
        rom[0xC00..0x1000].fill(0xFF);
        rom
    }

    #[test]
    fn sha256_and_entropy_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(shannon_entropy(&[0xFF; 64]), 0.0);
        let uniform: Vec<u8> = (0..=255).collect();
        assert_eq!(shannon_entropy(&uniform), 8.0);
    }

    #[test]
    fn generate_merge_verify_markdown_and_export_roundtrip() {
        let rom = make_synthetic_rom();
        let map = generate_asset_map(&rom, &GenerateOptions { with_code_refs: true }, None);

        assert_eq!(map.schema, ASSET_MAP_SCHEMA);
        assert_eq!(map.schema_version, 1);
        assert_eq!(map.kit, "gbakit");
        assert_eq!(map.media.total_bytes, Some(rom.len() as u64));

        // 100% byte accounting
        let accounted: u64 = map.entries.iter().map(|e| e.size).sum();
        assert_eq!(accounted, rom.len() as u64);

        // Expected auto-detected entries present
        assert!(map.entries.iter().any(|e| e.format_id.as_deref() == Some("gba.rom-header")));
        assert!(map.entries.iter().any(|e| e.format_id.as_deref() == Some("archive.pack")));
        assert!(map.entries.iter().any(|e| e.format_id.as_deref() == Some("compress.bios-lz77")));
        assert!(map.entries.iter().any(|e| e.format_id.as_deref() == Some("gfx.gba-palette")));
        assert!(map
            .entries
            .iter()
            .any(|e| e.status == AssetStatus::Opaque
                && e.mapping.as_ref().and_then(|m| m.opaque_class) == Some(OpaqueClass::Padding)));

        // Code refs and structured pointer references attached to palette at 0x200
        let pal = map
            .entries
            .iter()
            .find(|e| e.offset == Some(0x200))
            .expect("palette at 0x200");
        assert!(pal.code_refs.contains(&"0x08000100".to_string()));
        assert_eq!(pal.references.len(), 1);
        assert_eq!(
            pal.references[0],
            PointerReference {
                at: 0x100,
                width: 4,
                endian: Endian::Le,
                form: "gba_rom_ptr".to_string(),
                shift: None,
                add: Some(i64::from(ROM_BUS_BASE)),
                bank: None,
                note: None,
            }
        );

        // Trailing ROM padding auto-populated in free_space
        assert_eq!(map.free_space.len(), 1);
        assert_eq!(
            map.free_space[0],
            FreeSpaceRegion {
                offset: 0xC00,
                size: 0x400,
                bank: None,
                note: Some("trailing ROM padding".to_string()),
            }
        );

        // Verify passes
        let vr = verify_asset_map(&rom, &map);
        assert!(vr.valid, "verify errors: {:?}", vr.errors);
        assert_eq!(vr.accounted_bytes, rom.len() as u64);
        assert!(vr.coverage_pct > 20.0);

        // Add a curated mapped raw-image entry paired with the palette and merge
        let mut curated = map.clone();
        let mut params = serde_json::Map::new();
        params.insert("bpp".to_string(), serde_json::json!(4));
        params.insert("tiles_per_row".to_string(), serde_json::json!(1));
        curated.entries.push(AssetEntry {
            id: "hero_tile".to_string(),
            path: None,
            offset: Some(0x700),
            lba: None,
            size: 32,
            reserved_capacity: Some(64),
            sha256: Some(sha256_hex(&rom[0x700..0x720])),
            status: AssetStatus::Mapped,
            format_id: None,
            entropy: None,
            code_refs: Vec::new(),
            references: Vec::new(),
            mapping: Some(AssetMapping {
                kind: MappingKind::RawImage,
                params: Some(params),
                paired_with: Some(StringOrVec::One(pal.id.clone())),
                relations: BTreeMap::new(),
                inner_format: None,
                opaque_class: None,
                evidence: Some("manual unit test tile".to_string()),
            }),
            verified: None,
        });

        let merged = generate_asset_map(
            &rom,
            &GenerateOptions { with_code_refs: false },
            Some(&curated),
        );
        let merged_hero = merged
            .entries
            .iter()
            .find(|e| e.id == "hero_tile" && e.status == AssetStatus::Mapped)
            .expect("hero_tile preserved across merge");
        assert_eq!(merged_hero.reserved_capacity, Some(64));
        let merged_vr = verify_asset_map(&rom, &merged);
        assert!(merged_vr.valid, "merged verify errors: {:?}", merged_vr.errors);
        assert_eq!(merged_vr.mapped_bytes, 32);

        // Markdown rendering
        let md = render_asset_map_markdown(&merged, Some(&merged_vr));
        assert!(md.contains("formatkit.asset-map.v1"));
        assert!(md.contains("hero_tile"));
        assert!(md.contains("Coverage Summary"));

        // Export assets + --force guard
        let out_dir = std::env::temp_dir().join(format!(
            "gbakit_asset_map_unit_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&out_dir);
        let exp = export_assets_with_map(&rom, &merged, &out_dir, false).expect("export");
        assert!(exp.files_written >= 4);
        assert!(out_dir.join("hero_tile.png").is_file());
        assert!(out_dir.join("export-manifest.json").is_file());

        // Non-empty dir without --force fails
        let err = export_assets_with_map(&rom, &merged, &out_dir, false).unwrap_err();
        assert!(err.contains("refusing to overwrite"));

        // With --force succeeds
        export_assets_with_map(&rom, &merged, &out_dir, true).expect("export with force");
        let _ = fs::remove_dir_all(&out_dir);
    }

    #[test]
    fn free_space_shared_and_relocation_preserved_across_merge() {
        let rom = make_synthetic_rom();
        let mut map = generate_asset_map(&rom, &GenerateOptions { with_code_refs: true }, None);

        // Curate free_space, shared resources, and entry relocation metadata
        map.free_space.push(FreeSpaceRegion {
            offset: 0x800,
            size: 0x100,
            bank: Some(0),
            note: Some("curated scratch slack".to_string()),
        });
        let mut shared = serde_json::Map::new();
        shared.insert(
            "charmaps".to_string(),
            serde_json::json!({ "default": { "0x00": " " } }),
        );
        shared.insert(
            "palettes".to_string(),
            serde_json::json!({ "ui": "pal16_00000200" }),
        );
        map.shared = Some(shared.clone());

        // Curate reserved_capacity and a custom PointerReference on the identified LZ77 entry
        let lz_entry = map
            .entries
            .iter_mut()
            .find(|e| e.offset == Some(0x300))
            .expect("lz77 at 0x300");
        lz_entry.reserved_capacity = Some(512);
        lz_entry.references.push(PointerReference {
            at: 0x110,
            width: 4,
            endian: Endian::Le,
            form: "gba_rom_ptr".to_string(),
            shift: Some(0),
            add: Some(i64::from(ROM_BUS_BASE)),
            bank: Some(0),
            note: Some("curated table pointer".to_string()),
        });

        let merged = generate_asset_map(
            &rom,
            &GenerateOptions { with_code_refs: true },
            Some(&map),
        );

        // Curated free_space preserved without duplicating trailing padding
        assert_eq!(merged.free_space.len(), 2);
        assert!(merged
            .free_space
            .iter()
            .any(|r| r.offset == 0x800 && r.size == 0x100 && r.bank == Some(0)));
        assert_eq!(
            merged
                .free_space
                .iter()
                .filter(|r| r.offset == 0xC00 && r.size == 0x400)
                .count(),
            1
        );
        assert_eq!(merged.shared, Some(shared));

        // Curated reserved_capacity and references preserved on re-detected LZ77 entry
        let merged_lz = merged
            .entries
            .iter()
            .find(|e| e.offset == Some(0x300))
            .expect("merged lz77");
        assert_eq!(merged_lz.reserved_capacity, Some(512));
        assert!(merged_lz.references.iter().any(|r| r.at == 0x104));
        assert!(merged_lz
            .references
            .iter()
            .any(|r| r.at == 0x110 && r.note.as_deref() == Some("curated table pointer")));

        // JSON roundtrip preserves all v1.1 fields
        let json = serde_json::to_string_pretty(&merged).expect("serialize");
        let parsed: AssetMap = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, merged);
        let vr = verify_asset_map(&rom, &parsed);
        assert!(vr.valid, "verify errors: {:?}", vr.errors);
    }

    #[test]
    fn multi_companion_relations_and_paired_with_resolution() {
        let one = StringOrVec::One("pal_a".to_string());
        assert_eq!(one.first(), Some("pal_a"));
        assert_eq!(one.as_slice_iter().collect::<Vec<_>>(), vec!["pal_a"]);

        let many = StringOrVec::Many(vec!["pal_a".to_string(), "pal_b".to_string()]);
        assert_eq!(many.first(), Some("pal_a"));
        assert_eq!(
            many.as_slice_iter().collect::<Vec<_>>(),
            vec!["pal_a", "pal_b"]
        );

        // JSON untagged roundtrip
        assert_eq!(serde_json::to_string(&one).unwrap(), "\"pal_a\"");
        assert_eq!(
            serde_json::to_string(&many).unwrap(),
            "[\"pal_a\",\"pal_b\"]"
        );
        assert_eq!(
            serde_json::from_str::<StringOrVec>("\"pal_a\"").unwrap(),
            one
        );
        assert_eq!(
            serde_json::from_str::<StringOrVec>("[\"pal_a\",\"pal_b\"]").unwrap(),
            many
        );

        // Test relations["palette"] precedence over paired_with during verify + export
        let rom = make_synthetic_rom();
        let mut map = generate_asset_map(&rom, &GenerateOptions::default(), None);
        let pal_id = map
            .entries
            .iter()
            .find(|e| e.offset == Some(0x200))
            .expect("palette")
            .id
            .clone();

        let mut relations = BTreeMap::new();
        relations.insert("palette".to_string(), StringOrVec::One(pal_id.clone()));
        relations.insert(
            "tilemap".to_string(),
            StringOrVec::Many(vec!["tm_a".to_string(), "tm_b".to_string()]),
        );

        let mut params = serde_json::Map::new();
        params.insert("bpp".to_string(), serde_json::json!(4));
        params.insert("tiles_per_row".to_string(), serde_json::json!(1));

        map.entries.push(AssetEntry {
            id: "rel_tile".to_string(),
            path: None,
            offset: Some(0x700),
            lba: None,
            size: 32,
            reserved_capacity: None,
            sha256: Some(sha256_hex(&rom[0x700..0x720])),
            status: AssetStatus::Mapped,
            format_id: None,
            entropy: None,
            code_refs: Vec::new(),
            references: Vec::new(),
            mapping: Some(AssetMapping {
                kind: MappingKind::RawImage,
                params: Some(params.clone()),
                // Intentionally bogus paired_with to prove relations["palette"] takes priority
                paired_with: Some(StringOrVec::Many(vec![
                    "nonexistent_pal".to_string(),
                    "other_pal".to_string(),
                ])),
                relations,
                inner_format: None,
                opaque_class: None,
                evidence: Some("relations precedence test".to_string()),
            }),
            verified: None,
        });

        // Also add a second tile using paired_with: Many([pal_id, ...]) fallback
        map.entries.push(AssetEntry {
            id: "paired_many_tile".to_string(),
            path: None,
            offset: Some(0x720),
            lba: None,
            size: 32,
            reserved_capacity: None,
            sha256: Some(sha256_hex(&rom[0x720..0x740])),
            status: AssetStatus::Mapped,
            format_id: None,
            entropy: None,
            code_refs: Vec::new(),
            references: Vec::new(),
            mapping: Some(AssetMapping {
                kind: MappingKind::RawImage,
                params: Some(params),
                paired_with: Some(StringOrVec::Many(vec![
                    pal_id,
                    "unused_secondary".to_string(),
                ])),
                relations: BTreeMap::new(),
                inner_format: None,
                opaque_class: None,
                evidence: Some("paired_with Many fallback test".to_string()),
            }),
            verified: None,
        });

        let merged = generate_asset_map(&rom, &GenerateOptions::default(), Some(&map));
        let vr = verify_asset_map(&rom, &merged);
        assert!(vr.valid, "verify errors: {:?}", vr.errors);

        let out_dir = std::env::temp_dir().join(format!(
            "gbakit_asset_map_relations_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&out_dir);
        let exp = export_assets_with_map(&rom, &merged, &out_dir, false).expect("export");
        assert!(exp.files_written >= 5);
        assert!(out_dir.join("rel_tile.png").is_file());
        assert!(out_dir.join("paired_many_tile.png").is_file());
        let _ = fs::remove_dir_all(&out_dir);
    }

    #[test]
    fn verify_rejects_out_of_bounds_free_space_and_references() {
        let rom = make_synthetic_rom();
        let mut map = generate_asset_map(&rom, &GenerateOptions::default(), None);

        // Add out-of-bounds free_space region
        map.free_space.push(FreeSpaceRegion {
            offset: rom.len() as u64 - 16,
            size: 32,
            bank: None,
            note: Some("oob".to_string()),
        });

        // Add out-of-bounds and zero-width references on the first entry
        map.entries[0].references.push(PointerReference {
            at: rom.len() as u64 - 2,
            width: 4,
            endian: Endian::Le,
            form: "gba_rom_ptr".to_string(),
            shift: None,
            add: Some(i64::from(ROM_BUS_BASE)),
            bank: None,
            note: None,
        });
        map.entries[0].references.push(PointerReference {
            at: 0x100,
            width: 0,
            endian: Endian::Be,
            form: "invalid_width".to_string(),
            shift: None,
            add: None,
            bank: None,
            note: None,
        });

        let vr = verify_asset_map(&rom, &map);
        assert!(!vr.valid);
        assert!(vr.errors.iter().any(|e| e.contains("free_space")));
        assert!(vr.errors.iter().any(|e| e.contains("references[0]") && e.contains("exceeds ROM length")));
        assert!(vr.errors.iter().any(|e| e.contains("references[1]") && e.contains("invalid width 0")));
    }
}
