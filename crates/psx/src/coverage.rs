//! `psx code coverage`: what a corpus of traced runs executed, per function of the
//! inventory: its blocks executed and not, and the runs that reached it. So
//! "exercised" is a query, and the blocks never executed are a list to
//! classify (dead, debug only, unreachable on this disc, or needing a
//! recording).
//!
//! Each run's executed.bin (run --trace) gives every RAM word executed with
//! the instruction it executed as. A word is credited to the executable or
//! overlay whose code holds that instruction there, so code loaded over
//! other code is told apart; where several hold it, to the one holding most
//! of the run of consecutive words it was executed in. Code two functions
//! hold (one's chunk is another's body) is credited to both, and a run
//! reaches each. Executed words no image holds (code changed at runtime, an
//! overlay no snapshot held) and executed words in no function (a function
//! the inventory missed) are listed. The BIOS shell's code in RAM (the inventory's bios_code) is
//! credited but counted apart.
//!
//! Each run's branches.bin gives how often each conditional branch was taken
//! and fell through; a branch's ways are credited where its word was, so
//! each function lists the ways of its branches no run took.
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CoverageArgs {
    /// Traced runs (run --trace), or scenario recordings (.jsonl: the coverage kept beside each).
    #[arg(required_unless_present = "check")]
    #[serde(default)]
    pub runs: Vec<PathBuf>,
    /// Run the recipes coverage kept again; compare what each executed.
    #[arg(long, conflicts_with = "runs")]
    #[serde(default)]
    pub check: bool,
    /// Inventory (default inventory's out).
    #[arg(long)]
    #[serde(default)]
    pub inventory: Option<PathBuf>,
    /// Output (default beside the inventory).
    #[arg(long)]
    #[serde(default)]
    pub out: Option<PathBuf>,
}

/// `code coverage diff`: what traced runs reach that other traced runs do
/// not, by function, block and branch way.
#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CoverageDiffArgs {
    /// Traced runs (run --trace) or scenario recordings (.jsonl) that do the thing whose code you want.
    #[arg(required = true)]
    pub runs: Vec<PathBuf>,
    /// Traced runs or recordings that do not: what they reach is left out. Repeatable, or several after one flag.
    #[arg(long, required = true, num_args = 1..)]
    pub against: Vec<PathBuf>,
    /// Inventory (default inventory's out).
    #[arg(long)]
    #[serde(default)]
    pub inventory: Option<PathBuf>,
    /// Output (default coverage-diff.json beside the inventory).
    #[arg(long)]
    #[serde(default)]
    pub out: Option<PathBuf>,
}

/// Below this is the BIOS kernel's.
const GAME_RAM: u32 = 0x8001_0000;

struct Function {
    entry: u32,
    end: u32,
    /// The stretches of RAM it is, in order: [entry, end) unless other
    /// code lies between its parts.
    chunks: Vec<(u32, u32)>,
    blocks: Vec<u32>,
    /// The sha256 of its bytes, for code no executable holds.
    sha: Option<String>,
}

impl Function {
    fn chunk(&self, addr: u32) -> Option<(u32, u32)> {
        self.chunks.iter().copied().find(|&(s, e)| addr >= s && addr < e)
    }

    /// Its blocks as `[start, end)`: a block ends at the next, or at the
    /// end of its chunk.
    fn block_spans(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.blocks.iter().enumerate().map(|(bi, &s)| {
            let chunk_end = self.chunk(s).map_or(self.end, |c| c.1);
            (s, self.blocks.get(bi + 1).copied().unwrap_or(self.end).min(chunk_end))
        })
    }

    fn words(&self) -> impl Iterator<Item = u32> + '_ {
        self.chunks.iter().flat_map(|&(s, e)| (s..e).step_by(4))
    }
}

/// An executable or overlay: its code at its address and its functions.
struct Image {
    kind: &'static str,
    name: String,
    base: u32,
    /// Its code from base, word by word: None between a function's chunks.
    words: Vec<Option<u32>>,
    functions: Vec<Function>,
    /// Every function's chunks, by start: (start, end, function).
    chunks: Vec<(u32, u32, usize)>,
    /// The longest chunk's length.
    longest: u32,
    /// Executions of each word, over the corpus.
    count: Vec<u64>,
    /// The run (plus one) that a word was last credited in, so a branch
    /// that run took is credited where its word was.
    credited_in: Vec<u32>,
    /// Each conditional branch's ways over the corpus: 1 taken, 2 fell
    /// through.
    ways: Vec<u8>,
    /// The runs that executed each function.
    runs: Vec<BTreeSet<usize>>,
}

impl Image {
    fn word(&self, addr: u32) -> Option<u32> {
        let k = addr.checked_sub(self.base)? as usize / 4;
        self.words.get(k).copied().flatten()
    }

    fn function(&self, addr: u32) -> Option<usize> {
        let i = self.chunks.partition_point(|c| c.0 <= addr).checked_sub(1)?;
        let (_, end, f) = self.chunks[i];
        (addr < end).then_some(f)
    }

    /// Every function with a chunk holding addr: one's chunk can hold
    /// another's code.
    fn functions_at(&self, addr: u32) -> Vec<usize> {
        let i = self.chunks.partition_point(|c| c.0 <= addr);
        let mut out: Vec<usize> = self.chunks[..i]
            .iter()
            .rev()
            .take_while(|c| c.0.saturating_add(self.longest) > addr)
            .filter(|c| addr < c.1)
            .map(|c| c.2)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

fn parse_hex(v: &Value) -> Option<u32> {
    crate::commands::addr_value(v)
}

fn functions(v: &Value) -> crate::Result<Vec<Function>> {
    let mut out = Vec::new();
    for f in v.as_array().into_iter().flatten() {
        let (Some(entry), Some(end)) = (parse_hex(&f["entry"]), parse_hex(&f["end"])) else {
            return Err(Error::malformed("inventory function without entry and end"));
        };
        let mut blocks: Vec<u32> = f["block_starts"].as_array().into_iter().flatten().filter_map(parse_hex).collect();
        if blocks.is_empty() {
            return Err(Error::malformed(
                "the inventory has no block_starts (made by an older psx); take the inventory again",
            ));
        }
        blocks.sort_unstable();
        let chunks = match f["chunks"].as_array() {
            Some(cs) => cs
                .iter()
                .map(|c| parse_hex(&c[0]).zip(parse_hex(&c[1])))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| Error::malformed("inventory function chunk without start and end"))?,
            None => vec![(entry, end)],
        };
        out.push(Function { entry, end, chunks, blocks, sha: f["sha256"].as_str().map(String::from) });
    }
    out.sort_by_key(|f| f.entry);
    Ok(out)
}

fn to_words(bytes: &[u8]) -> Vec<u32> {
    bytes.as_chunks::<4>().0.iter().map(|w| u32::from_le_bytes(*w)).collect()
}

/// A function's code from its entry, from the bytes the inventory kept:
/// its chunks' bytes in order, placed each at its own address. None when
/// they are not as long as its chunks.
fn placed(f: &Function, bytes: &[u8]) -> Option<Vec<Option<u32>>> {
    let words = to_words(bytes);
    let mut out = vec![None; ((f.end - f.entry) / 4) as usize];
    let mut next = words.iter();
    for a in f.words() {
        *out.get_mut(((a - f.entry) / 4) as usize)? = Some(*next.next()?);
    }
    next.next().is_none().then_some(out)
}

/// coverage.json's format: 4 adds each function's branch ways; 3 credits code two functions of an overlay hold
/// to both; 2 read a function in chunks at its chunks' addresses (1 read
/// the bytes of all its chunks as one stretch from its entry).
pub const FORMAT: u64 = 4;

/// The inventory's executables, read again from its disc, and each function
/// of its overlays and of the BIOS's code in RAM, from the bytes inventory
/// kept.
fn images(root: &Path, inv: &Value) -> crate::Result<Vec<Image>> {
    if inv["inventory"].as_u64() < Some(2) {
        return Err(Error::invalid("the inventory was made by an older psx; take the inventory again"));
    }
    use psx_media::disc::Disc;
    use psx_media::iso9660::FileSystem;
    let mut out = Vec::new();
    let image = |kind, name: String, base, words: Vec<Option<u32>>, functions: Vec<Function>| {
        let n = words.len();
        let f = functions.len();
        let mut chunks: Vec<(u32, u32, usize)> =
            functions.iter().enumerate().flat_map(|(i, f)| f.chunks.iter().map(move |&(s, e)| (s, e, i))).collect();
        chunks.sort_unstable();
        let longest = chunks.iter().map(|c| c.1 - c.0).max().unwrap_or(0);
        Image {
            kind,
            name,
            base,
            words,
            functions,
            chunks,
            longest,
            count: vec![0; n],
            credited_in: vec![0; n],
            ways: vec![0; n],
            runs: vec![BTreeSet::new(); f],
        }
    };
    let exes = inv["executables"].as_array().cloned().unwrap_or_default();
    if !exes.is_empty() {
        let media = inv["media"].as_str().ok_or_else(|| Error::malformed("inventory without media"))?;
        let disc = Disc::open(Path::new(media))?;
        let fs = FileSystem::read(&disc)?;
        for e in &exes {
            let path = e["path"].as_str().unwrap_or_default();
            let entry = fs.find(path).ok_or_else(|| Error::invalid(format!("{path}: not on {media}")))?;
            let bytes = fs.read_file(&disc, entry)?;
            let exe = psx_media::PsxExe::parse(&bytes)?;
            let base = 0x8000_0000 | (exe.text_addr & 0x1F_FFFF);
            out.push(image(
                "executable",
                path.into(),
                base,
                to_words(exe.payload(&bytes)).into_iter().map(Some).collect(),
                functions(&e["functions"])?,
            ));
        }
    }
    let overlays = inv["overlays"].as_array().into_iter().flatten().map(|o| ("overlay", o));
    let bios = inv["bios_code"].as_array().into_iter().flatten().map(|o| ("bios", o));
    for (kind, o) in overlays.chain(bios) {
        let name = o["name"].as_str().unwrap_or_default();
        for f in functions(&o["functions"])? {
            let sha = f.sha.clone().unwrap_or_default();
            let words = std::fs::read(crate::inventory::overlay_bytes(root, &sha))
                .ok()
                .filter(|b| crate::stamp::sha256(b) == sha)
                .and_then(|b| placed(&f, &b))
                .ok_or_else(|| {
                    Error::invalid(format!(
                        "{name}: the bytes of its function at {:08X} are not in .psx/overlays; take the inventory again",
                        f.entry
                    ))
                })?;
            out.push(image(kind, name.into(), f.entry, words, vec![f]));
        }
    }
    Ok(out)
}

/// A run's executed.bin: `(address, instruction, executions)`.
pub(crate) fn executed(run: &Path) -> crate::Result<Vec<(u32, u32, u64)>> {
    let path = run.join("executed.bin");
    let bytes = std::fs::read(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::invalid(format!("{}: no executed.bin; record it with run --trace", run.display()))
        } else {
            Error::io(&path, e)
        }
    })?;
    let u = |r: &[u8], a: usize| u32::from_le_bytes(r[a..a + 4].try_into().unwrap());
    let n = |r: &[u8], a: usize| u64::from_le_bytes(r[a..a + 8].try_into().unwrap());
    if let Some(body) = bytes.strip_prefix(b"PSXEXEC1") {
        return Ok(body.as_chunks::<16>().0.iter().map(|r| (u(r, 0), u(r, 4), n(r, 8))).collect());
    }
    // Each record names the code page copy it ran from too (and from v4,
    // the moment it first ran and how); words run from several copies are
    // summed here.
    let (body, size) = match (bytes.strip_prefix(b"PSXEXEC2"), bytes.strip_prefix(b"PSXEXEC4")) {
        (Some(b), _) => (b, 20),
        (_, Some(b)) => (b, 28),
        _ => return Err(Error::malformed(format!("{}: not an executed.bin", path.display()))),
    };
    let mut words: BTreeMap<(u32, u32), u64> = BTreeMap::new();
    for r in body.chunks_exact(size) {
        *words.entry((u(r, 4), u(r, 8))).or_default() += n(r, size - 8);
    }
    Ok(words.into_iter().map(|((a, w), c)| (a, w, c)).collect())
}

/// Each executed RAM word of a trace with its instruction and executions,
/// summed over the code page copies it ran from.
pub(crate) fn summed(t: &psx_core::trace::Tracer) -> Vec<(u32, u32, u64)> {
    let mut words: BTreeMap<(u32, u32), u64> = BTreeMap::new();
    for (_, addr, word, r) in t.executed() {
        *words.entry((addr, word)).or_default() += r.count;
    }
    words.into_iter().map(|((a, w), n)| (a, w, n)).collect()
}

/// The coverage a recording keeps beside it (`<name>.coverage.json`).
pub(crate) const KEPT_FORMAT: &str = "psx-recording-coverage";

/// What a recording keeps of what the original ran, credited to the
/// inventory's images as coverage credits a run: per executable or overlay
/// (by name), the words run as `[start, end, executions]` ranges, the ways
/// its conditional branches went, and the sha256 of the instructions run (in
/// address order), which a later inventory's code must match; and the words
/// no image holds, as addresses. No instruction is kept, so it can be
/// committed with the recording.
fn kept_doc(images: &mut [Image], words: &[(u32, u32, u64)], branches: &[Branch]) -> Value {
    let (missing, _, _) = credit(images, 0, words);
    credit_branches(images, 0, branches);
    let mut groups: Vec<(&str, &str)> = images.iter().map(|i| (i.kind, i.name.as_str())).collect();
    groups.sort_unstable();
    groups.dedup();
    let mut out = Vec::new();
    for (kind, name) in groups {
        let members: Vec<&Image> = images.iter().filter(|i| i.kind == kind && i.name == name).collect();
        let mut ran: BTreeMap<u32, (u64, u32)> = BTreeMap::new();
        let mut ways: BTreeMap<u32, u8> = BTreeMap::new();
        for img in &members {
            for (k, &n) in img.count.iter().enumerate().filter(|(_, n)| **n > 0) {
                let a = img.base + 4 * k as u32;
                ran.entry(a).or_insert((n, img.words[k].unwrap_or(0)));
                if img.ways[k] != 0 {
                    *ways.entry(a).or_default() |= img.ways[k];
                }
            }
        }
        if ran.is_empty() {
            continue;
        }
        let code: Vec<u8> = ran.values().flat_map(|&(_, w)| w.to_le_bytes()).collect();
        let mut executed: Vec<(u32, u32, u64)> = Vec::new();
        for (&a, &(n, _)) in &ran {
            match executed.last_mut() {
                Some(r) if r.1 == a && r.2 == n => r.1 = a + 4,
                _ => executed.push((a, a + 4, n)),
            }
        }
        let way = |w: u8| match w {
            1 => "taken",
            2 => "fell through",
            _ => "both",
        };
        out.push(json!({
            "kind": kind, "name": name, "words": ran.len(), "sha256": crate::stamp::sha256(&code),
            "executed": executed.iter().map(|&(s, e, n)| json!([hex(s), hex(e), n])).collect::<Vec<_>>(),
            "branches": ways.iter().map(|(&a, &w)| json!([hex(a), way(w)])).collect::<Vec<_>>(),
        }));
    }
    let no_image: Vec<Value> =
        spans(missing.iter().map(|m| m.0)).iter().map(|&(s, e)| json!([hex(s), hex(e)])).collect();
    json!({"format": KEPT_FORMAT, "format_version": 1, "images": out, "in_no_image": no_image})
}

/// What a recording keeps of `t`'s run, against the inventory `inv`; and
/// the code it ran that the inventory knows only from the code that ran
/// (overlays bounded_by found_code) or not at all, by name.
pub(crate) fn keep(root: &Path, inv: &Value, t: &psx_core::trace::Tracer) -> crate::Result<(Value, Vec<String>)> {
    let mut images = images(root, inv)?;
    let mut doc = kept_doc(&mut images, &summed(t), &t.branches());
    doc["inventory"] = json!({"made": inv["made"]});
    let mut partial: Vec<String> = doc["images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|g| g["kind"] == "overlay" && found_code(inv, g["name"].as_str().unwrap_or_default()))
        .map(|g| format!("{} (known only from the code that ran)", g["name"].as_str().unwrap_or_default()))
        .collect();
    let lost: u32 = doc["in_no_image"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| Some((addr_of(&r[1])? - addr_of(&r[0])?) / 4))
        .sum();
    if lost > 0 {
        partial.push(format!("{lost} words no executable or overlay of the inventory holds"));
    }
    Ok((doc, partial))
}

/// Whether the inventory's overlay `name` is read only around the code
/// found in it (no file holds it).
fn found_code(inv: &Value, name: &str) -> bool {
    inv["overlays"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|o| o["name"].as_str() == Some(name) && o["bounded_by"] == "found_code")
}

fn addr_of(v: &Value) -> Option<u32> {
    crate::commands::addr_value(v)
}

/// Credits a recording's kept coverage `doc` to `images` as run `run`, as
/// coverage credits a traced run. Returns the words no image held, per
/// executable or overlay the words credited, and the ones whose code the
/// inventory no longer holds as the recording ran it.
#[allow(clippy::type_complexity)]
fn credit_kept(images: &mut [Image], run: usize, doc: &Value) -> (Vec<u32>, BTreeMap<String, u64>, Vec<String>) {
    let mut per = BTreeMap::new();
    let mut stale = Vec::new();
    for g in doc["images"].as_array().into_iter().flatten() {
        let name = g["name"].as_str().unwrap_or_default();
        let members: Vec<usize> =
            (0..images.len()).filter(|&i| g["kind"] == images[i].kind && images[i].name == name).collect();
        let mut ran: Vec<(u32, u64)> = Vec::new();
        for r in g["executed"].as_array().into_iter().flatten() {
            let (Some(s), Some(e), Some(n)) = (addr_of(&r[0]), addr_of(&r[1]), r[2].as_u64()) else { continue };
            ran.extend((s..e).step_by(4).map(|a| (a, n)));
        }
        let code: Option<Vec<u8>> = ran
            .iter()
            .map(|&(a, _)| members.iter().find_map(|&i| images[i].word(a)).map(u32::to_le_bytes))
            .collect::<Option<Vec<_>>>()
            .map(|w| w.concat());
        if code.map(|c| crate::stamp::sha256(&c)).as_deref() != g["sha256"].as_str() {
            stale.push(name.to_string());
            continue;
        }
        for &(a, n) in &ran {
            for &i in &members {
                let img = &mut images[i];
                if img.word(a).is_none() {
                    continue;
                }
                let at = ((a - img.base) / 4) as usize;
                img.count[at] += n;
                img.credited_in[at] = run as u32 + 1;
                for f in img.functions_at(a) {
                    img.runs[f].insert(run);
                }
            }
        }
        for b in g["branches"].as_array().into_iter().flatten() {
            let Some(a) = addr_of(&b[0]) else { continue };
            let ways = match b[1].as_str() {
                Some("taken") => 1,
                Some("fell through") => 2,
                _ => 3,
            };
            for &i in &members {
                let img = &mut images[i];
                if img.word(a).is_none() {
                    continue;
                }
                img.ways[((a - img.base) / 4) as usize] |= ways;
            }
        }
        per.insert(name.to_string(), ran.len() as u64);
    }
    let mut missing = Vec::new();
    for r in doc["in_no_image"].as_array().into_iter().flatten() {
        if let (Some(s), Some(e)) = (addr_of(&r[0]), addr_of(&r[1])) {
            missing.extend((s..e).step_by(4));
        }
    }
    (missing, per, stale)
}

/// branches.bin: each conditional branch run in RAM, its address, its
/// instruction, and the times it was taken and fell through.
pub(crate) fn branches_bin(t: &psx_core::trace::Tracer) -> Vec<u8> {
    let mut branches = b"PSXBRAN1".to_vec();
    for (addr, word, taken, fell) in t.branches() {
        branches.extend(addr.to_le_bytes());
        branches.extend(word.to_le_bytes());
        branches.extend(taken.to_le_bytes());
        branches.extend(fell.to_le_bytes());
    }
    branches
}

/// A conditional branch a run ran: `(address, instruction, taken, fell
/// through)`.
type Branch = (u32, u32, u64, u64);

/// A run's branches.bin; None for a run traced before psx recorded
/// branches.
fn branches(run: &Path) -> crate::Result<Option<Vec<Branch>>> {
    let path = run.join("branches.bin");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io(&path, e)),
    };
    let Some(body) = bytes.strip_prefix(b"PSXBRAN1") else {
        return Err(Error::malformed(format!("{}: not a branches.bin", path.display())));
    };
    let u = |r: &[u8], a: usize| u32::from_le_bytes(r[a..a + 4].try_into().unwrap());
    let n = |r: &[u8], a: usize| u64::from_le_bytes(r[a..a + 8].try_into().unwrap());
    Ok(Some(body.as_chunks::<24>().0.iter().map(|r| (u(r, 0), u(r, 4), n(r, 8), n(r, 16))).collect()))
}

/// Whether `i` is a conditional branch: one that can both be taken and fall
/// through (`beq zero, zero` always jumps, `bne zero, zero` never does).
fn conditional(i: u32) -> bool {
    let (rs, rt) = ((i >> 21) & 31, (i >> 16) & 31);
    match i >> 26 {
        0x01 => rs != 0 && matches!(rt, 0x00 | 0x01 | 0x10 | 0x11),
        0x04 | 0x05 => rs != rt,
        0x06 | 0x07 => rs != 0,
        _ => false,
    }
}

/// Credits one run's branch ways to the images its executed words were
/// credited to (`credit` for the same run first).
fn credit_branches(images: &mut [Image], run: usize, branches: &[Branch]) {
    for &(a, i, taken, fell) in branches.iter().filter(|b| b.0 >= GAME_RAM && conditional(b.1)) {
        let ways = (taken > 0) as u8 | ((fell > 0) as u8) << 1;
        for img in images.iter_mut().filter(|img| img.word(a) == Some(i)) {
            let at = ((a - img.base) / 4) as usize;
            if img.credited_in[at] == run as u32 + 1 {
                img.ways[at] |= ways;
            }
        }
    }
}

/// A traced run's call targets (calls.json), with the run frames of the
/// first and last call.
fn run_calls(run: &Path) -> Vec<(u32, i64, i64)> {
    let Ok(text) = std::fs::read_to_string(run.join("calls.json")) else { return Vec::new() };
    let v: Value = serde_json::from_str(&text).unwrap_or_default();
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let a = 0x8000_0000 | (crate::commands::addr_value(&c["address"])? & 0x1F_FFFF);
            Some((a, c["first_run_frame"].as_i64()?, c["last_run_frame"].as_i64()?))
        })
        .collect()
}

/// Consecutive addresses as `[start, end)` spans.
fn spans(addrs: impl IntoIterator<Item = u32>) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for a in addrs {
        match out.last_mut() {
            Some(s) if s.1 == a => s.1 = a + 4,
            _ => out.push((a, a + 4)),
        }
    }
    out
}

use psx_media::memmap::hex;

/// What credit gives: the words no image holds, per image the words
/// credited, and per executable or overlay (by name) the words.
type Credit = (Vec<(u32, u32)>, Vec<u64>, BTreeMap<String, u64>);

/// Credits one run's executed words to images; returns the words no image
/// holds, per image how many words were credited, and per executable or
/// overlay (by name) how many words. Code loaded over
/// other code shares nops and returns with what it replaced, so an image is
/// credited a word only when it holds a stretch of what ran there: four
/// consecutive executed words, the whole run of consecutive executed words
/// around it, or whole basic blocks of one of its functions; or a shorter
/// stretch where the words run beside it are other images' (code loaded
/// over it ran there). Of several, the one with a function there, then the
/// longest stretch, then the one the run otherwise ran.
fn credit(images: &mut [Image], run: usize, words: &[(u32, u32, u64)]) -> Credit {
    let words: Vec<(u32, u32, u64)> = words.iter().copied().filter(|r| r.0 >= GAME_RAM).collect();
    let holders: Vec<Vec<usize>> =
        words.iter().map(|&(a, w, _)| (0..images.len()).filter(|&i| images[i].word(a) == Some(w)).collect()).collect();
    // Per image, the length of the stretch of consecutive addresses it holds
    // what ran at, around each; and the run of executed addresses around each.
    let stretch = |held: Vec<u32>| -> BTreeMap<u32, (u32, u32)> {
        let mut out = BTreeMap::new();
        for (s, e) in spans(held) {
            for a in (s..e).step_by(4) {
                out.insert(a, (s, e));
            }
        }
        out
    };
    let mut by_image: Vec<Vec<u32>> = vec![Vec::new(); images.len()];
    for (j, h) in holders.iter().enumerate() {
        for &i in h {
            by_image[i].push(words[j].0);
        }
    }
    let held: Vec<BTreeMap<u32, (u32, u32)>> = by_image.into_iter().map(stretch).collect();
    let mut addrs: Vec<u32> = words.iter().map(|w| w.0).collect();
    addrs.dedup();
    let executed = stretch(addrs);
    let mut unique = vec![0u64; images.len()];
    for h in &holders {
        if let [i] = h[..] {
            unique[i] += 1;
        }
    }
    let mut credited = vec![0u64; images.len()];
    let mut by_name: BTreeMap<String, u64> = BTreeMap::new();
    let mut pending = Vec::new();
    // Credits word j to the best of images h: the one with a function
    // there, then the longest stretch, then the one the run otherwise ran.
    // An overlay's functions are an image each, and two of them can hold the
    // same code (one function's chunk is another's body): the word is the
    // overlay's, so each of its images holding it is credited.
    let give =
        |images: &mut [Image], credited: &mut [u64], by_name: &mut BTreeMap<String, u64>, h: &[usize], j: usize| {
            let (a, _, n) = words[j];
            let len = |i: usize| held[i].get(&a).map_or(1, |(s, e)| ((e - s) / 4) as u64);
            let key = |i: usize| (images[i].function(a).is_some(), len(i), unique[i]);
            let best = h.iter().map(|&i| key(i)).max();
            let mut chosen: Vec<usize> = h.iter().copied().filter(|&i| Some(key(i)) == best).collect();
            let same = |i: usize, c: usize| images[i].kind == images[c].kind && images[i].name == images[c].name;
            let more: Vec<usize> = holders[j]
                .iter()
                .copied()
                .filter(|&i| !chosen.contains(&i) && chosen.iter().any(|&c| same(i, c)))
                .collect();
            chosen.extend(more);
            let mut names: BTreeSet<&str> = BTreeSet::new();
            for &i in &chosen {
                let img = &mut images[i];
                let at = ((a - img.base) / 4) as usize;
                img.count[at] += n;
                img.credited_in[at] = run as u32 + 1;
                for f in img.functions_at(a) {
                    img.runs[f].insert(run);
                }
                credited[i] += 1;
            }
            for &i in &chosen {
                names.insert(&images[i].name);
            }
            for name in names {
                *by_name.entry(name.to_string()).or_default() += 1;
            }
        };
    for (j, &(a, _, _)) in words.iter().enumerate() {
        let (es, ee) = executed[&a];
        let span = |i: usize| held[i].get(&a).copied().unwrap_or((a, a));
        let len = |i: usize| ((span(i).1 - span(i).0) / 4) as u64;
        let blocks = |i: usize| {
            let (s, e) = span(i);
            images[i].function(a).is_some_and(|f| {
                let f = &images[i].functions[f];
                f.blocks.binary_search(&s).is_ok()
                    && (f.chunks.iter().any(|c| c.1 == e) || f.blocks.binary_search(&e).is_ok())
            })
        };
        let h: Vec<usize> =
            holders[j].iter().copied().filter(|&i| len(i) >= 4 || span(i) == (es, ee) || blocks(i)).collect();
        if h.is_empty() {
            pending.push(j);
        }
        give(images, &mut credited, &mut by_name, &h, j);
    }
    // A shorter stretch one image holds, where the words run beside it are
    // other images' (code loaded over it ran there): that image's.
    let unexplained: BTreeSet<u32> = pending.iter().map(|&j| words[j].0).collect();
    let mut missing = Vec::new();
    for j in pending {
        let a = words[j].0;
        let beside = |x: u32| !executed.contains_key(&x) || !unexplained.contains(&x);
        let h: Vec<usize> = holders[j]
            .iter()
            .copied()
            .filter(|&i| {
                let (s, e) = held[i].get(&a).copied().unwrap_or((a, a + 4));
                beside(s.wrapping_sub(4)) && beside(e)
            })
            .collect();
        if h.is_empty() {
            missing.push((a, words[j].1));
        }
        give(images, &mut credited, &mut by_name, &h, j);
    }
    (missing, credited, by_name)
}

#[derive(Default, Clone, Copy)]
struct Totals {
    functions: u64,
    exercised: u64,
    complete: u64,
    blocks: u64,
    blocks_executed: u64,
    instructions: u64,
    instructions_executed: u64,
    outside_functions: u64,
    branches: u64,
    branch_ways_taken: u64,
}

impl Totals {
    fn add(&mut self, o: &Totals) {
        self.functions += o.functions;
        self.exercised += o.exercised;
        self.complete += o.complete;
        self.blocks += o.blocks;
        self.blocks_executed += o.blocks_executed;
        self.instructions += o.instructions;
        self.instructions_executed += o.instructions_executed;
        self.outside_functions += o.outside_functions;
        self.branches += o.branches;
        self.branch_ways_taken += o.branch_ways_taken;
    }

    fn json(&self) -> Value {
        let share = |a: u64, b: u64| if b == 0 { 0.0 } else { (a as f64 / b as f64 * 1e4).round() / 1e4 };
        json!({
            "functions": self.functions, "functions_exercised": self.exercised, "functions_complete": self.complete,
            "blocks": self.blocks, "blocks_executed": self.blocks_executed,
            "instructions": self.instructions, "instructions_executed": self.instructions_executed,
            "executed_outside_functions": self.outside_functions,
            "block_share": share(self.blocks_executed, self.blocks),
            "branches": self.branches, "branch_ways": 2 * self.branches, "branch_ways_taken": self.branch_ways_taken,
        })
    }
}

/// Per function: its blocks executed, the ones not, the runs reaching it.
fn report(img: &Image) -> (Vec<Value>, Vec<Value>, Totals) {
    let mut t = Totals::default();
    let mut rows = Vec::new();
    let mut in_function = vec![false; img.words.len()];
    for (fi, f) in img.functions.iter().enumerate() {
        let k = |a: u32| ((a - img.base) / 4) as usize;
        let ran = |a: u32| img.count.get(k(a)).is_some_and(|&n| n > 0);
        for a in f.words() {
            if let Some(x) = in_function.get_mut(k(a)) {
                *x = true;
            }
        }
        let mut unexecuted = Vec::new();
        let mut blocks_executed = 0u64;
        for (s, e) in f.block_spans() {
            if (s..e).step_by(4).any(ran) {
                blocks_executed += 1;
            } else {
                unexecuted.push(json!([hex(s), hex(e)]));
            }
        }
        // Each conditional branch's two ways: the ones no run took.
        let mut ways_missed = Vec::new();
        let (mut branches, mut ways_taken) = (0u64, 0u64);
        for a in f.words() {
            if !img.word(a).is_some_and(conditional) {
                continue;
            }
            let w = img.ways.get(k(a)).copied().unwrap_or(0);
            branches += 1;
            ways_taken += (w & 1) as u64 + (w >> 1) as u64;
            let never = match w {
                3 => continue,
                0 => "ran",
                1 => "fell through",
                _ => "taken",
            };
            ways_missed.push(json!({"pc": hex(a), "never": never}));
        }
        t.branches += branches;
        t.branch_ways_taken += ways_taken;
        let instructions = f.words().count() as u64;
        let executed = f.words().filter(|&a| ran(a)).count() as u64;
        t.functions += 1;
        t.exercised += (blocks_executed > 0) as u64;
        t.complete += (unexecuted.is_empty()) as u64;
        t.blocks += f.blocks.len() as u64;
        t.blocks_executed += blocks_executed;
        t.instructions += instructions;
        t.instructions_executed += executed;
        let mut row = json!({
            "entry": hex(f.entry), "instructions": instructions, "executed": executed,
            "blocks": f.blocks.len(), "blocks_executed": blocks_executed,
            "branches": branches, "branch_ways_taken": ways_taken,
            "calls": img.count.get(k(f.entry)).copied().unwrap_or(0),
            "runs": img.runs[fi].iter().collect::<Vec<_>>(),
        });
        if blocks_executed > 0 && !unexecuted.is_empty() {
            row["unexecuted"] = json!(unexecuted);
        }
        if blocks_executed > 0 && !ways_missed.is_empty() {
            row["branch_ways_missed"] = json!(ways_missed);
        }
        if let Some(sha) = &f.sha {
            row["sha256"] = json!(sha);
        }
        rows.push(row);
    }
    let outside: Vec<u32> = (0..img.words.len())
        .filter(|&k| img.count[k] > 0 && !in_function[k])
        .map(|k| img.base + 4 * k as u32)
        .collect();
    t.outside_functions = outside.len() as u64;
    let outside = spans(outside)
        .into_iter()
        .map(|(s, e)| json!({"start": hex(s), "end": hex(e), "words": (e - s) / 4}))
        .collect();
    (rows, outside, t)
}

/// The inventory at `given` (default inventory's out), and its path.
pub(crate) fn inventory(ctx: &Ctx, given: Option<&Path>) -> crate::Result<(PathBuf, Value)> {
    let inv_path = given.map(|p| at(ctx, p)).unwrap_or_else(|| crate::inventory::default_out(ctx));
    let text = std::fs::read_to_string(&inv_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::invalid(format!("{}: no inventory; take one with inventory first", inv_path.display()))
        } else {
            Error::io(&inv_path, e)
        }
    })?;
    let inv: Value =
        serde_json::from_str(&text).map_err(|e| Error::malformed(format!("{}: {e}", inv_path.display())))?;
    Ok((inv_path, inv))
}

fn at(ctx: &Ctx, p: &Path) -> PathBuf {
    if p.is_absolute() { p.to_path_buf() } else { ctx.root.join(p) }
}

/// A corpus credited to the inventory's images.
struct Corpus {
    images: Vec<Image>,
    /// Per run: what it executed, and how to make it again.
    runs: Vec<Value>,
    /// Each run's call targets, with the run frames they were called first and last.
    calls: Vec<Vec<(u32, i64, i64)>>,
    /// Executed words no image holds, by address, with the runs that executed them.
    missing: BTreeMap<u32, BTreeSet<usize>>,
}

/// What to say when code ran that the inventory knows only from what ran
/// (overlays bounded_by found_code that coverage `images` reached) or not
/// at all (`no_image` words or spans).
fn unheld(inv: &Value, images: &Value, no_image: u64) -> Option<Value> {
    let partial: Vec<&Value> = images
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["kind"] == "overlay" && i["totals"]["functions_exercised"].as_u64() > Some(0))
        .filter(|i| found_code(inv, i["name"].as_str().unwrap_or_default()))
        .map(|i| &i["name"])
        .collect();
    (no_image > 0 || !partial.is_empty()).then(|| {
        json!({"overlays_known_only_from_what_ran": partial, "in_no_image": no_image,
               "how": crate::scenario::UNPACK_HINT})
    })
}

/// A scenario recording's kept coverage (`<name>.coverage.json`) credited
/// as run `r`; its row of runs.
fn recording(c: &mut Corpus, r: usize, path: &Path) -> crate::Result<Value> {
    let kept = path.with_extension("coverage.json");
    let text = std::fs::read_to_string(&kept).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::invalid(format!(
                "{}: the recording keeps no coverage (recorded before psx kept it): scenario record it again",
                path.display()
            ))
        } else {
            Error::io(&kept, e)
        }
    })?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| Error::malformed(format!("{}: {e}", kept.display())))?;
    if doc["format"] != KEPT_FORMAT {
        return Err(Error::malformed(format!("{}: not a recording's coverage", kept.display())));
    }
    let (missing, per, stale) = credit_kept(&mut c.images, r, &doc);
    for a in &missing {
        c.missing.entry(*a).or_default().insert(r);
    }
    let mut row = json!({
        "run": path, "words_executed": per.values().sum::<u64>() + missing.len() as u64,
        "words_in_no_image": missing.len(), "words_by_image": per, "branches_recorded": true,
        "recipe": null, "no_recipe": "a recording's coverage: scenario record makes it again",
    });
    if !stale.is_empty() {
        row["not_credited"] = json!(stale);
        row["why_not_credited"] = json!(
            "the inventory's code there is not what the recording ran (the inventory was taken again and read it otherwise, or an overlay was unpacked since): scenario record it again"
        );
    }
    Ok(row)
}

/// Credits the traced runs and recordings `dirs` to the inventory `inv`'s images.
fn corpus(ctx: &Ctx, inv: &Value, dirs: &[PathBuf]) -> crate::Result<Corpus> {
    let mut c =
        Corpus { images: images(&ctx.root, inv)?, runs: Vec::new(), calls: Vec::new(), missing: BTreeMap::new() };
    for (r, given) in dirs.iter().map(|d| at(ctx, d)).enumerate() {
        if given.extension().is_some_and(|e| e == "jsonl") {
            let row = recording(&mut c, r, &given)?;
            c.calls.push(Vec::new());
            c.runs.push(row);
            continue;
        }
        let dir = given;
        let words = executed(&dir)?;
        c.calls.push(run_calls(&dir));
        let (miss, _, per) = credit(&mut c.images, r, &words);
        let branched = match branches(&dir)? {
            Some(b) => {
                credit_branches(&mut c.images, r, &b);
                true
            }
            None => false,
        };
        let game = words.iter().filter(|w| w.0 >= GAME_RAM).count();
        for m in &miss {
            c.missing.entry(m.0).or_default().insert(r);
        }
        // How to make it again, kept with the coverage (run directories are not).
        let (recipe, no_recipe) = match crate::states::recipe_of(ctx, &dir) {
            Ok(r) => (r, Value::Null),
            Err(why) => (Value::Null, json!(why)),
        };
        c.runs.push(json!({
            "run": dir, "words_executed": game, "words_in_no_image": miss.len(), "words_by_image": per,
            "branches_recorded": branched,
            "recipe": recipe, "no_recipe": no_recipe,
        }));
    }
    Ok(c)
}

impl CoverageArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        let (inv_path, inv) = inventory(ctx, self.inventory.as_deref())?;
        let Corpus { images, runs, calls, missing } = corpus(ctx, &inv, &self.runs)?;

        // An overlay's functions are an image each: reported together.
        let mut totals = Totals::default();
        let mut groups: Vec<(&Image, Vec<Value>, Vec<Value>, Totals)> = Vec::new();
        let mut bios_words = 0u64;
        for img in &images {
            if img.kind == "bios" {
                bios_words += img.count.iter().filter(|&&n| n > 0).count() as u64;
                continue;
            }
            let (functions, outside, t) = report(img);
            totals.add(&t);
            match groups.iter_mut().find(|g| g.0.kind == img.kind && g.0.name == img.name) {
                Some(g) => {
                    g.1.extend(functions);
                    g.2.extend(outside);
                    g.3.add(&t);
                }
                None => groups.push((img, functions, outside, t)),
            }
        }
        let out_images: Vec<Value> = groups
            .into_iter()
            .map(|(img, functions, outside, t)| {
                // An overlay read only around the code found in it: its
                // functions no run reached may not all be listed.
                let bounded_by = inv["overlays"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|o| img.kind == "overlay" && o["name"].as_str() == Some(&img.name))
                    .map(|o| o["bounded_by"].clone());
                json!({
                    "kind": img.kind, "name": img.name, "start": hex(img.base), "bounded_by": bounded_by,
                    "totals": t.json(), "executed_outside_functions": outside, "functions": functions,
                })
            })
            .collect();
        // Words no image holds, by span: with the image covering the address
        // when there is one (its code differs there: changed at runtime).
        let mut no_image = Vec::new();
        let mut addrs: BTreeMap<u32, BTreeSet<usize>> = BTreeMap::new();
        for (a, r) in &missing {
            addrs.entry(*a).or_default().extend(r);
        }
        for (s, e) in spans(addrs.keys().copied()) {
            let covering: Vec<&str> = images.iter().filter(|i| i.word(s).is_some()).map(|i| i.name.as_str()).collect();
            let rs: BTreeSet<usize> = addrs.range(s..e).flat_map(|(_, r)| r.iter().copied()).collect();
            // When each run called into it: a snapshot then holds it.
            let called: Vec<Value> = rs
                .iter()
                .filter_map(|&r| {
                    let c = calls[r].iter().filter(|c| c.0 >= s && c.0 < e);
                    let (lo, hi) = c.fold((i64::MAX, i64::MIN), |(lo, hi), c| (lo.min(c.1), hi.max(c.2)));
                    (lo <= hi).then(|| json!({"run": r, "first_run_frame": lo, "last_run_frame": hi}))
                })
                .collect();
            no_image.push(json!({
                "start": hex(s), "end": hex(e), "words": (e - s) / 4,
                "differs_from": covering, "runs": rs, "called": called,
            }));
        }
        let mut t = totals.json();
        t["bios_words_executed"] = json!(bios_words);
        let summary = json!({
            "totals": t,
            "words_in_no_image": missing.len(),
        });
        let out = self.out.as_ref().map(|p| at(ctx, p)).unwrap_or_else(|| inv_path.with_file_name("coverage.json"));
        let doc = json!({
            "coverage": FORMAT, "made": crate::stamp::utc_now(), "psx": crate::context::version(),
            "inventory": {"path": inv_path, "made": inv["made"]},
        "how": "each run's executed words, credited to the executable or overlay whose code holds that instruction at that address; a block is executed when any of its words is; a conditional branch's ways (taken, fell through) are the ones a run took where its word was credited, and branch_ways_missed lists each branch with a way no run took (never ran, never taken, never fell through); runs are indices into runs; each run's recipe (the named state it started from, its arguments, its executed.bin's sha256) is what coverage check runs again",
            "runs": runs, "totals": summary["totals"], "in_no_image": no_image, "images": out_images,
        });
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
        }
        let text = serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n";
        std::fs::write(&out, text).map_err(|e| Error::io(&out, e))?;
        Ok(json!({
            "out": out, "totals": summary["totals"], "words_in_no_image": summary["words_in_no_image"],
            "images": doc["images"].as_array().into_iter().flatten().map(|i| json!({"name": i["name"], "bounded_by": i["bounded_by"], "totals": i["totals"]})).collect::<Vec<_>>(),
            "runs": runs.iter().map(|r| json!({"run": r["run"], "words_executed": r["words_executed"], "words_in_no_image": r["words_in_no_image"], "branches_recorded": r["branches_recorded"], "no_recipe": r["no_recipe"]})).collect::<Vec<_>>(),
            "next": "read the coverage for each function's unexecuted blocks and branch_ways_missed: classify them, or record the corpus reaching them; runs without branches_recorded were traced by an older psx (trace them again for their branch ways); executed_outside_functions and in_no_image mean the inventory missed code (take it again with these runs as ram)",
        }))
        .map(|mut r| {
            if let Some(hint) = unheld(&inv, &doc["images"], summary["words_in_no_image"].as_u64().unwrap_or(0)) {
                r["code_not_held"] = hint;
            }
            r
        })
    }
}

impl CoverageArgs {
    /// Each recipe run again into .psx/corpus-check/, its executed.bin
    /// compared with the one the coverage notes.
    fn check(&self, ctx: &Ctx) -> crate::Result<(bool, Value)> {
        let at = |p: &Path| if p.is_absolute() { p.to_path_buf() } else { ctx.root.join(p) };
        let inv_path = self.inventory.as_ref().map(|p| at(p)).unwrap_or_else(|| crate::inventory::default_out(ctx));
        let path = self.out.as_ref().map(|p| at(p)).unwrap_or_else(|| inv_path.with_file_name("coverage.json"));
        let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        let doc: Value =
            serde_json::from_str(&text).map_err(|e| Error::malformed(format!("{}: {e}", path.display())))?;
        let media = std::fs::read_to_string(&inv_path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .map(|i| i["media"].clone())
            .unwrap_or(Value::Null);
        let mut all_same = true;
        let mut results = Vec::new();
        let mut again = Vec::new();
        for (k, r) in doc["runs"].as_array().into_iter().flatten().enumerate() {
            let recipe = &r["recipe"];
            if recipe.is_null() {
                all_same = false;
                results.push(json!({"run": r["run"], "status": "no_recipe", "why": r["no_recipe"]}));
                continue;
            }
            let mut run = recipe["run"].clone();
            if let Some(name) = recipe["from"].as_str() {
                let state = crate::states::state_file(ctx, name);
                let sha = std::fs::read(&state).ok().map(|b| crate::stamp::sha256(&b));
                if sha.as_deref() != recipe["from_state_sha256"].as_str() {
                    all_same = false;
                    results.push(json!({"run": r["run"], "from": name, "status": "state_changed",
                        "why": "the named state is missing or not the one the run started from: states rebuild (replace), then take the corpus again"}));
                    continue;
                }
                run["from_state"] = json!(state);
            }
            let out = ctx.root.join(".psx/corpus-check").join(format!("{k:03}"));
            let _ = std::fs::remove_dir_all(&out);
            run["media"] = media.clone();
            run["out"] = json!(out);
            run["summary"] = json!(true);
            run["timeout"] = json!(7200);
            let args: crate::run::RunCmdArgs =
                serde_json::from_value(run).map_err(|e| Error::malformed(format!("a corpus recipe's run: {e}")))?;
            let (complete, report) = args.run(ctx)?;
            let executed = std::fs::read(out.join("executed.bin")).ok().map(|b| crate::stamp::sha256(&b));
            let status = if !complete {
                "incomplete"
            } else if executed.as_deref() == recipe["executed_sha256"].as_str() {
                "same"
            } else {
                "differs"
            };
            all_same &= status == "same";
            again.push(out.clone());
            let mut row = json!({"run": r["run"], "from": recipe["from"], "status": status, "again": out});
            if !complete {
                row["termination"] = report["termination"].clone();
            }
            results.push(row);
        }
        let mut out = json!({"status": if all_same { "same" } else { "differs" }, "coverage": path, "runs": results});
        if !all_same {
            out["next"] = json!(
                "a run that differs executed other code on this psx: coverage over the runs again (their again directories) takes the corpus anew; no_recipe and state_changed say what to make first"
            );
        }
        Ok((all_same, out))
    }
}

impl Op for CoverageArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        if self.check {
            return Outcome::report(self.check(ctx));
        }
        Outcome::result(self.run(ctx))
    }
}

/// A conditional branch's ways in `ways` and not in `not` (1 taken, 2 fell
/// through), named.
fn ways_only(ways: u8, not: u8) -> impl Iterator<Item = &'static str> {
    let only = ways & !not;
    [(1, "taken"), (2, "fell through")].into_iter().filter(move |(b, _)| only & b != 0).map(|(_, n)| n)
}

/// The functions of images `a` executed that `b`'s did not (`only`), and
/// the functions both executed with the blocks and branch ways only `a`'s
/// took (`further`); with the instructions of `only`, and the blocks and
/// ways of `further`. `a` and `b` are the same inventory's images.
fn compare(a: &[Image], b: &[Image]) -> (Vec<Value>, Vec<Value>, [u64; 3]) {
    let ran = |img: &Image, w: u32| img.count.get(((w - img.base) / 4) as usize).is_some_and(|&n| n > 0);
    let (mut only, mut further) = (Vec::new(), Vec::new());
    let (mut only_instructions, mut blocks_only, mut ways_count) = (0u64, 0u64, 0u64);
    for (ia, ib) in a.iter().zip(b).filter(|(i, _)| i.kind != "bios") {
        for (fi, f) in ia.functions.iter().enumerate() {
            let spans: Vec<(u32, u32)> = f.block_spans().collect();
            let in_a: Vec<bool> = spans.iter().map(|&(s, e)| (s..e).step_by(4).any(|w| ran(ia, w))).collect();
            if !in_a.contains(&true) {
                continue;
            }
            let in_b: Vec<bool> = spans.iter().map(|&(s, e)| (s..e).step_by(4).any(|w| ran(ib, w))).collect();
            let runs: Vec<&usize> = ia.runs[fi].iter().collect();
            let instructions = f.words().count() as u64;
            if !in_b.contains(&true) {
                only_instructions += instructions;
                only.push(json!({
                    "image": ia.name, "entry": hex(f.entry), "instructions": instructions,
                    "blocks": spans.len(), "blocks_executed": in_a.iter().filter(|x| **x).count(),
                    "calls": ia.count.get(((f.entry - ia.base) / 4) as usize).copied().unwrap_or(0),
                    "runs": runs,
                }));
                continue;
            }
            // The blocks only the runs executed, adjacent ones joined.
            let mut joined: Vec<(u32, u32)> = Vec::new();
            for (&(s, e), _) in spans.iter().zip(in_a.iter().zip(&in_b)).filter(|(_, (x, y))| **x && !**y) {
                match joined.last_mut() {
                    Some(last) if last.1 == s => last.1 = e,
                    _ => joined.push((s, e)),
                }
            }
            let blocks: Vec<Value> = joined.iter().map(|&(s, e)| json!([hex(s), hex(e)])).collect();
            let mut ways = Vec::new();
            for w in f.words().filter(|&w| ia.word(w).is_some_and(conditional)) {
                let k = ((w - ia.base) / 4) as usize;
                for way in ways_only(ia.ways[k], ib.ways[k]) {
                    ways.push(json!({"pc": hex(w), "way": way}));
                }
            }
            if blocks.is_empty() && ways.is_empty() {
                continue;
            }
            blocks_only += spans.iter().zip(in_a.iter().zip(&in_b)).filter(|(_, (x, y))| **x && !**y).count() as u64;
            ways_count += ways.len() as u64;
            let mut row = json!({"image": ia.name, "entry": hex(f.entry), "runs": runs});
            if !blocks.is_empty() {
                row["blocks"] = json!(blocks);
            }
            if !ways.is_empty() {
                row["branch_ways"] = json!(ways);
            }
            further.push(row);
        }
    }
    (only, further, [only_instructions, blocks_only, ways_count])
}

impl CoverageDiffArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        let (inv_path, inv) = inventory(ctx, self.inventory.as_deref())?;
        let a = corpus(ctx, &inv, &self.runs)?;
        let b = corpus(ctx, &inv, &self.against)?;
        let (only, further, [only_instructions, blocks_only, ways_count]) = compare(&a.images, &b.images);
        // Executed words no image holds that the other runs did not execute.
        let addrs: BTreeSet<u32> = a.missing.keys().filter(|k| !b.missing.contains_key(k)).copied().collect();
        let no_image: Vec<Value> = spans(addrs)
            .into_iter()
            .map(|(s, e)| json!({"start": hex(s), "end": hex(e), "words": (e - s) / 4}))
            .collect();
        let unbranched: Vec<&Value> =
            a.runs.iter().chain(&b.runs).filter(|r| r["branches_recorded"] == false).map(|r| &r["run"]).collect();
        let totals = json!({
            "functions_only": only.len(), "instructions_only": only_instructions,
            "functions_further": further.len(), "blocks_only": blocks_only, "branch_ways_only": ways_count,
            "in_no_image": no_image.len(),
        });
        let out =
            self.out.as_ref().map(|p| at(ctx, p)).unwrap_or_else(|| inv_path.with_file_name("coverage-diff.json"));
        let doc = json!({
            "coverage_diff": FORMAT, "made": crate::stamp::utc_now(), "psx": crate::context::version(),
            "inventory": {"path": inv_path, "made": inv["made"]},
            "how": "both sets of runs credited as coverage credits them; only: functions the runs executed and the against runs did not; further: functions both reached, with the blocks (adjacent ones joined into one range) and branch ways (taken, fell through) only the runs took; in_no_image: executed words no image holds that the against runs did not execute; runs are indices into runs",
            "runs": a.runs.iter().map(|r| &r["run"]).collect::<Vec<_>>(),
            "against": b.runs.iter().map(|r| &r["run"]).collect::<Vec<_>>(),
            "totals": totals, "only": only, "further": further, "in_no_image": no_image,
        });
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d).map_err(|e| Error::io(d, e))?;
        }
        let text = serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n";
        std::fs::write(&out, text).map_err(|e| Error::io(&out, e))?;
        let mut r = json!({
            "out": out, "totals": doc["totals"],
            "only": doc["only"].as_array().into_iter().flatten().take(20).map(|f| json!({"image": f["image"], "entry": f["entry"], "instructions": f["instructions"]})).collect::<Vec<_>>(),
            "next": "read out for every function, block and branch way: these are the candidates for the feature's code; disassemble or decompile them, and keep what a run without the feature also reaches out of the feature",
        });
        if let Some(hint) = unheld(&inv, &json!(null), no_image.len() as u64) {
            r["code_not_held"] = hint;
        }
        if !unbranched.is_empty() {
            r["branches_not_recorded"] = json!(unbranched);
            r["note"] = json!("runs traced by an older psx record no branch ways: trace them again for branch_ways");
        }
        Ok(r)
    }
}

impl Op for CoverageDiffArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(self.run(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(name: &str, base: u32, words: Vec<u32>, functions: Vec<Function>) -> Image {
        let (n, f) = (words.len(), functions.len());
        let mut chunks: Vec<(u32, u32, usize)> =
            functions.iter().enumerate().flat_map(|(i, f)| f.chunks.iter().map(move |&(s, e)| (s, e, i))).collect();
        chunks.sort_unstable();
        Image {
            kind: "overlay",
            name: name.into(),
            base,
            words: words.into_iter().map(Some).collect(),
            functions,
            longest: chunks.iter().map(|c| c.1 - c.0).max().unwrap_or(0),
            chunks,
            count: vec![0; n],
            credited_in: vec![0; n],
            ways: vec![0; n],
            runs: vec![BTreeSet::new(); f],
        }
    }

    #[test]
    fn code_loaded_over_code_is_credited_to_the_image_that_holds_it() {
        // Two overlays at one address share their first word (a prologue).
        let base = 0x8010_0000;
        let f = || {
            vec![Function {
                entry: base,
                end: base + 16,
                chunks: vec![(base, base + 16)],
                blocks: vec![base, base + 8],
                sha: None,
            }]
        };
        let mut images =
            vec![img("a", base, vec![0x27BD_FFE8, 1, 2, 3], f()), img("b", base, vec![0x27BD_FFE8, 5, 6, 7], f())];
        // The run ran b's first block only, and a word neither holds.
        let words = [(base, 0x27BD_FFE8, 3), (base + 4, 5, 3), (base + 8, 9, 1)];
        let (missing, credited, _) = credit(&mut images, 0, &words);
        assert_eq!(missing, vec![(base + 8, 9)]);
        assert_eq!(credited, vec![0, 2]);
        let (rows, outside, t) = report(&images[1]);
        assert_eq!(rows[0]["blocks_executed"], 1);
        assert_eq!(rows[0]["unexecuted"], json!([["0x80100008", "0x80100010"]]));
        assert_eq!(rows[0]["calls"], 3);
        assert!(outside.is_empty());
        assert_eq!((t.exercised, t.complete), (1, 0));
        let (_, _, t) = report(&images[0]);
        assert_eq!(t.exercised, 0);
    }

    #[test]
    fn a_short_stretch_between_other_code_is_its_holders() {
        // Code (a BIOS's shell, no functions) ran two words, `jr ra; nop`,
        // where the game's overlay later ran its own code around them.
        let base = 0x8010_0000;
        let f = vec![Function {
            entry: base,
            end: base + 16,
            chunks: vec![(base, base + 16)],
            blocks: vec![base],
            sha: None,
        }];
        let mut images =
            vec![img("game", base, vec![1, 2, 3, 4], f), img("shell", base + 4, vec![0x03E0_0008, 0, 0x1234], vec![])];
        let words = [(base, 1, 9), (base + 4, 2, 9), (base + 4, 0x03E0_0008, 1), (base + 8, 0, 1), (base + 8, 3, 9)];
        let (missing, credited, _) = credit(&mut images, 0, &words);
        assert!(missing.is_empty(), "{missing:?}");
        assert_eq!(credited, vec![3, 2]);
        // Beside a word no image holds, it stays unexplained.
        let mut images = vec![img("shell", base + 4, vec![0x03E0_0008, 0], vec![])];
        let words = [(base, 7, 1), (base + 4, 0x03E0_0008, 1), (base + 8, 0, 1), (base + 12, 5, 1)];
        let (missing, _, _) = credit(&mut images, 0, &words);
        assert_eq!(missing.len(), 4);
    }

    #[test]
    fn executed_words_in_no_function_are_listed() {
        let base = 0x8010_0000;
        let mut images = vec![img(
            "a",
            base,
            vec![1, 2, 3, 4],
            vec![Function {
                entry: base,
                end: base + 8,
                chunks: vec![(base, base + 8)],
                blocks: vec![base],
                sha: None,
            }],
        )];
        credit(&mut images, 0, &[(base, 1, 1), (base + 8, 3, 1), (base + 12, 4, 1)]);
        let (_, outside, t) = report(&images[0]);
        assert_eq!(outside, vec![json!({"start": "0x80100008", "end": "0x80100010", "words": 2})]);
        assert_eq!((t.complete, t.outside_functions), (1, 2));
    }

    #[test]
    fn a_function_in_chunks_is_its_chunks_bytes_each_at_its_own_address() {
        let base = 0x8010_0000;
        // Two chunks around a word of other code: the inventory keeps the
        // chunks' bytes back to back.
        let f = || Function {
            entry: base,
            end: base + 20,
            chunks: vec![(base, base + 8), (base + 12, base + 20)],
            blocks: vec![base, base + 12],
            sha: None,
        };
        let kept: Vec<u8> = [1u32, 2, 4, 5].iter().flat_map(|w| w.to_le_bytes()).collect();
        let words = placed(&f(), &kept).unwrap();
        assert_eq!(words, vec![Some(1), Some(2), None, Some(4), Some(5)]);
        assert_eq!(placed(&f(), &kept[..12]), None);
        let mut images = vec![img("a", base, vec![], vec![f()])];
        images[0].words = words;
        (images[0].count, images[0].credited_in, images[0].ways) = (vec![0; 5], vec![0; 5], vec![0; 5]);
        let (miss, _, _) = credit(&mut images, 0, &[(base + 8, 9, 1), (base + 12, 4, 1), (base + 16, 5, 1)]);
        // The second chunk ran; the word between the chunks is no code of it.
        assert_eq!(miss, vec![(base + 8, 9)]);
        let (rows, _, t) = report(&images[0]);
        assert_eq!((t.instructions, t.instructions_executed, t.blocks_executed), (4, 2, 1));
        assert_eq!(rows[0]["unexecuted"], json!([["0x80100000", "0x80100008"]]));
    }

    #[test]
    fn code_two_functions_of_an_overlay_hold_is_credited_to_both() {
        // As in a game's overlay: a's second chunk is b's body, after a word
        // (b's first) a does not hold. Each function is an image.
        let base = 0x8010_0000;
        let f = |entry: u32, chunks: Vec<(u32, u32)>| Function {
            entry,
            end: base + 32,
            blocks: vec![chunks[0].0],
            chunks,
            sha: None,
        };
        let code: Vec<u32> = (1..=8).collect();
        let a = f(base, vec![(base, base + 16), (base + 20, base + 32)]);
        let b = f(base + 16, vec![(base + 16, base + 32)]);
        let mut images =
            vec![img("ov", base, code.clone(), vec![a]), img("ov", base + 16, code[4..].to_vec(), vec![b])];
        images[0].words[4] = None;
        // The run ran b, all of it: a holds only three of its words.
        let words: Vec<(u32, u32, u64)> = (4..8).map(|k| (base + 4 * k, k + 1, 2)).collect();
        let (missing, credited, by_name) = credit(&mut images, 0, &words);
        assert!(missing.is_empty());
        assert_eq!(credited, vec![3, 4]);
        // The overlay ran four words, not seven.
        assert_eq!(by_name, BTreeMap::from([("ov".to_string(), 4)]));
        let (rows, _, _) = report(&images[0]);
        assert_eq!((rows[0]["executed"].clone(), rows[0]["runs"].clone()), (json!(3), json!([0])));
        let (rows, _, _) = report(&images[1]);
        assert_eq!((rows[0]["executed"].clone(), rows[0]["runs"].clone()), (json!(4), json!([0])));
    }

    #[test]
    fn a_run_reaches_every_function_whose_chunk_it_ran() {
        // In one executable: x's second chunk is y's code.
        let base = 0x8010_0000;
        let x = Function {
            entry: base,
            end: base + 20,
            chunks: vec![(base, base + 8), (base + 12, base + 20)],
            blocks: vec![base, base + 12],
            sha: None,
        };
        let y = Function {
            entry: base + 8,
            end: base + 20,
            chunks: vec![(base + 8, base + 20)],
            blocks: vec![base + 8],
            sha: None,
        };
        let mut images = vec![img("exe", base, vec![1, 2, 3, 4, 5], vec![x, y])];
        credit(&mut images, 0, &[(base + 12, 4, 1), (base + 16, 5, 1)]);
        let (rows, _, _) = report(&images[0]);
        assert_eq!((rows[0]["runs"].clone(), rows[1]["runs"].clone()), (json!([0]), json!([0])));
    }

    #[test]
    fn each_way_of_each_branch_no_run_took_is_listed() {
        let base = 0x8010_0000;
        // addiu; bne a0, zero, +3; nop; addiu; jr ra; nop.
        let code = vec![0x2484_0001, 0x1480_0003, 0, 0x2484_0001, 0x03E0_0008, 0];
        let f = || {
            vec![Function {
                entry: base,
                end: base + 24,
                chunks: vec![(base, base + 24)],
                blocks: vec![base, base + 12, base + 16],
                sha: None,
            }]
        };
        let ran =
            [(base, code[0], 1), (base + 4, code[1], 1), (base + 8, 0, 1), (base + 16, code[4], 1), (base + 20, 0, 1)];
        // No branch data: the branch is listed as never run.
        let mut images = vec![img("a", base, code.clone(), f())];
        credit(&mut images, 0, &ran);
        let (rows, _, t) = report(&images[0]);
        assert_eq!((t.branches, t.branch_ways_taken), (1, 0));
        assert_eq!(rows[0]["branch_ways_missed"], json!([{"pc": "0x80100004", "never": "ran"}]));
        // Always taken, past the addiu: it never fell through.
        let mut images = vec![img("a", base, code.clone(), f())];
        credit(&mut images, 0, &ran);
        credit_branches(&mut images, 0, &[(base + 4, code[1], 1, 0)]);
        let (rows, _, t) = report(&images[0]);
        assert_eq!((t.branches, t.branch_ways_taken), (1, 1));
        assert_eq!(rows[0]["branch_ways_missed"], json!([{"pc": "0x80100004", "never": "fell through"}]));
        // A second run falls through: both ways, nothing listed.
        let fell = [(base + 4, code[1], 1), (base + 8, 0, 1), (base + 12, code[3], 1)];
        credit(&mut images, 1, &fell);
        credit_branches(&mut images, 1, &[(base + 4, code[1], 0, 1)]);
        let (rows, _, t) = report(&images[0]);
        assert_eq!((t.branches, t.branch_ways_taken), (1, 2));
        assert!(rows[0].get("branch_ways_missed").is_none());
        // A branch another image holds at that address is not credited here.
        let mut images = vec![img("a", base, code.clone(), f())];
        credit(&mut images, 0, &ran);
        credit_branches(&mut images, 0, &[(base + 4, 0x1480_0009, 1, 1)]);
        assert_eq!(report(&images[0]).2.branch_ways_taken, 0);
    }

    #[test]
    fn a_diff_lists_what_only_the_runs_reached() {
        let base = 0x8010_0000;
        // f: addiu; bne a0, zero, +3; nop; addiu; jr ra; nop. g: jr ra; nop.
        let code = vec![0x2484_0001, 0x1480_0003, 0, 0x2484_0001, 0x03E0_0008, 0, 0x03E0_0008, 0];
        let fs = || {
            vec![
                Function {
                    entry: base,
                    end: base + 24,
                    chunks: vec![(base, base + 24)],
                    blocks: vec![base, base + 12, base + 16],
                    sha: None,
                },
                Function {
                    entry: base + 24,
                    end: base + 32,
                    chunks: vec![(base + 24, base + 32)],
                    blocks: vec![base + 24],
                    sha: None,
                },
            ]
        };
        let taken =
            [(base, code[0], 1), (base + 4, code[1], 1), (base + 8, 0, 1), (base + 16, code[4], 1), (base + 20, 0, 1)];
        // The against run takes the branch; the run falls through and calls g.
        let mut b = vec![img("a", base, code.clone(), fs())];
        credit(&mut b, 0, &taken);
        credit_branches(&mut b, 0, &[(base + 4, code[1], 1, 0)]);
        let mut a = vec![img("a", base, code.clone(), fs())];
        let fell: Vec<(u32, u32, u64)> = (0..8).map(|k| (base + 4 * k, code[k as usize], 1)).collect();
        credit(&mut a, 0, &fell);
        credit_branches(&mut a, 0, &[(base + 4, code[1], 0, 1)]);
        let (only, further, counts) = compare(&a, &b);
        assert_eq!(only.len(), 1);
        assert_eq!((&only[0]["entry"], &only[0]["instructions"]), (&json!("0x80100018"), &json!(2)));
        assert_eq!(
            further,
            [json!({"image": "a", "entry": "0x80100000", "runs": [0],
                    "blocks": [["0x8010000C", "0x80100010"]],
                    "branch_ways": [{"pc": "0x80100004", "way": "fell through"}]})]
        );
        assert_eq!(counts, [2, 1, 1]);
        // The other way round: b reaches nothing a does not, but took the branch.
        let (only, further, _) = compare(&b, &a);
        assert!(only.is_empty());
        assert_eq!(further[0]["branch_ways"], json!([{"pc": "0x80100004", "way": "taken"}]));
        assert!(further[0].get("blocks").is_none());
    }

    #[test]
    fn a_recording_keeps_coverage_without_its_code() {
        let base = 0x8010_0000;
        let code = vec![0x2484_0001, 0x1480_0003, 0, 0x2484_0001, 0x03E0_0008, 0, 0x03E0_0008, 0];
        let fs = || {
            vec![
                Function {
                    entry: base,
                    end: base + 24,
                    chunks: vec![(base, base + 24)],
                    blocks: vec![base, base + 12, base + 16],
                    sha: None,
                },
                Function {
                    entry: base + 24,
                    end: base + 32,
                    chunks: vec![(base + 24, base + 32)],
                    blocks: vec![base + 24],
                    sha: None,
                },
            ]
        };
        let words: Vec<(u32, u32, u64)> =
            (0..6).map(|k| (base + 4 * k, code[k as usize], 3)).chain([(0x8020_0000, 0x1234_5678, 1)]).collect();
        let branches = [(base + 4, code[1], 2, 1)];
        let mut direct = vec![img("ovl", base, code.clone(), fs())];
        credit(&mut direct, 0, &words);
        credit_branches(&mut direct, 0, &branches);
        let doc = kept_doc(&mut [img("ovl", base, code.clone(), fs())], &words, &branches);
        let text = doc.to_string();
        assert!(!text.contains("24840001") && !text.contains("12345678"), "no instruction is kept: {text}");
        assert_eq!(doc["in_no_image"], json!([["0x80200000", "0x80200004"]]));
        let mut again = vec![img("ovl", base, code.clone(), fs())];
        let (missing, per, stale) = credit_kept(&mut again, 0, &doc);
        assert_eq!((missing, per["ovl"], stale.len()), (vec![0x8020_0000], 6, 0));
        assert_eq!(report(&again[0]).0, report(&direct[0]).0);
        // Other code at those addresses: not credited.
        let mut other = code.clone();
        other[3] = 0x2484_0002;
        let mut changed = vec![img("ovl", base, other, fs())];
        assert_eq!(credit_kept(&mut changed, 0, &doc).2, ["ovl"]);
        assert_eq!(report(&changed[0]).2.exercised, 0);
    }

    #[test]
    fn code_known_only_from_what_ran_points_at_unpacking() {
        let inv = json!({"overlays": [{"name": "ovl@80100000", "bounded_by": "found_code"}, {"name": "/MENU.BIN", "bounded_by": "file"}]});
        let images = |n: u64| {
            json!([{"kind": "overlay", "name": "ovl@80100000", "totals": {"functions_exercised": n}},
                   {"kind": "overlay", "name": "/MENU.BIN", "totals": {"functions_exercised": 3}}])
        };
        assert_eq!(unheld(&inv, &images(0), 0), None);
        let hint = unheld(&inv, &images(2), 0).unwrap();
        assert_eq!(hint["overlays_known_only_from_what_ran"], json!(["ovl@80100000"]));
        assert!(hint["how"].as_str().unwrap().contains("Decompressed code"));
        assert_eq!(unheld(&inv, &json!(null), 5).unwrap()["in_no_image"], 5);
    }

    #[test]
    fn only_branches_that_can_go_both_ways_count() {
        assert!(conditional(0x1480_0003)); // bne a0, zero
        assert!(!conditional(0x1000_0003)); // b (beq zero, zero)
        assert!(!conditional(0x1400_0003)); // bne zero, zero
        assert!(conditional(0x0480_0003)); // bltz a0
        assert!(!conditional(0x0411_0003)); // bal
        assert!(conditional(0x1880_0003)); // blez a0
        assert!(!conditional(0x1800_0003)); // blez zero: always
        assert!(!conditional(0x0800_4000)); // j
    }
}
