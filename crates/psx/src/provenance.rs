//! Load provenance: which disc sectors (and files) CD DMA put where in RAM.
//!
//! Input is the CD-ROM controller and DMA3 register write log (`cd.jsonl`).
//! The decoder follows the documented protocol (psx-spx "CDROM Controller"):
//!   index register → selects the meaning of regs 1–3
//!   reg2/index0    → parameter FIFO
//!   reg1/index0    → command (02h Setloc, 06h ReadN, 1Bh ReadS, 0Eh Setmode, 09h Pause, ...)
//!   reg3/index0    → request register; bit 7 loads the next sector into the data FIFO
//!   DMA3 CHCR start → copies data-FIFO bytes to MADR
//!
//! Sector identity is established by **content**. The job snapshots each DMA3 payload
//! (first 64 bytes plus Adler-32) as soon as it lands. The decoder searches sectors near the
//! position implied by Setloc/ReadN and request counting, at plausible offsets inside
//! the sector FIFO, for an exact match. `lba_evidence` says which method identified a
//! transfer: `content` (exact payload match), `ambiguous` (several equally near
//! matches, e.g. zero-filled sectors) or `inferred` (no snapshot or no match; the
//! counted position is reported but is unproven).

use std::path::Path;

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use psx_media::Disc;
use psx_media::iso9660::FileSystem;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct LoadsArgs {
    /// A run directory recorded with --trace-cd.
    pub run_dir: PathBuf,
    /// The disc image the run used.
    #[arg(long)]
    pub disc: PathBuf,
    /// Include at most N individual runs (1..=1000); spans describe the full result.
    #[arg(long, default_value_t = crate::page::default_limit())]
    #[serde(default = "crate::page::default_limit")]
    pub limit: usize,
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub offset: usize,
    /// `.symbols.json` (verified against RAM) or `.sym` files from `psx code symbols`; repeatable.
    #[arg(long = "symbols")]
    #[serde(default)]
    pub symbols: Vec<PathBuf>,
}

impl Op for LoadsArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::result((|| -> crate::Result<Value> {
            crate::page::validate(self.limit).map_err(Error::invalid)?;
            let scopes = crate::load_symbols(&self.symbols)?;
            let mut v = report(&self.run_dir, &self.disc, &scopes).map_err(Error::malformed)?;
            if let Some(runs) = v["runs"].as_array_mut() {
                let total = runs.len();
                let rows: Vec<_> = runs.iter().skip(self.offset).take(self.limit).cloned().collect();
                *runs = rows;
                v["page"] = crate::page::metadata(total, self.offset, v["runs"].as_array().unwrap().len(), self.limit);
                v["runs_total"] = json!(total);
            }
            Ok(v)
        })())
    }
}

#[derive(Deserialize)]
struct CdEvent {
    frame: u32,
    cycle: u64,
    reg: Value,
    #[serde(default)]
    index: Option<u8>,
    #[serde(default)]
    value: Option<u32>,
    #[serde(default)]
    pc: u32,
    #[serde(default)]
    madr: Option<u32>,
    #[serde(default)]
    head: Option<String>,
    #[serde(default)]
    adler32: Option<u32>,
    /// Bytes actually copied (Redux clamps to what is left in the data FIFO).
    #[serde(default)]
    bytes: Option<u32>,
}

/// Disc sectors as the CD data FIFO presents them, cached.
struct Sectors<'a> {
    disc: &'a Disc,
    cache: std::collections::HashMap<(u32, u32), Option<std::rc::Rc<Vec<u8>>>>,
}

impl<'a> Sectors<'a> {
    fn fifo(&mut self, lba: u32, size: u32) -> Option<std::rc::Rc<Vec<u8>>> {
        let disc = self.disc;
        self.cache
            .entry((lba, size))
            .or_insert_with(|| {
                let (raw, mode) = disc.read_raw(lba).ok()?;
                if !mode.is_data() {
                    return None;
                }
                Some(std::rc::Rc::new(if size == 2340 && raw.len() == 2352 {
                    raw[12..].to_vec()
                } else {
                    disc.read_data(lba).ok()?.data[..2048].to_vec()
                }))
            })
            .clone()
    }
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in bytes {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect()
}

const WINDOW: i64 = 32;

/// Find `(lba, offset)` whose FIFO bytes match the snapshot; nearest to `centre` wins.
fn resolve(
    sectors: &mut Sectors,
    centre: u32,
    current: Option<(u32, u32)>,
    size: u32,
    bytes: u32,
    head: &[u8],
    sum: u32,
) -> (Option<(u32, u32)>, &'static str) {
    let mut offsets = vec![0u32, 12, 24];
    if let Some((_, used)) = current {
        if !offsets.contains(&used) {
            offsets.push(used);
        }
    }
    // Search outward from the counted position; the nearest distance with a match wins.
    let mut found: Vec<(i64, u32, u32)> = Vec::new();
    for d in 0..=WINDOW {
        let lbas: &[i64] = if d == 0 { &[centre as i64] } else { &[centre as i64 + d, centre as i64 - d] };
        for &lba in lbas {
            if lba < 0 {
                continue;
            }
            let Some(fifo) = sectors.fifo(lba as u32, size) else { continue };
            for &off in &offsets {
                let end = off as usize + bytes as usize;
                if end <= fifo.len() {
                    let slice = &fifo[off as usize..end];
                    if slice.starts_with(head) && adler32(slice) == sum {
                        found.push((d, lba as u32, off));
                    }
                }
            }
        }
        if !found.is_empty() {
            break;
        }
    }
    let Some(best) = found.iter().min_by_key(|f| f.0) else { return (None, "inferred") };
    let ties = found.iter().filter(|f| f.0 == best.0 && (f.1, f.2) != (best.1, best.2)).count();
    (Some((best.1, best.2)), if ties == 0 { "content" } else { "ambiguous" })
}

#[derive(Clone, Debug, Serialize)]
pub struct Load {
    pub frame: u32,
    pub cycle: u64,
    /// Inferred.
    pub lba: u32,
    /// Byte offset inside the sector's data-FIFO payload.
    pub sector_offset: u32,
    /// 2048 (data only) or 2340 (whole sector after sync; Setmode bit 5).
    pub sector_size: u32,
    pub ram: u32,
    pub bytes: u32,
    /// CPU PC that started the DMA3 transfer.
    pub pc: u32,
    /// How `lba`/`sector_offset` were established: content, ambiguous or inferred.
    pub lba_evidence: &'static str,
}

fn bcd(v: u32) -> u32 {
    (v >> 4) * 10 + (v & 15)
}

pub fn decode(events_path: &Path, disc: &Disc) -> std::io::Result<(Vec<Load>, Vec<String>)> {
    let text = std::fs::read_to_string(events_path)?;
    let mut sectors = Sectors { disc, cache: Default::default() };
    let mut loads: Vec<Load> = Vec::new();
    let mut notes = Vec::new();
    let mut params: Vec<u32> = Vec::new();
    let mut setloc: Option<u32> = None;
    let mut next_sector: Option<u32> = None;
    let mut sector_size = 2048;
    let mut current: Option<(u32, u32)> = None; // (lba, bytes already transferred from it)
    let (mut madr, mut bcr) = (0u32, 0u32);
    for line in text.lines() {
        let Ok(e) = serde_json::from_str::<CdEvent>(line) else { continue };
        let v = e.value.unwrap_or(0);
        match (e.reg.as_str(), e.reg.as_u64(), e.index) {
            (Some("dma3_data"), ..) => {
                // Snapshot of the transfer just recorded: resolve its sector by content.
                let (Some(l), Some(head), Some(sum)) = (loads.last_mut(), e.head.as_deref(), e.adler32) else {
                    continue;
                };
                if Some(l.ram) != e.madr {
                    continue;
                }
                if let Some(b) = e.bytes {
                    if let Some((_, used)) = current.as_mut() {
                        *used = used.saturating_sub(l.bytes) + b;
                    }
                    l.bytes = b;
                }
                let head = hex_bytes(head);
                let (hit, evidence) =
                    resolve(&mut sectors, l.lba, Some((l.lba, l.sector_offset)), l.sector_size, l.bytes, &head, sum);
                l.lba_evidence = evidence;
                if let Some((lba, off)) = hit {
                    l.lba = lba;
                    l.sector_offset = off;
                    current = Some((lba, off + l.bytes));
                    next_sector = Some(lba + 1);
                }
            }
            (Some("dma3_madr"), ..) => madr = v,
            (Some("dma3_bcr"), ..) => bcr = v,
            (Some("dma3_chcr"), ..) => {
                if v & 0x0100_0000 == 0 {
                    continue;
                }
                let words = (bcr & 0xFFFF).max(1) * (bcr >> 16).max(1);
                let bytes = words * 4;
                match current.as_mut() {
                    Some((lba, used)) => {
                        loads.push(Load {
                            frame: e.frame,
                            cycle: e.cycle,
                            lba: *lba,
                            sector_offset: *used,
                            sector_size,
                            ram: madr,
                            bytes,
                            pc: e.pc,
                            lba_evidence: "inferred",
                        });
                        *used += bytes;
                    }
                    None => notes.push(format!("frame {}: DMA3 of {bytes} bytes with no requested sector", e.frame)),
                }
            }
            (_, Some(2), Some(0)) => params.push(v),
            (_, Some(1), Some(0)) => {
                match v {
                    0x02 if params.len() >= 3 => {
                        let msf = (bcd(params[0]) * 60 + bcd(params[1])) * 75 + bcd(params[2]);
                        setloc = Some(msf.saturating_sub(150));
                    }
                    0x06 | 0x1B => {
                        if let Some(l) = setloc.take() {
                            next_sector = Some(l);
                        } else if next_sector.is_none() {
                            notes.push(format!("frame {}: read without a known position", e.frame));
                        }
                    }
                    0x0E if !params.is_empty() => sector_size = if params[0] & 0x20 != 0 { 2340 } else { 2048 },
                    0x15 | 0x16 => {
                        if let Some(l) = setloc {
                            next_sector = Some(l);
                        }
                    }
                    _ => {}
                }
                params.clear();
            }
            (_, Some(3), Some(0)) if v & 0x80 != 0 => {
                if let Some(n) = next_sector.as_mut() {
                    current = Some((*n, 0));
                    *n += 1;
                }
            }
            _ => {}
        }
    }
    Ok((loads, notes))
}

/// A contiguous run of sectors copied to contiguous RAM.
#[derive(Clone, Debug, Serialize)]
pub struct Run {
    pub first_frame: u32,
    pub last_frame: u32,
    pub lba: u32,
    pub sectors: u32,
    pub ram: u32,
    pub bytes: u32,
    pub sector_size: u32,
    /// Offset inside each sector's FIFO payload where the copied bytes start.
    pub first_offset: u32,
    pub pcs: Vec<u32>,
    /// Weakest evidence among the run's transfers.
    pub lba_evidence: &'static str,
    pub file: Option<String>,
    pub file_offset: Option<u64>,
    /// Compared with the final RAM dump: true = all bytes match the disc.
    pub verified: Option<bool>,
    pub matching_bytes: Option<u32>,
}

pub fn runs(loads: &[Load]) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for l in loads {
        if let Some(r) = out.last_mut() {
            let next_ram = r.ram.wrapping_add(r.bytes);
            let same_sector = l.lba == r.lba + r.sectors - 1 && l.ram == next_ram;
            let rank = |e: &str| match e {
                "native" => 3,
                "content" => 2,
                "ambiguous" => 1,
                _ => 0,
            };
            let next_sector = l.lba == r.lba + r.sectors && l.sector_offset == r.first_offset && l.ram == next_ram;
            if l.sector_size == r.sector_size && (same_sector || next_sector) {
                if next_sector {
                    r.sectors += 1;
                }
                r.bytes += l.bytes;
                r.last_frame = l.frame;
                if rank(l.lba_evidence) < rank(r.lba_evidence) {
                    r.lba_evidence = l.lba_evidence;
                }
                if !r.pcs.contains(&l.pc) {
                    r.pcs.push(l.pc);
                }
                continue;
            }
        }
        out.push(Run {
            first_frame: l.frame,
            last_frame: l.frame,
            lba: l.lba,
            sectors: 1,
            ram: l.ram,
            bytes: l.bytes,
            sector_size: l.sector_size,
            first_offset: l.sector_offset,
            pcs: vec![l.pc],
            lba_evidence: l.lba_evidence,
            file: None,
            file_offset: None,
            verified: None,
            matching_bytes: None,
        });
    }
    out
}

/// Expected FIFO bytes for `run` from the disc image.
fn expected(disc: &Disc, run: &Run) -> Option<Vec<u8>> {
    let mut data = Vec::new();
    for i in 0..run.sectors {
        let (raw, _) = disc.read_raw(run.lba + i).ok()?;
        let fifo = if run.sector_size == 2340 {
            raw.get(12..12 + 2340)?.to_vec()
        } else {
            disc.read_data(run.lba + i).ok()?.data[..2048].to_vec()
        };
        data.extend_from_slice(fifo.get(run.first_offset as usize..)?);
    }
    Some(data)
}

pub fn annotate(runs: &mut [Run], disc: &Disc, fs: Option<&FileSystem>, ram: Option<&[u8]>) {
    for r in runs.iter_mut() {
        if let Some(e) = fs.and_then(|fs| fs.owner_of(r.lba)) {
            r.file = Some(e.path.clone());
            // In 2340-byte mode user data starts 12 bytes into the FIFO payload.
            let data_off = if r.sector_size == 2340 { r.first_offset.saturating_sub(12) } else { r.first_offset };
            r.file_offset = Some((r.lba - e.lba) as u64 * 2048 + data_off as u64);
        }
        let (Some(ram), Some(want)) = (ram, expected(disc, r)) else { continue };
        let start = (r.ram & 0x1F_FFFF) as usize;
        let n = (r.bytes as usize).min(want.len()).min(ram.len().saturating_sub(start));
        let matching = (0..n).filter(|&i| ram[start + i] == want[i]).count() as u32;
        r.matching_bytes = Some(matching);
        r.verified = Some(matching == r.bytes && n == r.bytes as usize);
    }
}

/// Name a PC only where its executable's code is resident: the bytes match the
/// final RAM, and this run did not load that executable over the PC *after* the
/// event (which would mean other code was there at the time).
fn named(symbols: &[crate::ScopedSymbols], runs: &[Run], pc: u32, frame: u32, ram: Option<&[u8]>) -> Value {
    let Some((name, check, exe)) = crate::name_pc(symbols, pc, ram) else { return Value::Null };
    if check == crate::SymbolCheck::BodyMatch {
        // A final snapshot cannot establish which overlay occupied a PC when an
        // earlier load event occurred. Expose a candidate without labelling it.
        return json!({"candidate_name":name,"symbols":exe,"check":"final_ram_only",
            "diagnostic":"Function body matches final RAM; historical event residency is unknown."});
    }
    let pc_phys = pc & 0x1F_FFFF;
    let loaded_later = runs.iter().any(|r| {
        r.file.as_deref().is_some_and(|f| f.trim_start_matches('/').eq_ignore_ascii_case(&exe))
            && r.first_frame > frame
            && (r.ram & 0x1F_FFFF) <= pc_phys
            && pc_phys < (r.ram & 0x1F_FFFF) + r.bytes
    });
    if loaded_later {
        return Value::Null;
    }
    json!({"name": name, "exe": exe, "check": check.as_str()})
}

/// Runs grouped by (file, initiating PC) in order of first appearance: the
/// readable "what loaded what, where, when" view.
fn spans(runs: &[Run], symbols: &[crate::ScopedSymbols], ram: Option<&[u8]>) -> Vec<Value> {
    struct Span {
        file: Option<String>,
        pc: u32,
        first_frame: u32,
        last_frame: u32,
        file_lo: Option<u64>,
        file_hi: Option<u64>,
        ram: Vec<(u32, u32)>,
        bytes: u64,
        runs: u32,
        content: u32,
        ambiguous: u32,
        inferred: u32,
        ram_intact_bytes: u64,
    }
    let mut out: Vec<Span> = Vec::new();
    for r in runs {
        let pc = r.pcs.first().copied().unwrap_or(0);
        let i = match out.iter().position(|s| s.file == r.file && s.pc == pc) {
            Some(i) => i,
            None => {
                out.push(Span {
                    file: r.file.clone(),
                    pc,
                    first_frame: r.first_frame,
                    last_frame: r.last_frame,
                    file_lo: None,
                    file_hi: None,
                    ram: Vec::new(),
                    bytes: 0,
                    runs: 0,
                    content: 0,
                    ambiguous: 0,
                    inferred: 0,
                    ram_intact_bytes: 0,
                });
                out.len() - 1
            }
        };
        let s = &mut out[i];
        s.last_frame = s.last_frame.max(r.last_frame);
        if let Some(off) = r.file_offset {
            let end = off + r.bytes as u64;
            s.file_lo = Some(s.file_lo.map_or(off, |v| v.min(off)));
            s.file_hi = Some(s.file_hi.map_or(end, |v| v.max(end)));
        }
        let (lo, hi) = (r.ram, r.ram.wrapping_add(r.bytes));
        match s.ram.iter_mut().find(|(a, b)| lo <= *b && hi >= *a) {
            Some(range) => *range = (range.0.min(lo), range.1.max(hi)),
            None => s.ram.push((lo, hi)),
        }
        s.bytes += r.bytes as u64;
        s.runs += 1;
        match r.lba_evidence {
            "native" | "content" => s.content += 1,
            "ambiguous" => s.ambiguous += 1,
            _ => s.inferred += 1,
        }
        s.ram_intact_bytes += r.matching_bytes.unwrap_or(0) as u64;
    }
    let h = |v: u32| format!("0x{v:08X}");
    out.into_iter()
        .map(|s| {
            json!({
                "file": s.file,
                "initiating_pc": h(s.pc),
                "initiating_function": named(symbols, runs, s.pc, s.first_frame, ram),
                "frames": [s.first_frame, s.last_frame],
                "file_bytes": s.file_lo.zip(s.file_hi).map(|(a, b)| [a, b]),
                "ram": s.ram.iter().map(|(a, b)| [h(*a), h(*b)]).collect::<Vec<_>>(),
                "bytes": s.bytes,
                "runs": s.runs,
                "evidence": {"content": s.content, "ambiguous": s.ambiguous, "inferred": s.inferred},
                "still_in_ram_bytes": s.ram_intact_bytes,
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct NativeTransfer {
    frame: u64,
    cycle: u64,
    lba: u32,
    fifo_offset: u32,
    bytes: u32,
    sector_size: u32,
    ram: u32,
    pc: u32,
}

/// Exact transfers recorded by the native core (`cd_native.jsonl`).
fn native_loads(path: &Path) -> std::io::Result<Vec<Load>> {
    let text = std::fs::read_to_string(path)?;
    Ok(text
        .lines()
        .filter_map(|l| serde_json::from_str::<NativeTransfer>(l).ok())
        .map(|t| Load {
            frame: t.frame as u32,
            cycle: t.cycle,
            lba: t.lba,
            sector_offset: t.fifo_offset,
            sector_size: t.sector_size,
            ram: t.ram,
            bytes: t.bytes,
            pc: t.pc,
            lba_evidence: "native",
        })
        .collect())
}

pub fn report(run_dir: &Path, disc_path: &Path, symbols: &[crate::ScopedSymbols]) -> Result<Value, String> {
    let disc = Disc::open(disc_path).map_err(|e| e.to_string())?;
    let native = run_dir.join("cd_native.jsonl");
    let (loads, notes, method) = if native.exists() {
        let loads = native_loads(&native).map_err(|e| format!("cd_native.jsonl: {e}"))?;
        (
            loads,
            Vec::new(),
            "native core: the drive model recorded each DMA's sector and FIFO offset; runs also compared with final RAM",
        )
    } else {
        let (l, n) = decode(&run_dir.join("cd.jsonl"), &disc).map_err(|e| format!("cd.jsonl: {e}"))?;
        (
            l,
            n,
            "CD register protocol; each DMA3 payload matched to disc sectors by content; runs also compared with final RAM",
        )
    };
    let fs = FileSystem::read(&disc).ok();
    let ram = std::fs::read(run_dir.join("ram.bin")).ok();
    let mut runs = runs(&loads);
    annotate(&mut runs, &disc, fs.as_ref(), ram.as_deref());
    let mut by_file: std::collections::BTreeMap<String, (u64, u32)> = Default::default();
    for r in &runs {
        let e = by_file.entry(r.file.clone().unwrap_or_else(|| "(no file)".into())).or_default();
        e.0 += r.bytes as u64;
        e.1 += 1;
    }
    Ok(json!({
        "method": method,
        "transfers": loads.len(),
        "evidence": {
            "native": loads.iter().filter(|l| l.lba_evidence == "native").count(),
            "content": loads.iter().filter(|l| l.lba_evidence == "content").count(),
            "ambiguous": loads.iter().filter(|l| l.lba_evidence == "ambiguous").count(),
            "inferred": loads.iter().filter(|l| l.lba_evidence == "inferred").count(),
        },
        "spans": spans(&runs, symbols, ram.as_deref()),
        "runs": runs,
        "files": by_file.into_iter().map(|(f, (bytes, runs))| json!({"file": f, "bytes": bytes, "runs": runs})).collect::<Vec<_>>(),
        "notes": notes,
    }))
}
