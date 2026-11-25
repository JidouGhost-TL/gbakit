//! GBA DirectSound 16-byte WaveData header parsing, 8-bit signed PCM extraction,
//! and M4A/Sappy sound engine decompilation and MIDI/WAV export.

#![deny(unsafe_code)]

pub mod ead_sma_pool;
pub mod engine_sw_pool;
pub mod gax_pool;
pub mod konami_pool;
pub mod krawall_pool;
pub mod logik_pool;
pub mod m4a;
pub mod musyx_pool;
pub mod paragon5_pool;
pub mod rare_pool;
pub mod rd1_pool;
pub mod replace;
pub mod sf2;
pub mod sonix_pool;
pub mod southpaw_pool;

pub use cosmigo_sonix;
pub use ead_sma;
pub use engine_sw_audio;
pub use gax;
pub use konami_kce;
pub use krawall;
pub use logik_state;
pub use musyx;
pub use paragon5;
pub use rare_audio;
pub use rd1_kaede;
pub use southpaw_audio;
pub use ::m4a as m4a_core;

pub use ead_sma_pool::*;
pub use engine_sw_pool::*;
pub use gax_pool::*;
pub use konami_pool::*;
pub use krawall_pool::*;
pub use logik_pool::*;
pub use musyx_pool::*;
pub use paragon5_pool::*;
pub use rare_pool::*;
pub use rd1_pool::*;
pub use sonix_pool::*;
pub use southpaw_pool::*;

#[cfg(test)]
mod tests;

pub use formatkit_core::Pcm;
use formatkit_core::{Error, Result};

pub use ::m4a::WaveDataHeader;
pub use m4a::*;
pub use replace::*;
pub use sf2::*;

/// Detected sound engine across standard GBA drivers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectedSoundEngine {
    /// Nintendo MusicPlayer2000 (M4A / Sappy)
    M4a(M4aEngineScan),
    /// Shin'en GAX Sound Engine
    Gax(gax::version::GaxVersion),
    /// Factor 5 MusyX Sound System
    Musyx(musyx::version::MusyXVersion),
    /// Krawall / ProTracker MOD / Scream Tracker 3 Engine
    Krawall(Option<krawall::KrawallVersion>),
    /// Rareware GBA DirectSound Engine
    Rare(Option<rare_audio::RareAudioVersion>),
    /// Konami KCE Custom DirectSound Engine (KCEH / KCEJ)
    Konami(KonamiEngineVariant, Option<konami_kce::KonamiKceVersion>),
    /// Logik State GBAModPlay Engine (v1/v2/v3, PCM8 + 4-bit IMA ADPCM)
    LogikState(Option<logik_state::LogikVersion>),
    /// Paragon 5 GBA Tracker Engine (GBASS v1/v2)
    Paragon5(Option<paragon5::Paragon5Version>),
    /// Nintendo R&D1 "Kaede" Standard MIDI Engine
    Rd1(Option<rd1_kaede::KaedeVersion>),
    /// Southpaw GBA DirectSound SFX Engine
    Southpaw(Option<southpaw_audio::SouthpawVersion>),
    /// Nintendo EAD/SRD Super Mario Advance 2–4 & Zelda ALttP DirectSound Engine
    EadSma(ead_sma::EadSmaVersion),
    /// Engine Software / Bob Koon EIRE v2.0 DirectSound Engine
    EngineSw(engine_sw_audio::EngineSwVersion),
    /// Cosmigo Sonix Sound System (v1.09c / v1.096 + GSM / v1.16 + GSM)
    CosmigoSonix(cosmigo_sonix::SonixVersion),
    /// Unrecognized or custom direct sound driver
    Custom,
}

/// Detect the sound engine utilized by a GBA ROM.
pub fn detect_sound_engine(bytes: &[u8]) -> DetectedSoundEngine {
    if let Some(m4a) = scan_m4a_engine(bytes) {
        DetectedSoundEngine::M4a(m4a)
    } else if let Some(gax_ver) = gax::version::GaxVersion::detect_from_bytes(bytes) {
        DetectedSoundEngine::Gax(gax_ver)
    } else if let Some(musyx_ver) = musyx::version::MusyXVersion::detect_from_bytes(bytes) {
        DetectedSoundEngine::Musyx(musyx_ver)
    } else if let Some(ead_ver) = ead_sma::EadSmaVersion::detect_from_bytes(bytes) {
        DetectedSoundEngine::EadSma(ead_ver)
    } else if let Some(esw_ver) = engine_sw_audio::EngineSwVersion::detect_from_bytes(bytes) {
        DetectedSoundEngine::EngineSw(esw_ver)
    } else if let Some(sonix_ver) = cosmigo_sonix::SonixVersion::detect_from_bytes(bytes) {
        DetectedSoundEngine::CosmigoSonix(sonix_ver)
    } else if let Some(krawall_ver) = krawall::KrawallVersion::detect_from_bytes(bytes) {
        DetectedSoundEngine::Krawall(Some(krawall_ver))
    } else if krawall_pool::detect_krawall_fallback(bytes) {
        DetectedSoundEngine::Krawall(None)
    } else if let Some(rare_ver) = rare_audio::RareAudioVersion::detect_from_bytes(bytes) {
        DetectedSoundEngine::Rare(Some(rare_ver))
    } else if rare_audio::RareAudioVersion::has_rare_signature(bytes) {
        DetectedSoundEngine::Rare(None)
    } else if let Some((variant, konami_ver)) = konami_pool::detect_konami_engine_with_version(bytes)
    {
        DetectedSoundEngine::Konami(variant, konami_ver)
    } else if let Some(logik_ver) = logik_state::LogikVersion::detect_from_bytes(bytes) {
        DetectedSoundEngine::LogikState(Some(logik_ver))
    } else if logik_pool::has_logik_signature(bytes) {
        DetectedSoundEngine::LogikState(None)
    } else if let Some(p5_ver) = paragon5::Paragon5Version::detect_from_bytes(bytes) {
        DetectedSoundEngine::Paragon5(Some(p5_ver))
    } else if paragon5_pool::has_paragon5_descriptors(bytes) {
        DetectedSoundEngine::Paragon5(None)
    } else if detect_rd1_engine(bytes) {
        DetectedSoundEngine::Rd1(rd1_kaede::KaedeVersion::detect_from_bytes(bytes))
    } else if let Some(sp_ver) = southpaw_audio::SouthpawVersion::detect_from_bytes(bytes) {
        DetectedSoundEngine::Southpaw(Some(sp_ver))
    } else if southpaw_pool::has_southpaw_sample_table(bytes) {
        DetectedSoundEngine::Southpaw(None)
    } else {
        DetectedSoundEngine::Custom
    }
}

/// Size of the standard GBA DirectSound WaveData header in bytes.
pub const WAVE_HEADER_SIZE: usize = ::m4a::WAVE_DATA_HEADER_SIZE;

/// Extract the 8-bit signed PCM payload following a 16-byte `WaveDataHeader`.
///
/// Supports both uncompressed 8-bit signed PCM (`wave_type == 0`) and
/// Pokemon Gen 3 4-bit quadratic DPCM (`wave_type != 0`) via [`m4a::M4aSampleView`].
pub fn extract_pcm_i8(bytes: &[u8], max_samples: usize) -> Result<(WaveDataHeader, Vec<i8>)> {
    let view = ::m4a::M4aSampleView::from_wavdata_bytes(bytes).map_err(|e| match e {
        ::m4a::Error::UnexpectedEof => Error::Truncated {
            offset: 0,
            needed: WAVE_HEADER_SIZE,
            available: bytes.len(),
        },
        _ => Error::InvalidField {
            what: "wavedata_header",
            value: 0,
        },
    })?;
    let n = view.len();
    if n > max_samples {
        return Err(Error::ResourceLimit {
            resource: "wavedata_pcm_samples",
            requested: n as u64,
            limit: max_samples as u64,
        });
    }
    let pcm = if view.header.is_compressed_dpcm() {
        ::m4a::decode_dpcm_vec(view.pcm_bytes, n).map_err(|_| Error::Truncated {
            offset: WAVE_HEADER_SIZE,
            needed: ::m4a::dpcm_encoded_len(n),
            available: view.pcm_bytes.len(),
        })?
    } else {
        view.logical_pcm_bytes().iter().map(|&b| b as i8).collect()
    };
    Ok((view.header, pcm))
}

/// Extract the audio payload following a 16-byte `WaveDataHeader` as standard
/// formatkit 16-bit signed mono [`Pcm`] audio scaled from native 8-bit samples.
pub fn extract_pcm(bytes: &[u8], max_samples: usize) -> Result<(WaveDataHeader, Pcm)> {
    let (hdr, raw) = extract_pcm_i8(bytes, max_samples)?;
    let rate = hdr.base_sample_rate_hz().max(1);
    let samples16: Vec<i16> = raw.into_iter().map(|s| (s as i16) << 8).collect();
    Ok((hdr, Pcm::new(rate, samples16)))
}

/// Encode 8-bit signed PCM samples as standard 16-bit uncompressed mono RIFF WAV.
pub fn encode_wav_i16_mono(sample_rate: u32, pcm_i8: &[i8]) -> Result<Vec<u8>> {
    let rate = if sample_rate == 0 { 13_379 } else { sample_rate };
    let data_bytes = (pcm_i8.len() as u64).checked_mul(2).ok_or(Error::ResourceLimit {
        resource: "wav_data_bytes",
        requested: pcm_i8.len() as u64,
        limit: u64::MAX,
    })?;
    let header = formatkit_core::wav_header_pcm(rate, 1, 16, data_bytes)?;
    let mut out = Vec::with_capacity(44 + pcm_i8.len() * 2);
    out.extend_from_slice(&header);
    for &s in pcm_i8 {
        let s16 = (s as i16) << 8;
        out.extend_from_slice(&s16.to_le_bytes());
    }
    Ok(out)
}

/// Extract DirectSound sample and encode into a 16-bit uncompressed mono RIFF WAV.
pub fn export_wavedata_to_wav16(bytes: &[u8], max_samples: usize) -> Result<(WaveDataHeader, Vec<u8>)> {
    let (hdr, pcm) = extract_pcm_i8(bytes, max_samples)?;
    let rate = hdr.base_sample_rate_hz().max(1);
    let wav = encode_wav_i16_mono(rate, &pcm)?;
    Ok((hdr, wav))
}
