//! CLI command handler for GBA SIO Multiplayer Link Dissector.

#![deny(unsafe_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use gbakit::link::{dissect_sio_capture, LinkProtocolKind};
use gbakit::link_bridge::{LinkBridgeConfig, LinkBridgeMode, LinkBridgeServer};

/// Start a live TCP GBA SIO Multi-Play or Wireless Adapter (`AGB-015` RFU) Link Bridge Server.
#[allow(clippy::too_many_arguments)]
pub fn cmd_link_bridge(
    listen_addr: String,
    players: u8,
    mode: LinkBridgeMode,
    log_path: Option<PathBuf>,
    max_transfers: Option<u64>,
    idle_timeout_ms: Option<u64>,
    game_code: Option<String>,
    game_title: Option<String>,
    as_json: bool,
) -> Result<(), String> {
    let cfg = LinkBridgeConfig {
        listen_addr,
        players,
        mode,
        log_path: log_path.clone(),
        max_transfers,
        idle_timeout_ms,
        game_code,
        game_title,
    };

    let server = LinkBridgeServer::bind(cfg)?;
    let bound_addr = server.local_addr();
    let mode_label = match mode {
        LinkBridgeMode::Multi => "multi (16-bit Multi-Play + Normal SPI)",
        LinkBridgeMode::Rfu => "rfu (AGB-015 Wireless Adapter 32-bit Virtual RF Ether)",
    };

    if as_json {
        eprintln!("gbakit link bridge listening on {bound_addr} (players={players}, mode={mode_label})");
        let _ = std::io::stderr().flush();
    } else {
        println!("================================================================================");
        println!(" GBA SIO TCP LINK BRIDGE (GBALINK1)");
        println!("================================================================================");
        println!(" Listening:       {bound_addr}");
        println!(" Mode:            {mode_label}");
        println!(" Players:         {players} (Slot 0: Master/Host, Slots 1..{}: Slaves/Clients)", players - 1);
        if let Some(ref lp) = log_path {
            println!(" Capture Log:     {}", lp.display());
        }
        if let Some(cap) = max_transfers {
            println!(" Max Transfers:   {cap}");
        }
        if let Some(ms) = idle_timeout_ms {
            println!(" Idle Timeout:    {ms} ms");
        }
        println!(" Waiting for {players} players to connect...");
        let _ = std::io::stdout().flush();
    }

    let summary = server.run_with_callback(|slot| {
        let role = if slot == 0 { "Master" } else { "Slave" };
        if as_json {
            eprintln!("player slot {slot} ({role}) connected");
            let _ = std::io::stderr().flush();
        } else {
            println!(" [+] Player slot {slot} ({role}) connected");
            let _ = std::io::stdout().flush();
        }
    })?;

    if as_json {
        let json_out = serde_json::to_string_pretty(&summary)
            .map_err(|e| format!("Failed to serialize bridge summary: {e}"))?;
        println!("{json_out}");
    } else {
        println!("--------------------------------------------------------------------------------");
        println!(" BRIDGE SESSION SUMMARY");
        println!("--------------------------------------------------------------------------------");
        println!(" Listen Address:  {}", summary.listen_addr);
        println!(" Mode:            {}", summary.mode);
        println!(
            " Players:         {} / {} connected",
            summary.players_connected, summary.players_expected
        );
        println!(" Transfers:       {} relayed ({} both non-0xFFFF)", summary.transfers_relayed, summary.both_non_ff_transfers);
        println!(" Normal SPI:      {} relayed", summary.normal_transfers_relayed);
        println!(" RFU Commands:    {} relayed ({} handshakes)", summary.rfu_commands_relayed, summary.rfu_handshakes);
        println!(" Frame Syncs:     {}", summary.frame_syncs_relayed);
        println!(" Elapsed:         {} ms", summary.elapsed_ms);
        println!(" Stop Reason:     {}", summary.stop_reason);
        if let Some(ref lp) = summary.log_path {
            println!(" Capture Log:     {lp}");
        }
        println!("================================================================================");
    }

    Ok(())
}

/// Dissect a recorded SIO packet capture JSON file and display protocol analysis and statistics.
pub fn cmd_link_dissect(path: &Path, as_json: bool, rfu_mode: bool, joybus_mode: bool) -> Result<(), String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read capture file '{}': {e}", path.display()))?;

    let report = dissect_sio_capture(&content)?;

    if as_json {
        let json_out = serde_json::to_string_pretty(&report)
            .map_err(|e| format!("Failed to serialize dissection report: {e}"))?;
        println!("{json_out}");
        return Ok(());
    }

    let is_joybus_display = joybus_mode
        || report.protocol == LinkProtocolKind::GameCubeJoybus
        || !report.joybus_frames.is_empty();

    if is_joybus_display {
        println!("================================================================================");
        println!(" GBA JOYBUS SERIAL PROTOCOL (DOL-011) DISSECTOR — PROTOCOL ANALYSIS");
        println!("================================================================================");
        println!(" Target Game:     {} ({})", report.game_title, report.game_code);
        println!(" Protocol:        {}", report.protocol.display_name());
        println!(" SIO Mode:        {} (SI / SIO Pin 1 Serial Protocol, 2 Mbps)", report.arbitration_mode);
        println!(" Transport:       {}", report.transport);
        println!(
            " Baud Rate:       {} bps (Latency: {:.2} µs / {} cycles)",
            report.statistics.baud_rate_bps,
            report.statistics.latency_microseconds,
            report.statistics.transfer_cycles
        );
        println!(
            " Connected Nodes: {} (Node 0: GameCube Master, Node 1: GBA Slave Peripheral)",
            report.statistics.connected_nodes
        );
        println!();
        println!("--------------------------------------------------------------------------------");
        println!(" RECORDED JOYBUS PACKET TRANSFERS & EWRAM OPERATIONS");
        println!("--------------------------------------------------------------------------------");
        println!(" Step | Frame | Phase                | Cmd  | Opcode Name          | Address    | Len | Checksum | Status | Latency  | Description");
        println!("------+-------+----------------------+------+----------------------+------------+-----+----------+--------+----------+-----------------------------");

        for f in &report.joybus_frames {
            let chk_str = match (f.checksum, f.checksum_valid) {
                (Some(c), _) => format!("0x{c:02X}"),
                (None, _) => " -- ".to_string(),
            };
            let status_str = match f.checksum_valid {
                Some(true) => "[OK]",
                Some(false) => "[ERR]",
                None => "[OK]",
            };
            let addr_str = match f.address {
                Some(a) => format!("0x{a:08X}"),
                None => "    --    ".to_string(),
            };
            let len = f.master_payload.len().max(f.slave_response.len());
            let desc = f.description.as_deref().unwrap_or(&f.command_name);

            println!(
                "{:4} | {:5} | {:20} | 0x{:02X} | {:20} | {} | {:3} | {:8} | {:6} | {:5} µs | {}",
                f.step,
                f.frame,
                f.phase,
                f.command,
                f.command_name,
                addr_str,
                len,
                chk_str,
                status_str,
                format!("{:.1}", f.latency_us),
                desc
            );
            if !f.master_payload.is_empty() && f.master_payload.len() <= 16 {
                let m_hex: Vec<String> = f.master_payload.iter().map(|b| format!("0x{b:02X}")).collect();
                println!("      |       |  -> Host Payload:  [{}]", m_hex.join(", "));
            } else if f.master_payload.len() > 16 {
                let prefix: Vec<String> = f.master_payload[..8].iter().map(|b| format!("0x{b:02X}")).collect();
                println!("      |       |  -> Host Payload:  [{}, ... ({} bytes)]", prefix.join(", "), f.master_payload.len());
            }
            if !f.slave_response.is_empty() {
                let s_hex: Vec<String> = f.slave_response.iter().map(|b| format!("0x{b:02X}")).collect();
                println!("      |       |  <- GBA Response:  [{}]", s_hex.join(", "));
            }
        }

        if !report.annotations.is_empty() {
            println!();
            println!("--------------------------------------------------------------------------------");
            println!(" PROTOCOL ANNOTATIONS & RECOGNITION");
            println!("--------------------------------------------------------------------------------");
            for note in &report.annotations {
                println!(" * {note}");
            }
        }

        println!();
        println!("--------------------------------------------------------------------------------");
        println!(" JOYBUS SERIAL LINK STATISTICS SUMMARY");
        println!("--------------------------------------------------------------------------------");
        println!(" Total Joybus Transfers:      {}", report.statistics.total_transfers);
        println!(" Master Transmitted Volume:   {} bytes", report.statistics.master_bytes);
        println!(" Slave Transmitted Volume:    {} bytes", report.statistics.slave_bytes);
        println!(" Total Serial Bus Volume:     {} bytes", report.statistics.total_bytes_shifted);
        println!(" SIO Clock Rate:              {} bps", report.statistics.baud_rate_bps);
        println!(
            " Hardware Latency per Cycle:  {} CPU cycles ({:.2} µs @ 16.78 MHz)",
            report.statistics.transfer_cycles,
            report.statistics.latency_microseconds
        );
        println!(
            " Checksums Verified:          {} / {} valid",
            report.statistics.checksums_valid,
            report.statistics.checksums_verified
        );
        let joybus_status = if report.statistics.checksums_valid == report.statistics.checksums_verified {
            "VERIFIED (Cycle-accurate framing, 0 checksum errors)".to_string()
        } else {
            let errs = report
                .statistics
                .checksums_verified
                .saturating_sub(report.statistics.checksums_valid);
            format!("FAILED ({errs} checksum error(s))")
        };
        println!(" Joybus Integrity Status:     {joybus_status}");
        println!("================================================================================");
        return Ok(());
    }

    let is_rfu_display = rfu_mode
        || report.protocol == LinkProtocolKind::PokemonWirelessUnionRoom
        || !report.rfu_frames.is_empty();

    if is_rfu_display {
        println!("================================================================================");
        println!(" GBA SIO WIRELESS ADAPTER (AGB-RFU) DISSECTOR — PROTOCOL ANALYSIS");
        println!("================================================================================");
        println!(" Target Game:     {} ({})", report.game_title, report.game_code);
        println!(" Protocol:        {}", report.protocol.display_name());
        println!(" SIO Mode:        {} (AGB-RFU 32-bit Synchronous SIO)", report.arbitration_mode);
        println!(" Transport:       {}", report.transport);
        println!(
            " Baud Rate:       {} bps (Latency: {:.2} µs / {} cycles)",
            report.statistics.baud_rate_bps,
            report.statistics.latency_microseconds,
            report.statistics.transfer_cycles
        );
        println!(
            " Connected Nodes: {} (Slot 0: Host/Beacon, Slots 1..3: Clients)",
            report.statistics.connected_nodes
        );
        println!();
        println!("--------------------------------------------------------------------------------");
        println!(" RECORDED AGB-RFU 32-BIT PACKET TRANSFERS & STATE MACHINE PHASES");
        println!("--------------------------------------------------------------------------------");
        println!(" Step | Frame | Phase                       | Cmd Word   | Opcode | Len | Mask | CRC    | Status | Description");
        println!("------+-------+-----------------------------+------------+--------+-----+------+--------+--------+-----------------------------");

        for f in &report.rfu_frames {
            let status_str = if f.crc_valid { "[OK]" } else { "[ERR]" };
            let desc = f.description.as_deref().unwrap_or(&f.command_name);
            println!(
                "{:4} | {:5} | {:27} | 0x{:08X} | 0x{:04X} | {:3} | 0x{:02X} | 0x{:04X} | {:6} | {}",
                f.step,
                f.frame,
                f.phase,
                f.command_word,
                f.command_id,
                f.data_length,
                f.slot_mask,
                f.crc,
                status_str,
                desc
            );
            if !f.payload.is_empty() {
                let payload_hex: Vec<String> = f.payload.iter().map(|w| format!("0x{w:08X}")).collect();
                println!("      |       |  -> Payload Words: [{}]", payload_hex.join(", "));
            }
        }

        if !report.annotations.is_empty() {
            println!();
            println!("--------------------------------------------------------------------------------");
            println!(" PROTOCOL ANNOTATIONS & RECOGNITION");
            println!("--------------------------------------------------------------------------------");
            for note in &report.annotations {
                println!(" * {note}");
            }
        }

        println!();
        println!("--------------------------------------------------------------------------------");
        println!(" AGB-RFU WIRELESS LINK STATISTICS SUMMARY");
        println!("--------------------------------------------------------------------------------");
        println!(" Total RFU Packet Frames:     {}", report.statistics.total_transfers);
        println!(" Host Transmitted Volume:     {} bytes", report.statistics.master_bytes);
        println!(" Client Transmitted Volume:   {} bytes", report.statistics.slave_bytes);
        println!(" Total Over-The-Air Volume:   {} bytes", report.statistics.total_bytes_shifted);
        println!(" SIO Shift Clock Rate:        {} bps", report.statistics.baud_rate_bps);
        println!(
            " Hardware Latency per Cycle:  {} CPU cycles ({:.2} µs)",
            report.statistics.transfer_cycles,
            report.statistics.latency_microseconds
        );
        println!(
            " CRC-16 Checksums Verified:   {} / {} valid",
            report.statistics.checksums_valid,
            report.statistics.checksums_verified
        );
        let rfu_status = if report.statistics.checksums_valid == report.statistics.checksums_verified {
            "OPTIMAL (0 dropped packets, 0 CRC errors)".to_string()
        } else {
            let errs = report
                .statistics
                .checksums_verified
                .saturating_sub(report.statistics.checksums_valid);
            format!("DEGRADED ({errs} CRC error(s))")
        };
        println!(" RF Link Integrity Status:    {rfu_status}");
        println!("================================================================================");
        return Ok(());
    }

    if report.protocol == LinkProtocolKind::MultiBoot {
        println!("================================================================================");
        println!(" GBA SIO MULTIBOOT BOOTLOADER DISSECTOR — PROTOCOL ANALYSIS");
        println!("================================================================================");
        println!(" Target Game:     {} ({})", report.game_title, report.game_code);
        println!(" Protocol:        {}", report.protocol.display_name());
        println!(" SIO Mode:        {} (16-bit Multi-Play Bootloader)", report.arbitration_mode);
        println!(" Transport:       {}", report.transport);
        println!(
            " Baud Rate:       {} bps (Latency: {:.2} µs / {} cycles)",
            report.statistics.baud_rate_bps,
            report.statistics.latency_microseconds,
            report.statistics.transfer_cycles
        );
        println!(
            " Connected Nodes: {} (Unit 0: Master/Sender, Unit 1: Slave/Receiver)",
            report.statistics.connected_nodes
        );
        println!();
        println!("--------------------------------------------------------------------------------");
        println!(" RECORDED MULTIBOOT 16-BIT HANDSHAKE & EWRAM TRANSFERS");
        println!("--------------------------------------------------------------------------------");
        println!(" Step | Frame | Master Word | Slave Word  | Cycles | Checksum | Status | Description");
        println!("------+-------+-------------+-------------+--------+----------+--------+-----------------------------");

        for (i, t) in report.transfers.iter().enumerate() {
            let step = t.step.unwrap_or(i + 1);
            let chk_str = match (t.checksum, t.checksum_valid) {
                (Some(c), _) => format!("0x{c:04X}"),
                (None, _) => " ---- ".to_string(),
            };
            let status_str = match t.checksum_valid {
                Some(true) => "[OK]",
                Some(false) => "[ERR]",
                None => "[OK]",
            };
            let desc = t.description.as_deref().unwrap_or("-");

            println!(
                "{:4} | {:5} | 0x{:04X}      | 0x{:04X}      | {:6} | {:8} | {:6} | {}",
                step,
                t.frame,
                t.master_word,
                t.slave_word,
                t.transfer_cycles,
                chk_str,
                status_str,
                desc
            );
        }

        if !report.annotations.is_empty() {
            println!();
            println!("--------------------------------------------------------------------------------");
            println!(" PROTOCOL ANNOTATIONS & RECOGNITION");
            println!("--------------------------------------------------------------------------------");
            for note in &report.annotations {
                println!(" * {note}");
            }
        }

        println!();
        println!("--------------------------------------------------------------------------------");
        println!(" MULTIBOOT SIO BUS STATISTICS SUMMARY");
        println!("--------------------------------------------------------------------------------");
        println!(" Total Transfer Cycles:       {}", report.statistics.total_transfers);
        println!(" Master Transmitted Volume:   {} bytes", report.statistics.master_bytes);
        println!(" Slave Transmitted Volume:    {} bytes", report.statistics.slave_bytes);
        println!(" Total SIO Shuffled Volume:   {} bytes", report.statistics.total_bytes_shifted);
        println!(" SIO Baud Rate:               {} bps", report.statistics.baud_rate_bps);
        println!(
            " Hardware Latency per Cycle:  {} CPU cycles ({:.2} µs @ 16.78 MHz)",
            report.statistics.transfer_cycles,
            report.statistics.latency_microseconds
        );
        println!(
            " Checksums Verified:          {} / {} valid",
            report.statistics.checksums_valid,
            report.statistics.checksums_verified
        );
        let mb_status = if report.statistics.checksums_valid == report.statistics.checksums_verified {
            "VERIFIED (16-bit CRC authenticated, 0 framing errors)".to_string()
        } else {
            let errs = report
                .statistics
                .checksums_verified
                .saturating_sub(report.statistics.checksums_valid);
            format!("FAILED ({errs} CRC error(s))")
        };
        println!(" MultiBoot Integrity Status:  {mb_status}");
        println!("================================================================================");
        return Ok(());
    }

    // Human-readable formatting for standard multi-play
    println!("================================================================================");
    println!(" GBA SIO MULTIPLAYER LINK DISSECTOR — PROTOCOL ANALYSIS");
    println!("================================================================================");
    println!(" Target Game:     {} ({})", report.game_title, report.game_code);
    println!(" Protocol:        {}", report.protocol.display_name());
    println!(" SIO Mode:        {} (Ring Network)", report.arbitration_mode);
    println!(" Transport:       {}", report.transport);
    println!(
        " Baud Rate:       {} bps (Latency: {:.2} µs / {} cycles)",
        report.statistics.baud_rate_bps,
        report.statistics.latency_microseconds,
        report.statistics.transfer_cycles
    );
    println!(
        " Connected Nodes: {} (Unit 0: Master, Unit 1: Slave)",
        report.statistics.connected_nodes
    );
    println!();
    println!("--------------------------------------------------------------------------------");
    println!(" RECORDED SIO MULTI-PLAY WORD TRANSFERS");
    println!("--------------------------------------------------------------------------------");
    println!(" Step | Frame | SIOMULTI0 (M) | SIOMULTI1 (S) | SIOMULTI2 | SIOMULTI3 | Cycles | Chksum | Description");
    println!("------+-------+---------------+---------------+-----------+-----------+--------+--------+-----------------------------");

    for (i, t) in report.transfers.iter().enumerate() {
        let step = t.step.unwrap_or(i + 1);
        let chk_str = match (t.checksum, t.checksum_valid) {
            (Some(c), Some(true)) => format!("0x{c:04X} [OK]"),
            (Some(c), Some(false)) => format!("0x{c:04X} [ERR]"),
            (Some(c), None) => format!("0x{c:04X}"),
            (None, _) => "  ----  ".to_string(),
        };
        let desc = t.description.as_deref().unwrap_or("-");

        println!(
            "{:4} | {:5} | 0x{:04X}        | 0x{:04X}        | 0x{:04X}    | 0x{:04X}    | {:6} | {:6} | {}",
            step,
            t.frame,
            t.multi0,
            t.multi1,
            t.multi2,
            t.multi3,
            t.transfer_cycles,
            chk_str,
            desc
        );
    }

    if !report.annotations.is_empty() {
        println!();
        println!("--------------------------------------------------------------------------------");
        println!(" PROTOCOL ANNOTATIONS & RECOGNITION");
        println!("--------------------------------------------------------------------------------");
        for note in &report.annotations {
            println!(" * {note}");
        }
    }

    println!();
    println!("--------------------------------------------------------------------------------");
    println!(" SIO LINK BUS STATISTICS SUMMARY");
    println!("--------------------------------------------------------------------------------");
    println!(" Total Transfer Cycles:       {}", report.statistics.total_transfers);
    println!(
        " Master Transmitted Data:     {} bytes ({} words)",
        report.statistics.master_bytes,
        report.statistics.master_bytes / 2
    );
    println!(
        " Slave Transmitted Data:      {} bytes ({} words)",
        report.statistics.slave_bytes,
        report.statistics.slave_bytes / 2
    );
    println!(
        " Total Ring Shift Volume:     {} bytes (4 nodes x 16-bit)",
        report.statistics.total_bytes_shifted
    );
    println!(" Baud Rate:                   {} bps", report.statistics.baud_rate_bps);
    println!(
        " Hardware Latency per Cycle:  {} CPU cycles ({:.2} µs @ 16.78 MHz)",
        report.statistics.transfer_cycles,
        report.statistics.latency_microseconds
    );
    println!(
        " Checksums Verified:          {} / {} valid",
        report.statistics.checksums_valid,
        report.statistics.checksums_verified
    );
    let sio_status = if report.statistics.checksums_valid == report.statistics.checksums_verified {
        "HEALTHY (0 checksum errors)".to_string()
    } else {
        let errs = report
            .statistics
            .checksums_verified
            .saturating_sub(report.statistics.checksums_valid);
        format!("DEGRADED ({errs} checksum error(s))")
    };
    println!(" SIO Bus Integrity Status:    {sio_status}");
    println!("================================================================================");

    Ok(())
}
