//! `psx coverage`: what a corpus of traced runs executed, per function of the
//! inventory: its blocks executed and not, and the runs that reached it. So
//! "exercised" is a query, and the blocks never executed are a list to
//! classify (dead, debug only, unreachable on this disc, or needing a
//! recording).
//!
//! Each run's executed.bin (run --trace) gives every RAM word executed with
//! the instruction it executed as. A word is credited to the executable or
//! overlay whose code holds that instruction there, so code loaded over
//! other code is told apart; where several hold it, to the one holding most
//! of the run of consecutive words it was executed in. Executed words no
//! image holds (code changed at runtime, an overlay no snapshot held) and
//! executed words in no function (a function the inventory missed) are
//! listed. The BIOS shell's code in RAM (the inventory's bios_code) is
//! credited but counted apart.
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CoverageArgs {
    /// Traced runs (run --trace).
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
    /// Executions of each word, over the corpus.
    count: Vec<u64>,
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
}

fn parse_hex(v: &Value) -> Option<u32> {
    u32::from_str_radix(v.as_str()?, 16).ok()
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

/// coverage.json's format: 2 reads a function in chunks at its chunks'
/// addresses (1 read the bytes of all its chunks as one stretch from its entry).
pub const FORMAT: u64 = 2;

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
        Image { kind, name, base, words, functions, chunks, count: vec![0; n], runs: vec![BTreeSet::new(); f] }
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
                .filter(|b| crate::publish::sha256(b) == sha)
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

/// A traced run's call targets (calls.json), with the run frames of the
/// first and last call.
fn run_calls(run: &Path) -> Vec<(u32, i64, i64)> {
    let Ok(text) = std::fs::read_to_string(run.join("calls.json")) else { return Vec::new() };
    let v: Value = serde_json::from_str(&text).unwrap_or_default();
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let a = 0x8000_0000 | (c["address"].as_u64()? as u32 & 0x1F_FFFF);
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

fn hex(a: u32) -> String {
    format!("{a:08X}")
}

/// Credits one run's executed words to images; returns the words no image
/// holds, and per image how many words were credited. Code loaded over
/// other code shares nops and returns with what it replaced, so an image is
/// credited a word only when it holds a stretch of what ran there: four
/// consecutive executed words, the whole run of consecutive executed words
/// around it, or whole basic blocks of one of its functions; or a shorter
/// stretch where the words run beside it are other images' (code loaded
/// over it ran there). Of several, the one with a function there, then the
/// longest stretch, then the one the run otherwise ran.
fn credit(images: &mut [Image], run: usize, words: &[(u32, u32, u64)]) -> (Vec<(u32, u32)>, Vec<u64>) {
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
    let mut pending = Vec::new();
    // Credits word j to the best of images h: the one with a function
    // there, then the longest stretch, then the one the run otherwise ran.
    let give = |images: &mut [Image], credited: &mut [u64], h: &[usize], j: usize| {
        let (a, _, n) = words[j];
        let len = |i: usize| held[i].get(&a).map_or(1, |(s, e)| ((e - s) / 4) as u64);
        let key = |i: usize| (images[i].function(a).is_some(), len(i), unique[i]);
        let best = h.iter().map(|&i| key(i)).max();
        let chosen: Vec<usize> = h.iter().copied().filter(|&i| Some(key(i)) == best).collect();
        for i in chosen {
            let img = &mut images[i];
            let at = ((a - img.base) / 4) as usize;
            img.count[at] += n;
            if let Some(f) = img.function(a) {
                img.runs[f].insert(run);
            }
            credited[i] += 1;
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
        give(images, &mut credited, &h, j);
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
        give(images, &mut credited, &h, j);
    }
    (missing, credited)
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
    }

    fn json(&self) -> Value {
        let share = |a: u64, b: u64| if b == 0 { 0.0 } else { (a as f64 / b as f64 * 1e4).round() / 1e4 };
        json!({
            "functions": self.functions, "functions_exercised": self.exercised, "functions_complete": self.complete,
            "blocks": self.blocks, "blocks_executed": self.blocks_executed,
            "instructions": self.instructions, "instructions_executed": self.instructions_executed,
            "executed_outside_functions": self.outside_functions,
            "block_share": share(self.blocks_executed, self.blocks),
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
        for (bi, &s) in f.blocks.iter().enumerate() {
            // A block ends at the next, or at the end of its chunk.
            let chunk_end = f.chunk(s).map_or(f.end, |c| c.1);
            let e = f.blocks.get(bi + 1).copied().unwrap_or(f.end).min(chunk_end);
            if (s..e).step_by(4).any(ran) {
                blocks_executed += 1;
            } else {
                unexecuted.push(json!([hex(s), hex(e)]));
            }
        }
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
            "calls": img.count.get(k(f.entry)).copied().unwrap_or(0),
            "runs": img.runs[fi].iter().collect::<Vec<_>>(),
        });
        if blocks_executed > 0 && !unexecuted.is_empty() {
            row["unexecuted"] = json!(unexecuted);
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

impl CoverageArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        let at = |p: &Path| if p.is_absolute() { p.to_path_buf() } else { ctx.root.join(p) };
        let inv_path = self.inventory.as_ref().map(|p| at(p)).unwrap_or_else(|| crate::inventory::default_out(ctx));
        let text = std::fs::read_to_string(&inv_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::invalid(format!("{}: no inventory; take one with inventory first", inv_path.display()))
            } else {
                Error::io(&inv_path, e)
            }
        })?;
        let inv: Value =
            serde_json::from_str(&text).map_err(|e| Error::malformed(format!("{}: {e}", inv_path.display())))?;
        let mut images = images(&ctx.root, &inv)?;

        let mut runs = Vec::new();
        // Each run's call targets, with the run frames they were called first and last.
        let mut calls: Vec<Vec<(u32, i64, i64)>> = Vec::new();
        // Executed words no image holds, with the runs that executed them.
        let mut missing: BTreeMap<(u32, u32), BTreeSet<usize>> = BTreeMap::new();
        for (r, dir) in self.runs.iter().map(|d| at(d)).enumerate() {
            let words = executed(&dir)?;
            calls.push(run_calls(&dir));
            let (miss, credited) = credit(&mut images, r, &words);
            let game = words.iter().filter(|w| w.0 >= GAME_RAM).count();
            for m in &miss {
                missing.entry(*m).or_default().insert(r);
            }
            let per: serde_json::Map<String, Value> = images
                .iter()
                .zip(&credited)
                .filter(|(_, &n)| n > 0)
                .map(|(img, &n)| (img.name.clone(), json!(n)))
                .collect();
            // How to make it again, kept with the coverage (run directories are not).
            let (recipe, no_recipe) = match crate::states::recipe_of(ctx, &dir) {
                Ok(r) => (r, Value::Null),
                Err(why) => (Value::Null, json!(why)),
            };
            runs.push(json!({
                "run": dir, "words_executed": game, "words_in_no_image": miss.len(), "words_by_image": per,
                "recipe": recipe, "no_recipe": no_recipe,
            }));
        }

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
        for ((a, _), r) in &missing {
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
        let out = self.out.as_ref().map(|p| at(p)).unwrap_or_else(|| inv_path.with_file_name("coverage.json"));
        let doc = json!({
            "coverage": FORMAT, "made": crate::publish::utc_now(), "psx": crate::context::version(),
            "inventory": {"path": inv_path, "made": inv["made"]},
        "how": "each run's executed words, credited to the executable or overlay whose code holds that instruction at that address; a block is executed when any of its words is; runs are indices into runs; each run's recipe (the named state it started from, its arguments, its executed.bin's sha256) is what coverage check runs again",
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
            "runs": runs.iter().map(|r| json!({"run": r["run"], "words_executed": r["words_executed"], "words_in_no_image": r["words_in_no_image"], "no_recipe": r["no_recipe"]})).collect::<Vec<_>>(),
            "next": "artifact_read the coverage for each function's unexecuted blocks: classify them, or record the corpus reaching them; executed_outside_functions and in_no_image mean the inventory missed code (take it again with these runs as ram)",
        }))
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
                let sha = std::fs::read(&state).ok().map(|b| crate::publish::sha256(&b));
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
            let executed = std::fs::read(out.join("executed.bin")).ok().map(|b| crate::publish::sha256(&b));
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
            chunks,
            count: vec![0; n],
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
        let (missing, credited) = credit(&mut images, 0, &words);
        assert_eq!(missing, vec![(base + 8, 9)]);
        assert_eq!(credited, vec![0, 2]);
        let (rows, outside, t) = report(&images[1]);
        assert_eq!(rows[0]["blocks_executed"], 1);
        assert_eq!(rows[0]["unexecuted"], json!([["80100008", "80100010"]]));
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
        let (missing, credited) = credit(&mut images, 0, &words);
        assert!(missing.is_empty(), "{missing:?}");
        assert_eq!(credited, vec![3, 2]);
        // Beside a word no image holds, it stays unexplained.
        let mut images = vec![img("shell", base + 4, vec![0x03E0_0008, 0], vec![])];
        let words = [(base, 7, 1), (base + 4, 0x03E0_0008, 1), (base + 8, 0, 1), (base + 12, 5, 1)];
        let (missing, _) = credit(&mut images, 0, &words);
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
        assert_eq!(outside, vec![json!({"start": "80100008", "end": "80100010", "words": 2})]);
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
        images[0].count = vec![0; 5];
        let (miss, _) = credit(&mut images, 0, &[(base + 8, 9, 1), (base + 12, 4, 1), (base + 16, 5, 1)]);
        // The second chunk ran; the word between the chunks is no code of it.
        assert_eq!(miss, vec![(base + 8, 9)]);
        let (rows, _, t) = report(&images[0]);
        assert_eq!((t.instructions, t.instructions_executed, t.blocks_executed), (4, 2, 1));
        assert_eq!(rows[0]["unexecuted"], json!([["80100000", "80100008"]]));
    }
}
