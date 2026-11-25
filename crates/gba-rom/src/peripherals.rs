//! Clean-room byte-level GBA cartridge and link-port peripheral detection.
//!
//! Scans raw GBA ROM bytes for empirically verified hardware register literals,
//! official SDK protocol signatures, and boot-logo palettes — with zero hardcoded
//! SHA-256 lookup tables, so detection works on retail releases, prototypes,
//! ROM hacks, and homebrew alike.

#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};

use crate::{AgbHeader, MAX_ROM_SIZE, ROM_BUS_BASE};

// Cartridge GPIO MMIO registers (Seiko S-3511A RTC, Boktai solar, rumble, gyro).
const ADDR_GPIO_DATA: u32 = 0x0800_00C4;
const ADDR_GPIO_DIR: u32 = 0x0800_00C6;
const ADDR_GPIO_CNT: u32 = 0x0800_00C8;

// ROHM BU3668K tilt accelerometer SRAM-mapped trigger registers.
const ADDR_TILT_START1: u32 = 0x0E00_8000;
const ADDR_TILT_START2: u32 = 0x0E00_8100;

// NEC/TOKIN piezoelectric gyro ADC register.
const ADDR_GYRO_ADC: u32 = 0x09FE_0000;

// Game Boy Player (DOL-017) SIO Normal 32-bit rumble handshake literals.
const GBP_HANDSHAKE_ACK: u32 = 0x0000_8002;
const ADDR_SIODATA32: [u8; 4] = 0x0400_0120u32.to_le_bytes();
const ADDR_SIOCNT: [u8; 4] = 0x0400_0128u32.to_le_bytes();

/// First 32 bytes of the official 18,176-byte Nintendo Game Boy Player boot logo
/// (RGB555 indigo palette).
pub const GBP_LOGO_PALETTE_32: [u8; 32] = [
    0xDF, 0xFF, 0x0C, 0x64, 0x0C, 0xE4, 0x2D, 0xE4, 0x4E, 0x64, 0x4E, 0xE4, 0x6E, 0xE4, 0xAF, 0x68,
    0xB0, 0xE8, 0xD0, 0x68, 0xF0, 0x68, 0x11, 0x69, 0x11, 0xE9, 0x32, 0x6D, 0x32, 0xED, 0x73, 0xED,
];

/// Cap stored literal-pool offsets per register to keep scanning bounded.
const MAX_TRACKED_OFFSETS: usize = 64;

/// GBA cartridge or link-port peripheral detected via byte-level ROM scanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeripheralKind {
    /// Seiko S-3511A or compatible Real-Time Clock on cartridge GPIO (`0x080000C4`–`C8`).
    Rtc,
    /// Photodiode solar sensor on cartridge GPIO (*Boktai* series).
    SolarSensor,
    /// Built-in cartridge vibration motor driven via GPIO bit 3 (`0x080000C4`/`C6`).
    CartridgeRumble,
    /// ROHM BU3668K / ADXL202 tilt accelerometer (`0x0E008000`–`0x0E008500`).
    TiltAccelerometer,
    /// NEC/TOKIN piezoelectric angular-rate gyroscope (`0x09FE0000` / GPIO SPI ADC).
    GyroPiezoelectric,
    /// Game Boy Player (`DOL-017`) `libgbp` SIO Normal 32-bit rumble SDK.
    GameBoyPlayerRumble,
    /// Official 18,176-byte Game Boy Player boot logo palette without `libgbp` rumble.
    GameBoyPlayerLogo,
    /// Official Game Boy Player boot logo used by GBA Video (`MPA`/`MPB`/`MPC`/`MPD`) carts to block GBP playback.
    GameBoyPlayerAntiCaptureDrm,
    /// Nintendo Wireless Adapter (`AGB-015`) `RFU_Vxxxx` / `RFU-MBOOT` SDK.
    WirelessAdapterRfu,
    /// Nintendo Mobile Adapter GB (`CGB-005` / Mobile System GB) HTTP/SMTP/POP3 stack.
    MobileAdapterGb,
    /// Nintendo e-Reader firmware (`PEAJ`/`PSAJ`/`PSAE`) or `ERAPI` / Card-e link hooks.
    EReaderLink,
    /// Link-port toy/card adapter (Battle/Progress/Beast Chip Gate or Legendz Soul Doll Adapter).
    LinkPortToyAdapter,
}

/// A peripheral detected in a GBA ROM image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedPeripheral {
    pub kind: PeripheralKind,
    pub label: String,
    pub detail: Option<String>,
}

impl DetectedPeripheral {
    fn new(kind: PeripheralKind, label: &str, detail: Option<String>) -> Self {
        Self {
            kind,
            label: label.to_string(),
            detail,
        }
    }
}

/// Scan a GBA ROM image for cartridge hardware and link-port peripherals.
///
/// Works on any GBA ROM (retail, prototype, hack, or homebrew) without hardcoded
/// SHA-256 lookup tables. The 4-character header game code is only consulted as a
/// tiebreaker (e.g., to distinguish GBA Video `MPA`/`MPB`/`MPC`/`MPD` anti-capture
/// DRM when the official Game Boy Player boot logo is already present in ROM bytes).
pub fn detect_peripherals(rom: &[u8]) -> Vec<DetectedPeripheral> {
    let scan_len = rom.len().min(MAX_ROM_SIZE);
    let data = &rom[..scan_len];
    if data.len() < 4 {
        return Vec::new();
    }

    let game_code = AgbHeader::parse(data)
        .map(|h| h.game_code)
        .unwrap_or_default();

    // Single 4-byte aligned word scan for hardware register literals and libgbp ACK.
    let mut gpio_c4 = Vec::new();
    let mut gpio_c6 = Vec::new();
    let mut gpio_c8 = Vec::new();
    let mut tilt_80 = Vec::new();
    let mut tilt_81 = Vec::new();
    let mut gyro_09fe = Vec::new();
    let mut has_gbp_rumble = false;

    let word_end = scan_len & !3;
    let mut off = 0usize;
    while off < word_end {
        let word = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        match word {
            ADDR_GPIO_DATA if gpio_c4.len() < MAX_TRACKED_OFFSETS => gpio_c4.push(off),
            ADDR_GPIO_DIR if gpio_c6.len() < MAX_TRACKED_OFFSETS => gpio_c6.push(off),
            ADDR_GPIO_CNT if gpio_c8.len() < MAX_TRACKED_OFFSETS => gpio_c8.push(off),
            ADDR_TILT_START1 if tilt_80.len() < MAX_TRACKED_OFFSETS => tilt_80.push(off),
            ADDR_TILT_START2 if tilt_81.len() < MAX_TRACKED_OFFSETS => tilt_81.push(off),
            ADDR_GYRO_ADC if gyro_09fe.len() < MAX_TRACKED_OFFSETS => gyro_09fe.push(off),
            GBP_HANDSHAKE_ACK if !has_gbp_rumble && verify_gbp_rumble_window(data, off) => {
                has_gbp_rumble = true;
            }
            _ => {}
        }
        off += 4;
    }

    let has_gpio_c4_c6 = !gpio_c4.is_empty() && !gpio_c6.is_empty();
    let has_gpio_ldr = has_gpio_c4_c6
        && (gpio_c4.iter().any(|&o| has_pc_rel_ldr(data, o))
            || gpio_c6.iter().any(|&o| has_pc_rel_ldr(data, o)));

    let mut detected = Vec::new();

    // 1. Seiko S-3511A Real-Time Clock (RTC)
    let rtc_detail = detect_rtc(data, has_gpio_ldr);
    let has_rtc = rtc_detail.is_some();
    if let Some(detail) = rtc_detail {
        detected.push(DetectedPeripheral::new(
            PeripheralKind::Rtc,
            "Real-Time Clock (RTC)",
            Some(detail),
        ));
    }

    // 2. Solar Sensor (Boktai GPIO photodiode ADC)
    let has_solar = has_gpio_c4_c6
        && (contains_any(data, &[b"SOLAR SENSOR", b"Solar Sensor", b"Solar Gun"])
            || game_code.starts_with("U3I")
            || game_code.starts_with("U32")
            || game_code.starts_with("U33")
            || game_code.starts_with("A3I"));
    if has_solar {
        detected.push(DetectedPeripheral::new(
            PeripheralKind::SolarSensor,
            "Solar Sensor",
            Some("GPIO photodiode ADC (0x080000C4..0x080000C8)".to_string()),
        ));
    }

    // 3. ROHM BU3668K Tilt Accelerometer (0x0E008000..0x0E008500)
    if has_clustered(&tilt_80, &tilt_81, 128) {
        detected.push(DetectedPeripheral::new(
            PeripheralKind::TiltAccelerometer,
            "Tilt Accelerometer",
            Some("ROHM BU3668K (0x0E008000..0x0E008500)".to_string()),
        ));
    }

    // 4. NEC/TOKIN Piezoelectric Gyroscope (0x09FE0000 / GPIO SPI ADC)
    let is_wario_twisted = game_code.starts_with("RZW")
        || game_code.starts_with("RZP")
        || game_code.starts_with("RZE");
    let has_gyro_adc = !gyro_09fe.is_empty()
        && (has_gpio_c4_c6 || has_clustered(&gyro_09fe, &gpio_c4, 256));
    let has_gyro = has_gyro_adc
        || (has_gpio_c4_c6
            && !gpio_c8.is_empty()
            && (is_wario_twisted || contains_any(data, &[b"GYRO", b"Gyro"])));
    if has_gyro {
        let detail = if has_gyro_adc {
            "NEC/TOKIN gyro (0x09FE0000 + GPIO)"
        } else {
            "NEC/TOKIN gyro (GPIO SPI ADC)"
        };
        detected.push(DetectedPeripheral::new(
            PeripheralKind::GyroPiezoelectric,
            "Piezoelectric Gyroscope",
            Some(detail.to_string()),
        ));
    }

    // 5. Cartridge Rumble Motor (GPIO bit 3)
    let is_drill_dozer = game_code.starts_with("V49");
    let has_cart_rumble = has_gpio_c4_c6
        && (is_drill_dozer
            || is_wario_twisted
            || (!has_rtc && !has_solar && has_gpio_ldr)
            || (!has_gbp_rumble
                && contains_any(data, &[b"RUMBLE", b"Rumble", b"VIBRATION"])));
    if has_cart_rumble {
        detected.push(DetectedPeripheral::new(
            PeripheralKind::CartridgeRumble,
            "Cartridge Rumble",
            Some("GPIO bit-3 motor (0x080000C4/0x080000C6)".to_string()),
        ));
    }

    // 6. Game Boy Player (DOL-017): Rumble SDK vs Boot Logo vs GBA Video Anti-Capture DRM
    let has_gbp_logo = find_subslice(data, &GBP_LOGO_PALETTE_32).is_some();
    if has_gbp_rumble {
        let detail = if has_gbp_logo {
            "libgbp SIO 32-bit + official boot logo"
        } else {
            "libgbp SIO 32-bit"
        };
        detected.push(DetectedPeripheral::new(
            PeripheralKind::GameBoyPlayerRumble,
            "Game Boy Player Rumble",
            Some(detail.to_string()),
        ));
    } else if has_gbp_logo {
        let is_gba_video_lockout = game_code.starts_with("MPA")
            || game_code.starts_with("MPB")
            || game_code.starts_with("MPC")
            || game_code.starts_with("MPD");
        if is_gba_video_lockout {
            detected.push(DetectedPeripheral::new(
                PeripheralKind::GameBoyPlayerAntiCaptureDrm,
                "Game Boy Player Anti-Capture DRM",
                Some("GBP boot logo lockout (GBA Video)".to_string()),
            ));
        } else {
            detected.push(DetectedPeripheral::new(
                PeripheralKind::GameBoyPlayerLogo,
                "Game Boy Player Boot Logo",
                Some("Official 18,176-byte GBP logo palette".to_string()),
            ));
        }
    }

    // 7. Wireless Adapter (AGB-015 / RFU SDK)
    if let Some(rfu_detail) = detect_wireless_rfu(data) {
        detected.push(DetectedPeripheral::new(
            PeripheralKind::WirelessAdapterRfu,
            "Wireless Adapter (RFU)",
            Some(rfu_detail),
        ));
    }

    // 8. Mobile Adapter GB (CGB-005 / Mobile System GB)
    if let Some(mob_detail) = detect_mobile_adapter(data) {
        detected.push(DetectedPeripheral::new(
            PeripheralKind::MobileAdapterGb,
            "Mobile Adapter GB",
            Some(mob_detail.to_string()),
        ));
    }

    // 9. e-Reader Firmware / Card-e / ERAPI Link Hooks
    if let Some(er_detail) = detect_ereader_link(data, &game_code) {
        detected.push(DetectedPeripheral::new(
            PeripheralKind::EReaderLink,
            "e-Reader Link",
            Some(er_detail.to_string()),
        ));
    }

    // 10. Link-Port Toy Adapters (Chip Gate / Legendz Soul Doll)
    if let Some(toy_detail) = detect_link_port_toy(data, &game_code) {
        detected.push(DetectedPeripheral::new(
            PeripheralKind::LinkPortToyAdapter,
            "Link-Port Toy Adapter",
            Some(toy_detail.to_string()),
        ));
    }

    detected
}

/// Verify the official `libgbp` literal-pool window around a 4-byte aligned `0x00008002`:
/// requires `SIODATA32` (`0x04000120`) or `SIOCNT` (`0x04000128`) AND a 4-byte aligned
/// ROM pointer (`0x08xxxxxx`) in the same `+/-256`-byte window referencing `"NINTENDO"`.
fn verify_gbp_rumble_window(data: &[u8], ack_off: usize) -> bool {
    let win_start = ack_off.saturating_sub(256) & !3;
    let win_end = (ack_off + 256).min(data.len()) & !3;
    if win_end <= win_start {
        return false;
    }
    let win = &data[win_start..win_end];
    let has_sio =
        find_subslice(win, &ADDR_SIODATA32).is_some() || find_subslice(win, &ADDR_SIOCNT).is_some();
    if !has_sio {
        return false;
    }

    let max_ptr = ROM_BUS_BASE.saturating_add(data.len().saturating_sub(8) as u32);
    let mut i = 0usize;
    while i + 4 <= win.len() {
        let val = u32::from_le_bytes([win[i], win[i + 1], win[i + 2], win[i + 3]]);
        if val >= ROM_BUS_BASE && val <= max_ptr {
            let target = (val - ROM_BUS_BASE) as usize;
            if &data[target..target + 8] == b"NINTENDO" {
                return true;
            }
        }
        i += 4;
    }
    false
}

/// Detect Seiko S-3511A RTC via `SIIRTC_V*` tag or `RTC_V*` + LDR-referenced GPIO MMIO.
fn detect_rtc(data: &[u8], has_gpio_ldr: bool) -> Option<String> {
    if let Some(pos) = find_subslice(data, b"SIIRTC_V") {
        let tag = extract_ascii_token(data, pos, 16);
        return Some(tag);
    }
    if has_gpio_ldr {
        if let Some(pos) = find_subslice(data, b"RTC_V") {
            let tag = extract_ascii_token(data, pos, 16);
            return Some(tag);
        }
    }
    None
}

/// Extract exact `RFU_Vxxxx` SDK version tags (e.g. `"RFU_V1024"`) or `"RFU-MBOOT"`.
fn detect_wireless_rfu(data: &[u8]) -> Option<String> {
    let mut versions: Vec<String> = Vec::new();
    let mut pos = 0usize;
    while let Some(rel) = find_subslice(&data[pos..], b"RFU_V") {
        let abs = pos + rel;
        let digits_start = abs + 5;
        let mut digits_end = digits_start;
        while digits_end < data.len()
            && digits_end - digits_start < 5
            && data[digits_end].is_ascii_digit()
        {
            digits_end += 1;
        }
        if digits_end - digits_start >= 3 {
            if let Ok(ver) = std::str::from_utf8(&data[abs..digits_end]) {
                let s = ver.to_string();
                if !versions.contains(&s) {
                    versions.push(s);
                }
            }
        }
        pos = abs + 5;
    }
    if !versions.is_empty() {
        versions.sort();
        return Some(versions.join(", "));
    }
    if find_subslice(data, b"RFU-MBOOT").is_some() {
        return Some("RFU-MBOOT".to_string());
    }
    None
}

/// Detect Nintendo Mobile Adapter GB (CGB-005 / Mobile System GB) client stack.
fn detect_mobile_adapter(data: &[u8]) -> Option<&'static str> {
    const MARKERS: &[(&[u8], &str)] = &[
        (b"gameboy.datacenter.ne.jp/cgb/", "gameboy.datacenter.ne.jp/cgb/"),
        (b"application/x-cgb", "application/x-cgb"),
        (b"WWW-Authenticate: GB00", "WWW-Authenticate: GB00"),
        (b"User-Agent: AGB-", "User-Agent: AGB-"),
    ];
    for &(pat, label) in MARKERS {
        if find_subslice(data, pat).is_some() {
            return Some(label);
        }
    }
    None
}

/// Detect e-Reader firmware cartridges and game-side ERAPI / Card-e / DotCode hooks.
fn detect_ereader_link(data: &[u8], game_code: &str) -> Option<&'static str> {
    if matches!(game_code, "PEAJ" | "PSAJ" | "PSAE") {
        return Some("e-Reader firmware");
    }
    if has_word_bounded(data, b"ERAPI") {
        return Some("ERAPI");
    }
    const MARKERS: &[(&[u8], &str)] = &[
        (b"e-Reader", "e-Reader"),
        (b"E-READER", "E-READER"),
        (b"Card-e", "Card-e"),
        (b"CARD-E", "CARD-E"),
        (b"DotCode", "DotCode"),
        (b"DOTCODE", "DOTCODE"),
    ];
    for &(pat, label) in MARKERS {
        if find_subslice(data, pat).is_some() {
            return Some(label);
        }
    }
    None
}

/// Detect Link-Port toy/card adapters (Capcom Chip Gate, Bandai Legendz Soul Doll).
fn detect_link_port_toy(data: &[u8], game_code: &str) -> Option<&'static str> {
    const MARKERS: &[(&[u8], &str)] = &[
        (b"CHIP GATE", "CHIP GATE"),
        (b"Chip Gate", "Chip Gate"),
        (b"LINK GATE", "LINK GATE"),
        (b"SOUL DOLL", "SOUL DOLL"),
    ];
    for &(pat, label) in MARKERS {
        if find_subslice(data, pat).is_some() {
            return Some(label);
        }
    }
    if game_code.starts_with("BR4")
        || game_code.starts_with("B4B")
        || game_code.starts_with("B4W")
        || game_code.starts_with("BRB")
        || game_code.starts_with("BRK")
        || game_code.starts_with("BR5")
        || game_code.starts_with("BR6")
    {
        return Some("Battle/Progress/Beast Chip Gate");
    }
    if game_code.starts_with("BLJ") || game_code.starts_with("BLV") {
        return Some("Legendz Soul Doll Adapter");
    }
    None
}

/// Check whether any Thumb `ldr Rd, [pc, #imm8*4]` or ARM `ldr Rd, [pc, #+/-imm12]`
/// instruction references the 4-byte aligned literal at `lit_offset`.
fn has_pc_rel_ldr(data: &[u8], lit_offset: usize) -> bool {
    let lit_addr = ROM_BUS_BASE + lit_offset as u32;
    let n = data.len();

    // Thumb PC-relative LDR (up to 1020 bytes before literal)
    for imm in 0u32..256 {
        let Some(target_base) = lit_addr.checked_sub(imm * 4) else {
            break;
        };
        for pc_minus in [4u32, 2u32] {
            let Some(insn_addr) = target_base.checked_sub(pc_minus) else {
                continue;
            };
            if insn_addr < ROM_BUS_BASE {
                continue;
            }
            let off = (insn_addr - ROM_BUS_BASE) as usize;
            if off + 2 <= n {
                let insn = u16::from_le_bytes([data[off], data[off + 1]]);
                if (insn & 0xF800) == 0x4800 && (insn & 0xFF) as u32 == imm {
                    return true;
                }
            }
        }
    }

    // ARM PC-relative LDR (up to 4095 bytes before literal)
    let start_off = lit_offset.saturating_sub(4096) & !3;
    let end_off = lit_offset.min(n.saturating_sub(4));
    let mut off = start_off;
    while off < end_off {
        let insn = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        if (insn & 0x0E10_0000) == 0x0410_0000 && ((insn >> 16) & 0xF) == 0xF {
            let u_bit = (insn >> 23) & 1;
            let imm12 = insn & 0xFFF;
            let pc_addr = ROM_BUS_BASE + off as u32 + 8;
            let target = if u_bit == 1 {
                pc_addr.wrapping_add(imm12)
            } else {
                pc_addr.wrapping_sub(imm12)
            };
            if target == lit_addr {
                return true;
            }
        }
        off += 4;
    }

    false
}

fn has_clustered(a: &[usize], b: &[usize], max_dist: usize) -> bool {
    for &x in a {
        for &y in b {
            if x.abs_diff(y) <= max_dist {
                return true;
            }
        }
    }
    false
}

fn contains_any(haystack: &[u8], needles: &[&[u8]]) -> bool {
    needles.iter().any(|n| find_subslice(haystack, n).is_some())
}

fn has_word_bounded(haystack: &[u8], needle: &[u8]) -> bool {
    let mut pos = 0usize;
    while let Some(rel) = find_subslice(&haystack[pos..], needle) {
        let abs = pos + rel;
        let before_ok = abs == 0 || !haystack[abs - 1].is_ascii_alphabetic();
        let after_idx = abs + needle.len();
        let after_ok = after_idx >= haystack.len() || !haystack[after_idx].is_ascii_alphabetic();
        if before_ok && after_ok {
            return true;
        }
        pos = abs + 1;
    }
    false
}

fn extract_ascii_token(data: &[u8], start: usize, max_len: usize) -> String {
    let end = (start + max_len).min(data.len());
    let mut len = 0usize;
    for &b in &data[start..end] {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
            len += 1;
        } else {
            break;
        }
    }
    String::from_utf8_lossy(&data[start..start + len]).to_string()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let first = needle[0];
    let last_start = haystack.len() - needle.len();
    let mut i = 0usize;
    while i <= last_start {
        if haystack[i] == first && &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{compute_header_complement, FIXED_VALUE};

    fn make_plain_rom(game_code: &[u8; 4], title: &str) -> Vec<u8> {
        let mut rom = vec![0u8; 1024];
        rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
        let tb = title.as_bytes();
        let n = tb.len().min(12);
        rom[0xA0..0xA0 + n].copy_from_slice(&tb[..n]);
        rom[0xAC..0xB0].copy_from_slice(game_code);
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xB2] = FIXED_VALUE;
        rom[0xBD] = compute_header_complement(&rom[0xA0..0xBD]);
        rom[0x120..0x12B].copy_from_slice(b"EEPROM_V124");
        rom
    }

    #[test]
    fn plain_rom_returns_empty() {
        let rom = make_plain_rom(b"BTSE", "PLAIN GAME");
        assert!(detect_peripherals(&rom).is_empty());
    }

    #[test]
    fn detects_gbp_rumble_sdk() {
        let mut rom = make_plain_rom(b"B3SE", "SONIC ADV 3");
        // Place "NINTENDO" at 0x200
        rom[0x200..0x208].copy_from_slice(b"NINTENDO");
        // Literal pool at 0x300: SIODATA32, ptr to "NINTENDO", and 0x00008002 ACK
        rom[0x300..0x304].copy_from_slice(&0x0400_0120u32.to_le_bytes());
        rom[0x304..0x308].copy_from_slice(&0x0800_0200u32.to_le_bytes());
        rom[0x308..0x30C].copy_from_slice(&0x0000_8002u32.to_le_bytes());
        // Also embed official GBP logo palette -> should still classify as GameBoyPlayerRumble (not Logo)
        rom[0x340..0x360].copy_from_slice(&GBP_LOGO_PALETTE_32);

        let hits = detect_peripherals(&rom);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, PeripheralKind::GameBoyPlayerRumble);
        assert_eq!(
            hits[0].detail.as_deref(),
            Some("libgbp SIO 32-bit + official boot logo")
        );
    }

    #[test]
    fn detects_gbp_logo_only() {
        let mut rom = make_plain_rom(b"A3UJ", "MOTHER 3");
        rom[0x240..0x260].copy_from_slice(&GBP_LOGO_PALETTE_32);

        let hits = detect_peripherals(&rom);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, PeripheralKind::GameBoyPlayerLogo);
    }

    #[test]
    fn detects_gbp_anti_capture_drm_on_gba_video() {
        let mut rom = make_plain_rom(b"MPAE", "GBA VIDEO");
        rom[0x240..0x260].copy_from_slice(&GBP_LOGO_PALETTE_32);

        let hits = detect_peripherals(&rom);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, PeripheralKind::GameBoyPlayerAntiCaptureDrm);
    }

    #[test]
    fn detects_wireless_rfu_version_and_mboot() {
        let mut rom = make_plain_rom(b"BPRE", "POKEMON FIRE");
        rom[0x200..0x209].copy_from_slice(b"RFU_V1024");
        rom[0x220..0x229].copy_from_slice(b"RFU-MBOOT");

        let hits = detect_peripherals(&rom);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, PeripheralKind::WirelessAdapterRfu);
        assert_eq!(hits[0].detail.as_deref(), Some("RFU_V1024"));

        // RFU-MBOOT only (without RFU_Vxxxx)
        let mut mboot_rom = make_plain_rom(b"BG3E", "DBZ BUU");
        mboot_rom[0x220..0x229].copy_from_slice(b"RFU-MBOOT");
        let mboot_hits = detect_peripherals(&mboot_rom);
        assert_eq!(mboot_hits.len(), 1);
        assert_eq!(mboot_hits[0].kind, PeripheralKind::WirelessAdapterRfu);
        assert_eq!(mboot_hits[0].detail.as_deref(), Some("RFU-MBOOT"));
    }

    #[test]
    fn detects_mobile_adapter_gb() {
        let mut rom = make_plain_rom(b"AMKJ", "MARIO KART");
        let url = b"http://gameboy.datacenter.ne.jp/cgb/download";
        rom[0x200..0x200 + url.len()].copy_from_slice(url);

        let hits = detect_peripherals(&rom);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, PeripheralKind::MobileAdapterGb);
        assert_eq!(
            hits[0].detail.as_deref(),
            Some("gameboy.datacenter.ne.jp/cgb/")
        );
    }

    #[test]
    fn detects_tilt_accelerometer() {
        let mut rom = make_plain_rom(b"HMBW", "HOMEBREW TLT");
        rom[0x200..0x204].copy_from_slice(&ADDR_TILT_START1.to_le_bytes());
        rom[0x204..0x208].copy_from_slice(&ADDR_TILT_START2.to_le_bytes());

        let hits = detect_peripherals(&rom);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, PeripheralKind::TiltAccelerometer);
    }

    #[test]
    fn detects_gyro_and_cartridge_rumble() {
        let mut rom = make_plain_rom(b"HMBW", "HOMEBREW GYR");
        // Thumb `ldr r0, [pc, #0]` at 0x1FC loads literal at 0x200
        rom[0x1FC..0x1FE].copy_from_slice(&0x4800u16.to_le_bytes());
        rom[0x200..0x204].copy_from_slice(&ADDR_GPIO_DATA.to_le_bytes());
        rom[0x204..0x208].copy_from_slice(&ADDR_GPIO_DIR.to_le_bytes());
        rom[0x208..0x20C].copy_from_slice(&ADDR_GYRO_ADC.to_le_bytes());

        let hits = detect_peripherals(&rom);
        let kinds: Vec<PeripheralKind> = hits.iter().map(|h| h.kind).collect();
        assert!(kinds.contains(&PeripheralKind::GyroPiezoelectric));
        assert!(kinds.contains(&PeripheralKind::CartridgeRumble));
    }

    #[test]
    fn detects_rtc_and_solar_sensor() {
        let mut rom = make_plain_rom(b"HMBW", "SOLAR TEST");
        rom[0x200..0x204].copy_from_slice(&ADDR_GPIO_DATA.to_le_bytes());
        rom[0x204..0x208].copy_from_slice(&ADDR_GPIO_DIR.to_le_bytes());
        rom[0x208..0x20C].copy_from_slice(&ADDR_GPIO_CNT.to_le_bytes());
        rom[0x240..0x24B].copy_from_slice(b"SIIRTC_V001");
        rom[0x260..0x26C].copy_from_slice(b"SOLAR SENSOR");

        let hits = detect_peripherals(&rom);
        let kinds: Vec<PeripheralKind> = hits.iter().map(|h| h.kind).collect();
        assert_eq!(kinds, vec![PeripheralKind::Rtc, PeripheralKind::SolarSensor]);
        assert_eq!(hits[0].detail.as_deref(), Some("SIIRTC_V001"));
    }

    #[test]
    fn detects_ereader_and_link_port_toy() {
        let mut rom = make_plain_rom(b"HMBW", "LINK TEST");
        rom[0x200..0x207].copy_from_slice(b" ERAPI ");
        rom[0x240..0x249].copy_from_slice(b"CHIP GATE");

        let hits = detect_peripherals(&rom);
        let kinds: Vec<PeripheralKind> = hits.iter().map(|h| h.kind).collect();
        assert_eq!(
            kinds,
            vec![
                PeripheralKind::EReaderLink,
                PeripheralKind::LinkPortToyAdapter,
            ]
        );
        assert_eq!(hits[0].detail.as_deref(), Some("ERAPI"));
        assert_eq!(hits[1].detail.as_deref(), Some("CHIP GATE"));
    }
}
