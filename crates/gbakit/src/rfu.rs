//! Clean-room Game Boy Advance Wireless Adapter (`AGB-015` / `RFU_Vxxxx`)
//! Virtual Adapter & Shared RF Ether State Machine.
//!
//! Emulates the hardware 32-bit Normal Mode SPI (`SIOCNT` bit 12 = 1) command
//! protocol of the Nintendo GBA Wireless Adapter (`AGB-015`), enabling 2 to 4
//! independent GBA processes connected to `gbakit link bridge --mode rfu` to
//! perform authentic RFU handshakes, host broadcasts, client discovery scans,
//! peer connections, bidirectional packet exchange, and clean disconnects.
//!
//! # Hardware Framing (`AGB-015`)
//!
//! - **Link Recovery / Handshake**: GBA sends `0xB0BB_8001` (Master, internal
//!   clock); adapter replies `0x8001_B0BB` (and `0x8001_B0BB` -> `0xB0BB_8001`).
//! - **Command Header**: GBA sends `0x9966_LL_CC` (`LL` = number of 32-bit
//!   parameter words, `CC` = command opcode), followed by `LL` 32-bit words.
//!   Adapter replies `0x8000_0000` (ACK/Ready) during command reception.
//! - **Response Frame**: Adapter switches to Master (GBA uses external clock,
//!   `SIOCNT` bit 0 = 0) and clocks out `0x9966_RR_(0x80 | CC)` followed by
//!   `RR` 32-bit response words.

#![deny(unsafe_code)]

use std::collections::VecDeque;

use crate::link::compute_rfu_crc16;

/// RFU packet magic prefix in bits 31..16 (`0x9966_LL_CC`).
pub const RFU_CMD_MAGIC: u16 = 0x9966;
/// RFU hardware link recovery / detection ping word (GBA -> Adapter).
pub const RFU_HANDSHAKE_PING: u32 = 0xB0BB_8001;
/// RFU hardware link recovery / detection echo word (Adapter -> GBA).
pub const RFU_HANDSHAKE_PONG: u32 = 0x8001_B0BB;
/// RFU ready / idle acknowledgement word shifted out while receiving a command.
pub const RFU_ACK_WORD: u32 = 0x8000_0000;

/// Sio32ID "NINTENDO" handshake low-16-bit halfwords (`"NI"`, `"NT"`, `"EN"`, `"DO"`).
pub const RFU_SIO32ID_NI: u16 = 0x494E;
pub const RFU_SIO32ID_NT: u16 = 0x544E;
pub const RFU_SIO32ID_EN: u16 = 0x4E45;
pub const RFU_SIO32ID_DO: u16 = 0x4F44;
/// AGB-015 Wireless Adapter hardware device ID returned at the end of Sio32ID.
pub const RFU_DEVICE_TYPE_ID: u16 = 0x8001;

/// Status code returned in `NormalTransferResponse.status` when a transfer completed.
pub const RFU_STATUS_OK: u8 = 0;
/// Status code returned when GBA polls in external-clock mode but adapter has no word queued.
pub const RFU_STATUS_PENDING: u8 = 1;

// Standard AGB-015 RFU command opcodes.
pub const RFU_CMD_SYSTEM_RESET: u8 = 0x10;
pub const RFU_CMD_LINK_STATUS: u8 = 0x11;
pub const RFU_CMD_VERSION_STATUS: u8 = 0x12;
pub const RFU_CMD_SYSTEM_STATUS: u8 = 0x13;
pub const RFU_CMD_SLOT_STATUS: u8 = 0x14;
pub const RFU_CMD_CONFIG_STATUS: u8 = 0x16;
pub const RFU_CMD_SET_BROADCAST_DATA: u8 = 0x17;
pub const RFU_CMD_START_BROADCAST: u8 = 0x19;
pub const RFU_CMD_START_SEARCH: u8 = 0x1A;
pub const RFU_CMD_POLL_SEARCH: u8 = 0x1B;
pub const RFU_CMD_END_SEARCH: u8 = 0x1C;
pub const RFU_CMD_GET_BROADCAST_DATA: u8 = 0x1D;
pub const RFU_CMD_START_CONNECT: u8 = 0x1E;
pub const RFU_CMD_POLL_CONNECT: u8 = 0x1F;
pub const RFU_CMD_END_CONNECT: u8 = 0x20;
pub const RFU_CMD_SEND_DATA_READY: u8 = 0x21;
pub const RFU_CMD_SEND_DATA: u8 = 0x24;
pub const RFU_CMD_SEND_DATA_AND_WAIT: u8 = 0x25;
pub const RFU_CMD_RECV_DATA: u8 = 0x26;
pub const RFU_CMD_WAIT: u8 = 0x27;
pub const RFU_CMD_DISCONNECT: u8 = 0x30;
pub const RFU_CMD_SHUTDOWN: u8 = 0x3D;

/// Canonical AGB-015 RFU command / response opcode name.
pub fn rfu_opcode_name(cmd: u8) -> &'static str {
    let base = cmd & 0x7F;
    let is_resp = (cmd & 0x80) != 0;
    match (base, is_resp) {
        (0x10, false) => "SYSTEM_RESET",
        (0x10, true) => "SYSTEM_RESET_RESP",
        (0x11, false) => "LINK_STATUS",
        (0x11, true) => "LINK_STATUS_RESP",
        (0x12, false) => "VERSION_STATUS",
        (0x12, true) => "VERSION_STATUS_RESP",
        (0x13, false) => "SYSTEM_STATUS",
        (0x13, true) => "SYSTEM_STATUS_RESP",
        (0x14, false) => "SLOT_STATUS",
        (0x14, true) => "SLOT_STATUS_RESP",
        (0x16, false) => "CONFIG_STATUS",
        (0x16, true) => "CONFIG_STATUS_RESP",
        (0x17, false) => "SET_BROADCAST_DATA",
        (0x17, true) => "SET_BROADCAST_DATA_RESP",
        (0x19, false) => "START_BROADCAST",
        (0x19, true) => "START_BROADCAST_RESP",
        (0x1A, false) => "START_SEARCH",
        (0x1A, true) => "START_SEARCH_RESP",
        (0x1B, false) => "POLL_SEARCH",
        (0x1B, true) => "POLL_SEARCH_RESP",
        (0x1C, false) => "END_SEARCH",
        (0x1C, true) => "END_SEARCH_RESP",
        (0x1D, false) => "GET_BROADCAST_DATA",
        (0x1D, true) => "GET_BROADCAST_DATA_RESP",
        (0x1E, false) => "START_CONNECT",
        (0x1E, true) => "START_CONNECT_RESP",
        (0x1F, false) => "POLL_CONNECT",
        (0x1F, true) => "POLL_CONNECT_RESP",
        (0x20, false) => "END_CONNECT",
        (0x20, true) => "END_CONNECT_RESP",
        (0x21, false) => "SEND_DATA_READY",
        (0x21, true) => "SEND_DATA_READY_RESP",
        (0x24, false) => "SEND_DATA",
        (0x24, true) => "SEND_DATA_RESP",
        (0x25, false) => "SEND_DATA_AND_WAIT",
        (0x25, true) => "SEND_DATA_AND_WAIT_RESP",
        (0x26, false) => "RECV_DATA",
        (0x26, true) => "RECV_DATA_RESP",
        (0x27, false) => "WAIT_HEARTBEAT",
        (0x27, true) => "WAIT_HEARTBEAT_RESP",
        (0x30, false) => "DISCONNECT",
        (0x30, true) => "DISCONNECT_RESP",
        (0x3D, false) => "SHUTDOWN",
        (0x3D, true) => "SHUTDOWN_RESP",
        (_, false) => "RFU_CMD",
        (_, true) => "RFU_RESP",
    }
}

/// Phase label for JSONL dissection logs based on RFU command opcode.
pub fn rfu_command_phase(cmd: u8) -> &'static str {
    match cmd & 0x7F {
        0x10..=0x16 => "rfu_init",
        0x17 | 0x19 => "rfu_broadcast",
        0x1A..=0x1D => "rfu_search",
        0x1E..=0x20 => "rfu_connect",
        0x21 | 0x24..=0x27 => "rfu_data_exchange",
        0x30 | 0x3D => "rfu_disconnect",
        _ => "rfu_control",
    }
}

/// Encode an AGB-015 RFU 32-bit header word (`0x9966_LL_CC`).
pub fn encode_rfu_header(cmd: u8, word_len: usize) -> u32 {
    let len = (word_len.min(0xFF)) as u32;
    ((RFU_CMD_MAGIC as u32) << 16) | (len << 8) | (cmd as u32)
}

/// Event emitted when a complete RFU command or handshake occurs, for JSONL logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RfuAuditEvent {
    pub slot: usize,
    pub frame: u32,
    pub is_handshake: bool,
    pub phase: String,
    pub command_word: u32,
    pub command_id: u16,
    pub command_name: String,
    pub data_length: u8,
    pub slot_mask: u8,
    pub payload: Vec<u32>,
    pub response_word: u32,
    pub response_payload: Vec<u32>,
    pub crc: u16,
    pub description: String,
}

/// Outcome of a single 32-bit Normal Mode SPI word exchange with a Virtual RFU Adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RfuWordOutcome {
    /// `RFU_STATUS_OK` (0) if a 32-bit word was transferred; `RFU_STATUS_PENDING` (1) if
    /// GBA polled with external clock while the adapter had no word queued yet.
    pub status: u8,
    /// 32-bit word shifted back to the GBA into `SIODATA32`.
    pub rx_word: u32,
    /// Optional completed RFU command or handshake audit event for JSONL capture.
    pub audit: Option<RfuAuditEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdapterRole {
    Idle,
    HostBroadcasting,
    ClientSearching,
    ConnectedHost,
    ConnectedClient,
}

#[derive(Debug, Clone)]
enum RxParserState {
    Idle,
    ReceivingParams {
        cmd: u8,
        expected: usize,
        params: Vec<u32>,
    },
}

#[derive(Debug, Clone)]
struct RfuAdapterSlot {
    slot: usize,
    device_id: u16,
    role: AdapterRole,
    config_word: u32,
    broadcast_data: Vec<u32>,
    /// For a Host: connected client slots (`[Option<peer_slot>; 4]`).
    children: [Option<usize>; 4],
    /// For a Client: `(host_slot, assigned_child_idx)`.
    host_link: Option<(usize, u8)>,
    /// Incoming data packets waiting to be read via `0x26` (`RECV_DATA`).
    rx_packets: VecDeque<Vec<u32>>,
    /// Outgoing 32-bit SPI response words waiting to be clocked to the GBA.
    tx_fifo: VecDeque<u32>,
    /// Command parser state for incoming GBA -> Adapter 32-bit words.
    parser: RxParserState,
    /// Total commands executed on this adapter.
    commands_executed: u64,
    /// Total handshakes completed on this adapter.
    handshakes: u64,
}

impl RfuAdapterSlot {
    fn new(slot: usize) -> Self {
        Self {
            slot,
            device_id: 0x1001u16.wrapping_add(slot as u16),
            role: AdapterRole::Idle,
            config_word: 0,
            broadcast_data: Vec::new(),
            children: [None; 4],
            host_link: None,
            rx_packets: VecDeque::new(),
            tx_fifo: VecDeque::new(),
            parser: RxParserState::Idle,
            commands_executed: 0,
            handshakes: 0,
        }
    }

    fn reset_state(&mut self) {
        self.role = AdapterRole::Idle;
        self.broadcast_data.clear();
        self.children = [None; 4];
        self.host_link = None;
        self.rx_packets.clear();
        self.tx_fifo.clear();
        self.parser = RxParserState::Idle;
    }

    fn enqueue_response(&mut self, cmd: u8, words: &[u32]) -> u32 {
        let resp_cmd = cmd | 0x80;
        let header = encode_rfu_header(resp_cmd, words.len());
        self.tx_fifo.push_back(header);
        for &w in words {
            self.tx_fifo.push_back(w);
        }
        header
    }

    fn connected_mask(&self) -> u8 {
        let mut mask = 0u8;
        for (idx, child) in self.children.iter().enumerate() {
            if child.is_some() {
                mask |= 1u8 << idx;
            }
        }
        if let Some((_, child_idx)) = self.host_link {
            mask |= 1u8 << (child_idx & 0x3);
        }
        mask
    }
}

/// Multi-console Virtual RF Ether connecting 2..=4 [`RfuAdapterSlot`] instances.
#[derive(Debug)]
pub struct RfuVirtualEther {
    adapters: Vec<RfuAdapterSlot>,
    total_commands: u64,
    total_handshakes: u64,
}

impl RfuVirtualEther {
    /// Create a new Virtual RF Ether for `players` (`2..=4`) GBA consoles.
    pub fn new(players: usize) -> Self {
        let count = players.clamp(2, 4);
        let adapters = (0..count).map(RfuAdapterSlot::new).collect();
        Self {
            adapters,
            total_commands: 0,
            total_handshakes: 0,
        }
    }

    /// Return the 16-bit hardware device ID assigned to `slot`.
    pub fn device_id(&self, slot: usize) -> Option<u16> {
        self.adapters.get(slot).map(|a| a.device_id)
    }

    /// Total RFU commands executed across all slots.
    pub fn total_commands(&self) -> u64 {
        self.total_commands
    }

    /// Total RFU link-recovery handshakes across all slots.
    pub fn total_handshakes(&self) -> u64 {
        self.total_handshakes
    }

    /// Process a single 32-bit Normal Mode SPI word exchange for `slot`.
    ///
    /// - When `siocnt & 0x0001 != 0` (Internal Clock, GBA is Master):
    ///   Handles `0xB0BB_8001` / `0x8001_B0BB` handshake, `0x9966_LL_CC` command
    ///   headers, parameter words, and optional `0x8000_0000` response polling.
    /// - When `siocnt & 0x0001 == 0` (External Clock, Adapter is Master):
    ///   Clocks out the next queued 32-bit response word from `tx_fifo`, or returns
    ///   `RFU_STATUS_PENDING` if no response word is currently queued.
    pub fn transfer_word(
        &mut self,
        slot: usize,
        siocnt: u16,
        tx_word: u32,
        frame: u32,
    ) -> RfuWordOutcome {
        if slot >= self.adapters.len() {
            return RfuWordOutcome {
                status: RFU_STATUS_OK,
                rx_word: 0xFFFF_FFFF,
                audit: None,
            };
        }

        let is_internal_clock = (siocnt & 0x0001) != 0;

        // External clock (Adapter -> GBA Master response transfer).
        if !is_internal_clock {
            if let Some(word) = self.adapters[slot].tx_fifo.pop_front() {
                return RfuWordOutcome {
                    status: RFU_STATUS_OK,
                    rx_word: word,
                    audit: None,
                };
            }
            return RfuWordOutcome {
                status: RFU_STATUS_PENDING,
                rx_word: RFU_ACK_WORD,
                audit: None,
            };
        }

        // Internal clock (GBA -> Adapter Master command transfer).
        // 1. Check for hardware link-recovery / Sio32ID ("NINTENDO" -> 0x8001) handshake words.
        let tx_lo = (tx_word & 0xFFFF) as u16;
        let is_sio32id_word = matches!(self.adapters[slot].parser, RxParserState::Idle)
            && matches!(
                tx_lo,
                RFU_SIO32ID_NI
                    | RFU_SIO32ID_NT
                    | RFU_SIO32ID_EN
                    | RFU_SIO32ID_DO
                    | RFU_DEVICE_TYPE_ID
            );
        if tx_word == RFU_HANDSHAKE_PING || tx_word == RFU_HANDSHAKE_PONG || is_sio32id_word {
            let reply = if tx_word == RFU_HANDSHAKE_PING {
                RFU_HANDSHAKE_PONG
            } else if tx_word == RFU_HANDSHAKE_PONG {
                RFU_HANDSHAKE_PING
            } else {
                tx_word.rotate_left(16)
            };
            self.adapters[slot].parser = RxParserState::Idle;
            self.adapters[slot].tx_fifo.clear();
            let is_final_handshake = tx_word == RFU_HANDSHAKE_PING
                || tx_word == RFU_HANDSHAKE_PONG
                || tx_lo == RFU_DEVICE_TYPE_ID;
            let audit = if is_final_handshake {
                self.adapters[slot].handshakes += 1;
                self.total_handshakes += 1;
                let slot_mask = 1u8 << (slot as u8);
                let crc = compute_rfu_crc16(tx_word, slot_mask, &[reply]);
                Some(RfuAuditEvent {
                    slot,
                    frame,
                    is_handshake: true,
                    phase: "rfu_handshake".to_string(),
                    command_word: tx_word,
                    command_id: (tx_word & 0xFFFF) as u16,
                    command_name: "RFU_HANDSHAKE".to_string(),
                    data_length: 1,
                    slot_mask,
                    payload: vec![reply],
                    response_word: reply,
                    response_payload: Vec::new(),
                    crc,
                    description: format!(
                        "Slot {slot} RFU Sio32ID handshake (0x{tx_word:08X} <-> 0x{reply:08X})"
                    ),
                })
            } else {
                None
            };
            return RfuWordOutcome {
                status: RFU_STATUS_OK,
                rx_word: reply,
                audit,
            };
        }

        // 2. Continue receiving parameter words if inside a multi-word command.
        let completed: Option<(u8, Vec<u32>)> = match &mut self.adapters[slot].parser {
            RxParserState::ReceivingParams {
                cmd,
                expected,
                params,
            } => {
                params.push(tx_word);
                if params.len() >= *expected {
                    let c = *cmd;
                    let p = std::mem::take(params);
                    self.adapters[slot].parser = RxParserState::Idle;
                    Some((c, p))
                } else {
                    None
                }
            }
            RxParserState::Idle => {
                // Check for 0x9966_LL_CC command header.
                if ((tx_word >> 16) as u16) == RFU_CMD_MAGIC {
                    let expected = ((tx_word >> 8) & 0xFF) as usize;
                    let cmd = (tx_word & 0xFF) as u8;
                    self.adapters[slot].tx_fifo.clear();
                    if expected == 0 {
                        Some((cmd, Vec::new()))
                    } else {
                        self.adapters[slot].parser = RxParserState::ReceivingParams {
                            cmd,
                            expected,
                            params: Vec::with_capacity(expected),
                        };
                        None
                    }
                } else if tx_word == RFU_ACK_WORD && !self.adapters[slot].tx_fifo.is_empty() {
                    // Convenience: if a host/client clocks 0x8000_0000 with internal clock
                    // while a response is queued, pop the response word directly.
                    let word = self.adapters[slot]
                        .tx_fifo
                        .pop_front()
                        .unwrap_or(RFU_ACK_WORD);
                    return RfuWordOutcome {
                        status: RFU_STATUS_OK,
                        rx_word: word,
                        audit: None,
                    };
                } else {
                    None
                }
            }
        };

        if let Some((cmd, params)) = completed {
            let audit = self.execute_command(slot, cmd, &params, frame);
            return RfuWordOutcome {
                status: RFU_STATUS_OK,
                rx_word: RFU_ACK_WORD,
                audit: Some(audit),
            };
        }

        RfuWordOutcome {
            status: RFU_STATUS_OK,
            rx_word: RFU_ACK_WORD,
            audit: None,
        }
    }

    fn disconnect_slot(&mut self, slot: usize, mask_filter: Option<u8>) {
        // If acting as Host, disconnect matching children.
        let children = self.adapters[slot].children;
        for (child_idx, child_opt) in children.iter().enumerate() {
            if let Some(child_slot) = *child_opt {
                let bit = 1u8 << child_idx;
                if mask_filter.map(|m| (m & bit) != 0).unwrap_or(true) {
                    self.adapters[slot].children[child_idx] = None;
                    if let Some((h, _)) = self.adapters[child_slot].host_link {
                        if h == slot {
                            self.adapters[child_slot].host_link = None;
                            if self.adapters[child_slot].role == AdapterRole::ConnectedClient {
                                self.adapters[child_slot].role = AdapterRole::Idle;
                            }
                        }
                    }
                }
            }
        }
        if self.adapters[slot].children.iter().all(Option::is_none)
            && self.adapters[slot].role == AdapterRole::ConnectedHost
        {
            self.adapters[slot].role = AdapterRole::Idle;
        }

        // If acting as Client, unlink from Host.
        if let Some((host_slot, child_idx)) = self.adapters[slot].host_link {
            let bit = 1u8 << (child_idx & 0x3);
            if mask_filter.map(|m| (m & bit) != 0).unwrap_or(true) {
                self.adapters[slot].host_link = None;
                if (child_idx as usize) < 4
                    && self.adapters[host_slot].children[child_idx as usize] == Some(slot)
                {
                    self.adapters[host_slot].children[child_idx as usize] = None;
                }
                if self.adapters[slot].role == AdapterRole::ConnectedClient {
                    self.adapters[slot].role = AdapterRole::Idle;
                }
            }
        }
    }

    fn connect_client_to_host(&mut self, slot: usize, target_id: u16) {
        let target_host = self.adapters.iter().position(|a| {
            a.slot != slot
                && matches!(
                    a.role,
                    AdapterRole::HostBroadcasting | AdapterRole::ConnectedHost
                )
                && (target_id == 0 || a.device_id == target_id)
        });
        if let Some(host_slot) = target_host {
            let existing = self.adapters[host_slot]
                .children
                .iter()
                .position(|c| *c == Some(slot));
            let child_idx = existing.or_else(|| {
                self.adapters[host_slot]
                    .children
                    .iter()
                    .position(Option::is_none)
            });
            if let Some(cidx) = child_idx {
                self.adapters[host_slot].children[cidx] = Some(slot);
                self.adapters[host_slot].role = AdapterRole::ConnectedHost;
                self.adapters[slot].host_link = Some((host_slot, cidx as u8));
                self.adapters[slot].role = AdapterRole::ConnectedClient;
            }
        }
    }

    fn collect_search_results(&self, searcher_slot: usize) -> Vec<u32> {
        let mut out = Vec::new();
        for adapter in &self.adapters {
            if adapter.slot == searcher_slot {
                continue;
            }
            if matches!(
                adapter.role,
                AdapterRole::HostBroadcasting | AdapterRole::ConnectedHost
            ) {
                // Header word: high 16 bits = slot index & active flag (0x0100 | slot),
                // low 16 bits = host 16-bit device ID.
                let host_word = ((adapter.slot as u32) << 16) | (adapter.device_id as u32);
                out.push(host_word);
                out.extend_from_slice(&adapter.broadcast_data);
            }
        }
        out
    }

    fn execute_command(
        &mut self,
        slot: usize,
        cmd: u8,
        params: &[u32],
        frame: u32,
    ) -> RfuAuditEvent {
        self.adapters[slot].commands_executed += 1;
        self.total_commands += 1;

        let mut resp_words: Vec<u32> = Vec::new();

        match cmd {
            RFU_CMD_SYSTEM_RESET => {
                self.disconnect_slot(slot, None);
                self.adapters[slot].reset_state();
            }
            RFU_CMD_LINK_STATUS => {
                // Return one word per connected peer: (child_idx << 16) | peer_device_id.
                for (child_idx, child_opt) in self.adapters[slot].children.iter().enumerate() {
                    if let Some(child_slot) = *child_opt {
                        let pid = self.adapters[child_slot].device_id as u32;
                        resp_words.push(((child_idx as u32) << 16) | pid);
                    }
                }
                if let Some((host_slot, child_idx)) = self.adapters[slot].host_link {
                    let hid = self.adapters[host_slot].device_id as u32;
                    resp_words.push(((child_idx as u32) << 16) | hid);
                }
            }
            RFU_CMD_VERSION_STATUS => {
                // AGB-015 firmware/hardware version word.
                resp_words.push(0x0000_0001);
            }
            RFU_CMD_SYSTEM_STATUS => {
                let mode_bits: u32 = match self.adapters[slot].role {
                    AdapterRole::Idle => 0x0000,
                    AdapterRole::HostBroadcasting => 0x0001,
                    AdapterRole::ClientSearching => 0x0002,
                    AdapterRole::ConnectedHost => 0x0003,
                    AdapterRole::ConnectedClient => 0x0004,
                };
                let dev_id = self.adapters[slot].device_id as u32;
                resp_words.push((mode_bits << 16) | dev_id);
            }
            RFU_CMD_SLOT_STATUS => {
                for (child_idx, child_opt) in self.adapters[slot].children.iter().enumerate() {
                    if let Some(child_slot) = *child_opt {
                        let pid = self.adapters[child_slot].device_id as u32;
                        resp_words.push(((child_idx as u32) << 16) | pid);
                    }
                }
                if let Some((host_slot, child_idx)) = self.adapters[slot].host_link {
                    let hid = self.adapters[host_slot].device_id as u32;
                    resp_words.push(((child_idx as u32) << 16) | hid);
                }
            }
            RFU_CMD_CONFIG_STATUS => {
                // In RFU_V1024, 0x16 with >=2 words is SET_BROADCAST_DATA; with 1 word it is CONFIG_STATUS.
                if params.len() >= 2 {
                    self.adapters[slot].broadcast_data = params.to_vec();
                } else if let Some(&cfg) = params.first() {
                    self.adapters[slot].config_word = cfg;
                }
            }
            RFU_CMD_SET_BROADCAST_DATA => {
                // In RFU_V1024, 0x17 with 1 word is SYSTEM_CONFIG; with >=2 words it is SET_BROADCAST_DATA.
                if params.len() == 1 {
                    self.adapters[slot].config_word = params[0];
                } else {
                    self.adapters[slot].broadcast_data = params.to_vec();
                }
            }
            RFU_CMD_START_BROADCAST => {
                self.adapters[slot].role = AdapterRole::HostBroadcasting;
            }
            RFU_CMD_START_SEARCH => {
                if self.adapters[slot].role == AdapterRole::HostBroadcasting
                    || self.adapters[slot].role == AdapterRole::ConnectedHost
                {
                    // In RFU_V1024, 0x1A while Host is POLL_BROADCAST_CHILDREN.
                    for (child_idx, child_opt) in self.adapters[slot].children.iter().enumerate() {
                        if let Some(child_slot) = *child_opt {
                            let pid = self.adapters[child_slot].device_id as u32;
                            resp_words.push(((child_idx as u32) << 16) | pid);
                        }
                    }
                } else {
                    self.adapters[slot].role = AdapterRole::ClientSearching;
                }
            }
            RFU_CMD_POLL_SEARCH => {
                resp_words = self.collect_search_results(slot);
            }
            RFU_CMD_END_SEARCH => {
                if self.adapters[slot].role == AdapterRole::ClientSearching {
                    resp_words = self.collect_search_results(slot);
                    self.adapters[slot].role = AdapterRole::Idle;
                } else if self.adapters[slot].role == AdapterRole::HostBroadcasting {
                    self.adapters[slot].role = AdapterRole::Idle;
                } else {
                    // In RFU_V1024, 0x1C from Idle is START_SEARCH_PARENT.
                    self.adapters[slot].role = AdapterRole::ClientSearching;
                }
            }
            RFU_CMD_GET_BROADCAST_DATA => {
                let filter_id = params.first().map(|&w| (w & 0xFFFF) as u16);
                for adapter in &self.adapters {
                    if adapter.slot == slot {
                        continue;
                    }
                    let matches_id = filter_id
                        .map(|id| id == 0 || id == adapter.device_id)
                        .unwrap_or(true);
                    if matches_id && !adapter.broadcast_data.is_empty() {
                        resp_words.push(((adapter.slot as u32) << 16) | (adapter.device_id as u32));
                        resp_words.extend_from_slice(&adapter.broadcast_data);
                    }
                }
            }
            RFU_CMD_START_CONNECT => {
                if params.is_empty() && self.adapters[slot].role == AdapterRole::ClientSearching {
                    // In RFU_V1024, 0x1E with 0 params is END_SEARCH_PARENT.
                    resp_words = self.collect_search_results(slot);
                    self.adapters[slot].role = AdapterRole::Idle;
                } else {
                    let target_id = params.first().map(|&w| (w & 0xFFFF) as u16).unwrap_or(0);
                    self.connect_client_to_host(slot, target_id);
                }
            }
            RFU_CMD_POLL_CONNECT | RFU_CMD_END_CONNECT | RFU_CMD_SEND_DATA_READY => {
                if cmd == RFU_CMD_POLL_CONNECT && !params.is_empty() {
                    // In RFU_V1024, 0x1F with 1 param is START_CONNECT_PARENT(target_id).
                    let target_id = (params[0] & 0xFFFF) as u16;
                    self.connect_client_to_host(slot, target_id);
                } else if let Some((host_slot, child_idx)) = self.adapters[slot].host_link {
                    let hid = self.adapters[host_slot].device_id as u32;
                    resp_words.push(((child_idx as u32) << 16) | hid);
                } else {
                    for (child_idx, child_opt) in self.adapters[slot].children.iter().enumerate() {
                        if let Some(child_slot) = *child_opt {
                            let pid = self.adapters[child_slot].device_id as u32;
                            resp_words.push(((child_idx as u32) << 16) | pid);
                        }
                    }
                }
            }
            RFU_CMD_SEND_DATA | RFU_CMD_SEND_DATA_AND_WAIT => {
                if !params.is_empty() {
                    // Host -> all connected children.
                    let children = self.adapters[slot].children;
                    let mut delivered = false;
                    for child_slot in children.into_iter().flatten() {
                        self.adapters[child_slot].rx_packets.push_back(params.to_vec());
                        delivered = true;
                    }
                    // Client -> its connected host.
                    if let Some((host_slot, _)) = self.adapters[slot].host_link {
                        self.adapters[host_slot].rx_packets.push_back(params.to_vec());
                        delivered = true;
                    }
                    let _ = delivered;
                }
            }
            RFU_CMD_RECV_DATA => {
                if let Some(pkt) = self.adapters[slot].rx_packets.pop_front() {
                    resp_words = pkt;
                }
            }
            RFU_CMD_WAIT => {}
            RFU_CMD_DISCONNECT => {
                let mask = params.first().map(|&w| (w & 0xFF) as u8);
                self.disconnect_slot(slot, mask);
            }
            RFU_CMD_SHUTDOWN => {
                self.disconnect_slot(slot, None);
                self.adapters[slot].reset_state();
            }
            _ => {
                // Unknown/auxiliary RFU command: acknowledge cleanly with 0 response words.
            }
        }

        let resp_header = self.adapters[slot].enqueue_response(cmd, &resp_words);
        let cmd_word = encode_rfu_header(cmd, params.len());
        let slot_mask = (1u8 << (slot as u8)) | self.adapters[slot].connected_mask();
        let audit_payload = if !params.is_empty() {
            params.to_vec()
        } else {
            resp_words.clone()
        };
        let crc = compute_rfu_crc16(cmd_word, slot_mask, &audit_payload);
        let name = rfu_opcode_name(cmd).to_string();
        let role_label = match self.adapters[slot].role {
            AdapterRole::HostBroadcasting | AdapterRole::ConnectedHost => "Host",
            AdapterRole::ClientSearching | AdapterRole::ConnectedClient => "Client",
            AdapterRole::Idle => "Adapter",
        };
        let description = format!(
            "Slot {slot} ({role_label}) {name} (tx={} words, rx={} words)",
            params.len(),
            resp_words.len()
        );

        RfuAuditEvent {
            slot,
            frame,
            is_handshake: false,
            phase: rfu_command_phase(cmd).to_string(),
            command_word: cmd_word,
            command_id: cmd as u16,
            command_name: name,
            data_length: audit_payload.len().min(0xFF) as u8,
            slot_mask,
            payload: audit_payload,
            response_word: resp_header,
            response_payload: resp_words,
            crc,
            description,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: send a full RFU command (header + params) with internal clock,
    /// then read back the response header + response words with external clock.
    fn run_rfu_cmd(
        ether: &mut RfuVirtualEther,
        slot: usize,
        cmd: u8,
        params: &[u32],
        frame: u32,
    ) -> (u32, Vec<u32>) {
        let header = encode_rfu_header(cmd, params.len());
        let out = ether.transfer_word(slot, 0x5081, header, frame);
        assert_eq!(out.status, RFU_STATUS_OK);
        assert_eq!(out.rx_word, RFU_ACK_WORD);

        for &p in params {
            let p_out = ether.transfer_word(slot, 0x5081, p, frame);
            assert_eq!(p_out.status, RFU_STATUS_OK);
            assert_eq!(p_out.rx_word, RFU_ACK_WORD);
        }

        // Switch to external clock (SIOCNT bit 0 = 0) to clock in response header.
        let r_hdr = ether.transfer_word(slot, 0x5080, RFU_ACK_WORD, frame);
        assert_eq!(r_hdr.status, RFU_STATUS_OK);
        assert_eq!((r_hdr.rx_word >> 16) as u16, RFU_CMD_MAGIC);
        assert_eq!((r_hdr.rx_word & 0xFF) as u8, cmd | 0x80);
        let resp_len = ((r_hdr.rx_word >> 8) & 0xFF) as usize;

        let mut resp_words = Vec::with_capacity(resp_len);
        for _ in 0..resp_len {
            let rw = ether.transfer_word(slot, 0x5080, RFU_ACK_WORD, frame);
            assert_eq!(rw.status, RFU_STATUS_OK);
            resp_words.push(rw.rx_word);
        }
        (r_hdr.rx_word, resp_words)
    }

    #[test]
    fn test_rfu_handshake_and_init_commands() {
        let mut ether = RfuVirtualEther::new(2);

        // 0xB0BB_8001 -> 0x8001_B0BB and vice-versa
        let h0 = ether.transfer_word(0, 0x5081, RFU_HANDSHAKE_PING, 1);
        assert_eq!(h0.status, RFU_STATUS_OK);
        assert_eq!(h0.rx_word, RFU_HANDSHAKE_PONG);
        assert!(h0.audit.as_ref().unwrap().is_handshake);

        let h1 = ether.transfer_word(1, 0x5081, RFU_HANDSHAKE_PONG, 1);
        assert_eq!(h1.status, RFU_STATUS_OK);
        assert_eq!(h1.rx_word, RFU_HANDSHAKE_PING);
        assert_eq!(ether.total_handshakes(), 2);

        // System Reset (0x10), Version Status (0x12), System Status (0x13)
        let (_, r10) = run_rfu_cmd(&mut ether, 0, RFU_CMD_SYSTEM_RESET, &[], 2);
        assert!(r10.is_empty());

        let (_, r12) = run_rfu_cmd(&mut ether, 0, RFU_CMD_VERSION_STATUS, &[], 3);
        assert_eq!(r12, vec![0x0000_0001]);

        let (_, r13_0) = run_rfu_cmd(&mut ether, 0, RFU_CMD_SYSTEM_STATUS, &[], 4);
        let (_, r13_1) = run_rfu_cmd(&mut ether, 1, RFU_CMD_SYSTEM_STATUS, &[], 4);
        assert_eq!(r13_0[0] & 0xFFFF, 0x1001);
        assert_eq!(r13_1[0] & 0xFFFF, 0x1002);

        // External clock poll when FIFO is empty returns RFU_STATUS_PENDING.
        let pending = ether.transfer_word(0, 0x5080, RFU_ACK_WORD, 5);
        assert_eq!(pending.status, RFU_STATUS_PENDING);
    }

    #[test]
    fn test_rfu_full_session_broadcast_search_connect_exchange_disconnect() {
        let mut ether = RfuVirtualEther::new(2);

        // 1. Host (slot 0) sets broadcast data (0x17) and starts broadcasting (0x19).
        let beacon = [0x4250_5245, 0x5245_4400, 0x0001_1337]; // "BPRE", "RED\0", metadata
        let (_, r17) = run_rfu_cmd(&mut ether, 0, RFU_CMD_SET_BROADCAST_DATA, &beacon, 10);
        assert!(r17.is_empty());
        let (_, r19) = run_rfu_cmd(&mut ether, 0, RFU_CMD_START_BROADCAST, &[], 11);
        assert!(r19.is_empty());

        // 2. Client (slot 1) starts search (0x1A) and polls search (0x1B) -> discovers Host!
        run_rfu_cmd(&mut ether, 1, RFU_CMD_START_SEARCH, &[], 12);
        let (_, found) = run_rfu_cmd(&mut ether, 1, RFU_CMD_POLL_SEARCH, &[], 13);
        assert_eq!(found.len(), 1 + beacon.len());
        assert_eq!(found[0] & 0xFFFF, 0x1001); // Host device ID
        assert_eq!(&found[1..], &beacon);

        // Client can also query 0x1D (GET_BROADCAST_DATA) by host device ID.
        let (_, bcast) = run_rfu_cmd(&mut ether, 1, RFU_CMD_GET_BROADCAST_DATA, &[0x1001], 14);
        assert_eq!(&bcast[1..], &beacon);
        run_rfu_cmd(&mut ether, 1, RFU_CMD_END_SEARCH, &[], 15);

        // 3. Client connects to Host (0x1E) and both poll connect (0x1F) / link status (0x11).
        run_rfu_cmd(&mut ether, 1, RFU_CMD_START_CONNECT, &[0x1001], 16);
        let (_, c_conn) = run_rfu_cmd(&mut ether, 1, RFU_CMD_POLL_CONNECT, &[], 17);
        assert_eq!(c_conn, vec![0x0000_1001]); // child_idx=0, host_id=0x1001
        let (_, h_conn) = run_rfu_cmd(&mut ether, 0, RFU_CMD_LINK_STATUS, &[], 17);
        assert_eq!(h_conn, vec![0x0000_1002]); // child_idx=0, client_id=0x1002
        let (_, c_end) = run_rfu_cmd(&mut ether, 1, RFU_CMD_END_CONNECT, &[], 18);
        assert_eq!(c_end, vec![0x0000_1001]);

        // 4. Bidirectional data exchange: Host 0x24 -> Client 0x26, Client 0x25 -> Host 0x26.
        let host_msg = [0xCAFE_BABE, 0x1122_3344];
        run_rfu_cmd(&mut ether, 0, RFU_CMD_SEND_DATA, &host_msg, 20);
        let (_, client_rx) = run_rfu_cmd(&mut ether, 1, RFU_CMD_RECV_DATA, &[], 21);
        assert_eq!(client_rx, host_msg);

        let client_msg = [0xDEAD_BEEF, 0x5566_7788];
        run_rfu_cmd(&mut ether, 1, RFU_CMD_SEND_DATA_AND_WAIT, &client_msg, 22);
        let (_, host_rx) = run_rfu_cmd(&mut ether, 0, RFU_CMD_RECV_DATA, &[], 23);
        assert_eq!(host_rx, client_msg);

        // Heartbeat (0x27)
        let (_, hb) = run_rfu_cmd(&mut ether, 0, RFU_CMD_WAIT, &[], 24);
        assert!(hb.is_empty());

        // 5. Disconnect (0x30) & Shutdown (0x3D) -> Link Status is now empty.
        run_rfu_cmd(&mut ether, 1, RFU_CMD_DISCONNECT, &[0x01], 25);
        let (_, h_after) = run_rfu_cmd(&mut ether, 0, RFU_CMD_LINK_STATUS, &[], 26);
        assert!(h_after.is_empty());
        run_rfu_cmd(&mut ether, 0, RFU_CMD_SHUTDOWN, &[], 27);
        assert!(ether.total_commands() >= 16);
    }

    #[test]
    fn test_rfu_v1024_sio32id_nintendo_handshake_and_parent_discovery() {
        let mut ether = RfuVirtualEther::new(2);

        // Authentic RFU_V1024 Sio32ID "NINTENDO" handshake sequence (rot16 echo -> 0x8001_B0BB).
        let sio32id_words: [(u32, u32); 5] = [
            (0xB6B1_494E, 0x494E_B6B1), // "NI"
            (0xB6B1_544E, 0x544E_B6B1), // "NT"
            (0xABB1_4E45, 0x4E45_ABB1), // "EN"
            (0xB1BA_4F44, 0x4F44_B1BA), // "DO"
            (0xB0BB_8001, 0x8001_B0BB), // AGB-015 Wireless Adapter ID
        ];
        for (tx, expected_rx) in sio32id_words {
            let out = ether.transfer_word(0, 0x5081, tx, 1);
            assert_eq!(out.status, RFU_STATUS_OK);
            assert_eq!(out.rx_word, expected_rx);
        }
        assert_eq!(ether.total_handshakes(), 1);

        // RFU_V1024 command mapping: 0x17 (1-word config), 0x16 (6-word broadcast), 0x1C (start search), 0x1D (poll).
        run_rfu_cmd(&mut ether, 0, RFU_CMD_SET_BROADCAST_DATA, &[0x003C_0420], 2);
        let red_beacon = [0x1282_0002, 0x0000_E197, 0x0000_0000, 0xAB00_0015, 0xFFBE_BFCC, 0x0000_0000];
        run_rfu_cmd(&mut ether, 0, RFU_CMD_CONFIG_STATUS, &red_beacon, 3);
        run_rfu_cmd(&mut ether, 1, RFU_CMD_END_SEARCH, &[], 4); // 0x1C from Idle -> START_SEARCH_PARENT
        let (_, discovered) = run_rfu_cmd(&mut ether, 1, RFU_CMD_GET_BROADCAST_DATA, &[], 5);
        assert_eq!(discovered.len(), 7);
        assert_eq!(discovered[0] & 0xFFFF, 0x1001);
        assert_eq!(&discovered[1..], &red_beacon);
    }
}
