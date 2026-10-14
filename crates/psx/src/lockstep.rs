//! `psx lockstep`: run the native core and PCSX-Redux side by side from the
//! BIOS reset vector, comparing PC and registers after every instruction.
//!
//! Until devices exist, two things the oracle knows are injected rather than
//! counted as divergence:
//! - the value of every load from I/O space (applied to the destination
//!   register once the load retires, and stored into the I/O register file);
//! - interrupts: when Redux enters an exception vector and the core did not,
//!   the core takes an external interrupt at the same point.
//!
//! Injection counts are reported; anything else that differs stops the run.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Instant;

use psx_core::disasm::{disasm, reg};
use psx_core::{Machine, Width};
use psx_redux::gdb::{GdbSession, Regs};
use psx_redux::Backend;
use serde_json::{json, Value};

fn h(v: u32) -> String {
    format!("0x{v:08X}")
}

fn diffs(m: &Machine, r: &Regs) -> Vec<(String, u32, u32)> {
    let mut d = Vec::new();
    if m.cpu.pc != r.pc {
        d.push(("pc".into(), m.cpu.pc, r.pc));
    }
    for i in 1..32 {
        if m.cpu.regs[i] != r.gpr[i] {
            d.push((reg(i as u32).into(), m.cpu.regs[i], r.gpr[i]));
        }
    }
    if m.cpu.hi != r.hi {
        d.push(("hi".into(), m.cpu.hi, r.hi));
    }
    if m.cpu.lo != r.lo {
        d.push(("lo".into(), m.cpu.lo, r.lo));
    }
    d
}

/// One parsed line of Redux's `-trace` output.
struct TraceLine {
    pc: u32,
    code: u32,
    /// `(register name, value before execution)` for each operand shown.
    operands: Vec<(String, u32)>,
    /// `[addr] = value`: memory word before a store, or the value a load reads.
    mem: Option<(u32, u32)>,
    text: String,
}

fn parse_trace_line(line: &str) -> Option<TraceLine> {
    let b = line.as_bytes();
    if b.len() < 18 || b[8] != b' ' || b[17] != b':' {
        return None;
    }
    let pc = u32::from_str_radix(&line[..8], 16).ok()?;
    let code = u32::from_str_radix(&line[9..17], 16).ok()?;
    let rest = &line[18..];
    let mut operands = Vec::new();
    let mut i = 0;
    while let Some(d) = rest[i..].find('$') {
        let start = i + d + 1;
        let Some(open) = rest[start..].find('(') else { break };
        let name = &rest[start..start + open];
        let vstart = start + open + 1;
        if let Some(v) = rest.get(vstart..vstart + 8).and_then(|v| u32::from_str_radix(v, 16).ok()) {
            operands.push((name.to_string(), v));
        }
        i = vstart;
    }
    let mem = rest.find("([").and_then(|o| {
        let a = u32::from_str_radix(rest.get(o + 2..o + 10)?, 16).ok()?;
        let eq = rest[o..].find("= ")? + o + 2;
        let v = u32::from_str_radix(rest.get(eq..eq + 8)?.trim_end_matches(')'), 16).ok()?;
        Some((a, v))
    });
    Some(TraceLine { pc, code, operands, mem, text: line.to_string() })
}

fn reg_index(name: &str) -> Option<usize> {
    (0..32).find(|&i| reg(i as u32) == name || (name == "s8" && i == 30))
}

/// Fast mode: stream Redux's instruction trace and check each instruction's
/// PC, word and source-operand values before the core executes it.
pub fn run_trace(root: &Path, bios_path: &Path, steps: u64, context: usize) -> (bool, Value) {
    use std::io::BufRead;
    let err = |code: &str, m: String| (false, json!({"error": {"code": code, "message": m}}));
    let backend = match Backend::from_project(root) {
        Ok(b) => b,
        Err(e) => return err(e.code(), e.to_string()),
    };
    let bios = match std::fs::read(bios_path) {
        Ok(b) => b,
        Err(e) => return err("io_error", format!("{}: {e}", bios_path.display())),
    };
    let dir = root.join(".psx/lockstep").join(format!("{}", std::process::id()));
    let _ = std::fs::create_dir_all(dir.join("config"));
    let _ = std::fs::write(dir.join("config/pcsx.json"), "{\"SPU\":{\"NullSync\":true,\"Speed\":0}}\n");
    let abs = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let mut child = match std::process::Command::new(&backend.binary)
        .current_dir(&dir)
        .args(["-no-ui", "-run", "-interpreter", "-debugger", "-trace", "-portable"])
        .arg(abs(&dir.join("config")))
        .arg("-bios")
        .arg(abs(bios_path))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return err("backend_unavailable", e.to_string()),
    };
    let reader = std::io::BufReader::with_capacity(1 << 20, child.stdout.take().unwrap());
    let mut m = Machine::new(bios);
    let started = Instant::now();
    let mut history: VecDeque<String> = VecDeque::with_capacity(context + 1);
    let (mut io_injected, mut irq_injected, mut operand_checks) = (0u64, 0u64, 0u64);
    let mut done = 0u64;
    let mut divergence = Value::Null;
    let vectors = [0x8000_0080u32, 0xBFC0_0180];
    let mut io_dest: [Option<(u32, Width)>; 32] = [None; 32];
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let Some(t) = parse_trace_line(&line) else { continue };
        if done >= steps {
            break;
        }
        if t.pc != m.cpu.pc && vectors.contains(&t.pc) && !vectors.contains(&m.cpu.pc) {
            m.force_interrupt();
            irq_injected += 1;
        }
        let _ = t.mem;
        let mut problems = Vec::new();
        if t.pc != m.cpu.pc {
            problems.push(json!({"what": "pc", "core": h(m.cpu.pc), "redux": h(t.pc)}));
        } else {
            let ours = m.bus.read(t.pc, Width::Word).unwrap_or(0);
            if ours != t.code {
                problems.push(json!({"what": "instruction", "core": h(ours), "redux": h(t.code)}));
            }
        }
        for (name, v) in &t.operands {
            if let Some(r) = reg_index(name) {
                operand_checks += 1;
                if m.cpu.regs[r] != *v {
                    // Hardware register value the core has no device for yet: adopt it.
                    if let Some((addr, w)) = io_dest[r].take() {
                        m.cpu.regs[r] = *v;
                        m.bus.poke_io(addr, w, *v);
                        io_injected += 1;
                    } else {
                        problems.push(json!({"what": name, "core": h(m.cpu.regs[r]), "redux": h(*v)}));
                    }
                } else {
                    io_dest[r] = None;
                }
            }
        }
        history.push_back(t.text.clone());
        if history.len() > context {
            history.pop_front();
        }
        if !problems.is_empty() {
            divergence = json!({
                "step": done,
                "trace_line": t.text,
                "core_asm": disasm(m.bus.read(m.cpu.pc, Width::Word).unwrap_or(0), m.cpu.pc),
                "problems": problems,
                "core": {"sr": h(m.cpu.cop0[12]), "cause": h(m.cpu.cop0[13]), "epc": h(m.cpu.cop0[14])},
                "recent_redux_trace": history.iter().cloned().collect::<Vec<_>>(),
            });
            break;
        }
        let info = m.step();
        // Loads from I/O: the trace's memory column is a side-effect-free peek, not
        // what the hardware returned, so the value is adopted when the register is next used.
        if let Some((addr, rt)) = info.io_load {
            let w = match t.code >> 26 {
                0x20 | 0x24 => Width::Byte,
                0x21 | 0x25 => Width::Half,
                _ => Width::Word,
            };
            if rt != 0 {
                io_dest[rt as usize] = Some((addr & 0x1FFF_FFFF, w));
            }
        }
        done += 1;
    }
    let _ = child.kill();
    let _ = child.wait();
    let secs = started.elapsed().as_secs_f64();
    let ok = divergence.is_null() && done >= steps;
    let mut unmodelled: std::collections::BTreeMap<String, u64> = Default::default();
    for d in &m.bus.unmodelled_dma {
        *unmodelled.entry(format!("ch{}", d.channel)).or_default() += 1;
    }
    (
        ok,
        json!({
            "mode": "trace",
            "status": if !divergence.is_null() { "diverged" } else if done >= steps { "matched" } else { "trace_ended" },
            "steps_compared": done,
            "steps_requested": steps,
            "operand_values_checked": operand_checks,
            "final_pc": h(m.cpu.pc),
            "io_values_injected": io_injected,
            "interrupts_injected": irq_injected,
            "unmodelled_dma": unmodelled,
            "divergence": divergence,
            "steps_per_second": (done as f64 / secs.max(1e-9)).round(),
            "compared": ["pc", "instruction word", "source operand registers before each instruction"],
            "oracle": {"backend": backend.binary, "backend_sha256": backend.identity().ok(), "flags": "-interpreter -debugger -trace"},
        }),
    )
}

/// Exact mode: GDB single-step, full register file after every instruction (slow).
pub fn run(root: &Path, bios_path: &Path, steps: u64, context: usize) -> (bool, Value) {
    let err = |code: &str, m: String| (false, json!({"error": {"code": code, "message": m}}));
    let backend = match Backend::from_project(root) {
        Ok(b) => b,
        Err(e) => return err(e.code(), e.to_string()),
    };
    let bios = match std::fs::read(bios_path) {
        Ok(b) => b,
        Err(e) => return err("io_error", format!("{}: {e}", bios_path.display())),
    };
    let dir = root.join(".psx/lockstep").join(format!("{}", std::process::id()));
    let mut g = match GdbSession::start(&backend, bios_path, &dir) {
        Ok(g) => g,
        Err(e) => return err(e.code(), e.to_string()),
    };
    let mut m = Machine::new(bios);
    let started = Instant::now();
    let mut history: VecDeque<Value> = VecDeque::with_capacity(context + 1);
    let (mut io_injected, mut irq_injected, mut io_loads) = (0u64, 0u64, 0u64);
    // I/O load waiting to retire: (register, physical address, width, issued at step).
    let mut pending_io: Option<(usize, u32, u64)> = None;
    let mut done = 0u64;
    let mut divergence = Value::Null;

    for n in 0..steps {
        let info = m.step();
        if let Err(e) = g.step() {
            return err(e.code(), e.to_string());
        }
        let r = match g.registers() {
            Ok(r) => r,
            Err(e) => return err(e.code(), e.to_string()),
        };
        history.push_back(json!({"step": n, "pc": h(info.pc), "asm": disasm(info.instruction, info.pc)}));
        if history.len() > context {
            history.pop_front();
        }

        // Oracle took an interrupt that the core (with no devices) could not see.
        let vectors = [0x8000_0080, 0xBFC0_0180];
        if vectors.contains(&r.pc) && !vectors.contains(&m.cpu.pc) && info.exception.is_none() {
            m.force_interrupt();
            irq_injected += 1;
        }

        // An I/O load issued last step has now retired: adopt the oracle's value.
        if let Some((rt, phys, _)) = pending_io.take() {
            if m.cpu.regs[rt] != r.gpr[rt] {
                m.cpu.regs[rt] = r.gpr[rt];
                m.bus.poke_io(phys & !3, Width::Word, r.gpr[rt]);
                io_injected += 1;
            }
        }
        if let Some((addr, rt)) = info.io_load {
            io_loads += 1;
            if rt != 0 {
                pending_io = Some((rt as usize, addr & 0x1FFF_FFFF, n));
            }
        }

        let d = diffs(&m, &r);
        // A difference in the register of an I/O load still in flight resolves next step.
        let d: Vec<_> =
            d.into_iter().filter(|(name, ..)| pending_io.is_none_or(|(rt, ..)| name != reg(rt as u32))).collect();
        done = n + 1;
        if !d.is_empty() {
            divergence = json!({
                "step": n,
                "differences": d.iter().map(|(name, core, oracle)| json!({"reg": name, "core": h(*core), "redux": h(*oracle)})).collect::<Vec<_>>(),
                "redux": {"sr": h(r.sr), "cause": h(r.cause), "epc_or_pc": h(r.pc)},
                "core": {"sr": h(m.cpu.cop0[12]), "cause": h(m.cpu.cop0[13]), "epc": h(m.cpu.cop0[14])},
                "recent": history.iter().cloned().collect::<Vec<_>>(),
            });
            break;
        }
    }
    let secs = started.elapsed().as_secs_f64();
    let ok = divergence.is_null();
    (
        ok,
        json!({
            "status": if ok { "matched" } else { "diverged" },
            "steps_compared": done,
            "steps_requested": steps,
            "final_pc": h(m.cpu.pc),
            "io_loads": io_loads,
            "io_values_injected": io_injected,
            "interrupts_injected": irq_injected,
            "divergence": divergence,
            "steps_per_second": (done as f64 / secs.max(1e-9)).round(),
            "compared": ["pc", "r1-r31", "hi", "lo"],
            "oracle": {"backend": backend.binary, "backend_sha256": backend.identity().ok(), "dir": g.dir},
        }),
    )
}
