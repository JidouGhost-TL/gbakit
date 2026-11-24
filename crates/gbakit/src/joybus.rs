//! Clean-room Nintendo GameCube Joybus (DOL-011) Serial Protocol Bridge.
//!
//! Provides a hardware cycle-accurate GameCube Joybus serial protocol bridge,
//! framing encoder/decoder, EWRAM MultiBoot streaming engine, and dual-system
//! handshake simulation over virtual SIO / SI Pin 1 link channels.

#![deny(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Master clock frequency of the Game Boy Advance ARM7TDMI core (16.777216 MHz).
pub const GBA_CLOCK_FREQ_HZ: f64 = 16_777_216.0;

/// Joybus serial shift clock frequency over GBA SI / SIO Pin 1 (2 Mbps).
pub const JOYBUS_CLOCK_RATE_BPS: u32 = 2_000_000;

/// Joybus nominal bit period in microseconds (~4.7 µs per bit).
pub const JOYBUS_BIT_PERIOD_US: f64 = 4.7;

/// CPU cycles per Joybus serial bit at 16.777216 MHz (~78.85 cycles).
pub const CYCLES_PER_BIT: f64 = 78.852_915_2;

/// Standard Joybus MultiBoot payload block size (32 bytes per transfer).
pub const JOYBUS_BLOCK_SIZE: usize = 32;

/// Game Boy Advance External Work RAM (EWRAM) base physical address.
pub const EWRAM_BASE_ADDR: u32 = 0x02000000;

/// Total capacity of GBA On-Board EWRAM (256 KiB).
pub const EWRAM_MAX_SIZE: usize = 256 * 1024;

/// Standard Game Boy Advance peripheral identification word in Joybus mode (`0x0004`).
pub const DEVICE_ID_GBA: u16 = 0x0004;

// -----------------------------------------------------------------------------
// SIO & Joybus Memory-Mapped IO (MMIO) Registers
// -----------------------------------------------------------------------------

/// SIO Control / General Purpose Mode Register (written with `0xC000` / `0xC304`).
pub const REG_RCNT: u32 = 0x04000134;

/// Joybus Control Register.
pub const REG_JOYCNT: u32 = 0x04000140;

/// Joybus Receive Data Register (4 bytes from GameCube host).
pub const REG_JOY_RECV: u32 = 0x04000150;

/// Joybus Transmit Data Register (4 bytes sent to GameCube host).
pub const REG_JOY_TRANS: u32 = 0x04000154;

/// Joybus Status Register (returned during peripheral probe).
pub const REG_JOYSTAT: u32 = 0x04000158;

// -----------------------------------------------------------------------------
// Joybus Register Bitmasks & Flags
// -----------------------------------------------------------------------------

/// `JOYCNT` Bit 0: Reset flag.
pub const JOYCNT_RESET: u16 = 0x0001;

/// `JOYCNT` Bit 1: RECV Completed flag.
pub const JOYCNT_RECV_COMPLETE: u16 = 0x0002;

/// `JOYCNT` Bit 2: SEND Completed flag.
pub const JOYCNT_SEND_COMPLETE: u16 = 0x0004;

/// `JOYCNT` Bit 6: Joybus Interrupt Request (IRQ) Enable.
pub const JOYCNT_IRQ_ENABLE: u16 = 0x0040;

/// `JOYSTAT` Bit 1: Send completed acknowledgement.
pub const JOYSTAT_SEND_COMPLETE: u8 = 0x02;

/// `JOYSTAT` Bit 3: Receive completed acknowledgement.
pub const JOYSTAT_RECV_COMPLETE: u8 = 0x08;

// -----------------------------------------------------------------------------
// Joybus Protocol Command Set Opcodes
// -----------------------------------------------------------------------------

/// Command `0x00`: Reset / Peripheral Identification.
/// GameCube queries connected device; GBA returns `0x0004` (Device ID) + `JOYSTAT`.
pub const CMD_RESET_ID: u8 = 0x00;

/// Command `0x14`: Read GBA Controller Status & Buttons.
/// Returns 16-bit key input state (`KEYINPUT`) + 16-bit status flags.
pub const CMD_READ_STATUS: u8 = 0x14;

/// Command `0x15`: Joybus MultiBoot Bootstrap Handshake.
/// GameCube master initiates MultiBoot program load into GBA EWRAM (`0x02000000`).
pub const CMD_MULTIBOOT_BOOTSTRAP: u8 = 0x15;

/// Command `0x16`: Joybus Send EWRAM Block.
/// GameCube sends a 32-byte payload chunk accompanied by an 8-bit checksum.
pub const CMD_SEND_EWRAM_BLOCK: u8 = 0x16;

/// Command `0x17`: Joybus Receive EWRAM Block / Telemetry.
/// Returns GBA synchronization counter and real-time execution status.
pub const CMD_RECV_EWRAM_BLOCK: u8 = 0x17;

/// Command `0xFF`: Software Reset.
/// Resets the Joybus serial buffers and restores initial listening state.
pub const CMD_SOFTWARE_RESET: u8 = 0xFF;

// -----------------------------------------------------------------------------
// Helper Functions: Timing & Checksum
// -----------------------------------------------------------------------------

/// Calculate transfer latency in GBA ARM7TDMI CPU cycles for a given bit length at 2 Mbps (~4.7 µs/bit).
#[inline]
pub fn calculate_joybus_cycles(bits: usize) -> u32 {
    ((bits as f64) * CYCLES_PER_BIT).round() as u32
}

/// Calculate transfer latency in microseconds for a given bit length at ~4.7 µs per bit.
#[inline]
pub fn calculate_joybus_latency_us(bits: usize) -> f64 {
    (bits as f64) * JOYBUS_BIT_PERIOD_US
}

/// Compute authentic Joybus 8-bit additive wrapping checksum across a payload block.
#[inline]
pub fn compute_joybus_checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
}

/// Translate standard Joybus command opcode into canonical mnemonic string.
pub fn joybus_command_name(cmd: u8) -> &'static str {
    match cmd {
        CMD_RESET_ID => "RESET_ID",
        CMD_READ_STATUS => "READ_STATUS",
        CMD_MULTIBOOT_BOOTSTRAP => "MULTIBOOT_BOOTSTRAP",
        CMD_SEND_EWRAM_BLOCK => "SEND_EWRAM_BLOCK",
        CMD_RECV_EWRAM_BLOCK => "RECV_EWRAM_BLOCK",
        CMD_SOFTWARE_RESET => "SOFTWARE_RESET",
        _ => "JOYBUS_CUSTOM",
    }
}

// -----------------------------------------------------------------------------
// Joybus Protocol Phases and Frame Structures
// -----------------------------------------------------------------------------

/// Operational phases of a GameCube Joybus link session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoybusPhase {
    /// Initial system boot and SI line sensing.
    Boot,
    /// Command `0x00` peripheral identification and handshaking.
    JoybusHandshake,
    /// Command `0x15`/`0x16` MultiBoot EWRAM block streaming.
    PayloadTransfer,
    /// Command `0x14`/`0x17` synchronized controller input and telemetry.
    SynchronizedSession,
}

impl JoybusPhase {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Boot => "boot",
            Self::JoybusHandshake => "joybus_handshake",
            Self::PayloadTransfer => "payload_transfer",
            Self::SynchronizedSession => "synchronized_session",
        }
    }
}

/// A parsed Joybus serial exchange record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoybusFrame {
    pub step: usize,
    pub frame: u64,
    pub phase: String,
    pub command: u8,
    pub command_name: String,
    pub master_payload: Vec<u8>,
    pub slave_response: Vec<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum_valid: Option<bool>,
    pub latency_cycles: u32,
    pub latency_us: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// -----------------------------------------------------------------------------
// Joybus Device Hardware Simulation
// -----------------------------------------------------------------------------

/// Simulated Game Boy Advance hardware in Joybus operating mode.
#[derive(Debug, Clone)]
pub struct JoybusDevice {
    /// Peripheral Device ID (defaults to `0x0004`).
    pub device_id: u16,
    /// Hardware status register (`JOYSTAT`).
    pub status: u8,
    /// Current button register state (active low: `0x03FF` when idle).
    pub keyinput: u16,
    /// Monotonically incrementing dual-system sync counter.
    pub sync_counter: u32,
    /// Simulated 256 KiB EWRAM memory (`0x02000000`..`0x0203FFFF`).
    pub ewram: Vec<u8>,
    /// MultiBoot bootstrap handshake completion flag.
    pub bootstrap_ready: bool,
    /// Total 32-byte blocks received.
    pub blocks_received: usize,
    /// Current auto-increment EWRAM write address pointer.
    pub current_address: u32,
}

impl Default for JoybusDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl JoybusDevice {
    /// Create a new Joybus device initialized to clean-room default state.
    pub fn new() -> Self {
        Self {
            device_id: DEVICE_ID_GBA,
            status: 0x00,
            keyinput: 0x03FF, // All buttons released (active-low)
            sync_counter: 0,
            ewram: vec![0u8; EWRAM_MAX_SIZE],
            bootstrap_ready: false,
            blocks_received: 0,
            current_address: EWRAM_BASE_ADDR,
        }
    }

    /// Reset internal state machine to listening mode.
    pub fn reset(&mut self) {
        self.status = 0x00;
        self.keyinput = 0x03FF;
        self.sync_counter = 0;
        self.bootstrap_ready = false;
        self.blocks_received = 0;
        self.current_address = EWRAM_BASE_ADDR;
    }

    /// Set button input register state (`0x03FF` unpressed).
    pub fn set_keyinput(&mut self, keys: u16) {
        self.keyinput = keys;
    }

    /// Get current button input register state.
    pub fn keyinput(&self) -> u16 {
        self.keyinput
    }

    /// Set hardware status register.
    pub fn set_status(&mut self, status: u8) {
        self.status = status;
    }

    /// Get current sync counter value.
    pub fn sync_counter(&self) -> u32 {
        self.sync_counter
    }

    /// Immutable reference to EWRAM buffer.
    pub fn ewram(&self) -> &[u8] {
        &self.ewram
    }

    /// Mutable reference to EWRAM buffer.
    pub fn ewram_mut(&mut self) -> &mut [u8] {
        &mut self.ewram
    }

    /// Process an incoming Joybus serial command and payload from GameCube host.
    pub fn process_command(&mut self, cmd: u8, payload: &[u8]) -> Result<Vec<u8>, String> {
        match cmd {
            CMD_RESET_ID => {
                // Returns 3 bytes (24 bits): Device ID (16-bit BE) + JOYSTAT
                self.status |= JOYSTAT_SEND_COMPLETE;
                let id_bytes = self.device_id.to_be_bytes();
                Ok(vec![id_bytes[0], id_bytes[1], self.status])
            }

            CMD_READ_STATUS => {
                // Returns 4 bytes (32 bits): 16-bit KEYINPUT + 16-bit status flags
                self.status |= JOYSTAT_SEND_COMPLETE;
                let key_bytes = self.keyinput.to_be_bytes();
                let status_bytes = (self.status as u16).to_be_bytes();
                Ok(vec![key_bytes[0], key_bytes[1], status_bytes[0], status_bytes[1]])
            }

            CMD_MULTIBOOT_BOOTSTRAP => {
                // GameCube initiates MultiBoot load.
                // Payload optionally provides target EWRAM entry address (4 bytes).
                if payload.len() >= 4 {
                    let addr = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
                    if addr >= EWRAM_BASE_ADDR {
                        self.current_address = addr;
                    }
                } else {
                    self.current_address = EWRAM_BASE_ADDR;
                }
                self.bootstrap_ready = true;
                self.status |= JOYSTAT_RECV_COMPLETE;
                Ok(vec![self.status])
            }

            CMD_SEND_EWRAM_BLOCK => {
                // Payload can be:
                // Format A (37 bytes): 4 bytes address + 32 bytes data + 1 byte checksum
                // Format B (33 bytes): 32 bytes data + 1 byte checksum (auto-incrementing address)
                let (target_addr, data_slice, expected_chk) = if payload.len() >= 37 {
                    let addr = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
                    let data = &payload[4..36];
                    let chk = payload[36];
                    (addr, data, chk)
                } else if payload.len() >= 33 {
                    let addr = self.current_address;
                    let data = &payload[0..32];
                    let chk = payload[32];
                    (addr, data, chk)
                } else {
                    return Err(format!(
                        "Invalid Joybus Send EWRAM Block payload length: {} (expected >= 33 bytes)",
                        payload.len()
                    ));
                };

                let computed_chk = compute_joybus_checksum(data_slice);
                if computed_chk != expected_chk {
                    return Err(format!(
                        "Joybus EWRAM block checksum mismatch: expected 0x{:02X}, computed 0x{:02X}",
                        expected_chk, computed_chk
                    ));
                }

                // Write into EWRAM
                if target_addr >= EWRAM_BASE_ADDR {
                    let offset = (target_addr - EWRAM_BASE_ADDR) as usize;
                    if offset
                        .checked_add(data_slice.len())
                        .is_some_and(|end| end <= self.ewram.len())
                    {
                        self.ewram[offset..offset + data_slice.len()].copy_from_slice(data_slice);
                    }
                }
                self.current_address = target_addr.wrapping_add(data_slice.len() as u32);
                self.blocks_received += 1;
                self.status |= JOYSTAT_RECV_COMPLETE;

                // Return 1-byte acknowledgement status
                Ok(vec![self.status])
            }

            CMD_RECV_EWRAM_BLOCK => {
                // Returns 4-byte telemetry / sync counter
                self.sync_counter = self.sync_counter.wrapping_add(1);
                self.status |= JOYSTAT_SEND_COMPLETE;
                let counter_bytes = self.sync_counter.to_be_bytes();
                Ok(counter_bytes.to_vec())
            }

            CMD_SOFTWARE_RESET => {
                self.reset();
                let id_bytes = self.device_id.to_be_bytes();
                Ok(vec![id_bytes[0], id_bytes[1], self.status])
            }

            _ => {
                // Custom or unrecognized opcode: echo status
                Ok(vec![self.status])
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Unit Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_joybus_checksum_vectors() {
        let block1 = [0u8; 32];
        assert_eq!(compute_joybus_checksum(&block1), 0x00);

        let block2 = [1u8; 32];
        assert_eq!(compute_joybus_checksum(&block2), 32);

        let mut block3 = [0u8; 32];
        for (i, b) in block3.iter_mut().enumerate() {
            *b = i as u8;
        }
        // Sum 0..31 = 31 * 16 = 496. 496 % 256 = 240 (0xF0).
        assert_eq!(compute_joybus_checksum(&block3), 0xF0);

        let block4 = [0xFFu8; 32];
        // 32 * 255 = 8160. 8160 % 256 = 224 (0xE0).
        assert_eq!(compute_joybus_checksum(&block4), 0xE0);
    }

    #[test]
    fn test_joybus_cycle_and_latency_calculations() {
        // 1 byte = 8 bits
        let cycles_8 = calculate_joybus_cycles(8);
        assert_eq!(cycles_8, 631);
        let us_8 = calculate_joybus_latency_us(8);
        assert!((us_8 - 37.6).abs() < 1e-4);

        // 24 bits (Command 0x00 response)
        let cycles_24 = calculate_joybus_cycles(24);
        assert_eq!(cycles_24, 1892);
        let us_24 = calculate_joybus_latency_us(24);
        assert!((us_24 - 112.8).abs() < 1e-4);

        // 32 bits (Command 0x14 response)
        let cycles_32 = calculate_joybus_cycles(32);
        assert_eq!(cycles_32, 2523);
        let us_32 = calculate_joybus_latency_us(32);
        assert!((us_32 - 150.4).abs() < 1e-4);
    }

    #[test]
    fn test_joybus_command_names() {
        assert_eq!(joybus_command_name(0x00), "RESET_ID");
        assert_eq!(joybus_command_name(0x14), "READ_STATUS");
        assert_eq!(joybus_command_name(0x15), "MULTIBOOT_BOOTSTRAP");
        assert_eq!(joybus_command_name(0x16), "SEND_EWRAM_BLOCK");
        assert_eq!(joybus_command_name(0x17), "RECV_EWRAM_BLOCK");
        assert_eq!(joybus_command_name(0xFF), "SOFTWARE_RESET");
        assert_eq!(joybus_command_name(0x42), "JOYBUS_CUSTOM");
    }

    #[test]
    fn test_joybus_device_handshake_flow() {
        let mut dev = JoybusDevice::new();

        // 1. Reset / ID Probe (0x00)
        let resp_id = dev.process_command(CMD_RESET_ID, &[]).expect("process 0x00");
        assert_eq!(resp_id.len(), 3);
        assert_eq!(resp_id[0], 0x00);
        assert_eq!(resp_id[1], 0x04); // Device ID 0x0004
        assert_eq!(resp_id[2] & JOYSTAT_SEND_COMPLETE, JOYSTAT_SEND_COMPLETE);

        // 2. Read Controller Status (0x14)
        dev.set_keyinput(0x03F7); // START pressed (bit 3 low)
        let resp_keys = dev.process_command(CMD_READ_STATUS, &[]).expect("process 0x14");
        assert_eq!(resp_keys.len(), 4);
        assert_eq!(resp_keys[0], 0x03);
        assert_eq!(resp_keys[1], 0xF7);

        // 3. MultiBoot Bootstrap Handshake (0x15)
        let bootstrap_payload = EWRAM_BASE_ADDR.to_be_bytes();
        let resp_boot = dev.process_command(CMD_MULTIBOOT_BOOTSTRAP, &bootstrap_payload).expect("process 0x15");
        assert_eq!(resp_boot.len(), 1);
        assert!(dev.bootstrap_ready);

        // 4. Send EWRAM Block (0x16)
        let chunk = [0x55u8; 32];
        let chk = compute_joybus_checksum(&chunk);
        let mut block_payload = Vec::new();
        block_payload.extend_from_slice(&EWRAM_BASE_ADDR.to_be_bytes());
        block_payload.extend_from_slice(&chunk);
        block_payload.push(chk);

        let resp_block = dev.process_command(CMD_SEND_EWRAM_BLOCK, &block_payload).expect("process 0x16");
        assert_eq!(resp_block.len(), 1);
        assert_eq!(dev.blocks_received, 1);
        assert_eq!(&dev.ewram[0..32], &chunk[..]);

        // 5. Corrupt Checksum Rejection
        let mut bad_payload = block_payload.clone();
        bad_payload[36] ^= 0xFF; // Invert checksum
        let err = dev.process_command(CMD_SEND_EWRAM_BLOCK, &bad_payload);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("checksum mismatch"));

        // 6. Receive Telemetry / Sync Counter (0x17)
        let resp_sync1 = dev.process_command(CMD_RECV_EWRAM_BLOCK, &[]).expect("process 0x17");
        assert_eq!(resp_sync1, vec![0, 0, 0, 1]);
        let resp_sync2 = dev.process_command(CMD_RECV_EWRAM_BLOCK, &[]).expect("process 0x17");
        assert_eq!(resp_sync2, vec![0, 0, 0, 2]);

        // 7. Software Reset (0xFF)
        let resp_reset = dev.process_command(CMD_SOFTWARE_RESET, &[]).expect("process 0xFF");
        assert_eq!(resp_reset[0], 0x00);
        assert_eq!(resp_reset[1], 0x04);
        assert_eq!(dev.sync_counter(), 0);
        assert!(!dev.bootstrap_ready);
    }
}
