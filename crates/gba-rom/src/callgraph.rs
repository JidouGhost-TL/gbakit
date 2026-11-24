//! Callgraph construction, reachability analysis, and Graphviz DOT / JSON visualization.

#![deny(unsafe_code)]

use formatkit_core::{Error, Result};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::trace::RuntimeMode;
use crate::{scan_pointer_tables, AgbHeader, ROM_BUS_BASE};

/// Reachability status of a function or entry point in the ROM callgraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallgraphNodeStatus {
    /// Reached via static traversal from the cartridge reset entry point.
    Reachable,
    /// Detected function prologue with in-degree 0 (no incoming call/branch edges discovered).
    UncalledLeaf,
    /// Candidate entry point that was not reached during traversal from the reset vector.
    Unreached,
}

impl CallgraphNodeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reachable => "reachable",
            Self::UncalledLeaf => "uncalled_leaf",
            Self::Unreached => "unreached",
        }
    }
}

/// Category / role of a callgraph node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallgraphNodeKind {
    /// Primary cartridge boot entry point (0x080000C0 or header branch target).
    Entry,
    /// Standard subroutine identified by function prologue (e.g. `push {..., lr}`).
    Function,
    /// Destination reached via developer / dispatcher jump table.
    JumpTableTarget,
}

impl CallgraphNodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Entry => "entry",
            Self::Function => "function",
            Self::JumpTableTarget => "jump_table_target",
        }
    }
}

/// Classification of a callgraph transition edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallgraphEdgeKind {
    /// Direct function call (ARM/Thumb BL / BLX).
    DirectCall,
    /// Direct unconditional/conditional branch (ARM/Thumb B / B<cond>).
    DirectBranch,
    /// Literal pool code pointer reference (LDR rN, [pc, #imm]).
    LiteralPointer,
    /// Dispatch transition via developer jump table.
    JumpTableDispatch,
}

impl CallgraphEdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DirectCall => "direct_call",
            Self::DirectBranch => "direct_branch",
            Self::LiteralPointer => "literal_pointer",
            Self::JumpTableDispatch => "jump_table_dispatch",
        }
    }
}

/// A node in the GBA ROM callgraph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallgraphNode {
    pub id: String,
    pub address: u32,
    pub rom_offset: usize,
    pub mode: RuntimeMode,
    pub status: CallgraphNodeStatus,
    pub kind: CallgraphNodeKind,
    pub in_degree: usize,
    pub out_degree: usize,
    pub label: String,
}

/// A directed edge in the GBA ROM callgraph.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallgraphEdge {
    pub caller: u32,
    pub callee: u32,
    pub kind: CallgraphEdgeKind,
}

/// A discovered developer or scene dispatcher jump table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallgraphJumpTable {
    pub offset: usize,
    pub bus_address: u32,
    pub entries_count: usize,
    pub targets: Vec<u32>,
    pub developer_keywords: Vec<String>,
}

/// Aggregated reachability metrics for a ROM callgraph.
#[derive(Debug, Clone, PartialEq)]
pub struct CallgraphSummary {
    pub total_nodes: usize,
    pub reachable_nodes: usize,
    pub uncalled_leaf_nodes: usize,
    pub unreached_nodes: usize,
    pub jump_tables_count: usize,
    pub total_edges: usize,
    pub reachability_ratio: f64,
    pub dead_code_ratio: f64,
}

/// Complete report of ROM callgraph analysis and visualization.
#[derive(Debug, Clone, PartialEq)]
pub struct CallgraphReport {
    pub game_code: String,
    pub title: String,
    pub entry_point: u32,
    pub entry_mode: RuntimeMode,
    pub summary: CallgraphSummary,
    pub nodes: Vec<CallgraphNode>,
    pub edges: Vec<CallgraphEdge>,
    pub jump_tables: Vec<CallgraphJumpTable>,
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
];

fn escape_dot(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

impl CallgraphReport {
    /// Render the callgraph as a Graphviz DOT script.
    pub fn to_dot(&self) -> String {
        let mut dot = String::new();
        dot.push_str("digraph GBA_Callgraph {\n");
        dot.push_str("  graph [\n");
        dot.push_str("    rankdir=LR,\n");
        dot.push_str("    bgcolor=\"#181824\",\n");
        dot.push_str("    fontcolor=\"#cdd6f4\",\n");
        dot.push_str(&format!(
            "    label=\"GBA Callgraph: {} ({})\\nReachability: {:.1}% ({}/{} functions)\"\n",
            escape_dot(&self.title),
            escape_dot(&self.game_code),
            self.summary.reachability_ratio * 100.0,
            self.summary.reachable_nodes,
            self.summary.total_nodes
        ));
        dot.push_str("  ];\n");
        dot.push_str("  node [\n");
        dot.push_str("    shape=box,\n");
        dot.push_str("    style=\"filled,rounded\",\n");
        dot.push_str("    fontname=\"Courier, monospace\",\n");
        dot.push_str("    fontsize=9,\n");
        dot.push_str("    penwidth=1.2\n");
        dot.push_str("  ];\n");
        dot.push_str("  edge [\n");
        dot.push_str("    fontname=\"Courier, monospace\",\n");
        dot.push_str("    fontsize=8,\n");
        dot.push_str("    penwidth=1.0\n");
        dot.push_str("  ];\n\n");

        // Emit jump tables cluster
        if !self.jump_tables.is_empty() {
            dot.push_str("  subgraph cluster_jump_tables {\n");
            dot.push_str("    graph [\n");
            dot.push_str("      label=\"Developer Jump Tables\",\n");
            dot.push_str("      style=\"dashed,rounded\",\n");
            dot.push_str("      color=\"#b4befe\",\n");
            dot.push_str("      fontcolor=\"#b4befe\"\n");
            dot.push_str("    ];\n");
            for jt in &self.jump_tables {
                let node_id = format!("jt_{:08X}", jt.bus_address);
                let kw_str = if jt.developer_keywords.is_empty() {
                    String::new()
                } else {
                    let escaped: Vec<String> = jt
                        .developer_keywords
                        .iter()
                        .map(|k| escape_dot(k))
                        .collect();
                    format!("\\n[{}]", escaped.join(","))
                };
                dot.push_str(&format!(
                    "    \"{}\" [label=\"JumpTable @ 0x{:08X}\\n({} targets){}\", fillcolor=\"#45475a\", fontcolor=\"#cdd6f4\", shape=hexagon];\n",
                    node_id, jt.bus_address, jt.entries_count, kw_str
                ));
            }
            dot.push_str("  }\n\n");
        }

        // Emit nodes
        for node in &self.nodes {
            let (fillcolor, fontcolor, shape, style) = match (node.kind, node.status) {
                (CallgraphNodeKind::Entry, _) => {
                    ("#2e7d32", "#ffffff", "doubleoctagon", "filled,rounded")
                }
                (_, CallgraphNodeStatus::Reachable) => {
                    ("#1e66f5", "#ffffff", "box", "filled,rounded")
                }
                (_, CallgraphNodeStatus::UncalledLeaf) => {
                    ("#e64553", "#ffffff", "box", "filled,dashed,rounded")
                }
                (_, CallgraphNodeStatus::Unreached) => {
                    ("#6c7086", "#11111b", "box", "filled,rounded")
                }
            };

            let label = format!(
                "{}\\n0x{:08X} ({})\\n[in:{}, out:{}]",
                escape_dot(&node.label),
                node.address,
                node.mode.as_str(),
                node.in_degree,
                node.out_degree
            );

            dot.push_str(&format!(
                "  \"{}\" [label=\"{}\", fillcolor=\"{}\", fontcolor=\"{}\", shape={}, style=\"{}\"];\n",
                escape_dot(&node.id),
                label,
                fillcolor,
                fontcolor,
                shape,
                style
            ));
        }

        dot.push('\n');

        // Emit jump table dispatch edges
        for jt in &self.jump_tables {
            let jt_id = format!("jt_{:08X}", jt.bus_address);
            for target in &jt.targets {
                let pure_target = target & !1;
                let target_id = format!("fn_{:08X}", pure_target);
                dot.push_str(&format!(
                    "  \"{}\" -> \"{}\" [label=\"dispatch\", color=\"#cba6f7\", style=dashed];\n",
                    jt_id, target_id
                ));
            }
        }

        // Emit regular call/branch edges
        for edge in &self.edges {
            let caller_id = format!("fn_{:08X}", edge.caller);
            let callee_id = format!("fn_{:08X}", edge.callee);
            let (color, style, label) = match edge.kind {
                CallgraphEdgeKind::DirectCall => ("#89b4fa", "solid", "call"),
                CallgraphEdgeKind::DirectBranch => ("#a6adc8", "dotted", "branch"),
                CallgraphEdgeKind::LiteralPointer => ("#94e2d5", "dashed", "ptr"),
                CallgraphEdgeKind::JumpTableDispatch => ("#cba6f7", "dashed", "dispatch"),
            };

            dot.push_str(&format!(
                "  \"{}\" -> \"{}\" [label=\"{}\", color=\"{}\", style={}];\n",
                caller_id, callee_id, label, color, style
            ));
        }

        dot.push_str("}\n");
        dot
    }

    /// Render the callgraph analysis report as a JSON string.
    pub fn to_json(&self) -> String {
        let nodes_json: Vec<String> = self
            .nodes
            .iter()
            .map(|n| {
                format!(
                    "{{\"id\":\"{}\",\"address\":\"0x{:08X}\",\"rom_offset\":\"0x{:08X}\",\"mode\":\"{}\",\"status\":\"{}\",\"kind\":\"{}\",\"in_degree\":{},\"out_degree\":{},\"label\":\"{}\"}}",
                    escape_json(&n.id),
                    n.address,
                    n.rom_offset,
                    n.mode.as_str(),
                    n.status.as_str(),
                    n.kind.as_str(),
                    n.in_degree,
                    n.out_degree,
                    escape_json(&n.label)
                )
            })
            .collect();

        let edges_json: Vec<String> = self
            .edges
            .iter()
            .map(|e| {
                format!(
                    "{{\"caller\":\"0x{:08X}\",\"callee\":\"0x{:08X}\",\"kind\":\"{}\"}}",
                    e.caller,
                    e.callee,
                    e.kind.as_str()
                )
            })
            .collect();

        let jt_json: Vec<String> = self
            .jump_tables
            .iter()
            .map(|j| {
                let targets: Vec<String> = j.targets.iter().map(|t| format!("\"0x{:08X}\"", t)).collect();
                let kws: Vec<String> = j
                    .developer_keywords
                    .iter()
                    .map(|k| format!("\"{}\"", escape_json(k)))
                    .collect();
                format!(
                    "{{\"offset\":\"0x{:08X}\",\"bus_address\":\"0x{:08X}\",\"entries_count\":{},\"targets\":[{}],\"developer_keywords\":[{}]}}",
                    j.offset, j.bus_address, j.entries_count, targets.join(","), kws.join(",")
                )
            })
            .collect();

        format!(
            "{{\"game_code\":\"{}\",\"title\":\"{}\",\"entry_point\":\"0x{:08X}\",\"entry_mode\":\"{}\",\
             \"summary\":{{\"total_nodes\":{},\"reachable_nodes\":{},\"uncalled_leaf_nodes\":{},\"unreached_nodes\":{},\
             \"jump_tables_count\":{},\"total_edges\":{},\"reachability_ratio\":{:.4},\"dead_code_ratio\":{:.4}}},\
             \"nodes\":[{}],\"edges\":[{}],\"jump_tables\":[{}]}}",
            escape_json(&self.game_code),
            escape_json(&self.title),
            self.entry_point,
            self.entry_mode.as_str(),
            self.summary.total_nodes,
            self.summary.reachable_nodes,
            self.summary.uncalled_leaf_nodes,
            self.summary.unreached_nodes,
            self.summary.jump_tables_count,
            self.summary.total_edges,
            self.summary.reachability_ratio,
            self.summary.dead_code_ratio,
            nodes_json.join(","),
            edges_json.join(","),
            jt_json.join(",")
        )
    }
}

/// Analyze a GBA cartridge ROM and construct its reachability callgraph.
pub fn analyze_callgraph(rom: &[u8]) -> Result<CallgraphReport> {
    if rom.len() < crate::HEADER_SIZE {
        return Err(Error::Truncated {
            offset: 0,
            needed: crate::HEADER_SIZE,
            available: rom.len(),
        });
    }

    let hdr = AgbHeader::parse(rom)?;
    let rom_len = rom.len().min(crate::MAX_ROM_SIZE);
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

    // 1. Discover candidate functions across the executable ROM range
    let mut candidate_functions: HashMap<u32, (RuntimeMode, CallgraphNodeKind, String)> = HashMap::new();

    // Register entry point
    candidate_functions.insert(
        entry_target,
        (entry_mode, CallgraphNodeKind::Entry, "reset_entry".to_string()),
    );

    // Scan Thumb candidates (push {..., lr})
    let mut off = 0xC0;
    while off + 2 <= rom_len {
        let h = u16::from_le_bytes([rom[off], rom[off + 1]]);
        if (h & 0xFF00) == 0xB500 {
            let addr = ROM_BUS_BASE + off as u32;
            let reg_list = describe_thumb_push_lr(h & 0x00FF);
            candidate_functions
                .entry(addr)
                .or_insert((RuntimeMode::Thumb, CallgraphNodeKind::Function, format!("push {{{reg_list}}}")));
        }
        off += 2;
    }

    // Scan ARM candidates (stmdb sp!, {..., lr})
    let mut off4 = 0xC0;
    while off4 + 4 <= rom_len {
        let w = u32::from_le_bytes([rom[off4], rom[off4 + 1], rom[off4 + 2], rom[off4 + 3]]);
        if (w & 0x0FFF_0000) == 0x092D_0000 && (w & (1 << 14)) != 0 {
            let addr = ROM_BUS_BASE + off4 as u32;
            let reg_list = describe_arm_push_lr(w as u16);
            candidate_functions
                .entry(addr)
                .or_insert((RuntimeMode::Arm, CallgraphNodeKind::Function, format!("stmdb sp!, {{{reg_list}}}")));
        }
        off4 += 4;
    }

    // 2. Discover jump tables and pointer tables
    let p_tables = scan_pointer_tables(rom, 4);
    let mut dev_jump_tables: Vec<CallgraphJumpTable> = Vec::new();

    for pt in &p_tables {
        let bus_addr = ROM_BUS_BASE + pt.offset as u32;
        let mut code_targets: Vec<u32> = Vec::new();

        for &target in &pt.entries {
            let pure_addr = target & !1;
            if pure_addr >= ROM_BUS_BASE + 0xC0 && pure_addr < max_rom_addr {
                code_targets.push(target);
                let mode = if (target & 1) != 0 {
                    RuntimeMode::Thumb
                } else {
                    RuntimeMode::Arm
                };
                candidate_functions
                    .entry(pure_addr)
                    .or_insert((mode, CallgraphNodeKind::JumpTableTarget, format!("jt_target_0x{:08X}", bus_addr)));
            }
        }

        if code_targets.len() >= 4 {
            let kw = scan_nearby_keywords(rom, pt.offset, 256);
            dev_jump_tables.push(CallgraphJumpTable {
                offset: pt.offset,
                bus_address: bus_addr,
                entries_count: code_targets.len(),
                targets: code_targets,
                developer_keywords: kw,
            });
        }
    }

    // 3. Graph traversal from entry point
    let mut reached_addrs: HashSet<u32> = HashSet::new();
    let mut visited_instructions: HashSet<(u32, RuntimeMode)> = HashSet::new();
    let mut worklist: VecDeque<(u32, RuntimeMode)> = VecDeque::new();
    let mut edges_set: HashSet<CallgraphEdge> = HashSet::new();

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

                // Track ARM LDR rN, [pc, #imm]
                if (w & 0x0E5F_0000) == 0x041F_0000 && (w & (1 << 20)) != 0 {
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
                        let pure_target = val & !1;
                        if pure_target >= ROM_BUS_BASE + 0xC0 && pure_target < max_rom_addr {
                            let mode = if (val & 1) != 0 {
                                RuntimeMode::Thumb
                            } else {
                                RuntimeMode::Arm
                            };
                            reached_addrs.insert(pure_target);
                            edges_set.insert(CallgraphEdge {
                                caller: curr_addr,
                                callee: pure_target,
                                kind: CallgraphEdgeKind::LiteralPointer,
                            });
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
                        reached_addrs.insert(target);
                        let kind = if is_bl {
                            CallgraphEdgeKind::DirectCall
                        } else {
                            CallgraphEdgeKind::DirectBranch
                        };
                        edges_set.insert(CallgraphEdge {
                            caller: curr_addr,
                            callee: target,
                            kind,
                        });
                        worklist.push_back((target, RuntimeMode::Arm));
                    }
                }

                if !(is_b && cond == 0xE) && !is_pop_pc && !(is_bx && (w & 0xF) == 14) {
                    worklist.push_back((curr_addr + 4, RuntimeMode::Arm));
                }
            }
            RuntimeMode::Thumb => {
                if curr_off + 2 > rom_len || (curr_addr % 2) != 0 {
                    continue;
                }
                let h = u16::from_le_bytes([rom[curr_off], rom[curr_off + 1]]);

                // Track Thumb LDR rN, [pc, #imm]
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
                        if pure_target >= ROM_BUS_BASE + 0xC0 && pure_target < max_rom_addr && pure_target != curr_addr {
                            let mode = if (val & 1) != 0 {
                                RuntimeMode::Thumb
                            } else {
                                RuntimeMode::Arm
                            };
                            reached_addrs.insert(pure_target);
                            edges_set.insert(CallgraphEdge {
                                caller: curr_addr,
                                callee: pure_target,
                                kind: CallgraphEdgeKind::LiteralPointer,
                            });
                            worklist.push_back((pure_target, mode));
                        }
                    }
                }

                // Check 32-bit Thumb BL
                if (h & 0xF800) == 0xF000 && curr_off + 4 <= rom_len {
                    let h2 = u16::from_le_bytes([rom[curr_off + 2], rom[curr_off + 3]]);
                    if (h2 & 0xF800) == 0xF800 {
                        let imm11_h1 = (h & 0x07FF) as i32;
                        let imm11_h2 = (h2 & 0x07FF) as i32;
                        let offset = (imm11_h1 << 21 >> 9) | (imm11_h2 << 1);
                        let pc = (curr_addr + 4) as i64;
                        let target = (pc + offset as i64) as u32;

                        if target >= ROM_BUS_BASE + 0xC0 && target < max_rom_addr {
                            reached_addrs.insert(target);
                            edges_set.insert(CallgraphEdge {
                                caller: curr_addr,
                                callee: target,
                                kind: CallgraphEdgeKind::DirectCall,
                            });
                            worklist.push_back((target, RuntimeMode::Thumb));
                        }

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
                        edges_set.insert(CallgraphEdge {
                            caller: curr_addr,
                            callee: target,
                            kind: CallgraphEdgeKind::DirectBranch,
                        });
                        worklist.push_back((target, RuntimeMode::Thumb));
                    }
                    worklist.push_back((curr_addr + 2, RuntimeMode::Thumb));
                } else if is_uncond_b {
                    let imm11 = (h & 0x07FF) as i32;
                    let signed_imm11 = (imm11 << 21) >> 20;
                    let target = (curr_addr as i64 + 4 + signed_imm11 as i64) as u32;
                    if target >= ROM_BUS_BASE + 0xC0 && target < max_rom_addr {
                        edges_set.insert(CallgraphEdge {
                            caller: curr_addr,
                            callee: target,
                            kind: CallgraphEdgeKind::DirectBranch,
                        });
                        worklist.push_back((target, RuntimeMode::Thumb));
                    }
                } else if !is_bx_lr && !is_pop_pc {
                    worklist.push_back((curr_addr + 2, RuntimeMode::Thumb));
                }
            }
        }
    }

    // 4. Calculate degrees and assemble CallgraphNode entries
    let mut in_degrees: HashMap<u32, usize> = HashMap::new();
    let mut out_degrees: HashMap<u32, usize> = HashMap::new();

    for edge in &edges_set {
        *in_degrees.entry(edge.callee).or_insert(0) += 1;
        *out_degrees.entry(edge.caller).or_insert(0) += 1;
    }

    let mut nodes: Vec<CallgraphNode> = Vec::new();
    let mut reachable_count = 0;
    let mut uncalled_leaf_count = 0;
    let mut unreached_count = 0;

    let mut sorted_addrs: Vec<u32> = candidate_functions.keys().copied().collect();
    sorted_addrs.sort_unstable();

    for addr in sorted_addrs {
        let Some(&(mode, kind, ref label)) = candidate_functions.get(&addr) else {
            continue;
        };
        let in_deg = in_degrees.get(&addr).copied().unwrap_or(0);
        let out_deg = out_degrees.get(&addr).copied().unwrap_or(0);

        let is_reached = reached_addrs.contains(&addr);
        let status = if is_reached {
            reachable_count += 1;
            CallgraphNodeStatus::Reachable
        } else if in_deg == 0 && kind != CallgraphNodeKind::Entry {
            uncalled_leaf_count += 1;
            CallgraphNodeStatus::UncalledLeaf
        } else {
            unreached_count += 1;
            CallgraphNodeStatus::Unreached
        };

        nodes.push(CallgraphNode {
            id: format!("fn_{:08X}", addr),
            address: addr,
            rom_offset: (addr - ROM_BUS_BASE) as usize,
            mode,
            status,
            kind,
            in_degree: in_deg,
            out_degree: out_deg,
            label: label.clone(),
        });
    }

    let total_nodes = nodes.len();
    let reachability_ratio = if total_nodes > 0 {
        reachable_count as f64 / total_nodes as f64
    } else {
        0.0
    };
    let dead_code_ratio = 1.0 - reachability_ratio;

    let mut edges: Vec<CallgraphEdge> = edges_set.into_iter().collect();
    edges.sort_by_key(|e| (e.caller, e.callee));

    Ok(CallgraphReport {
        game_code: hdr.game_code,
        title: hdr.title,
        entry_point: entry_target,
        entry_mode,
        summary: CallgraphSummary {
            total_nodes,
            reachable_nodes: reachable_count,
            uncalled_leaf_nodes: uncalled_leaf_count,
            unreached_nodes: unreached_count,
            jump_tables_count: dev_jump_tables.len(),
            total_edges: edges.len(),
            reachability_ratio,
            dead_code_ratio,
        },
        nodes,
        edges,
        jump_tables: dev_jump_tables,
    })
}

fn describe_thumb_push_lr(rlist: u16) -> String {
    let mut regs = Vec::new();
    for i in 0..8 {
        if (rlist & (1 << i)) != 0 {
            regs.push(format!("r{}", i));
        }
    }
    regs.push("lr".to_string());
    regs.join(", ")
}

fn describe_arm_push_lr(rlist: u16) -> String {
    let mut regs = Vec::new();
    for i in 0..16 {
        if (rlist & (1 << i)) != 0 {
            let name = match i {
                13 => "sp".to_string(),
                14 => "lr".to_string(),
                15 => "pc".to_string(),
                n => format!("r{n}"),
            };
            regs.push(name);
        }
    }
    regs.join(", ")
}

fn scan_nearby_keywords(rom: &[u8], offset: usize, radius: usize) -> Vec<String> {
    let start = offset.saturating_sub(radius);
    let end = (offset + radius).min(rom.len());
    let slice = &rom[start..end];
    let mut found = Vec::new();

    for &kw in DEV_KEYWORDS {
        let kw_bytes = kw.as_bytes();
        let kw_len = kw_bytes.len();
        if slice.len() < kw_len {
            continue;
        }
        let mut hit = false;
        for window in slice.windows(kw_len) {
            if window.iter().zip(kw_bytes).all(|(&a, &b)| a.eq_ignore_ascii_case(&b)) {
                hit = true;
                break;
            }
        }
        if hit {
            found.push(kw.to_string());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute_header_complement;

    #[test]
    fn test_analyze_callgraph_synthetic() {
        let mut rom = vec![0u8; 4096];
        // Header entry: B 0xC0 (EA00002E)
        rom[0..4].copy_from_slice(&0xEA00_002Eu32.to_le_bytes());
        rom[0xA0..0xAC].copy_from_slice(b"CALLGRAPH T ");
        rom[0xAC..0xB0].copy_from_slice(b"ACGE");
        rom[0xB0..0xB2].copy_from_slice(b"01");
        rom[0xB2] = 0x96;
        rom[0xBD] = compute_header_complement(&rom[0xA0..0xBD]);

        // At 0xC0 (ARM reset entry):
        // BL 0xD0 (EA000002) -> 0xC0 + 8 + (2*4) = 0xD0
        // B 0xC0 (loop forever)
        rom[0xC0..0xC4].copy_from_slice(&0xEB00_0002u32.to_le_bytes()); // BL 0xD0
        rom[0xC4..0xC8].copy_from_slice(&0xEAFF_FFFEu32.to_le_bytes()); // B 0xC4

        // Function at 0xD0 (ARM function):
        // stmdb sp!, {r4, lr} -> 0xE92D4010
        // bx lr -> 0xE12FFF1E
        rom[0xD0..0xD4].copy_from_slice(&0xE92D_4010u32.to_le_bytes());
        rom[0xD4..0xD8].copy_from_slice(&0xE12F_FF1Eu32.to_le_bytes());

        // Isolated uncalled function at 0x200 (Thumb):
        // push {r4, lr} -> 0xB510
        // pop {r4, pc} -> 0xBD10
        rom[0x200..0x202].copy_from_slice(&0xB510u16.to_le_bytes());
        rom[0x202..0x204].copy_from_slice(&0xBD10u16.to_le_bytes());

        // Developer jump table at 0x300 (4 entries of 0x080000D0 / 0x08000200)
        let bus_base = ROM_BUS_BASE;
        rom[0x300..0x304].copy_from_slice(&(bus_base + 0xD0).to_le_bytes());
        rom[0x304..0x308].copy_from_slice(&(bus_base + 0x200).to_le_bytes());
        rom[0x308..0x30C].copy_from_slice(&(bus_base + 0xD0).to_le_bytes());
        rom[0x30C..0x310].copy_from_slice(&(bus_base + 0x200).to_le_bytes());
        rom[0x310..0x320].copy_from_slice(b"DEBUG MENU TEST ");

        let report = analyze_callgraph(&rom).expect("analyze synthetic callgraph");
        assert_eq!(report.game_code, "ACGE");
        assert_eq!(report.summary.reachable_nodes, 2); // 0xC0 and 0xD0
        assert_eq!(report.summary.uncalled_leaf_nodes, 1); // 0x200
        assert_eq!(report.summary.jump_tables_count, 1);

        // Verify Graphviz DOT generation
        let dot = report.to_dot();
        assert!(dot.contains("digraph GBA_Callgraph"));
        assert!(dot.contains("fn_080000C0"));
        assert!(dot.contains("fn_080000D0"));
        assert!(dot.contains("fn_08000200"));
        assert!(dot.contains("jt_08000300"));
        assert!(dot.contains("DEBUG"));
        assert!(dot.contains("TEST"));
        assert!(dot.contains("MENU"));

        // Verify JSON generation
        let json_str = report.to_json();
        assert!(json_str.contains("\"game_code\":\"ACGE\""));
        assert!(json_str.contains("\"reachable_nodes\":2"));
        assert!(json_str.contains("\"uncalled_leaf_nodes\":1"));
        assert!(json_str.contains("\"jump_tables_count\":1"));
    }

    #[test]
    fn describe_arm_push_lr_collects_multiple_registers() {
        // r4, r5, r6, lr -> bits 4, 5, 6, 14
        let mask = (1u16 << 4) | (1u16 << 5) | (1u16 << 6) | (1u16 << 14);
        assert_eq!(describe_arm_push_lr(mask), "r4, r5, r6, lr");
        // r0, sp, lr, pc
        let mask2 = (1u16 << 0) | (1u16 << 13) | (1u16 << 14) | (1u16 << 15);
        assert_eq!(describe_arm_push_lr(mask2), "r0, sp, lr, pc");
    }

    #[test]
    fn dot_and_json_escape_special_characters() {
        let report = CallgraphReport {
            game_code: "A\"\\B".to_string(),
            title: "TEST\"TITLE\\\n\t".to_string(),
            entry_point: ROM_BUS_BASE + 0xC0,
            entry_mode: RuntimeMode::Arm,
            summary: CallgraphSummary {
                total_nodes: 1,
                reachable_nodes: 1,
                uncalled_leaf_nodes: 0,
                unreached_nodes: 0,
                jump_tables_count: 0,
                total_edges: 0,
                reachability_ratio: 1.0,
                dead_code_ratio: 0.0,
            },
            nodes: vec![CallgraphNode {
                id: "fn_080000C0".to_string(),
                address: ROM_BUS_BASE + 0xC0,
                rom_offset: 0xC0,
                mode: RuntimeMode::Arm,
                status: CallgraphNodeStatus::Reachable,
                kind: CallgraphNodeKind::Entry,
                in_degree: 0,
                out_degree: 0,
                label: "entry\"\\label".to_string(),
            }],
            edges: Vec::new(),
            jump_tables: Vec::new(),
        };
        let dot = report.to_dot();
        assert!(dot.contains("TEST\\\"TITLE\\\\\\n\\t"));
        assert!(dot.contains("entry\\\"\\\\label"));
        let json = report.to_json();
        assert!(json.contains("\"title\":\"TEST\\\"TITLE\\\\\\n\\t\""));
        assert!(json.contains("\"game_code\":\"A\\\"\\\\B\""));
        assert!(json.contains("\"label\":\"entry\\\"\\\\label\""));
    }
}
