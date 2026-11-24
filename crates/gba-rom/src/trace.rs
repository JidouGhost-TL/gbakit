//! ROM callgraph reachability analysis and developer jump table detection.

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use std::collections::{HashSet, VecDeque};

use crate::{scan_pointer_tables, AgbHeader, ROM_BUS_BASE};

/// Execution mode for ARM7TDMI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeMode {
    Arm,
    Thumb,
}

impl RuntimeMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuntimeMode::Arm => "arm",
            RuntimeMode::Thumb => "thumb",
        }
    }
}

/// An entry point discovered in ROM that is not reached from the boot entry sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreachedEntryPoint {
    pub address: u32,
    pub rom_offset: usize,
    pub mode: RuntimeMode,
    pub prologue: String,
    pub reason: String,
}

/// A detected developer or scene dispatcher jump table in ROM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeveloperJumpTable {
    pub offset: usize,
    pub bus_address: u32,
    pub entries_count: usize,
    pub targets: Vec<u32>,
    pub signal: String,
    pub developer_keywords: Vec<String>,
}

/// Quantitative summary of callgraph reachability.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceAnalysisSummary {
    pub total_functions_detected: usize,
    pub reached_functions: usize,
    pub unreached_functions: usize,
    pub reachability_ratio: f64,
    pub callgraph_edges: usize,
    pub developer_jump_tables_detected: usize,
}

/// Complete report of ROM trace callgraph analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct TraceAnalysisReport {
    pub game_code: String,
    pub title: String,
    pub entry_point: u32,
    pub entry_mode: RuntimeMode,
    pub summary: TraceAnalysisSummary,
    pub unreached_entry_points: Vec<UnreachedEntryPoint>,
    pub developer_jump_tables: Vec<DeveloperJumpTable>,
}

const DEV_KEYWORDS: &[&str] = &[
    "DEBUG",
    "TEST",
    "SOUND TEST",
    "MENU",
    "SELECT",
    "FLAG",
    "MONITOR",
    "STAGE SELECT",
    "SCENE",
    "DISPATCH",
    "SYSTEM",
    "WARP",
    "VIEW",
];

/// Analyze ROM callgraph reachability from boot entry point, reporting unreached
/// entry points and detected developer jump tables.
pub fn analyze_trace(rom: &[u8]) -> Result<TraceAnalysisReport> {
    if rom.len() < crate::HEADER_SIZE {
        return Err(Error::Truncated {
            offset: 0,
            needed: crate::HEADER_SIZE,
            available: rom.len(),
        });
    }

    let hdr = AgbHeader::parse(rom)?;
    let rom_len = rom.len();
    let max_rom_addr = ROM_BUS_BASE + rom_len as u32;

    // Decode entry point from ARM branch at 0x08000000
    let entry_opcode = hdr.entry_opcode;
    let entry_target = if (entry_opcode >> 24) == 0xEA {
        let imm24 = (entry_opcode & 0x00FF_FFFF) as i32;
        let signed_words = (imm24 << 8) >> 8;
        let pc = (ROM_BUS_BASE + 8) as i64;
        (pc + (signed_words as i64 * 4)) as u32
    } else {
        ROM_BUS_BASE + 0xC0
    };

    let entry_mode = RuntimeMode::Arm;

    // 1. Scan candidate function entry points across ROM
    // Thumb: push {..., lr} (0xB5xx)
    // ARM: stmdb sp!, {..., lr} (0xE92D4xxx or 0xE92D5xxx)
    let mut candidate_functions: Vec<(u32, RuntimeMode, String)> = Vec::new();
    let mut candidate_set: HashSet<u32> = HashSet::new();

    // Scan Thumb candidates (2-byte alignment)
    let mut off = 0xC0;
    while off + 2 <= rom_len {
        let h = u16::from_le_bytes([rom[off], rom[off + 1]]);
        if (h & 0xFF00) == 0xB500 {
            let addr = ROM_BUS_BASE + off as u32;
            let reg_list = describe_thumb_push_lr(h & 0x00FF);
            candidate_functions.push((addr, RuntimeMode::Thumb, format!("push {{{reg_list}}}")));
            candidate_set.insert(addr);
        }
        off += 2;
    }

    // Scan ARM candidates (4-byte alignment)
    let mut off4 = 0xC0;
    while off4 + 4 <= rom_len {
        let w = u32::from_le_bytes([rom[off4], rom[off4 + 1], rom[off4 + 2], rom[off4 + 3]]);
        if (w & 0x0FFF_0000) == 0x092D_0000 && (w & (1 << 14)) != 0 {
            let addr = ROM_BUS_BASE + off4 as u32;
            let reg_list = describe_arm_push_lr(w as u16);
            candidate_functions.push((addr, RuntimeMode::Arm, format!("stmdb sp!, {{{reg_list}}}")));
            candidate_set.insert(addr);
        }
        off4 += 4;
    }

    // 2. Discover jump tables and pointer tables
    let p_tables = scan_pointer_tables(rom, 4);
    let mut dev_jump_tables: Vec<DeveloperJumpTable> = Vec::new();

    for pt in &p_tables {
        let bus_addr = ROM_BUS_BASE + pt.offset as u32;
        let mut code_targets: Vec<u32> = Vec::new();

        for &target in &pt.entries {
            let pure_addr = target & !1;
            if pure_addr >= ROM_BUS_BASE + 0xC0 && pure_addr < max_rom_addr {
                code_targets.push(target);
                // If not already in candidate functions, add as jump-table candidate
                if !candidate_set.contains(&pure_addr) {
                    let mode = if (target & 1) != 0 {
                        RuntimeMode::Thumb
                    } else {
                        RuntimeMode::Arm
                    };
                    candidate_functions.push((
                        pure_addr,
                        mode,
                        "jump_table_target".to_string(),
                    ));
                    candidate_set.insert(pure_addr);
                }
            }
        }

        if code_targets.len() >= 3 && code_targets.len() * 4 >= pt.entries.len() * 3 {
            // Check nearby developer strings (within 1024 bytes around table or targets)
            let mut keywords_found: Vec<String> = Vec::new();
            let check_start = pt.offset.saturating_sub(512);
            let check_end = (pt.offset + pt.entries.len() * 4 + 512).min(rom_len);
            let slice = &rom[check_start..check_end];
            for &kw in DEV_KEYWORDS {
                if contains_ascii_case_insensitive(slice, kw.as_bytes()) {
                    keywords_found.push(kw.to_string());
                }
            }

            // Also check targets for nearby strings
            for &target in code_targets.iter().take(5) {
                let t_off = (target & !1).saturating_sub(ROM_BUS_BASE) as usize;
                if t_off + 256 <= rom_len {
                    let t_slice = &rom[t_off..t_off + 256];
                    for &kw in DEV_KEYWORDS {
                        if !keywords_found.iter().any(|k| k == kw)
                            && contains_ascii_case_insensitive(t_slice, kw.as_bytes())
                        {
                            keywords_found.push(kw.to_string());
                        }
                    }
                }
            }

            let is_early = pt.offset < 0x20000;
            let signal = if !keywords_found.is_empty() {
                "developer_debug_handler_table".to_string()
            } else if is_early {
                "scene_dispatcher_jump_table".to_string()
            } else {
                "code_branch_jump_table".to_string()
            };

            dev_jump_tables.push(DeveloperJumpTable {
                offset: pt.offset,
                bus_address: bus_addr,
                entries_count: code_targets.len(),
                targets: code_targets,
                signal,
                developer_keywords: keywords_found,
            });
        }
    }

    // 3. Callgraph reachability traversal starting from entry_target
    let mut reached_addrs: HashSet<u32> = HashSet::new();
    let mut visited_instructions: HashSet<(u32, RuntimeMode)> = HashSet::new();
    let mut worklist: VecDeque<(u32, RuntimeMode)> = VecDeque::new();
    let mut total_edges: usize = 0;

    worklist.push_back((entry_target, entry_mode));
    reached_addrs.insert(entry_target);

    let max_instructions_to_trace = 200_000;
    let mut instruction_count = 0;

    while let Some((curr_addr, curr_mode)) = worklist.pop_front() {
        if instruction_count >= max_instructions_to_trace {
            break;
        }

        if !visited_instructions.insert((curr_addr, curr_mode)) {
            continue;
        }
        instruction_count += 1;

        if curr_addr < ROM_BUS_BASE + 0xC0 || curr_addr >= max_rom_addr {
            continue;
        }

        let curr_off = (curr_addr - ROM_BUS_BASE) as usize;

        match curr_mode {
            RuntimeMode::Arm => {
                if curr_off + 4 > rom_len || (curr_addr % 4) != 0 {
                    continue;
                }
                let w = u32::from_le_bytes([
                    rom[curr_off],
                    rom[curr_off + 1],
                    rom[curr_off + 2],
                    rom[curr_off + 3],
                ]);

                let cond = w >> 28;
                let is_bl = (w & 0x0F00_0000) == 0x0B00_0000 && cond != 0xF;
                let is_b = (w & 0x0F00_0000) == 0x0A00_0000 && cond != 0xF;
                let is_bx = (w & 0x0FFF_FFF0) == 0x012F_FF10;
                let is_pop_pc = (w & 0x0FFF_0000) == 0x08BD_0000 && (w & (1 << 15)) != 0;

                // Track LDR rN, [pc, #imm]
                // 0x059F0000 or 0x051F0000: LDR rd, [pc, #+/-offset]
                if (w & 0x0E5F_0000) == 0x041F_0000 && (w & (1 << 20)) != 0 {
                    let _rd = ((w >> 12) & 0xF) as usize;
                    let imm12 = (w & 0xFFF) as i64;
                    let u_bit = (w & (1 << 23)) != 0;
                    let lit_addr = if u_bit {
                        curr_addr as i64 + 8 + imm12
                    } else {
                        curr_addr as i64 + 8 - imm12
                    };
                    if lit_addr >= (ROM_BUS_BASE + 0xC0) as i64 && (lit_addr + 4) <= max_rom_addr as i64 {
                        let lit_off = (lit_addr as u32 - ROM_BUS_BASE) as usize;
                        let val = u32::from_le_bytes([
                            rom[lit_off],
                            rom[lit_off + 1],
                            rom[lit_off + 2],
                            rom[lit_off + 3],
                        ]);
                        // If literal is a valid ROM code target, queue it
                        let pure_target = val & !1;
                        if pure_target >= ROM_BUS_BASE + 0xC0 && pure_target < max_rom_addr {
                            let mode = if (val & 1) != 0 {
                                RuntimeMode::Thumb
                            } else {
                                RuntimeMode::Arm
                            };
                            total_edges += 1;
                            reached_addrs.insert(pure_target);
                            worklist.push_back((pure_target, mode));
                        }
                    }
                }

                if is_bl || is_b {
                    let imm24 = (w & 0x00FF_FFFF) as i32;
                    let signed_words = (imm24 << 8) >> 8;
                    let pc = (curr_addr + 8) as i64;
                    let target = (pc + (signed_words as i64 * 4)) as u32;

                    if target >= ROM_BUS_BASE + 0xC0 && target < max_rom_addr {
                        total_edges += 1;
                        reached_addrs.insert(target);
                        worklist.push_back((target, RuntimeMode::Arm));
                    }
                }

                // Unconditional branch terminates sequential flow
                if is_b && cond == 0xE {
                    // sequential flow stops
                } else if is_pop_pc || (is_bx && (w & 0xF) == 14) {
                    // return terminates sequential flow
                } else {
                    // fallthrough to next instruction
                    worklist.push_back((curr_addr + 4, RuntimeMode::Arm));
                }
            }
            RuntimeMode::Thumb => {
                if curr_off + 2 > rom_len || (curr_addr % 2) != 0 {
                    continue;
                }
                let h = u16::from_le_bytes([rom[curr_off], rom[curr_off + 1]]);

                // Track Thumb LDR rN, [pc, #imm] (0x4800..0x4FFF)
                if (h & 0xF800) == 0x4800 {
                    let imm8 = (h & 0xFF) as u32;
                    let lit_addr = ((curr_addr + 4) & !3) + (imm8 * 4);
                    if lit_addr >= ROM_BUS_BASE + 0xC0 && (lit_addr + 4) <= max_rom_addr {
                        let lit_off = (lit_addr - ROM_BUS_BASE) as usize;
                        let val = u32::from_le_bytes([
                            rom[lit_off],
                            rom[lit_off + 1],
                            rom[lit_off + 2],
                            rom[lit_off + 3],
                        ]);
                        let pure_target = val & !1;
                        // If literal points to code in ROM, register as reached target
                        if pure_target >= ROM_BUS_BASE + 0xC0 && pure_target < max_rom_addr && pure_target != curr_addr {
                            let mode = if (val & 1) != 0 {
                                RuntimeMode::Thumb
                            } else {
                                RuntimeMode::Arm
                            };
                            total_edges += 1;
                            reached_addrs.insert(pure_target);
                            worklist.push_back((pure_target, mode));
                        }
                    }
                }

                // Check 32-bit Thumb BL (h1 = 0xF000..0xF7FF, h2 = 0xF800..0xFFFF)
                if (h & 0xF800) == 0xF000 && curr_off + 4 <= rom_len {
                    let h2 = u16::from_le_bytes([rom[curr_off + 2], rom[curr_off + 3]]);
                    if (h2 & 0xF800) == 0xF800 {
                        let imm11_h1 = (h & 0x07FF) as i32;
                        let imm11_h2 = (h2 & 0x07FF) as i32;
                        let offset = (imm11_h1 << 21 >> 9) | (imm11_h2 << 1);
                        let pc = (curr_addr + 4) as i64;
                        let target = (pc + offset as i64) as u32;

                        if target >= ROM_BUS_BASE + 0xC0 && target < max_rom_addr {
                            total_edges += 1;
                            reached_addrs.insert(target);
                            worklist.push_back((target, RuntimeMode::Thumb));
                        }

                        // fallthrough after 4-byte instruction
                        worklist.push_back((curr_addr + 4, RuntimeMode::Thumb));
                        continue;
                    }
                }

                let is_cond_b = (h & 0xF000) == 0xD000 && (h & 0x0F00) < 0x0E00;
                let is_uncond_b = (h & 0xF800) == 0xE000;
                let is_bx_lr = h == 0x4770;
                let is_pop_pc = (h & 0xFF00) == 0xBD00;

                if is_cond_b {
                    let imm8 = (h & 0x00FF) as i8 as i32;
                    let target = (curr_addr as i64 + 4 + (imm8 as i64 * 2)) as u32;
                    if target >= ROM_BUS_BASE + 0xC0 && target < max_rom_addr {
                        total_edges += 1;
                        worklist.push_back((target, RuntimeMode::Thumb));
                    }
                    worklist.push_back((curr_addr + 2, RuntimeMode::Thumb));
                } else if is_uncond_b {
                    let imm11 = (h & 0x07FF) as i32;
                    let signed_imm11 = (imm11 << 21) >> 20;
                    let target = (curr_addr as i64 + 4 + signed_imm11 as i64) as u32;
                    if target >= ROM_BUS_BASE + 0xC0 && target < max_rom_addr {
                        total_edges += 1;
                        worklist.push_back((target, RuntimeMode::Thumb));
                    }
                } else if is_bx_lr || is_pop_pc {
                    // return terminates sequential flow
                } else {
                    // regular 16-bit instruction fallthrough
                    worklist.push_back((curr_addr + 2, RuntimeMode::Thumb));
                }
            }
        }
    }

    // 4. Identify unreached entry points
    let mut unreached: Vec<UnreachedEntryPoint> = Vec::new();
    let mut reached_fn_count = 0;

    for (addr, mode, prologue) in candidate_functions {
        let is_reached = reached_addrs.contains(&addr);
        if is_reached {
            reached_fn_count += 1;
        } else {
            let reason = if prologue.contains("jump_table") {
                "unreached_jump_table_target".to_string()
            } else {
                "uncalled_isolated_subroutine".to_string()
            };
            unreached.push(UnreachedEntryPoint {
                address: addr,
                rom_offset: (addr - ROM_BUS_BASE) as usize,
                mode,
                prologue,
                reason,
            });
        }
    }

    let total_fns = reached_fn_count + unreached.len();
    let ratio = if total_fns > 0 {
        reached_fn_count as f64 / total_fns as f64
    } else {
        0.0
    };

    let summary = TraceAnalysisSummary {
        total_functions_detected: total_fns,
        reached_functions: reached_fn_count,
        unreached_functions: unreached.len(),
        reachability_ratio: (ratio * 1000.0).round() / 1000.0,
        callgraph_edges: total_edges,
        developer_jump_tables_detected: dev_jump_tables.len(),
    };

    Ok(TraceAnalysisReport {
        game_code: hdr.game_code,
        title: hdr.title,
        entry_point: entry_target,
        entry_mode,
        summary,
        unreached_entry_points: unreached,
        developer_jump_tables: dev_jump_tables,
    })
}

fn describe_thumb_push_lr(mask: u16) -> String {
    let mut regs: Vec<&'static str> = Vec::new();
    for i in 0..8 {
        if (mask & (1 << i)) != 0 {
            match i {
                0 => regs.push("r0"),
                1 => regs.push("r1"),
                2 => regs.push("r2"),
                3 => regs.push("r3"),
                4 => regs.push("r4"),
                5 => regs.push("r5"),
                6 => regs.push("r6"),
                7 => regs.push("r7"),
                _ => {}
            }
        }
    }
    regs.push("lr");
    regs.join(", ")
}

fn describe_arm_push_lr(mask: u16) -> String {
    let mut regs: Vec<String> = Vec::new();
    for i in 0..14 {
        if (mask & (1 << i)) != 0 {
            regs.push(format!("r{i}"));
        }
    }
    regs.push("lr".to_string());
    regs.join(", ")
}

fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| {
        w.iter()
            .zip(needle.iter())
            .all(|(&a, &b)| a.eq_ignore_ascii_case(&b))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_synthetic_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x1000];
        // Header
        // Entry: B 0xC0 -> opcode = 0xEA00002E
        let b_c0 = 0xEA00_002Eu32.to_le_bytes();
        rom[0..4].copy_from_slice(&b_c0);
        rom[0xA0..0xAC].copy_from_slice(b"TESTGAME\0\0\0\0");
        rom[0xAC..0xB0].copy_from_slice(b"TEST");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xB2] = 0x96;
        let mut chk = 0u8;
        for b in &rom[0xA0..0xBD] {
            chk = chk.wrapping_sub(*b);
        }
        rom[0xBD] = chk.wrapping_sub(0x19);

        // At 0xC0 (boot entry in ARM):
        // BL to 0xD0: delta = 0xD0 - (0xC0 + 8) = 8 bytes = 2 words -> 0xEB000002
        let bl_d0 = 0xEB00_0002u32.to_le_bytes();
        rom[0xC0..0xC4].copy_from_slice(&bl_d0);
        // Infinite loop at 0xC4: B 0xC4 -> delta = 0xC4 - (0xC4 + 8) = -8 = -2 words -> 0xEAFFFFFE
        let b_loop = 0xEAFF_FFFEu32.to_le_bytes();
        rom[0xC4..0xC8].copy_from_slice(&b_loop);

        // At 0xD0: function reached!
        // stmdb sp!, {r4, lr}: 0xE92D4010
        let stmdb = 0xE92D_4010u32.to_le_bytes();
        rom[0xD0..0xD4].copy_from_slice(&stmdb);
        // ldmia sp!, {r4, pc}: 0xE8BD8010
        let ldmia = 0xE8BD_8010u32.to_le_bytes();
        rom[0xD4..0xD8].copy_from_slice(&ldmia);

        // At 0x200: dead / unreached function!
        // stmdb sp!, {r5, lr}: 0xE92D4020
        let stmdb_dead = 0xE92D_4020u32.to_le_bytes();
        rom[0x200..0x204].copy_from_slice(&stmdb_dead);
        rom[0x204..0x208].copy_from_slice(&ldmia);

        // At 0x300: developer jump table with 4 targets
        let t1 = (ROM_BUS_BASE + 0xD0).to_le_bytes();
        let t2 = (ROM_BUS_BASE + 0x200).to_le_bytes();
        let t3 = (ROM_BUS_BASE + 0xD0).to_le_bytes();
        let t4 = (ROM_BUS_BASE + 0x200).to_le_bytes();
        rom[0x300..0x304].copy_from_slice(&t1);
        rom[0x304..0x308].copy_from_slice(&t2);
        rom[0x308..0x30C].copy_from_slice(&t3);
        rom[0x30C..0x310].copy_from_slice(&t4);

        // Put "DEBUG MENU" keyword near jump table
        rom[0x314..0x31E].copy_from_slice(b"DEBUG MENU");

        rom
    }

    #[test]
    fn test_analyze_trace_synthetic() {
        let rom = build_synthetic_rom();
        let report = analyze_trace(&rom).expect("analysis should succeed");

        assert_eq!(report.game_code, "TEST");
        assert_eq!(report.entry_point, ROM_BUS_BASE + 0xC0);
        assert_eq!(report.summary.reached_functions, 1);
        assert!(report.summary.unreached_functions >= 1);
        assert_eq!(report.developer_jump_tables.len(), 1);
        let jt = &report.developer_jump_tables[0];
        assert_eq!(jt.offset, 0x300);
        assert!(jt.developer_keywords.contains(&"DEBUG".to_string()));
        assert!(jt.developer_keywords.contains(&"MENU".to_string()));
    }
}
