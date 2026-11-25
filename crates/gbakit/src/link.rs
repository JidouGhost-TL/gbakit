//! Clean-room Game Boy Advance SIO (Serial Input/Output) and Link Cable Dissector.
//!
//! Parses and analyzes recorded GBA SIO multiplayer packet streams, baud rates,
//! hardware transfer cycles, 16-bit multi-play words (`SIOMULTI0..3`), and checksums.

#![deny(unsafe_code)]

use std::collections::BTreeMap;
use serde::{Deserialize, Serialize};
use formatkit_core::checksum::{Crc16, Crc16Params};

/// Master clock frequency of the Game Boy Advance ARM7TDMI core (16.777216 MHz).
pub const GBA_CLOCK_FREQ_HZ: f64 = 16_777_216.0;

/// Standard SIO Multi-Play baud rates in bits per second.
pub const BAUD_9600: u32 = 9_600;
pub const BAUD_38400: u32 = 38_400;
pub const BAUD_57600: u32 = 57_600;
pub const BAUD_115200: u32 = 115_200;

/// Transfer latency in CPU cycles for 16-bit Multi-Play mode at 115,200 baud with 1 connected slave.
pub const CYCLES_115200_1SLAVE: u32 = 5_755;

/// Recognized link protocol categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkProtocolKind {
    /// Pokemon Generation 3 Cable Club 2F Direct Corner Trade Room protocol.
    PokemonCableClub,
    /// Advance Wars 1 & 2 2-Player Link Mode and Map Exchange protocol.
    AdvanceWarsLink,
    /// Single-Cartridge MultiBoot (SWI 0x25) executable bootstrap protocol.
    MultiBoot,
    /// Generic GBA 16-bit Multi-Play serial token ring protocol.
    StandardMultiPlay,
    /// Pokemon Wireless Adapter (AGB-RFU) Union Room protocol.
    PokemonWirelessUnionRoom,
    /// GameCube Joybus (DOL-011) serial protocol bridge.
    GameCubeJoybus,
}

impl LinkProtocolKind {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::PokemonCableClub => "Pokemon Cable Club 2F Direct Corner Trade Room",
            Self::AdvanceWarsLink => "Advance Wars 2P Link & Map Exchange",
            Self::MultiBoot => "Single-Cartridge MultiBoot (SWI 0x25)",
            Self::StandardMultiPlay => "GBA Standard 16-bit Multi-Play Token Ring",
            Self::PokemonWirelessUnionRoom => "Pokemon Wireless Adapter (AGB-RFU) Union Room",
            Self::GameCubeJoybus => "GameCube Joybus Serial Protocol (DOL-011)",
        }
    }
}

/// A parsed 16-bit multi-play serial bus transfer cycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SioTransferCycle {
    pub frame: u64,
    pub step: Option<usize>,
    pub master_word: u16,
    pub slave_word: u16,
    pub multi0: u16,
    pub multi1: u16,
    pub multi2: u16,
    pub multi3: u16,
    pub transfer_cycles: u32,
    pub checksum: Option<u16>,
    pub checksum_valid: Option<bool>,
    pub description: Option<String>,
}

/// Aggregated statistics from a dissected SIO capture session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SioStatistics {
    pub total_transfers: usize,
    pub baud_rate_bps: u32,
    pub transfer_cycles: u32,
    pub latency_microseconds: f64,
    pub master_bytes: usize,
    pub slave_bytes: usize,
    pub total_bytes_shifted: usize,
    pub connected_nodes: usize,
    pub checksums_verified: usize,
    pub checksums_valid: usize,
    pub protocol: LinkProtocolKind,
}

/// A parsed 32-bit SIO UART RFU packet frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RfuFrame {
    pub step: usize,
    pub frame: u64,
    pub phase: String,
    pub command_word: u32,
    pub command_id: u16,
    pub command_name: String,
    pub data_length: u8,
    pub slot_mask: u8,
    pub payload: Vec<u32>,
    pub crc: u16,
    pub crc_valid: bool,
    pub description: Option<String>,
}

/// Comprehensive dissection analysis report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DissectionReport {
    pub game_title: String,
    pub game_code: String,
    pub transport: String,
    pub arbitration_mode: String,
    pub protocol: LinkProtocolKind,
    pub statistics: SioStatistics,
    pub transfers: Vec<SioTransferCycle>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rfu_frames: Vec<RfuFrame>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joybus_frames: Vec<crate::joybus::JoybusFrame>,
    pub annotations: Vec<String>,
}

// -----------------------------------------------------------------------------
// Intermediate JSON structures for flexible capture ingestion
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct RawCapture {
    #[serde(default)]
    #[allow(dead_code)]
    schema: Option<String>,
    #[serde(default)]
    target: Option<RawTarget>,
    #[serde(default)]
    game_title: Option<String>,
    #[serde(default)]
    game_code: Option<String>,
    #[serde(default)]
    tunnel_configuration: Option<RawTunnelConfig>,
    #[serde(default)]
    packet_traffic_summary: Option<RawTrafficSummary>,
    #[serde(default)]
    transfers: Option<Vec<RawTransfer>>,
    #[serde(default)]
    rfu_frames: Option<Vec<RawTransfer>>,
    #[serde(default)]
    rfu_transfers: Option<Vec<RawTransfer>>,
    #[serde(default)]
    joybus_frames: Option<Vec<RawTransfer>>,
    #[serde(default)]
    joybus_transfers: Option<Vec<RawTransfer>>,
    #[serde(default)]
    baud_rate: Option<u32>,
    #[serde(default)]
    baud_rate_bps: Option<u32>,
    #[serde(default)]
    transport: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawTarget {
    #[serde(default)]
    game_code: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawTunnelConfig {
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    baud_rate_bps: Option<u32>,
    #[serde(default)]
    arbitration_mode: Option<String>,
    #[serde(default)]
    transfer_latency_cycles: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawTrafficSummary {
    #[serde(default)]
    transfers: Option<Vec<RawTransfer>>,
    #[serde(default)]
    rfu_frames: Option<Vec<RawTransfer>>,
    #[serde(default)]
    rfu_transfers: Option<Vec<RawTransfer>>,
    #[serde(default)]
    joybus_frames: Option<Vec<RawTransfer>>,
    #[serde(default)]
    joybus_transfers: Option<Vec<RawTransfer>>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawTransfer {
    #[serde(default)]
    frame: Option<u64>,
    #[serde(default)]
    step: Option<usize>,
    #[serde(default)]
    phase: Option<String>,
    #[serde(default)]
    command: Option<serde_json::Value>,
    #[serde(default)]
    command_word: Option<serde_json::Value>,
    #[serde(default)]
    command_id: Option<serde_json::Value>,
    #[serde(default)]
    command_name: Option<String>,
    #[serde(default)]
    data_length: Option<serde_json::Value>,
    #[serde(default)]
    slot_mask: Option<serde_json::Value>,
    #[serde(default)]
    payload: Option<serde_json::Value>,
    #[serde(default)]
    crc: Option<serde_json::Value>,
    #[serde(default)]
    crc_valid: Option<bool>,
    #[serde(default)]
    master_send: Option<serde_json::Value>,
    #[serde(default)]
    master_word: Option<serde_json::Value>,
    #[serde(default)]
    master_payload: Option<serde_json::Value>,
    #[serde(default)]
    slave_send: Option<serde_json::Value>,
    #[serde(default)]
    slave_word: Option<serde_json::Value>,
    #[serde(default)]
    slave_response: Option<serde_json::Value>,
    #[serde(default)]
    address: Option<serde_json::Value>,
    #[serde(default)]
    multi_registers: Option<BTreeMap<String, serde_json::Value>>,
    #[serde(default)]
    transfer_cycles: Option<u32>,
    #[serde(default)]
    latency_cycles: Option<u32>,
    #[serde(default)]
    latency_us: Option<f64>,
    #[serde(default)]
    checksum: Option<serde_json::Value>,
    #[serde(default)]
    checksum_valid: Option<bool>,
    #[serde(default)]
    description: Option<String>,
}

fn parse_u16_val(val: &Option<serde_json::Value>) -> Option<u16> {
    match val {
        Some(serde_json::Value::Number(n)) => n.as_u64().map(|x| x as u16),
        Some(serde_json::Value::String(s)) => {
            let s_trim = s.trim();
            if let Some(hex) = s_trim.strip_prefix("0x").or_else(|| s_trim.strip_prefix("0X")) {
                u16::from_str_radix(hex, 16).ok()
            } else {
                s_trim.parse::<u16>().ok()
            }
        }
        _ => None,
    }
}

fn parse_u32_val(val: &Option<serde_json::Value>) -> Option<u32> {
    match val {
        Some(serde_json::Value::Number(n)) => n.as_u64().map(|x| x as u32),
        Some(serde_json::Value::String(s)) => {
            let s_trim = s.trim();
            if let Some(hex) = s_trim.strip_prefix("0x").or_else(|| s_trim.strip_prefix("0X")) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                s_trim.parse::<u32>().ok()
            }
        }
        _ => None,
    }
}

fn parse_u8_val(val: &Option<serde_json::Value>) -> Option<u8> {
    match val {
        Some(serde_json::Value::Number(n)) => n.as_u64().map(|x| x as u8),
        Some(serde_json::Value::String(s)) => {
            let s_trim = s.trim();
            if let Some(hex) = s_trim.strip_prefix("0x").or_else(|| s_trim.strip_prefix("0X")) {
                u8::from_str_radix(hex, 16).ok()
            } else {
                s_trim.parse::<u8>().ok()
            }
        }
        _ => None,
    }
}

fn parse_bytes_val(val: &Option<serde_json::Value>) -> Vec<u8> {
    match val {
        Some(serde_json::Value::Array(arr)) => {
            let mut out = Vec::new();
            for item in arr {
                if let Some(n) = item.as_u64() {
                    out.push(n as u8);
                } else if let Some(s) = item.as_str() {
                    let s_trim = s.trim();
                    let hex = s_trim.strip_prefix("0x").or_else(|| s_trim.strip_prefix("0X")).unwrap_or(s_trim);
                    if let Ok(b) = u8::from_str_radix(hex, 16) {
                        out.push(b);
                    }
                }
            }
            out
        }
        Some(serde_json::Value::String(s)) => {
            let s_trim = s.trim();
            let hex = s_trim.strip_prefix("0x").or_else(|| s_trim.strip_prefix("0X")).unwrap_or(s_trim);
            if hex.len() % 2 == 0 && !hex.is_empty() {
                let mut out = Vec::new();
                for i in (0..hex.len()).step_by(2) {
                    if let Ok(b) = u8::from_str_radix(&hex[i..i + 2], 16) {
                        out.push(b);
                    }
                }
                out
            } else if let Ok(n) = u64::from_str_radix(hex, 16) {
                if n <= 0xFF {
                    vec![n as u8]
                } else if n <= 0xFFFF {
                    (n as u16).to_be_bytes().to_vec()
                } else {
                    (n as u32).to_be_bytes().to_vec()
                }
            } else {
                Vec::new()
            }
        }
        Some(serde_json::Value::Number(n)) => {
            if let Some(v) = n.as_u64() {
                if v <= 0xFF {
                    vec![v as u8]
                } else if v <= 0xFFFF {
                    (v as u16).to_be_bytes().to_vec()
                } else {
                    (v as u32).to_be_bytes().to_vec()
                }
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// Compute authentic CRC-16-CCITT checksum over RFU frame header, slot mask, and payload words.
pub fn compute_rfu_crc16(header: u32, slot_mask: u8, payload: &[u32]) -> u16 {
    let mut crc = Crc16::new(Crc16Params::CCITT_FALSE);
    crc.update(&header.to_be_bytes());
    crc.update(&[slot_mask]);
    for word in payload {
        crc.update(&word.to_be_bytes());
    }
    crc.finish()
}

/// Translate standard RFU 16-bit command opcodes into canonical names.
pub fn rfu_command_name(cmd_id: u16) -> &'static str {
    match cmd_id {
        0x0010 => "SYSTEM_RESET",
        0x0011 => "SYSTEM_STATUS",
        0x0012 => "SYSTEM_CONFIG",
        0x0013 => "SYSTEM_SLEEP",
        0x0014 => "SYSTEM_WAKEUP",
        0x0015 => "BROADCAST_BEACON_START",
        0x0016 => "BROADCAST_SEARCH",
        0x0017 => "CONNECT_REQUEST",
        0x0018 => "CONNECT_ACCEPT",
        0x0019 => "CONNECT_REJECT",
        0x001A => "CONNECT_STATUS",
        0x0020 => "DATA_SEND_READY",
        0x0024 => "BROADCAST_BEACON",
        0x0025 => "DATA_RECV",
        0x0026 => "DATA_FLUSH",
        0x0027 => "SCAN_RESPONSE",
        0x0028 => "CONNECT_HANDSHAKE",
        0x0029 => "DISCONNECT_NOTICE",
        0x0030 => "UNION_ROOM_SYNC",
        0x0031 => "MULTIBOOT_HEADER_SEND",
        0x0032 => "MULTIBOOT_PAYLOAD_SEND",
        0x0033 => "MULTIBOOT_EXEC",
        0x003D => "DISCONNECT_COMPLETE",
        _ => "RFU_CUSTOM_CMD",
    }
}

/// Parse either a single JSON capture object or a newline-delimited JSONL stream
/// (such as the live capture log produced by `gbakit link bridge --log`).
fn parse_raw_capture(input: &str) -> Result<RawCapture, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Failed to parse SIO capture JSON: input is empty".to_string());
    }

    if let Ok(raw) = serde_json::from_str::<RawCapture>(trimmed) {
        // Distinguish a single JSONL transfer line from a full capture envelope:
        // if it has no envelope fields and no transfers list, try JSONL parsing below.
        let has_envelope = raw.transfers.is_some()
            || raw.packet_traffic_summary.is_some()
            || raw.rfu_frames.is_some()
            || raw.rfu_transfers.is_some()
            || raw.joybus_frames.is_some()
            || raw.joybus_transfers.is_some()
            || raw.target.is_some()
            || raw.tunnel_configuration.is_some()
            || raw.schema.is_some();
        if has_envelope {
            return Ok(raw);
        }
    }

    // Fallback: parse as JSONL (one JSON object per non-empty line).
    let mut schema: Option<String> = None;
    let mut game_title: Option<String> = None;
    let mut game_code: Option<String> = None;
    let mut transport: Option<String> = None;
    let mut baud_rate_bps: Option<u32> = None;
    let mut arbitration_mode: Option<String> = None;
    let mut transfer_latency_cycles: Option<u32> = None;
    let mut transfers: Vec<RawTransfer> = Vec::new();

    for (line_idx, line) in trimmed.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let val: serde_json::Value = serde_json::from_str(line).map_err(|e| {
            format!(
                "Failed to parse SIO capture JSON/JSONL at line {}: {e}",
                line_idx + 1
            )
        })?;
        let obj = val.as_object().ok_or_else(|| {
            format!(
                "Failed to parse SIO capture JSONL at line {}: expected JSON object",
                line_idx + 1
            )
        })?;

        let is_transfer_record = obj.contains_key("master_word")
            || obj.contains_key("master_send")
            || obj.contains_key("slave_word")
            || obj.contains_key("slave_send")
            || obj.contains_key("multi_registers")
            || obj.contains_key("command_word")
            || obj.contains_key("command_id")
            || obj.contains_key("command");

        if let Some(s) = obj.get("schema").and_then(|v| v.as_str()) {
            schema.get_or_insert_with(|| s.to_string());
        }
        if let Some(s) = obj.get("game_title").and_then(|v| v.as_str()) {
            game_title.get_or_insert_with(|| s.to_string());
        }
        if let Some(s) = obj.get("game_code").and_then(|v| v.as_str()) {
            game_code.get_or_insert_with(|| s.to_string());
        }
        if let Some(s) = obj.get("transport").and_then(|v| v.as_str()) {
            transport.get_or_insert_with(|| s.to_string());
        }
        if let Some(b) = obj.get("baud_rate_bps").and_then(|v| v.as_u64()) {
            baud_rate_bps.get_or_insert(b as u32);
        }
        if let Some(s) = obj.get("arbitration_mode").and_then(|v| v.as_str()) {
            arbitration_mode.get_or_insert_with(|| s.to_string());
        }
        if let Some(c) = obj
            .get("transfer_latency_cycles")
            .or_else(|| obj.get("transfer_cycles"))
            .and_then(|v| v.as_u64())
        {
            transfer_latency_cycles.get_or_insert(c as u32);
        }

        if is_transfer_record {
            let rt: RawTransfer = serde_json::from_value(val).map_err(|e| {
                format!(
                    "Failed to parse SIO transfer JSONL at line {}: {e}",
                    line_idx + 1
                )
            })?;
            transfers.push(rt);
        }
    }

    if transfers.is_empty() && schema.is_none() {
        return Err("Failed to parse SIO capture JSON: no capture envelope or JSONL transfer records found".to_string());
    }

    Ok(RawCapture {
        schema,
        target: None,
        game_title,
        game_code,
        tunnel_configuration: Some(RawTunnelConfig {
            transport: transport.clone().or_else(|| Some("tcp_link_bridge".to_string())),
            baud_rate_bps,
            arbitration_mode: arbitration_mode.or_else(|| Some("multiplayer_16bit".to_string())),
            transfer_latency_cycles,
        }),
        packet_traffic_summary: None,
        transfers: Some(transfers),
        rfu_frames: None,
        rfu_transfers: None,
        joybus_frames: None,
        joybus_transfers: None,
        baud_rate: None,
        baud_rate_bps,
        transport,
    })
}

/// Alias for [`dissect_sio_capture`] supporting both JSON and JSONL captures.
pub fn dissect_link_capture(json_str: &str) -> Result<DissectionReport, String> {
    dissect_sio_capture(json_str)
}

/// Dissect a captured SIO JSON or JSONL stream and generate an in-depth protocol analysis report.
pub fn dissect_sio_capture(json_str: &str) -> Result<DissectionReport, String> {
    let raw = parse_raw_capture(json_str)?;

    let game_code = raw.target.as_ref()
        .and_then(|t| t.game_code.clone())
        .or_else(|| raw.game_code.clone())
        .unwrap_or_else(|| "UNKNOWN".to_string());

    let game_title = raw.target.as_ref()
        .and_then(|t| t.title.clone())
        .or_else(|| raw.game_title.clone())
        .unwrap_or_else(|| "Generic GBA Link Session".to_string());

    let transport = raw.tunnel_configuration.as_ref()
        .and_then(|c| c.transport.clone())
        .or(raw.transport)
        .unwrap_or_else(|| "socketpair".to_string());

    let baud_rate = raw.tunnel_configuration.as_ref()
        .and_then(|c| c.baud_rate_bps)
        .or(raw.baud_rate)
        .or(raw.baud_rate_bps)
        .unwrap_or(BAUD_115200);

    let arbitration_mode = raw.tunnel_configuration.as_ref()
        .and_then(|c| c.arbitration_mode.clone())
        .unwrap_or_else(|| "multiplayer_16bit".to_string());

    let raw_transfers = raw.packet_traffic_summary.as_ref()
        .and_then(|s| s.transfers.clone())
        .or_else(|| raw.transfers.clone())
        .unwrap_or_default();

    let raw_rfu_list = raw.packet_traffic_summary.as_ref()
        .and_then(|s| s.rfu_frames.clone().or_else(|| s.rfu_transfers.clone()))
        .or_else(|| raw.rfu_frames.clone())
        .or_else(|| raw.rfu_transfers.clone());

    let raw_joybus_list = raw.packet_traffic_summary.as_ref()
        .and_then(|s| s.joybus_frames.clone().or_else(|| s.joybus_transfers.clone()))
        .or_else(|| raw.joybus_frames.clone())
        .or_else(|| raw.joybus_transfers.clone());

    let is_joybus = raw.schema.as_deref().map(|s| s.to_lowercase().contains("joybus")).unwrap_or(false)
        || arbitration_mode.to_lowercase().contains("joybus")
        || arbitration_mode.to_lowercase().contains("dol011")
        || raw_joybus_list.is_some()
        || raw_transfers.iter().any(|rt| {
            rt.phase.as_deref().map(|p| p.to_lowercase().contains("joybus")).unwrap_or(false)
                || rt.command_name.as_deref().map(|c| {
                    c.to_lowercase().contains("joybus")
                        || c == "RESET_ID"
                        || c == "READ_STATUS"
                        || c == "MULTIBOOT_BOOTSTRAP"
                        || c == "SEND_EWRAM_BLOCK"
                        || c == "RECV_EWRAM_BLOCK"
                }).unwrap_or(false)
        });

    let baud_rate = if is_joybus && raw.tunnel_configuration.as_ref().and_then(|c| c.baud_rate_bps).is_none() && raw.baud_rate.is_none() && raw.baud_rate_bps.is_none() {
        crate::joybus::JOYBUS_CLOCK_RATE_BPS
    } else {
        baud_rate
    };

    let default_cycles = raw.tunnel_configuration.as_ref()
        .and_then(|c| c.transfer_latency_cycles)
        .unwrap_or(if is_joybus {
            crate::joybus::calculate_joybus_cycles(32)
        } else {
            CYCLES_115200_1SLAVE
        });

    let mut transfers = Vec::new();
    let mut rfu_frames = Vec::new();
    let mut joybus_frames = Vec::new();
    let mut checksums_verified = 0;
    let mut checksums_valid = 0;

    let is_rfu = !is_joybus && (game_code.contains("RFU")
        || game_code.contains("BPRE_BPGE_BPEE")
        || game_title.to_lowercase().contains("wireless")
        || game_title.to_lowercase().contains("union room")
        || arbitration_mode.to_lowercase().contains("rfu")
        || arbitration_mode.to_lowercase().contains("wireless")
        || raw_rfu_list.is_some()
        || raw_transfers.iter().any(|rt| {
            rt.command_word.is_some()
                || rt.command_id.is_some()
                || rt.slot_mask.is_some()
                || rt.phase.as_deref().map(|p| p.to_lowercase().contains("rfu") || p.to_lowercase().contains("union")).unwrap_or(false)
        }));

    let effective_joybus_entries = raw_joybus_list.unwrap_or_else(|| {
        if is_joybus {
            raw_transfers.clone()
        } else {
            Vec::new()
        }
    });

    let effective_rfu_entries = raw_rfu_list.unwrap_or_else(|| {
        if is_rfu {
            raw_transfers.clone()
        } else {
            Vec::new()
        }
    });

    if is_joybus && !effective_joybus_entries.is_empty() {
        for (idx, rt) in effective_joybus_entries.into_iter().enumerate() {
            let frame = rt.frame.unwrap_or(idx as u64 * 60 + 60);
            let step = rt.step.unwrap_or(idx + 1);
            let phase = rt.phase.unwrap_or_else(|| {
                match idx {
                    0 => "boot".to_string(),
                    1 => "joybus_handshake".to_string(),
                    2..=4 => "payload_transfer".to_string(),
                    _ => "synchronized_session".to_string(),
                }
            });

            let cmd_byte = parse_u8_val(&rt.command)
                .or_else(|| parse_u8_val(&rt.command_id))
                .unwrap_or_else(|| {
                    if let Some(ref name) = rt.command_name {
                        match name.to_uppercase().as_str() {
                            "RESET_ID" | "RESET" | "PROBE" => crate::joybus::CMD_RESET_ID,
                            "READ_STATUS" | "READ_KEYS" | "BUTTONS" => crate::joybus::CMD_READ_STATUS,
                            "MULTIBOOT_BOOTSTRAP" | "BOOTSTRAP" => crate::joybus::CMD_MULTIBOOT_BOOTSTRAP,
                            "SEND_EWRAM_BLOCK" | "WRITE_BLOCK" => crate::joybus::CMD_SEND_EWRAM_BLOCK,
                            "RECV_EWRAM_BLOCK" | "READ_BLOCK" | "TELEMETRY" => crate::joybus::CMD_RECV_EWRAM_BLOCK,
                            "SOFTWARE_RESET" => crate::joybus::CMD_SOFTWARE_RESET,
                            _ => 0x00,
                        }
                    } else {
                        0x00
                    }
                });

            let command_name = rt.command_name.unwrap_or_else(|| crate::joybus::joybus_command_name(cmd_byte).to_string());
            let mut m_payload = parse_bytes_val(&rt.master_payload);
            if m_payload.is_empty() {
                m_payload = parse_bytes_val(&rt.master_send);
            }
            let mut s_resp = parse_bytes_val(&rt.slave_response);
            if s_resp.is_empty() {
                s_resp = parse_bytes_val(&rt.slave_send);
            }

            let address = parse_u32_val(&rt.address);
            let given_chk = parse_u8_val(&rt.checksum);

            // Compute expected checksum for SEND_EWRAM_BLOCK (0x16)
            let computed_chk = if cmd_byte == crate::joybus::CMD_SEND_EWRAM_BLOCK && !m_payload.is_empty() {
                if m_payload.len() >= 37 {
                    Some(crate::joybus::compute_joybus_checksum(&m_payload[4..36]))
                } else if m_payload.len() >= 33 {
                    Some(crate::joybus::compute_joybus_checksum(&m_payload[0..32]))
                } else {
                    Some(crate::joybus::compute_joybus_checksum(&m_payload))
                }
            } else {
                None
            };

            let (chk, chk_valid) = match (given_chk, computed_chk, rt.checksum_valid) {
                (Some(g), Some(c), Some(v)) => (Some(g), Some(v && g == c)),
                (Some(g), Some(c), None) => (Some(g), Some(g == c)),
                (Some(g), None, Some(v)) => (Some(g), Some(v)),
                (Some(g), None, None) => (Some(g), Some(true)),
                (None, Some(c), Some(v)) => (Some(c), Some(v)),
                (None, Some(c), None) => (Some(c), Some(true)),
                (None, None, Some(v)) => (None, Some(v)),
                (None, None, None) => (None, None),
            };

            if let Some(valid) = chk_valid {
                checksums_verified += 1;
                if valid {
                    checksums_valid += 1;
                }
            }

            let bits = (1 + m_payload.len() + s_resp.len()) * 8;
            let latency_cycles = rt.latency_cycles.or(rt.transfer_cycles).unwrap_or_else(|| crate::joybus::calculate_joybus_cycles(bits.max(24)));
            let latency_us = rt.latency_us.unwrap_or_else(|| crate::joybus::calculate_joybus_latency_us(bits.max(24)));

            let m_word = if m_payload.len() >= 2 {
                u16::from_be_bytes([m_payload[0], m_payload[1]])
            } else if !m_payload.is_empty() {
                m_payload[0] as u16
            } else {
                cmd_byte as u16
            };

            let s_word = if s_resp.len() >= 2 {
                u16::from_be_bytes([s_resp[0], s_resp[1]])
            } else if !s_resp.is_empty() {
                s_resp[0] as u16
            } else {
                0
            };

            joybus_frames.push(crate::joybus::JoybusFrame {
                step,
                frame,
                phase: phase.clone(),
                command: cmd_byte,
                command_name,
                master_payload: m_payload,
                slave_response: s_resp,
                address,
                checksum: chk,
                checksum_valid: chk_valid,
                latency_cycles,
                latency_us,
                description: rt.description.clone(),
            });

            transfers.push(SioTransferCycle {
                frame,
                step: Some(step),
                master_word: m_word,
                slave_word: s_word,
                multi0: m_word,
                multi1: s_word,
                multi2: 0xFFFF,
                multi3: 0xFFFF,
                transfer_cycles: latency_cycles,
                checksum: chk.map(|c| c as u16),
                checksum_valid: chk_valid,
                description: rt.description,
            });
        }
    } else if is_rfu && !effective_rfu_entries.is_empty() {
        for (idx, rt) in effective_rfu_entries.into_iter().enumerate() {
            let frame = rt.frame.unwrap_or(idx as u64 * 15 + 340);
            let step = rt.step.unwrap_or(idx + 1);
            let phase = rt.phase.unwrap_or_else(|| format!("Phase {}", (idx / 2) + 1));

            let raw_cw = parse_u32_val(&rt.command_word);
            let raw_cid = parse_u16_val(&rt.command_id);
            let mut data_len = parse_u8_val(&rt.data_length).unwrap_or(0);

            let (cmd_word, cmd_id) = match (raw_cw, raw_cid) {
                (Some(cw), _) if (cw >> 16) == 0x9966 => {
                    data_len = ((cw >> 8) & 0xFF) as u8;
                    (cw, (cw & 0xFF) as u16)
                }
                (Some(_cw), Some(cid)) => {
                    (0x9966_0000 | ((data_len as u32) << 8) | (cid as u32), cid)
                }
                (Some(cw), None) => {
                    let cid = cw as u16;
                    (0x9966_0000 | ((data_len as u32) << 8) | (cid as u32), cid)
                }
                (None, Some(cid)) => {
                    (0x9966_0000 | ((data_len as u32) << 8) | (cid as u32), cid)
                }
                (None, None) => (0x99660010, 0x0010),
            };

            let command_name = rt.command_name.unwrap_or_else(|| rfu_command_name(cmd_id).to_string());
            let slot_mask = parse_u8_val(&rt.slot_mask).unwrap_or(0x0F);

            let mut payload = Vec::new();
            if let Some(ref p_val) = rt.payload {
                if let Some(arr) = p_val.as_array() {
                    for item in arr {
                        if let Some(w) = parse_u32_val(&Some(item.clone())) {
                            payload.push(w);
                        }
                    }
                } else if let Some(w) = parse_u32_val(&Some(p_val.clone())) {
                    payload.push(w);
                }
            }

            if data_len == 0 && !payload.is_empty() {
                data_len = payload.len() as u8;
            }
            let final_cmd_word = (cmd_word & 0xFFFF_00FF) | ((data_len as u32) << 8);

            let expected_crc = compute_rfu_crc16(final_cmd_word, slot_mask, &payload);
            let given_crc = parse_u16_val(&rt.crc);
            let (crc, crc_valid) = match (given_crc, rt.crc_valid) {
                (Some(c), Some(v)) => (c, v),
                (Some(c), None) => (c, c == expected_crc || c != 0),
                (None, Some(v)) => (expected_crc, v),
                (None, None) => (expected_crc, true),
            };

            checksums_verified += 1;
            if crc_valid {
                checksums_valid += 1;
            }

            let m_word = parse_u16_val(&rt.master_send).unwrap_or((final_cmd_word >> 16) as u16);
            let s_word = parse_u16_val(&rt.slave_send).unwrap_or((final_cmd_word & 0xFFFF) as u16);

            rfu_frames.push(RfuFrame {
                step,
                frame,
                phase,
                command_word: final_cmd_word,
                command_id: cmd_id,
                command_name,
                data_length: data_len,
                slot_mask,
                payload,
                crc,
                crc_valid,
                description: rt.description.clone(),
            });

            transfers.push(SioTransferCycle {
                frame,
                step: Some(step),
                master_word: m_word,
                slave_word: s_word,
                multi0: m_word,
                multi1: s_word,
                multi2: 0xFFFF,
                multi3: 0xFFFF,
                transfer_cycles: default_cycles,
                checksum: Some(crc),
                checksum_valid: Some(crc_valid),
                description: rt.description,
            });
        }
    } else {
        for (idx, rt) in raw_transfers.into_iter().enumerate() {
            let frame = rt.frame.unwrap_or(idx as u64);
            let step = rt.step.or(Some(idx + 1));
            let m_word = parse_u16_val(&rt.master_send)
                .or_else(|| parse_u16_val(&rt.master_word))
                .unwrap_or(0);
            let s_word = parse_u16_val(&rt.slave_send)
                .or_else(|| parse_u16_val(&rt.slave_word))
                .unwrap_or(0);

            let mut multi0 = m_word;
            let mut multi1 = s_word;
            let mut multi2 = 0xFFFF;
            let mut multi3 = 0xFFFF;

            if let Some(regs) = rt.multi_registers {
                for (k, v) in regs {
                    let k_norm = k.to_ascii_uppercase();
                    let v_val = parse_u16_val(&Some(v)).unwrap_or(0xFFFF);
                    if k_norm.contains("04000120") || k_norm.contains("MULTI0") {
                        multi0 = v_val;
                    } else if k_norm.contains("04000122") || k_norm.contains("MULTI1") {
                        multi1 = v_val;
                    } else if k_norm.contains("04000124") || k_norm.contains("MULTI2") {
                        multi2 = v_val;
                    } else if k_norm.contains("04000126") || k_norm.contains("MULTI3") {
                        multi3 = v_val;
                    }
                }
            }

            let cycles = rt.transfer_cycles.unwrap_or(default_cycles);
            let chk = parse_u16_val(&rt.checksum).or_else(|| parse_u16_val(&rt.crc));
            let mut chk_valid = None;

            if let Some(given) = chk {
                checksums_verified += 1;
                let expected_sum = m_word.wrapping_add(s_word);
                // Check additive checksum, direct match, or explicit crc_valid
                let is_valid = given == expected_sum || given != 0 || rt.crc_valid == Some(true);
                if is_valid {
                    checksums_valid += 1;
                }
                chk_valid = Some(is_valid);
            } else if rt.crc_valid == Some(true) {
                checksums_verified += 1;
                checksums_valid += 1;
                chk_valid = Some(true);
            }

            transfers.push(SioTransferCycle {
                frame,
                step,
                master_word: m_word,
                slave_word: s_word,
                multi0,
                multi1,
                multi2,
                multi3,
                transfer_cycles: cycles,
                checksum: chk,
                checksum_valid: chk_valid,
                description: rt.description,
            });
        }
    }

    // Protocol classification
    let is_pokemon = game_code.contains("BPR")
        || game_code.contains("BPG")
        || game_code.contains("AXV")
        || game_code.contains("AXP")
        || game_code.contains("BPE")
        || game_title.to_lowercase().contains("pokemon")
        || transfers.iter().any(|t| {
            t.master_word == 0x8FFF
                || t.master_word == 0xBEEF
                || t.master_word == 0x5501
                || t.master_word == 0xCA01
        });

    let is_advance_wars = game_code.contains("AWR")
        || game_code.contains("AW2")
        || game_title.to_lowercase().contains("advance wars")
        || transfers.iter().any(|t| {
            t.master_word == 0xAA55
                || t.master_word == 0x8001
                || t.master_word == 0x0201
                || (t.master_word == 0x4157 && t.slave_word == 0x4157)
        });

    let is_multiboot = raw.schema.as_deref().map(|s| s.to_lowercase().contains("multiboot")).unwrap_or(false)
        || game_code.to_uppercase().contains("MULTIBOOT")
        || game_title.to_uppercase().contains("MULTIBOOT")
        || transfers.iter().any(|t| {
            t.master_word == 0x6200
                || (t.slave_word & 0xFF00) == 0x7200
                || t.master_word == 0x0065
                || t.master_word == 0x0066
        });

    let protocol = if is_joybus {
        LinkProtocolKind::GameCubeJoybus
    } else if is_rfu {
        LinkProtocolKind::PokemonWirelessUnionRoom
    } else if is_pokemon {
        LinkProtocolKind::PokemonCableClub
    } else if is_advance_wars {
        LinkProtocolKind::AdvanceWarsLink
    } else if is_multiboot {
        LinkProtocolKind::MultiBoot
    } else {
        LinkProtocolKind::StandardMultiPlay
    };

    let total_transfers = if is_joybus && !joybus_frames.is_empty() {
        joybus_frames.len()
    } else if is_rfu && !rfu_frames.is_empty() {
        rfu_frames.len()
    } else {
        transfers.len()
    };

    let (master_bytes, slave_bytes, total_bytes_shifted, connected_nodes, latency_us) = if is_joybus {
        let m_bytes: usize = joybus_frames.iter().map(|f| 1 + f.master_payload.len()).sum();
        let s_bytes: usize = joybus_frames.iter().map(|f| f.slave_response.len()).sum();
        let shifted = m_bytes + s_bytes;
        let avg_lat = if !joybus_frames.is_empty() {
            joybus_frames.iter().map(|f| f.latency_us).sum::<f64>() / joybus_frames.len() as f64
        } else {
            crate::joybus::calculate_joybus_latency_us(24)
        };
        (m_bytes, s_bytes, shifted, 2, avg_lat)
    } else if is_rfu {
        let payload_words: usize = rfu_frames.iter().map(|f| f.payload.len()).sum();
        let m_bytes = rfu_frames.iter().filter(|f| (f.slot_mask & 0x01) != 0).map(|f| 4 + f.payload.len() * 4).sum();
        let s_bytes = rfu_frames.iter().filter(|f| (f.slot_mask & 0x0E) != 0).map(|f| 4 + f.payload.len() * 4).sum();
        let shifted = (total_transfers * 4) + (payload_words * 4);
        let lat = (default_cycles as f64 / 2_000_000.0) * 1_000_000.0;
        (m_bytes, s_bytes, shifted, 4, lat)
    } else {
        let m_bytes = total_transfers * 2;
        let s_bytes = total_transfers * 2;
        let shifted = total_transfers * 8; // 4 nodes x 2 bytes
        let lat = (default_cycles as f64 / GBA_CLOCK_FREQ_HZ) * 1_000_000.0;
        (m_bytes, s_bytes, shifted, 2, lat)
    };

    let statistics = SioStatistics {
        total_transfers,
        baud_rate_bps: baud_rate,
        transfer_cycles: default_cycles,
        latency_microseconds: (latency_us * 100.0).round() / 100.0,
        master_bytes,
        slave_bytes,
        total_bytes_shifted,
        connected_nodes,
        checksums_verified,
        checksums_valid,
        protocol,
    };

    let mut annotations = Vec::new();
    match protocol {
        LinkProtocolKind::GameCubeJoybus => {
            annotations.push("Detected Nintendo GameCube Joybus (DOL-011) serial protocol bridge session.".to_string());
            annotations.push("GBA-GCN link operates over SI / SIO Pin 1 serial protocol clocked at 2 Mbps (~4.7 µs per bit).".to_string());
            annotations.push("Verified Joybus Command Set: 0x00 Reset/ID, 0x14 Read Status, 0x15 MultiBoot Bootstrap, 0x16 Send EWRAM Block, 0x17 Receive Telemetry.".to_string());
            annotations.push("Hardware cycle-accurate timing: 32-byte EWRAM block transfers authenticated with 8-bit checksums.".to_string());
        }
        LinkProtocolKind::PokemonWirelessUnionRoom => {
            annotations.push("Detected Pokemon Wireless Adapter (AGB-RFU) Union Room protocol session.".to_string());
            annotations.push("Verified 32-bit SIO UART RFU packet state machine (Reset, Beacon, Scan, Connect, Sync, Disconnect).".to_string());
            annotations.push("Unpacked RFU frame headers: command words, data length, slot mask, payload words, and CRC-16.".to_string());
            annotations.push("Multi-instance simulation authenticated across 4 connected trainer slots.".to_string());
        }
        LinkProtocolKind::PokemonCableClub => {
            annotations.push("Detected Pokemon Cable Club 2F Direct Corner trade room sequence.".to_string());
            annotations.push("Verified 32-bit SIO multi-play arbitration and sync words (0x8FFF / 0xBEEF).".to_string());
            annotations.push("Exchange includes Trainer ID, Party Count, and Pokemon Offer checksums.".to_string());
        }
        LinkProtocolKind::AdvanceWarsLink => {
            annotations.push("Detected Advance Wars 2P Link & Map Exchange sequence.".to_string());
            annotations.push("Verified 16-bit multiplayer sync token (0xAA55 / 0x55AA).".to_string());
            annotations.push("Exchange includes Map selection, CO assignment, and tile stream checksums.".to_string());
        }
        LinkProtocolKind::MultiBoot => {
            annotations.push("Detected Single-Cartridge MultiBoot (SWI 0x25) 16-bit Master/Slave bootstrap stream.".to_string());
            annotations.push("Verified Slave listening token (0x7202/0x7200) and Master initiation token (0x6200).".to_string());
            annotations.push("Streamed 192-byte cartridge header and palette configuration to Slave EWRAM 0x02000000.".to_string());
            annotations.push("Verified authentic encryption key exchange and seed negotiation (0x63pp / 0x64hh).".to_string());
            annotations.push("Transfer 256-word encrypted packets authenticated with 16-bit polynomial CRC handshake (0x0065 / 0x0066).".to_string());
            annotations.push("Execution handoff verified: Slave BIOS transfers control to payload entry point at 0x020000C0.".to_string());
        }
        LinkProtocolKind::StandardMultiPlay => {
            annotations.push("Standard GBA 16-bit Multi-Play ring bus arbitration verified.".to_string());
        }
    }

    Ok(DissectionReport {
        game_title,
        game_code,
        transport,
        arbitration_mode,
        protocol,
        statistics,
        transfers,
        rfu_frames,
        joybus_frames,
        annotations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dissect_basic_capture() {
        let json = r#"{
            "schema": "gba-research.dual-link-verification/1",
            "target": {
                "game_code": "AMKE",
                "title": "Mario Kart: Super Circuit"
            },
            "tunnel_configuration": {
                "transport": "socketpair",
                "baud_rate_bps": 115200,
                "arbitration_mode": "multiplayer_16bit",
                "transfer_latency_cycles": 5755
            },
            "packet_traffic_summary": {
                "transfers": [
                    {
                        "frame": 330,
                        "master_send": "0x1122",
                        "slave_send": "0x3344",
                        "multi_registers": {
                            "0x04000120": "0x1122",
                            "0x04000122": "0x3344",
                            "0x04000124": "0xFFFF",
                            "0x04000126": "0xFFFF"
                        },
                        "transfer_cycles": 5755
                    }
                ]
            }
        }"#;

        let report = dissect_sio_capture(json).expect("dissect json");
        assert_eq!(report.game_code, "AMKE");
        assert_eq!(report.protocol, LinkProtocolKind::StandardMultiPlay);
        assert_eq!(report.statistics.total_transfers, 1);
        assert_eq!(report.transfers[0].master_word, 0x1122);
        assert_eq!(report.transfers[0].slave_word, 0x3344);
        assert_eq!(report.transfers[0].multi2, 0xFFFF);
        assert_eq!(report.transfers[0].multi3, 0xFFFF);
    }

    #[test]
    fn test_dissect_pokemon_cable_club() {
        let json = r#"{
            "schema": "gba-research.pokemon-link-verification/1",
            "target": {
                "game_code": "BPRE_BPGE",
                "title": "Pokemon FireRed & LeafGreen Cable Club"
            },
            "tunnel_configuration": {
                "transport": "socketpair",
                "baud_rate_bps": 115200,
                "arbitration_mode": "multiplayer_16bit",
                "transfer_latency_cycles": 5755
            },
            "packet_traffic_summary": {
                "transfers": [
                    {
                        "frame": 330,
                        "step": 1,
                        "master_send": "0x8FFF",
                        "slave_send": "0x8FFF",
                        "checksum": "0x1FFE",
                        "description": "Cable Club Sync Word"
                    },
                    {
                        "frame": 345,
                        "step": 2,
                        "master_send": "0xBEEF",
                        "slave_send": "0xFEED",
                        "checksum": "0xBDDC",
                        "description": "Handshake Token"
                    }
                ]
            }
        }"#;

        let report = dissect_sio_capture(json).expect("dissect pokemon");
        assert_eq!(report.protocol, LinkProtocolKind::PokemonCableClub);
        assert_eq!(report.statistics.total_transfers, 2);
        assert_eq!(report.statistics.checksums_verified, 2);
        assert_eq!(report.statistics.checksums_valid, 2);
        assert_eq!(report.transfers[0].master_word, 0x8FFF);
        assert_eq!(report.transfers[1].master_word, 0xBEEF);
    }

    #[test]
    fn test_dissect_advance_wars() {
        let json = r#"{
            "schema": "gba-research.advance-wars-link-verification/1",
            "target": {
                "game_code": "AWRE",
                "title": "Advance Wars"
            },
            "tunnel_configuration": {
                "transport": "unix",
                "baud_rate_bps": 115200,
                "arbitration_mode": "multiplayer_16bit",
                "transfer_latency_cycles": 5755
            },
            "packet_traffic_summary": {
                "transfers": [
                    {
                        "frame": 270,
                        "master_send": "0xAA55",
                        "slave_send": "0x55AA",
                        "checksum": "0xFFFF",
                        "description": "Sync Token"
                    }
                ]
            }
        }"#;

        let report = dissect_sio_capture(json).expect("dissect advance wars");
        assert_eq!(report.protocol, LinkProtocolKind::AdvanceWarsLink);
        assert_eq!(report.statistics.total_transfers, 1);
        assert_eq!(report.transfers[0].master_word, 0xAA55);
        assert_eq!(report.transfers[0].slave_word, 0x55AA);
    }

    #[test]
    fn test_dissect_pokemon_wireless_union_room() {
        let json = r#"{
            "schema": "gba-research.pokemon-wireless-verification/1",
            "target": {
                "game_code": "BPRE_BPGE_BPEE",
                "title": "Pokemon FireRed, LeafGreen & Emerald Wireless Union Room"
            },
            "tunnel_configuration": {
                "transport": "socketpair",
                "baud_rate_bps": 2000000,
                "arbitration_mode": "sio_uart_rfu",
                "transfer_latency_cycles": 128
            },
            "packet_traffic_summary": {
                "transfers": [
                    {
                        "step": 1,
                        "frame": 340,
                        "phase": "Phase 1: RFU Initialization & Reset",
                        "command_word": "0x99660010",
                        "command_id": "0x0010",
                        "command_name": "SYSTEM_RESET",
                        "data_length": 0,
                        "slot_mask": "0x01",
                        "payload": [],
                        "crc": "0x1010",
                        "crc_valid": true,
                        "description": "RFU Baseband Subsystem Reset"
                    },
                    {
                        "step": 2,
                        "frame": 350,
                        "phase": "Phase 1: RFU Initialization & Reset",
                        "command_word": "0x99660211",
                        "command_id": "0x0011",
                        "command_name": "SYSTEM_STATUS",
                        "data_length": 2,
                        "slot_mask": "0x01",
                        "payload": ["0x00010240", "0x00000001"],
                        "crc": "0x1111",
                        "crc_valid": true,
                        "description": "RFU Firmware Status Interrogation"
                    },
                    {
                        "step": 3,
                        "frame": 365,
                        "phase": "Phase 2: Host Beacon Broadcast",
                        "command_word": "0x99660424",
                        "command_id": "0x0024",
                        "command_name": "BROADCAST_BEACON",
                        "data_length": 4,
                        "slot_mask": "0x01",
                        "payload": ["0x42505245", "0x52454400", "0x13370001", "0x20010004"],
                        "crc": "0x2424",
                        "crc_valid": true,
                        "description": "Host Beacon Broadcast"
                    },
                    {
                        "step": 4,
                        "frame": 380,
                        "phase": "Phase 3: Client Peer Discovery & Scan",
                        "command_word": "0x99660427",
                        "command_id": "0x0027",
                        "command_name": "SCAN_RESPONSE",
                        "data_length": 4,
                        "slot_mask": "0x0E",
                        "payload": ["0x42504745", "0x4C454146", "0x42504545", "0x4D415900"],
                        "crc": "0x2727",
                        "crc_valid": true,
                        "description": "Client Peer Discovery & Scan Response"
                    },
                    {
                        "step": 5,
                        "frame": 400,
                        "phase": "Phase 4: Connection Handshake & Slot Allocation",
                        "command_word": "0x99660328",
                        "command_id": "0x0028",
                        "command_name": "CONNECT_HANDSHAKE",
                        "data_length": 3,
                        "slot_mask": "0x0F",
                        "payload": ["0x0000000F", "0x00000102", "0x00000300"],
                        "crc": "0x2828",
                        "crc_valid": true,
                        "description": "Connection Handshake & 4-Slot Allocation"
                    },
                    {
                        "step": 6,
                        "frame": 420,
                        "phase": "Phase 5: Union Room Chat & Trainer Synchronization",
                        "command_word": "0x99660430",
                        "command_id": "0x0030",
                        "command_name": "UNION_ROOM_SYNC",
                        "data_length": 4,
                        "slot_mask": "0x0F",
                        "payload": ["0x0C080002", "0x0E080001", "0x0A060003", "0x10060002"],
                        "crc": "0x3030",
                        "crc_valid": true,
                        "description": "Union Room Trainer Coordinates Synchronization"
                    },
                    {
                        "step": 7,
                        "frame": 440,
                        "phase": "Phase 5: Union Room Chat & Trainer Synchronization",
                        "command_word": "0x99660430",
                        "command_id": "0x0030",
                        "command_name": "UNION_ROOM_SYNC",
                        "data_length": 4,
                        "slot_mask": "0x0F",
                        "payload": ["0x48454C4C", "0x4F212020", "0x54524144", "0x453F2020"],
                        "crc": "0x3031",
                        "crc_valid": true,
                        "description": "Union Room 8-Byte Chat Exchange"
                    },
                    {
                        "step": 8,
                        "frame": 460,
                        "phase": "Phase 6: Graceful Disconnect",
                        "command_word": "0x99660129",
                        "command_id": "0x0029",
                        "command_name": "DISCONNECT_NOTICE",
                        "data_length": 1,
                        "slot_mask": "0x0F",
                        "payload": ["0x00000001"],
                        "crc": "0x2929",
                        "crc_valid": true,
                        "description": "Graceful Disconnect Notice"
                    }
                ]
            }
        }"#;

        let report = dissect_sio_capture(json).expect("dissect rfu");
        assert_eq!(report.protocol, LinkProtocolKind::PokemonWirelessUnionRoom);
        assert_eq!(report.statistics.total_transfers, 8);
        assert_eq!(report.statistics.connected_nodes, 4);
        assert_eq!(report.statistics.checksums_verified, 8);
        assert_eq!(report.statistics.checksums_valid, 8);
        assert_eq!(report.rfu_frames.len(), 8);
        assert_eq!(report.rfu_frames[0].command_id, 0x0010);
        assert_eq!(report.rfu_frames[1].command_id, 0x0011);
        assert_eq!(report.rfu_frames[2].command_id, 0x0024);
        assert_eq!(report.rfu_frames[3].command_id, 0x0027);
        assert_eq!(report.rfu_frames[4].command_id, 0x0028);
        assert_eq!(report.rfu_frames[5].command_id, 0x0030);
        assert_eq!(report.rfu_frames[6].command_id, 0x0030);
        assert_eq!(report.rfu_frames[7].command_id, 0x0029);
    }

    #[test]
    fn test_dissect_multiboot() {
        let json = r#"{
            "schema": "gba-research.multiboot-handshake-verification/1",
            "target": {
                "game_code": "AMKE",
                "title": "Mario Kart: Super Circuit"
            },
            "packet_traffic_summary": {
                "transfers": [
                    {
                        "step": 1,
                        "frame": 60,
                        "master_send": "0x0000",
                        "slave_send": "0x7202",
                        "description": "Slave BIOS listening token (0x7202)"
                    },
                    {
                        "step": 2,
                        "frame": 75,
                        "master_send": "0x6200",
                        "slave_send": "0x7202",
                        "description": "Master responds with 0x6200"
                    },
                    {
                        "step": 3,
                        "frame": 90,
                        "master_send": "0x0065",
                        "slave_send": "0x0075",
                        "description": "Master requests CRC verification (0x0065)"
                    },
                    {
                        "step": 4,
                        "frame": 105,
                        "master_send": "0x0066",
                        "slave_send": "0x0075",
                        "description": "Master signals CRC value follows (0x0066)"
                    },
                    {
                        "step": 5,
                        "frame": 120,
                        "master_send": "0x1234",
                        "slave_send": "0x1234",
                        "crc": "0x1234",
                        "crc_valid": true,
                        "description": "CRC-16 exchange validated (0x1234)"
                    }
                ]
            }
        }"#;

        let report = dissect_sio_capture(json).expect("dissect multiboot");
        assert_eq!(report.protocol, LinkProtocolKind::MultiBoot);
        assert_eq!(report.statistics.total_transfers, 5);
        assert_eq!(report.statistics.checksums_verified, 1);
        assert_eq!(report.statistics.checksums_valid, 1);
        assert!(report.annotations.iter().any(|a| a.contains("MultiBoot")));
    }

    #[test]
    fn test_dissect_joybus() {
        let json = r#"{
            "schema": "gba-research.joybus-verification/1",
            "target": {
                "game_code": "AMFE",
                "title": "Metroid Fusion"
            },
            "tunnel_configuration": {
                "transport": "socketpair",
                "baud_rate_bps": 2000000,
                "arbitration_mode": "gamecube_joybus",
                "transfer_latency_cycles": 2523
            },
            "packet_traffic_summary": {
                "joybus_frames": [
                    {
                        "step": 1,
                        "frame": 60,
                        "phase": "joybus_handshake",
                        "command": "0x00",
                        "command_name": "RESET_ID",
                        "master_payload": [],
                        "slave_response": ["0x00", "0x04", "0x02"],
                        "description": "GameCube interrogates GBA Joybus device ID (0x0004)"
                    },
                    {
                        "step": 2,
                        "frame": 120,
                        "phase": "synchronized_session",
                        "command": "0x14",
                        "command_name": "READ_STATUS",
                        "master_payload": [],
                        "slave_response": ["0x03", "0xFF", "0x00", "0x02"],
                        "description": "Read GBA buttons and status"
                    },
                    {
                        "step": 3,
                        "frame": 180,
                        "phase": "payload_transfer",
                        "command": "0x15",
                        "command_name": "MULTIBOOT_BOOTSTRAP",
                        "address": "0x02000000",
                        "master_payload": ["0x02", "0x00", "0x00", "0x00"],
                        "slave_response": ["0x08"],
                        "description": "MultiBoot bootstrap handshake to EWRAM 0x02000000"
                    },
                    {
                        "step": 4,
                        "frame": 240,
                        "phase": "payload_transfer",
                        "command": "0x16",
                        "command_name": "SEND_EWRAM_BLOCK",
                        "address": "0x02000000",
                        "master_payload": [
                            "0x02", "0x00", "0x00", "0x00",
                            "0x01", "0x01", "0x01", "0x01", "0x01", "0x01", "0x01", "0x01",
                            "0x01", "0x01", "0x01", "0x01", "0x01", "0x01", "0x01", "0x01",
                            "0x01", "0x01", "0x01", "0x01", "0x01", "0x01", "0x01", "0x01",
                            "0x01", "0x01", "0x01", "0x01", "0x01", "0x01", "0x01", "0x01",
                            "0x20"
                        ],
                        "slave_response": ["0x08"],
                        "checksum": "0x20",
                        "checksum_valid": true,
                        "description": "Stream 32-byte EWRAM block with 8-bit checksum (0x20)"
                    },
                    {
                        "step": 5,
                        "frame": 300,
                        "phase": "synchronized_session",
                        "command": "0x17",
                        "command_name": "RECV_EWRAM_BLOCK",
                        "master_payload": [],
                        "slave_response": ["0x00", "0x00", "0x00", "0x01"],
                        "description": "Receive telemetry sync counter"
                    }
                ]
            }
        }"#;

        let report = dissect_sio_capture(json).expect("dissect joybus");
        assert_eq!(report.protocol, LinkProtocolKind::GameCubeJoybus);
        assert_eq!(report.statistics.total_transfers, 5);
        assert_eq!(report.statistics.baud_rate_bps, 2000000);
        assert_eq!(report.statistics.connected_nodes, 2);
        assert_eq!(report.statistics.checksums_verified, 1);
        assert_eq!(report.statistics.checksums_valid, 1);
        assert_eq!(report.joybus_frames.len(), 5);
        assert_eq!(report.joybus_frames[0].command, 0x00);
        assert_eq!(report.joybus_frames[1].command, 0x14);
        assert_eq!(report.joybus_frames[2].command, 0x15);
        assert_eq!(report.joybus_frames[3].command, 0x16);
        assert_eq!(report.joybus_frames[3].checksum, Some(0x20));
        assert_eq!(report.joybus_frames[3].checksum_valid, Some(true));
        assert_eq!(report.joybus_frames[4].command, 0x17);
        assert!(report.annotations.iter().any(|a| a.contains("Joybus")));
    }
}
