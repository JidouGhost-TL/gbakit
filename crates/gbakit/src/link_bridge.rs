//! Clean-room cross-platform TCP GBA SIO Multi-Play Link-Cable Relay/Bridge Server.
//!
//! Implements the `GBALINK1` fixed-size binary wire protocol over standard TCP
//! (`std::net::TcpListener` / `TcpStream`), allowing 2 to 4 independent GBA
//! emulator processes to perform hardware-accurate 16-bit SIO Multi-Play
//! (`SIOMULTI0..3`) transfers across OS processes or over loopback/LAN.
//!
//! # Wire Protocol (`GBALINK1`, Little-Endian)
//!
//! 1. **Client -> Server Hello** (8 bytes):
//!    - `b"GBAL"` (4 bytes)
//!    - `version`: `u8` (`1`)
//!    - `requested_slot`: `u8` (`0..=3` for explicit slot, `0xFF` for auto-assign)
//!    - `flags`: `u8` (bit 0 = `FLAG_SLAVE_POLL`: server sends `MSG_POLL` to this slave when master initiates a transfer)
//!    - `reserved`: `u8` (`0`)
//!
//! 2. **Server -> Client Welcome** (8 bytes, sent once all `players` have connected):
//!    - `b"GBOK"` (4 bytes)
//!    - `assigned_slot`: `u8` (`0..=3`; slot `0` is Master, `1..=3` are Slaves)
//!    - `total_players`: `u8` (`2..=4`)
//!    - `flags`: `u8` (`0`)
//!    - `reserved`: `u8` (`0`)
//!
//! 3. **Client -> Server Transfer Request** (10 bytes):
//!    - `msg_type`: `u8` (`1`)
//!    - `slot`: `u8` (`0..=3`)
//!    - `siocnt`: `u16` (LE)
//!    - `send_word`: `u16` (LE, value of `SIOMLT_SEND`)
//!    - `frame`: `u32` (LE, emulator frame counter or sequence)
//!
//! 4. **Server -> Client Transfer Response** (12 bytes):
//!    - `msg_type`: `u8` (`2`)
//!    - `status`: `u8` (`0` = OK)
//!    - `multi`: `[u16; 4]` (LE, `SIOMULTI0..3`, unused slots = `0xFFFF`)
//!    - `seq`: `u16` (LE, wrapping 16-bit transfer sequence counter)
//!
//! 5. **Client -> Server Bye** (1 byte):
//!    - `msg_type`: `u8` (`3`) — clean disconnect
//!
//! 6. **Server -> Client Poll** (4 bytes, sent only to slaves with `FLAG_SLAVE_POLL`):
//!    - `msg_type`: `u8` (`4`)
//!    - `master_slot`: `u8` (`0`)
//!    - `siocnt`: `u16` (LE, master's `SIOCNT`)
//!
//! 7. **Client -> Server Frame Sync** (6 bytes, optional lockstep frame barrier):
//!    - `msg_type`: `u8` (`5`)
//!    - `slot`: `u8` (`0..=3`)
//!    - `frame`: `u32` (LE)
//!
//! 8. **Server -> Client Frame Ack** (6 bytes):
//!    - `msg_type`: `u8` (`6`)
//!    - `status`: `u8` (`0` = OK)
//!    - `frame`: `u32` (LE)
//!
//! 9. **Client -> Server Normal Mode SPI Request** (14 bytes):
//!    - `msg_type`: `u8` (`7`)
//!    - `slot`: `u8` (`0..=1`)
//!    - `width_bits`: `u8` (`8` or `32`)
//!    - `reserved`: `u8` (`0`)
//!    - `siocnt`: `u16` (LE)
//!    - `tx_word`: `u32` (LE, value of `SIODATA8` or `SIODATA32`)
//!    - `frame`: `u32` (LE, emulator frame counter or sequence)
//!
//! 10. **Server -> Client Normal Mode SPI Response** (12 bytes):
//!     - `msg_type`: `u8` (`8`)
//!     - `status`: `u8` (`0` = OK)
//!     - `width_bits`: `u8` (`8` or `32`)
//!     - `reserved`: `u8` (`0`)
//!     - `rx_word`: `u32` (LE, peer's swapped `tx_word`, masked to `0xFF` for 8-bit)
//!     - `seq`: `u32` (LE, Normal Mode transfer sequence counter)

#![deny(unsafe_code)]

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::link::{BAUD_115200, BAUD_38400, BAUD_57600, BAUD_9600, CYCLES_115200_1SLAVE};
use crate::rfu::{RfuVirtualEther, RFU_STATUS_OK};

/// Magic header for Client -> Server Hello (`GBAL`).
pub const PROTOCOL_MAGIC_HELLO: [u8; 4] = *b"GBAL";
/// Magic header for Server -> Client Welcome (`GBOK`).
pub const PROTOCOL_MAGIC_WELCOME: [u8; 4] = *b"GBOK";
/// Current wire protocol version.
pub const PROTOCOL_VERSION: u8 = 1;
/// Sentinel value in Hello `requested_slot` for automatic slot assignment.
pub const SLOT_AUTO: u8 = 0xFF;

/// Hello flag: slave wants server-initiated `MSG_POLL` whenever master initiates a transfer.
pub const FLAG_SLAVE_POLL: u8 = 0x01;

/// Message type: Client -> Server SIO Multi-Play transfer submission (10 bytes).
pub const MSG_TRANSFER_REQ: u8 = 1;
/// Message type: Server -> Client SIO Multi-Play transfer broadcast (12 bytes).
pub const MSG_TRANSFER_RESP: u8 = 2;
/// Message type: Client -> Server clean disconnect (1 byte).
pub const MSG_BYE: u8 = 3;
/// Message type: Server -> Slave poll for current `SIOMLT_SEND` word (4 bytes).
pub const MSG_POLL: u8 = 4;
/// Message type: Client -> Server optional frame-boundary lockstep barrier (6 bytes).
pub const MSG_FRAME_SYNC: u8 = 5;
/// Message type: Server -> Client frame-boundary barrier release (6 bytes).
pub const MSG_FRAME_ACK: u8 = 6;
/// Message type: Client -> Server Normal Mode (8-bit / 32-bit SPI) transfer request (14 bytes).
pub const MSG_NORMAL_REQ: u8 = 7;
/// Message type: Server -> Client Normal Mode (8-bit / 32-bit SPI) transfer response (12 bytes).
pub const MSG_NORMAL_RESP: u8 = 8;

/// Client -> Server Hello packet (8 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelloPacket {
    pub version: u8,
    pub requested_slot: u8,
    pub flags: u8,
}

impl HelloPacket {
    pub fn to_bytes(self) -> [u8; 8] {
        [
            PROTOCOL_MAGIC_HELLO[0],
            PROTOCOL_MAGIC_HELLO[1],
            PROTOCOL_MAGIC_HELLO[2],
            PROTOCOL_MAGIC_HELLO[3],
            self.version,
            self.requested_slot,
            self.flags,
            0,
        ]
    }

    pub fn from_bytes(buf: [u8; 8]) -> Result<Self, String> {
        if buf[0..4] != PROTOCOL_MAGIC_HELLO {
            return Err(format!("invalid GBALINK1 hello magic: {:?}", &buf[0..4]));
        }
        if buf[4] != PROTOCOL_VERSION {
            return Err(format!(
                "unsupported GBALINK1 protocol version {} (expected {})",
                buf[4], PROTOCOL_VERSION
            ));
        }
        Ok(Self {
            version: buf[4],
            requested_slot: buf[5],
            flags: buf[6],
        })
    }
}

/// Server -> Client Welcome packet (8 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WelcomePacket {
    pub assigned_slot: u8,
    pub total_players: u8,
    pub flags: u8,
}

impl WelcomePacket {
    pub fn to_bytes(self) -> [u8; 8] {
        [
            PROTOCOL_MAGIC_WELCOME[0],
            PROTOCOL_MAGIC_WELCOME[1],
            PROTOCOL_MAGIC_WELCOME[2],
            PROTOCOL_MAGIC_WELCOME[3],
            self.assigned_slot,
            self.total_players,
            self.flags,
            0,
        ]
    }

    pub fn from_bytes(buf: [u8; 8]) -> Result<Self, String> {
        if buf[0..4] != PROTOCOL_MAGIC_WELCOME {
            return Err(format!("invalid GBALINK1 welcome magic: {:?}", &buf[0..4]));
        }
        Ok(Self {
            assigned_slot: buf[4],
            total_players: buf[5],
            flags: buf[6],
        })
    }
}

/// Client -> Server transfer submission (10 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferRequest {
    pub slot: u8,
    pub siocnt: u16,
    pub send_word: u16,
    pub frame: u32,
}

impl TransferRequest {
    pub fn to_bytes(self) -> [u8; 10] {
        let sc = self.siocnt.to_le_bytes();
        let sw = self.send_word.to_le_bytes();
        let fr = self.frame.to_le_bytes();
        [
            MSG_TRANSFER_REQ,
            self.slot,
            sc[0],
            sc[1],
            sw[0],
            sw[1],
            fr[0],
            fr[1],
            fr[2],
            fr[3],
        ]
    }

    pub fn from_payload(payload: [u8; 9]) -> Self {
        Self {
            slot: payload[0],
            siocnt: u16::from_le_bytes([payload[1], payload[2]]),
            send_word: u16::from_le_bytes([payload[3], payload[4]]),
            frame: u32::from_le_bytes([payload[5], payload[6], payload[7], payload[8]]),
        }
    }
}

/// Server -> Client transfer broadcast (12 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferResponse {
    pub status: u8,
    pub multi: [u16; 4],
    pub seq: u16,
}

impl TransferResponse {
    pub fn to_bytes(self) -> [u8; 12] {
        let m0 = self.multi[0].to_le_bytes();
        let m1 = self.multi[1].to_le_bytes();
        let m2 = self.multi[2].to_le_bytes();
        let m3 = self.multi[3].to_le_bytes();
        let sq = self.seq.to_le_bytes();
        [
            MSG_TRANSFER_RESP,
            self.status,
            m0[0],
            m0[1],
            m1[0],
            m1[1],
            m2[0],
            m2[1],
            m3[0],
            m3[1],
            sq[0],
            sq[1],
        ]
    }

    pub fn from_bytes(buf: [u8; 12]) -> Result<Self, String> {
        if buf[0] != MSG_TRANSFER_RESP {
            return Err(format!(
                "expected TransferResp (msg_type={MSG_TRANSFER_RESP}), got {}",
                buf[0]
            ));
        }
        Ok(Self {
            status: buf[1],
            multi: [
                u16::from_le_bytes([buf[2], buf[3]]),
                u16::from_le_bytes([buf[4], buf[5]]),
                u16::from_le_bytes([buf[6], buf[7]]),
                u16::from_le_bytes([buf[8], buf[9]]),
            ],
            seq: u16::from_le_bytes([buf[10], buf[11]]),
        })
    }
}

/// Client -> Server Normal Mode (8-bit / 32-bit SPI) transfer submission (14 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalTransferRequest {
    pub slot: u8,
    pub width_bits: u8,
    pub siocnt: u16,
    pub tx_word: u32,
    pub frame: u32,
}

impl NormalTransferRequest {
    pub fn to_bytes(self) -> [u8; 14] {
        let sc = self.siocnt.to_le_bytes();
        let tx = self.tx_word.to_le_bytes();
        let fr = self.frame.to_le_bytes();
        [
            MSG_NORMAL_REQ,
            self.slot,
            self.width_bits,
            0,
            sc[0],
            sc[1],
            tx[0],
            tx[1],
            tx[2],
            tx[3],
            fr[0],
            fr[1],
            fr[2],
            fr[3],
        ]
    }

    pub fn from_payload(payload: [u8; 13]) -> Self {
        Self {
            slot: payload[0],
            width_bits: payload[1],
            siocnt: u16::from_le_bytes([payload[3], payload[4]]),
            tx_word: u32::from_le_bytes([payload[5], payload[6], payload[7], payload[8]]),
            frame: u32::from_le_bytes([payload[9], payload[10], payload[11], payload[12]]),
        }
    }
}

/// Server -> Client Normal Mode (8-bit / 32-bit SPI) transfer response (12 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalTransferResponse {
    pub status: u8,
    pub width_bits: u8,
    pub rx_word: u32,
    pub seq: u32,
}

impl NormalTransferResponse {
    pub fn to_bytes(self) -> [u8; 12] {
        let rx = self.rx_word.to_le_bytes();
        let sq = self.seq.to_le_bytes();
        [
            MSG_NORMAL_RESP,
            self.status,
            self.width_bits,
            0,
            rx[0],
            rx[1],
            rx[2],
            rx[3],
            sq[0],
            sq[1],
            sq[2],
            sq[3],
        ]
    }

    pub fn from_bytes(buf: [u8; 12]) -> Result<Self, String> {
        if buf[0] != MSG_NORMAL_RESP {
            return Err(format!(
                "expected NormalTransferResp (msg_type={MSG_NORMAL_RESP}), got {}",
                buf[0]
            ));
        }
        Ok(Self {
            status: buf[1],
            width_bits: buf[2],
            rx_word: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            seq: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
        })
    }
}

/// Operating mode for [`LinkBridgeServer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkBridgeMode {
    /// Standard GBA Link-Cable relay (16-bit Multi-Play + 8/32-bit Normal Mode SPI).
    #[default]
    Multi,
    /// GBA Wireless Adapter (`AGB-015` / `RFU_Vxxxx`) 32-bit Virtual RF Ether relay.
    Rfu,
}

impl LinkBridgeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Multi => "multi",
            Self::Rfu => "rfu",
        }
    }
}

/// Configuration for [`LinkBridgeServer`].
#[derive(Debug, Clone)]
pub struct LinkBridgeConfig {
    /// Listen address (e.g. `"127.0.0.1:5000"` or `"127.0.0.1:0"`).
    pub listen_addr: String,
    /// Number of players (`2..=4`).
    pub players: u8,
    /// Bridge operating mode (`Multi` or `Rfu`).
    pub mode: LinkBridgeMode,
    /// Optional path to write a live JSONL SIO capture compatible with `gbakit link dissect`.
    pub log_path: Option<PathBuf>,
    /// Optional transfer cap; server exits cleanly after relaying this many rounds.
    pub max_transfers: Option<u64>,
    /// Optional idle timeout in milliseconds before the server exits cleanly.
    pub idle_timeout_ms: Option<u64>,
    /// Optional game code recorded in the JSONL capture header.
    pub game_code: Option<String>,
    /// Optional game title recorded in the JSONL capture header.
    pub game_title: Option<String>,
}

impl Default for LinkBridgeConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:5000".to_string(),
            players: 2,
            mode: LinkBridgeMode::Multi,
            log_path: None,
            max_transfers: None,
            idle_timeout_ms: None,
            game_code: None,
            game_title: None,
        }
    }
}

/// Summary report returned when a [`LinkBridgeServer`] session completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkBridgeSummary {
    pub listen_addr: String,
    pub mode: String,
    pub players_expected: u8,
    pub players_connected: u8,
    pub transfers_relayed: u64,
    pub both_non_ff_transfers: u64,
    pub normal_transfers_relayed: u64,
    pub rfu_commands_relayed: u64,
    pub rfu_handshakes: u64,
    pub frame_syncs_relayed: u64,
    pub elapsed_ms: u64,
    pub stop_reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
}

/// Live TCP SIO Multi-Play Link-Cable Relay/Bridge Server.
pub struct LinkBridgeServer {
    listener: TcpListener,
    local_addr: SocketAddr,
    config: LinkBridgeConfig,
}

enum ClientEvent {
    Transfer { slot: usize, req: TransferRequest },
    NormalTransfer { slot: usize, req: NormalTransferRequest },
    FrameSync { slot: usize, frame: u32 },
    Bye { slot: usize },
    Disconnected { slot: usize },
}

fn baud_from_siocnt(siocnt: u16) -> u32 {
    match siocnt & 0x0003 {
        0 => BAUD_9600,
        1 => BAUD_38400,
        2 => BAUD_57600,
        _ => BAUD_115200,
    }
}

fn cycles_for_multi_transfer(baud_bps: u32, players: u8) -> u32 {
    let n = (players.clamp(2, 4)) as u32;
    // Approximate GBA hardware cycle count per 16-bit Multi-Play round.
    match baud_bps {
        BAUD_9600 => 33_554 * n,
        BAUD_38400 => 8_388 * n,
        BAUD_57600 => 5_592 * n,
        _ => {
            if n == 2 {
                CYCLES_115200_1SLAVE
            } else {
                2_878 * n
            }
        }
    }
}

impl LinkBridgeServer {
    /// Bind the TCP listener on `config.listen_addr`.
    pub fn bind(config: LinkBridgeConfig) -> Result<Self, String> {
        if !(2..=4).contains(&config.players) {
            return Err(format!(
                "invalid player count {}: expected 2, 3, or 4",
                config.players
            ));
        }
        let listener = TcpListener::bind(&config.listen_addr)
            .map_err(|e| format!("failed to bind '{}': {e}", config.listen_addr))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| format!("failed to query local address: {e}"))?;
        Ok(Self {
            listener,
            local_addr,
            config,
        })
    }

    /// Return the resolved socket address (useful when binding port `:0`).
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Accept all configured players, relay SIO Multi-Play transfers until completion,
    /// and return session statistics.
    pub fn run(self) -> Result<LinkBridgeSummary, String> {
        self.run_with_callback(|_| {})
    }

    /// Same as [`Self::run`], invoking `on_connect(slot)` as each player completes Hello.
    pub fn run_with_callback<F>(self, mut on_connect: F) -> Result<LinkBridgeSummary, String>
    where
        F: FnMut(u8),
    {
        let start = Instant::now();
        let players = self.config.players as usize;

        let mut writers: Vec<Option<TcpStream>> = (0..players).map(|_| None).collect();
        let mut readers: Vec<Option<TcpStream>> = (0..players).map(|_| None).collect();
        let mut client_flags: Vec<u8> = vec![0; players];
        let mut connected: usize = 0;

        while connected < players {
            let (mut stream, _peer) = self
                .listener
                .accept()
                .map_err(|e| format!("accept failed: {e}"))?;
            let _ = stream.set_nodelay(true);

            let mut hello_buf = [0u8; 8];
            stream
                .read_exact(&mut hello_buf)
                .map_err(|e| format!("failed to read GBALINK1 Hello: {e}"))?;
            let hello = HelloPacket::from_bytes(hello_buf)?;

            let slot = if hello.requested_slot == SLOT_AUTO {
                writers
                    .iter()
                    .position(Option::is_none)
                    .ok_or_else(|| "no free player slots available".to_string())?
            } else {
                let req = hello.requested_slot as usize;
                if req >= players {
                    return Err(format!(
                        "client requested slot {req}, but bridge is configured for {players} players"
                    ));
                }
                if writers[req].is_some() {
                    return Err(format!("player slot {req} is already occupied"));
                }
                req
            };

            let write_half = stream
                .try_clone()
                .map_err(|e| format!("failed to clone client stream: {e}"))?;
            readers[slot] = Some(stream);
            writers[slot] = Some(write_half);
            client_flags[slot] = hello.flags;
            connected += 1;
            on_connect(slot as u8);
        }

        // Send Welcome to all players as a synchronized start barrier.
        for (slot, writer_opt) in writers.iter_mut().enumerate() {
            let welcome = WelcomePacket {
                assigned_slot: slot as u8,
                total_players: self.config.players,
                flags: 0,
            };
            if let Some(writer) = writer_opt.as_mut() {
                writer
                    .write_all(&welcome.to_bytes())
                    .map_err(|e| format!("failed to send Welcome to slot {slot}: {e}"))?;
            }
        }

        // Prepare optional JSONL capture writer.
        let mut log_writer: Option<BufWriter<File>> = match &self.config.log_path {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    if !parent.as_os_str().is_empty() {
                        std::fs::create_dir_all(parent).map_err(|e| {
                            format!("failed to create log dir '{}': {e}", parent.display())
                        })?;
                    }
                }
                let file = File::create(path)
                    .map_err(|e| format!("failed to create log file '{}': {e}", path.display()))?;
                let mut bw = BufWriter::new(file);
                let (arb_mode, baud_bps) = match self.config.mode {
                    LinkBridgeMode::Multi => ("multiplayer_16bit", BAUD_115200),
                    LinkBridgeMode::Rfu => ("rfu_wireless_32bit", 2_000_000),
                };
                let header = serde_json::json!({
                    "schema": "gbakit.link-bridge.capture/1",
                    "game_code": self.config.game_code.as_deref().unwrap_or("LIVE"),
                    "game_title": self.config.game_title.as_deref().unwrap_or("GBA Live TCP Link Bridge Session"),
                    "transport": "tcp_link_bridge",
                    "arbitration_mode": arb_mode,
                    "baud_rate_bps": baud_bps,
                    "players": self.config.players,
                });
                writeln!(bw, "{header}")
                    .map_err(|e| format!("failed to write JSONL header: {e}"))?;
                bw.flush()
                    .map_err(|e| format!("failed to flush JSONL header: {e}"))?;
                Some(bw)
            }
            None => None,
        };

        // Spawn one blocking reader thread per connected client.
        let (tx, rx) = mpsc::channel::<ClientEvent>();
        let mut join_handles = Vec::with_capacity(players);
        for (slot, reader_opt) in readers.into_iter().enumerate() {
            let mut reader = reader_opt.expect("connected reader");
            let tx_c = tx.clone();
            let handle = thread::spawn(move || loop {
                let mut tag = [0u8; 1];
                if reader.read_exact(&mut tag).is_err() {
                    let _ = tx_c.send(ClientEvent::Disconnected { slot });
                    break;
                }
                match tag[0] {
                    MSG_TRANSFER_REQ => {
                        let mut payload = [0u8; 9];
                        if reader.read_exact(&mut payload).is_err() {
                            let _ = tx_c.send(ClientEvent::Disconnected { slot });
                            break;
                        }
                        let req = TransferRequest::from_payload(payload);
                        if tx_c.send(ClientEvent::Transfer { slot, req }).is_err() {
                            break;
                        }
                    }
                    MSG_NORMAL_REQ => {
                        let mut payload = [0u8; 13];
                        if reader.read_exact(&mut payload).is_err() {
                            let _ = tx_c.send(ClientEvent::Disconnected { slot });
                            break;
                        }
                        let req = NormalTransferRequest::from_payload(payload);
                        if tx_c.send(ClientEvent::NormalTransfer { slot, req }).is_err() {
                            break;
                        }
                    }
                    MSG_FRAME_SYNC => {
                        let mut payload = [0u8; 5];
                        if reader.read_exact(&mut payload).is_err() {
                            let _ = tx_c.send(ClientEvent::Disconnected { slot });
                            break;
                        }
                        let frame = u32::from_le_bytes([
                            payload[1], payload[2], payload[3], payload[4],
                        ]);
                        if tx_c.send(ClientEvent::FrameSync { slot, frame }).is_err() {
                            break;
                        }
                    }
                    MSG_BYE => {
                        let _ = tx_c.send(ClientEvent::Bye { slot });
                        break;
                    }
                    _ => {
                        let _ = tx_c.send(ClientEvent::Disconnected { slot });
                        break;
                    }
                }
            });
            join_handles.push(handle);
        }
        drop(tx);

        let mut pending_transfers: [Option<TransferRequest>; 4] = [None; 4];
        let mut pending_normal: [Option<NormalTransferRequest>; 2] = [None; 2];
        let mut pending_frames: [Option<u32>; 4] = [None; 4];
        let mut rfu_ether = RfuVirtualEther::new(players);
        let mut rfu_step: u64 = 0;
        let mut transfers_relayed: u64 = 0;
        let mut both_non_ff_transfers: u64 = 0;
        let mut normal_transfers_relayed: u64 = 0;
        let mut frame_syncs_relayed: u64 = 0;
        let idle_timeout = self.config.idle_timeout_ms.map(Duration::from_millis);
        let stop_reason: String;

        loop {
            let evt = match idle_timeout {
                Some(dur) => match rx.recv_timeout(dur) {
                    Ok(e) => e,
                    Err(RecvTimeoutError::Timeout) => {
                        stop_reason = "idle_timeout".to_string();
                        break;
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        stop_reason = "all_clients_disconnected".to_string();
                        break;
                    }
                },
                None => match rx.recv() {
                    Ok(e) => e,
                    Err(_) => {
                        stop_reason = "all_clients_disconnected".to_string();
                        break;
                    }
                },
            };

            match evt {
                ClientEvent::Transfer { slot, req } => {
                    pending_transfers[slot] = Some(req);

                    // If master initiated, poll any poll-enabled slaves that haven't submitted yet.
                    if slot == 0 {
                        let sc = req.siocnt.to_le_bytes();
                        let poll_pkt = [MSG_POLL, 0u8, sc[0], sc[1]];
                        for s in 1..players {
                            if pending_transfers[s].is_none()
                                && (client_flags[s] & FLAG_SLAVE_POLL) != 0
                            {
                                if let Some(w) = writers[s].as_mut() {
                                    let _ = w.write_all(&poll_pkt);
                                }
                            }
                        }
                    }

                    // Check if all N players have submitted their words for this round.
                    if (0..players).all(|s| pending_transfers[s].is_some()) {
                        let mut multi = [0xFFFFu16; 4];
                        for (s, word) in multi.iter_mut().enumerate().take(players) {
                            if let Some(r) = pending_transfers[s] {
                                *word = r.send_word;
                            }
                        }
                        let master_req = pending_transfers[0].expect("master request present");
                        transfers_relayed += 1;
                        if multi[0] != 0xFFFF && multi[1] != 0xFFFF {
                            both_non_ff_transfers += 1;
                        }

                        let resp = TransferResponse {
                            status: 0,
                            multi,
                            seq: (transfers_relayed & 0xFFFF) as u16,
                        };
                        let resp_bytes = resp.to_bytes();
                        let mut write_failed = false;
                        for w_opt in writers.iter_mut().take(players) {
                            if let Some(w) = w_opt.as_mut() {
                                if w.write_all(&resp_bytes).is_err() {
                                    write_failed = true;
                                }
                            }
                        }

                        if let Some(bw) = log_writer.as_mut() {
                            let baud = baud_from_siocnt(master_req.siocnt);
                            let cycles = cycles_for_multi_transfer(baud, self.config.players);
                            let record = serde_json::json!({
                                "step": transfers_relayed,
                                "frame": master_req.frame,
                                "master_word": format!("0x{:04X}", multi[0]),
                                "slave_word": format!("0x{:04X}", multi[1]),
                                "multi_registers": {
                                    "SIOMULTI0": format!("0x{:04X}", multi[0]),
                                    "SIOMULTI1": format!("0x{:04X}", multi[1]),
                                    "SIOMULTI2": format!("0x{:04X}", multi[2]),
                                    "SIOMULTI3": format!("0x{:04X}", multi[3]),
                                },
                                "siocnt": format!("0x{:04X}", master_req.siocnt),
                                "baud_rate_bps": baud,
                                "transfer_cycles": cycles,
                            });
                            if let Err(e) = writeln!(bw, "{record}") {
                                return Err(format!("failed to write JSONL transfer record: {e}"));
                            }
                        }

                        pending_transfers = [None; 4];

                        if write_failed {
                            stop_reason = "client_write_error".to_string();
                            break;
                        }
                        if let Some(cap) = self.config.max_transfers {
                            if transfers_relayed + normal_transfers_relayed >= cap {
                                stop_reason = "max_transfers_reached".to_string();
                                break;
                            }
                        }
                    }
                }
                ClientEvent::NormalTransfer { slot, req } => {
                    if self.config.mode == LinkBridgeMode::Rfu {
                        let outcome = rfu_ether.transfer_word(slot, req.siocnt, req.tx_word, req.frame);
                        if outcome.status == RFU_STATUS_OK {
                            normal_transfers_relayed += 1;
                        }
                        let seq = (normal_transfers_relayed & 0xFFFF_FFFF) as u32;
                        let resp_bytes = NormalTransferResponse {
                            status: outcome.status,
                            width_bits: 32,
                            rx_word: outcome.rx_word,
                            seq,
                        }
                        .to_bytes();

                        let mut write_failed = false;
                        if let Some(w) = writers[slot].as_mut() {
                            if w.write_all(&resp_bytes).is_err() {
                                write_failed = true;
                            }
                        }

                        if let Some(audit) = outcome.audit {
                            rfu_step += 1;
                            if let Some(bw) = log_writer.as_mut() {
                                let payload_hex: Vec<String> = audit
                                    .payload
                                    .iter()
                                    .map(|w| format!("0x{w:08X}"))
                                    .collect();
                                let resp_payload_hex: Vec<String> = audit
                                    .response_payload
                                    .iter()
                                    .map(|w| format!("0x{w:08X}"))
                                    .collect();
                                let record = serde_json::json!({
                                    "step": rfu_step,
                                    "frame": audit.frame,
                                    "slot": audit.slot,
                                    "phase": audit.phase,
                                    "command_word": format!("0x{:08X}", audit.command_word),
                                    "command_id": format!("0x{:04X}", audit.command_id),
                                    "command_name": audit.command_name,
                                    "data_length": audit.data_length,
                                    "slot_mask": format!("0x{:02X}", audit.slot_mask),
                                    "payload": payload_hex,
                                    "response_word": format!("0x{:08X}", audit.response_word),
                                    "response_payload": resp_payload_hex,
                                    "crc": format!("0x{:04X}", audit.crc),
                                    "crc_valid": true,
                                    "description": audit.description,
                                });
                                if let Err(e) = writeln!(bw, "{record}") {
                                    return Err(format!(
                                        "failed to write JSONL RFU record: {e}"
                                    ));
                                }
                            }
                        }

                        if write_failed {
                            stop_reason = "client_write_error".to_string();
                            break;
                        }
                        if let Some(cap) = self.config.max_transfers {
                            if transfers_relayed + normal_transfers_relayed >= cap {
                                stop_reason = "max_transfers_reached".to_string();
                                break;
                            }
                        }
                    } else if slot < 2 {
                        pending_normal[slot] = Some(req);

                        // If P0 (Master) initiated Normal SPI and P1 (Slave) has FLAG_SLAVE_POLL
                        // and hasn't submitted yet, send MSG_POLL with master's SIOCNT.
                        if slot == 0
                            && pending_normal[1].is_none()
                            && (client_flags[1] & FLAG_SLAVE_POLL) != 0
                        {
                            let sc = req.siocnt.to_le_bytes();
                            let poll_pkt = [MSG_POLL, 0u8, sc[0], sc[1]];
                            if let Some(w) = writers[1].as_mut() {
                                let _ = w.write_all(&poll_pkt);
                            }
                        }

                        if let (Some(p0_req), Some(p1_req)) = (pending_normal[0], pending_normal[1])
                        {
                            let width = if p0_req.width_bits == 8 { 8u8 } else { 32u8 };
                            let mask: u32 = if width == 8 { 0xFF } else { 0xFFFF_FFFF };
                            let p0_word = p0_req.tx_word & mask;
                            let p1_word = p1_req.tx_word & mask;
                            normal_transfers_relayed += 1;
                            let seq = (normal_transfers_relayed & 0xFFFF_FFFF) as u32;

                            // Full-duplex SPI swap: P0 receives P1's tx_word, P1 receives P0's tx_word.
                            let resp0 = NormalTransferResponse {
                                status: 0,
                                width_bits: width,
                                rx_word: p1_word,
                                seq,
                            }
                            .to_bytes();
                            let resp1 = NormalTransferResponse {
                                status: 0,
                                width_bits: width,
                                rx_word: p0_word,
                                seq,
                            }
                            .to_bytes();

                            let mut write_failed = false;
                            if let Some(w0) = writers[0].as_mut() {
                                if w0.write_all(&resp0).is_err() {
                                    write_failed = true;
                                }
                            }
                            if let Some(w1) = writers[1].as_mut() {
                                if w1.write_all(&resp1).is_err() {
                                    write_failed = true;
                                }
                            }

                            if let Some(bw) = log_writer.as_mut() {
                                let mode_str = if width == 8 { "normal_8" } else { "normal_32" };
                                let record = serde_json::json!({
                                    "step": transfers_relayed + normal_transfers_relayed,
                                    "frame": p0_req.frame,
                                    "mode": mode_str,
                                    "width_bits": width,
                                    "p0_tx": format!("0x{:08X}", p0_word),
                                    "p1_tx": format!("0x{:08X}", p1_word),
                                    "p0_rx": format!("0x{:08X}", p1_word),
                                    "p1_rx": format!("0x{:08X}", p0_word),
                                    "siocnt": format!("0x{:04X}", p0_req.siocnt),
                                });
                                if let Err(e) = writeln!(bw, "{record}") {
                                    return Err(format!(
                                        "failed to write JSONL normal transfer record: {e}"
                                    ));
                                }
                            }

                            pending_normal = [None; 2];

                            if write_failed {
                                stop_reason = "client_write_error".to_string();
                                break;
                            }
                            if let Some(cap) = self.config.max_transfers {
                                if transfers_relayed + normal_transfers_relayed >= cap {
                                    stop_reason = "max_transfers_reached".to_string();
                                    break;
                                }
                            }
                        }
                    }
                }
                ClientEvent::FrameSync { slot, frame } => {
                    pending_frames[slot] = Some(frame);
                    if (0..players).all(|s| pending_frames[s].is_some()) {
                        let f_bytes = frame.to_le_bytes();
                        let ack = [
                            MSG_FRAME_ACK,
                            0u8,
                            f_bytes[0],
                            f_bytes[1],
                            f_bytes[2],
                            f_bytes[3],
                        ];
                        let mut write_failed = false;
                        for w_opt in writers.iter_mut().take(players) {
                            if let Some(w) = w_opt.as_mut() {
                                if w.write_all(&ack).is_err() {
                                    write_failed = true;
                                }
                            }
                        }
                        pending_frames = [None; 4];
                        frame_syncs_relayed += 1;
                        if write_failed {
                            stop_reason = "client_write_error".to_string();
                            break;
                        }
                    }
                }
                ClientEvent::Bye { slot } => {
                    stop_reason = format!("client_slot_{slot}_bye");
                    break;
                }
                ClientEvent::Disconnected { slot } => {
                    stop_reason = format!("client_slot_{slot}_disconnected");
                    break;
                }
            }
        }

        if let Some(bw) = log_writer.as_mut() {
            let _ = bw.flush();
        }

        for w in writers.into_iter().flatten() {
            let _ = w.shutdown(Shutdown::Both);
        }
        for handle in join_handles {
            let _ = handle.join();
        }

        Ok(LinkBridgeSummary {
            listen_addr: self.local_addr.to_string(),
            mode: self.config.mode.as_str().to_string(),
            players_expected: self.config.players,
            players_connected: connected as u8,
            transfers_relayed,
            both_non_ff_transfers,
            normal_transfers_relayed,
            rfu_commands_relayed: rfu_ether.total_commands(),
            rfu_handshakes: rfu_ether.total_handshakes(),
            frame_syncs_relayed,
            elapsed_ms: start.elapsed().as_millis() as u64,
            stop_reason,
            log_path: self
                .config
                .log_path
                .map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::link::dissect_link_capture;

    #[test]
    fn test_live_tcp_bridge_2p_roundtrip_and_jsonl_dissect() {
        let log_path = std::env::temp_dir().join(format!(
            "gbakit_link_bridge_test_{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&log_path);

        let cfg = LinkBridgeConfig {
            listen_addr: "127.0.0.1:0".to_string(),
            players: 2,
            mode: LinkBridgeMode::Multi,
            log_path: Some(log_path.clone()),
            max_transfers: Some(4),
            idle_timeout_ms: Some(5_000),
            game_code: Some("AMKE".to_string()),
            game_title: Some("Mario Kart: Super Circuit".to_string()),
        };

        let server = LinkBridgeServer::bind(cfg).expect("bind bridge on ephemeral port");
        let addr = server.local_addr();
        let server_handle = thread::spawn(move || server.run());

        // Connect 2 independent TcpStream clients (Master slot 0, Slave slot 1).
        let mut c0 = TcpStream::connect(addr).expect("connect p1");
        let mut c1 = TcpStream::connect(addr).expect("connect p2");
        let _ = c0.set_nodelay(true);
        let _ = c1.set_nodelay(true);

        c0.write_all(
            &HelloPacket {
                version: PROTOCOL_VERSION,
                requested_slot: 0,
                flags: 0,
            }
            .to_bytes(),
        )
        .expect("send hello p1");
        c1.write_all(
            &HelloPacket {
                version: PROTOCOL_VERSION,
                requested_slot: SLOT_AUTO,
                flags: 0,
            }
            .to_bytes(),
        )
        .expect("send hello p2");

        let mut w0_buf = [0u8; 8];
        let mut w1_buf = [0u8; 8];
        c0.read_exact(&mut w0_buf).expect("recv welcome p1");
        c1.read_exact(&mut w1_buf).expect("recv welcome p2");
        let w0 = WelcomePacket::from_bytes(w0_buf).expect("parse welcome p1");
        let w1 = WelcomePacket::from_bytes(w1_buf).expect("parse welcome p2");
        assert_eq!(w0.assigned_slot, 0);
        assert_eq!(w1.assigned_slot, 1);
        assert_eq!(w0.total_players, 2);
        assert_eq!(w1.total_players, 2);

        let rounds: [(u16, u16, u32); 4] = [
            (0x1122, 0x3344, 100),
            (0xAA55, 0x55AA, 101),
            (0xBEEF, 0xCAFE, 102),
            (0x0001, 0x0002, 103),
        ];

        for (idx, (m_word, s_word, frame)) in rounds.iter().enumerate() {
            c0.write_all(
                &TransferRequest {
                    slot: 0,
                    siocnt: 0x6083, // 115200 baud, Multi-Play, IRQ, Busy
                    send_word: *m_word,
                    frame: *frame,
                }
                .to_bytes(),
            )
            .expect("send req p1");
            c1.write_all(
                &TransferRequest {
                    slot: 1,
                    siocnt: 0x601F,
                    send_word: *s_word,
                    frame: *frame,
                }
                .to_bytes(),
            )
            .expect("send req p2");

            let mut r0_buf = [0u8; 12];
            let mut r1_buf = [0u8; 12];
            c0.read_exact(&mut r0_buf).expect("recv resp p1");
            c1.read_exact(&mut r1_buf).expect("recv resp p2");
            let r0 = TransferResponse::from_bytes(r0_buf).expect("parse resp p1");
            let r1 = TransferResponse::from_bytes(r1_buf).expect("parse resp p2");

            assert_eq!(r0, r1);
            assert_eq!(r0.status, 0);
            assert_eq!(r0.multi, [*m_word, *s_word, 0xFFFF, 0xFFFF]);
            assert_eq!(r0.seq, (idx + 1) as u16);
        }

        let summary = server_handle
            .join()
            .expect("join server thread")
            .expect("bridge run ok");
        assert_eq!(summary.players_connected, 2);
        assert_eq!(summary.transfers_relayed, 4);
        assert_eq!(summary.both_non_ff_transfers, 4);
        assert_eq!(summary.stop_reason, "max_transfers_reached");

        // Verify the JSONL capture parses cleanly with gbakit::link::dissect_link_capture!
        let jsonl = std::fs::read_to_string(&log_path).expect("read jsonl capture");
        let report = dissect_link_capture(&jsonl).expect("dissect jsonl capture");
        assert_eq!(report.game_code, "AMKE");
        assert_eq!(report.statistics.total_transfers, 4);
        assert_eq!(report.statistics.baud_rate_bps, 115_200);
        assert_eq!(report.transfers[0].multi0, 0x1122);
        assert_eq!(report.transfers[0].multi1, 0x3344);
        assert_eq!(report.transfers[0].multi2, 0xFFFF);
        assert_eq!(report.transfers[0].multi3, 0xFFFF);
        assert_eq!(report.transfers[3].multi0, 0x0001);
        assert_eq!(report.transfers[3].multi1, 0x0002);

        let _ = std::fs::remove_file(&log_path);
    }

    #[test]
    fn test_live_tcp_bridge_slave_poll_and_frame_sync() {
        let cfg = LinkBridgeConfig {
            listen_addr: "127.0.0.1:0".to_string(),
            players: 2,
            mode: LinkBridgeMode::Multi,
            log_path: None,
            max_transfers: Some(2),
            idle_timeout_ms: Some(5_000),
            game_code: None,
            game_title: None,
        };
        let server = LinkBridgeServer::bind(cfg).expect("bind bridge");
        let addr = server.local_addr();
        let server_handle = thread::spawn(move || server.run());

        let mut c0 = TcpStream::connect(addr).expect("connect p1");
        let mut c1 = TcpStream::connect(addr).expect("connect p2");
        let _ = c0.set_nodelay(true);
        let _ = c1.set_nodelay(true);

        // Master without poll flag, Slave WITH FLAG_SLAVE_POLL.
        c0.write_all(
            &HelloPacket {
                version: PROTOCOL_VERSION,
                requested_slot: 0,
                flags: 0,
            }
            .to_bytes(),
        )
        .unwrap();
        c1.write_all(
            &HelloPacket {
                version: PROTOCOL_VERSION,
                requested_slot: 1,
                flags: FLAG_SLAVE_POLL,
            }
            .to_bytes(),
        )
        .unwrap();

        let mut wbuf = [0u8; 8];
        c0.read_exact(&mut wbuf).unwrap();
        c1.read_exact(&mut wbuf).unwrap();

        // Test frame sync barrier first.
        c0.write_all(&[MSG_FRAME_SYNC, 0, 1, 0, 0, 0]).unwrap();
        c1.write_all(&[MSG_FRAME_SYNC, 1, 1, 0, 0, 0]).unwrap();
        let mut ack0 = [0u8; 6];
        let mut ack1 = [0u8; 6];
        c0.read_exact(&mut ack0).unwrap();
        c1.read_exact(&mut ack1).unwrap();
        assert_eq!(ack0[0], MSG_FRAME_ACK);
        assert_eq!(ack1[0], MSG_FRAME_ACK);

        // Now Master initiates two transfers; Slave responds to server MSG_POLL.
        for round in 0u16..2 {
            c0.write_all(
                &TransferRequest {
                    slot: 0,
                    siocnt: 0x6083,
                    send_word: 0x4000 + round,
                    frame: 10 + round as u32,
                }
                .to_bytes(),
            )
            .unwrap();

            let mut poll = [0u8; 4];
            c1.read_exact(&mut poll).unwrap();
            assert_eq!(poll[0], MSG_POLL);
            assert_eq!(poll[1], 0);

            c1.write_all(
                &TransferRequest {
                    slot: 1,
                    siocnt: 0x601F,
                    send_word: 0x8000 + round,
                    frame: 10 + round as u32,
                }
                .to_bytes(),
            )
            .unwrap();

            let mut r0 = [0u8; 12];
            let mut r1 = [0u8; 12];
            c0.read_exact(&mut r0).unwrap();
            c1.read_exact(&mut r1).unwrap();
            let tr0 = TransferResponse::from_bytes(r0).unwrap();
            let tr1 = TransferResponse::from_bytes(r1).unwrap();
            assert_eq!(tr0, tr1);
            assert_eq!(tr0.multi, [0x4000 + round, 0x8000 + round, 0xFFFF, 0xFFFF]);
        }

        let summary = server_handle.join().unwrap().unwrap();
        assert_eq!(summary.transfers_relayed, 2);
        assert_eq!(summary.frame_syncs_relayed, 1);
    }

    #[test]
    fn test_live_tcp_bridge_normal_mode_8bit_and_32bit_swaps() {
        let log_path = std::env::temp_dir().join(format!(
            "gbakit_link_bridge_normal_test_{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&log_path);

        let cfg = LinkBridgeConfig {
            listen_addr: "127.0.0.1:0".to_string(),
            players: 2,
            mode: LinkBridgeMode::Multi,
            log_path: Some(log_path.clone()),
            max_transfers: Some(4),
            idle_timeout_ms: Some(5_000),
            game_code: Some("NORM".to_string()),
            game_title: Some("Normal Mode SPI Test".to_string()),
        };

        let server = LinkBridgeServer::bind(cfg).expect("bind bridge");
        let addr = server.local_addr();
        let server_handle = thread::spawn(move || server.run());

        let mut c0 = TcpStream::connect(addr).expect("connect p0");
        let mut c1 = TcpStream::connect(addr).expect("connect p1");
        let _ = c0.set_nodelay(true);
        let _ = c1.set_nodelay(true);

        c0.write_all(
            &HelloPacket {
                version: PROTOCOL_VERSION,
                requested_slot: 0,
                flags: 0,
            }
            .to_bytes(),
        )
        .unwrap();
        c1.write_all(
            &HelloPacket {
                version: PROTOCOL_VERSION,
                requested_slot: 1,
                flags: 0,
            }
            .to_bytes(),
        )
        .unwrap();

        let mut wbuf = [0u8; 8];
        c0.read_exact(&mut wbuf).unwrap();
        c1.read_exact(&mut wbuf).unwrap();

        // 2x 8-bit Normal Mode swaps (including upper bits that must be masked to 0xFF)
        // + 2x 32-bit Normal Mode full-duplex swaps
        let cases: [(u8, u16, u32, u32, u32, u32); 4] = [
            (8, 0x4081, 0xDEAD_BE5A, 0x1234_56A5, 0x0000_00A5, 0x0000_005A),
            (8, 0x4081, 0x0000_00F0, 0x0000_000F, 0x0000_000F, 0x0000_00F0),
            (32, 0x5081, 0xCAFE_BABE, 0xDEAD_BEEF, 0xDEAD_BEEF, 0xCAFE_BABE),
            (32, 0x5081, 0x1122_3344, 0x8877_6655, 0x8877_6655, 0x1122_3344),
        ];

        for (idx, &(width, siocnt, p0_tx, p1_tx, exp_p0_rx, exp_p1_rx)) in cases.iter().enumerate() {
            c0.write_all(
                &NormalTransferRequest {
                    slot: 0,
                    width_bits: width,
                    siocnt,
                    tx_word: p0_tx,
                    frame: 10 + idx as u32,
                }
                .to_bytes(),
            )
            .unwrap();
            c1.write_all(
                &NormalTransferRequest {
                    slot: 1,
                    width_bits: width,
                    siocnt: siocnt & !0x0001,
                    tx_word: p1_tx,
                    frame: 10 + idx as u32,
                }
                .to_bytes(),
            )
            .unwrap();

            let mut r0_buf = [0u8; 12];
            let mut r1_buf = [0u8; 12];
            c0.read_exact(&mut r0_buf).unwrap();
            c1.read_exact(&mut r1_buf).unwrap();

            let r0 = NormalTransferResponse::from_bytes(r0_buf).unwrap();
            let r1 = NormalTransferResponse::from_bytes(r1_buf).unwrap();
            assert_eq!(r0.status, 0);
            assert_eq!(r1.status, 0);
            assert_eq!(r0.width_bits, width);
            assert_eq!(r1.width_bits, width);
            assert_eq!(r0.rx_word, exp_p0_rx);
            assert_eq!(r1.rx_word, exp_p1_rx);
            assert_eq!(r0.seq, (idx + 1) as u32);
            assert_eq!(r1.seq, (idx + 1) as u32);
        }

        let summary = server_handle.join().unwrap().unwrap();
        assert_eq!(summary.players_connected, 2);
        assert_eq!(summary.transfers_relayed, 0);
        assert_eq!(summary.normal_transfers_relayed, 4);
        assert_eq!(summary.stop_reason, "max_transfers_reached");

        let jsonl = std::fs::read_to_string(&log_path).expect("read normal jsonl");
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 5); // 1 header + 4 normal transfer records
        assert!(lines[1].contains("\"mode\":\"normal_8\""));
        assert!(lines[2].contains("\"mode\":\"normal_8\""));
        assert!(lines[3].contains("\"mode\":\"normal_32\""));
        assert!(lines[4].contains("\"mode\":\"normal_32\""));

        let _ = std::fs::remove_file(&log_path);
    }

    #[test]
    fn test_live_tcp_bridge_rfu_mode_and_jsonl_dissect() {
        use crate::link::LinkProtocolKind;
        use crate::rfu::{
            encode_rfu_header, RFU_ACK_WORD, RFU_CMD_DISCONNECT, RFU_CMD_POLL_CONNECT,
            RFU_CMD_POLL_SEARCH, RFU_CMD_RECV_DATA, RFU_CMD_SEND_DATA, RFU_CMD_SET_BROADCAST_DATA,
            RFU_CMD_START_BROADCAST, RFU_CMD_START_CONNECT, RFU_CMD_START_SEARCH,
            RFU_CMD_SYSTEM_RESET, RFU_HANDSHAKE_PING, RFU_HANDSHAKE_PONG,
        };

        let log_path = std::env::temp_dir().join(format!(
            "gbakit_link_bridge_rfu_test_{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&log_path);

        let cfg = LinkBridgeConfig {
            listen_addr: "127.0.0.1:0".to_string(),
            players: 2,
            mode: LinkBridgeMode::Rfu,
            log_path: Some(log_path.clone()),
            max_transfers: None,
            idle_timeout_ms: Some(5_000),
            game_code: Some("BPRE".to_string()),
            game_title: Some("Pokemon FireRed Wireless Adapter".to_string()),
        };

        let server = LinkBridgeServer::bind(cfg).expect("bind rfu bridge");
        let addr = server.local_addr();
        let server_handle = thread::spawn(move || server.run());

        let mut c0 = TcpStream::connect(addr).expect("connect host");
        let mut c1 = TcpStream::connect(addr).expect("connect client");
        let _ = c0.set_nodelay(true);
        let _ = c1.set_nodelay(true);

        c0.write_all(
            &HelloPacket {
                version: PROTOCOL_VERSION,
                requested_slot: 0,
                flags: 0,
            }
            .to_bytes(),
        )
        .unwrap();
        c1.write_all(
            &HelloPacket {
                version: PROTOCOL_VERSION,
                requested_slot: 1,
                flags: 0,
            }
            .to_bytes(),
        )
        .unwrap();

        let mut wbuf = [0u8; 8];
        c0.read_exact(&mut wbuf).unwrap();
        c1.read_exact(&mut wbuf).unwrap();

        let send_word = |stream: &mut TcpStream, slot: u8, siocnt: u16, tx_word: u32, frame: u32| -> NormalTransferResponse {
            stream
                .write_all(
                    &NormalTransferRequest {
                        slot,
                        width_bits: 32,
                        siocnt,
                        tx_word,
                        frame,
                    }
                    .to_bytes(),
                )
                .unwrap();
            let mut rbuf = [0u8; 12];
            stream.read_exact(&mut rbuf).unwrap();
            NormalTransferResponse::from_bytes(rbuf).unwrap()
        };

        let exec_cmd = |stream: &mut TcpStream, slot: u8, cmd: u8, params: &[u32], frame: u32| -> Vec<u32> {
            let hdr = encode_rfu_header(cmd, params.len());
            let r = send_word(stream, slot, 0x5081, hdr, frame);
            assert_eq!(r.status, 0);
            assert_eq!(r.rx_word, RFU_ACK_WORD);
            for &p in params {
                let pr = send_word(stream, slot, 0x5081, p, frame);
                assert_eq!(pr.status, 0);
                assert_eq!(pr.rx_word, RFU_ACK_WORD);
            }
            // Clock out response with external clock (SIOCNT bit 0 = 0).
            let rh = send_word(stream, slot, 0x5080, RFU_ACK_WORD, frame);
            assert_eq!(rh.status, 0);
            assert_eq!((rh.rx_word & 0xFF) as u8, cmd | 0x80);
            let rlen = ((rh.rx_word >> 8) & 0xFF) as usize;
            let mut out = Vec::with_capacity(rlen);
            for _ in 0..rlen {
                let rw = send_word(stream, slot, 0x5080, RFU_ACK_WORD, frame);
                assert_eq!(rw.status, 0);
                out.push(rw.rx_word);
            }
            out
        };

        // 1. Handshakes on both Host & Client
        assert_eq!(send_word(&mut c0, 0, 0x5081, RFU_HANDSHAKE_PING, 1).rx_word, RFU_HANDSHAKE_PONG);
        assert_eq!(send_word(&mut c1, 1, 0x5081, RFU_HANDSHAKE_PING, 1).rx_word, RFU_HANDSHAKE_PONG);

        // 2. Reset, Host sets broadcast data + starts broadcast
        exec_cmd(&mut c0, 0, RFU_CMD_SYSTEM_RESET, &[], 2);
        exec_cmd(&mut c1, 1, RFU_CMD_SYSTEM_RESET, &[], 2);
        let beacon = [0x4250_5245, 0x5245_4400];
        exec_cmd(&mut c0, 0, RFU_CMD_SET_BROADCAST_DATA, &beacon, 3);
        exec_cmd(&mut c0, 0, RFU_CMD_START_BROADCAST, &[], 4);

        // 3. Client searches & discovers Host's live broadcast packet
        exec_cmd(&mut c1, 1, RFU_CMD_START_SEARCH, &[], 5);
        let found = exec_cmd(&mut c1, 1, RFU_CMD_POLL_SEARCH, &[], 6);
        assert_eq!(found.len(), 3);
        let host_id = found[0] & 0xFFFF;
        assert_eq!(host_id, 0x1001);
        assert_eq!(&found[1..], &beacon);

        // 4. Client connects & exchanges data with Host
        exec_cmd(&mut c1, 1, RFU_CMD_START_CONNECT, &[host_id], 7);
        let conn = exec_cmd(&mut c1, 1, RFU_CMD_POLL_CONNECT, &[], 8);
        assert_eq!(conn, vec![0x0000_1001]);

        exec_cmd(&mut c0, 0, RFU_CMD_SEND_DATA, &[0x1234_5678, 0x9ABC_DEF0], 9);
        let rx_client = exec_cmd(&mut c1, 1, RFU_CMD_RECV_DATA, &[], 10);
        assert_eq!(rx_client, vec![0x1234_5678, 0x9ABC_DEF0]);

        exec_cmd(&mut c1, 1, RFU_CMD_DISCONNECT, &[0x01], 11);

        // Clean disconnect via MSG_BYE
        c0.write_all(&[MSG_BYE]).unwrap();

        let summary = server_handle.join().unwrap().unwrap();
        assert_eq!(summary.mode, "rfu");
        assert_eq!(summary.rfu_handshakes, 2);
        assert_eq!(summary.rfu_commands_relayed, 11);

        // Verify JSONL capture decodes cleanly via dissect_link_capture!
        let jsonl = std::fs::read_to_string(&log_path).expect("read rfu jsonl");
        let report = dissect_link_capture(&jsonl).expect("dissect rfu jsonl");
        assert_eq!(report.protocol, LinkProtocolKind::PokemonWirelessUnionRoom);
        assert_eq!(report.rfu_frames.len(), 13); // 2 handshakes + 11 commands
        assert_eq!(report.rfu_frames[0].command_name, "RFU_HANDSHAKE");
        assert_eq!(report.rfu_frames[4].command_name, "SET_BROADCAST_DATA");
        assert_eq!(report.rfu_frames[7].command_name, "POLL_SEARCH");
        assert_eq!(report.rfu_frames[10].command_name, "SEND_DATA");
        assert_eq!(report.rfu_frames[11].command_name, "RECV_DATA");

        let _ = std::fs::remove_file(&log_path);
    }
}
