//! Which of the project's names apply to an analysis. A function's name
//! applies where the analysed code is the code it was written for: the
//! inventory knows each function of the executables and overlays by the
//! sha256 of its bytes, so overlays loaded at one address are told apart by
//! what they hold, with nothing recorded. `in MODULE` names an overlay (or
//! an executable) by its file; a name without it is the executable's.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use psx_symbols::names::{Name, Names};
use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// The analysed memory: its bytes from `base`.
pub(crate) struct Code<'a> {
    pub base: u32,
    pub bytes: &'a [u8],
}

impl Code<'_> {
    fn sha(&self, chunks: &[(u32, u32)]) -> Option<String> {
        let mut bytes = Vec::new();
        for &(s, e) in chunks {
            let o = s.checked_sub(self.base)? as usize;
            bytes.extend_from_slice(self.bytes.get(o..o + e.checked_sub(s)? as usize)?);
        }
        Some(crate::stamp::sha256(&bytes))
    }
}

/// A function of the inventory.
struct Known {
    /// The module as the inventory names it, for messages.
    module: String,
    /// What `in` may call it, lowercase: the overlay's name and its file's.
    labels: Vec<String>,
    executable: bool,
    chunks: Vec<(u32, u32)>,
    sha: String,
}

pub(crate) struct Inventory {
    by_entry: BTreeMap<u32, Vec<Known>>,
    /// Whether a module's code is what the analysis holds, once asked.
    present: HashMap<String, bool>,
}

pub(crate) enum Bind {
    Apply,
    /// Not in the analysed code: outside its memory, or another module's
    /// code is there. Nothing to say.
    Elsewhere,
    Skip(String),
}

fn hex(v: &Value) -> Option<u32> {
    crate::commands::addr_value(v)
}

fn chunks(f: &Value) -> Option<Vec<(u32, u32)>> {
    match f["chunks"].as_array() {
        Some(cs) => cs.iter().map(|c| Some((hex(&c[0])?, hex(&c[1])?))).collect(),
        None => Some(vec![(hex(&f["entry"])?, hex(&f["end"])?)]),
    }
}

fn base_name(path: &str) -> String {
    let first = path.split_whitespace().next().unwrap_or(path);
    first.rsplit(['/', '\\', ':']).next().unwrap_or(first).to_lowercase()
}

impl Inventory {
    /// The project's inventory, None without one. Executables' functions are
    /// hashed from the disc, as the inventory hashes overlays'.
    pub(crate) fn load(root: &Path) -> Option<Inventory> {
        let inv: Value = serde_json::from_slice(&std::fs::read(root.join(".psx/inventory.json")).ok()?).ok()?;
        let mut by_entry: BTreeMap<u32, Vec<Known>> = BTreeMap::new();
        let mut add = |module: &str, labels: Vec<String>, executable, f: &Value, sha: String| {
            let (Some(entry), Some(chunks)) = (hex(&f["entry"]), chunks(f)) else { return };
            by_entry.entry(entry).or_default().push(Known { module: module.into(), labels, executable, chunks, sha });
        };
        let exes = inv["executables"].as_array().cloned().unwrap_or_default();
        if !exes.is_empty() {
            use psx_media::disc::Disc;
            use psx_media::iso9660::FileSystem;
            let disc = Disc::open(root.join(inv["media"].as_str()?)).ok()?;
            let fs = FileSystem::read(&disc).ok()?;
            for e in &exes {
                let path = e["path"].as_str().unwrap_or_default();
                let Some(bytes) = fs.find(path).and_then(|f| fs.read_file(&disc, f).ok()) else { continue };
                let Ok(exe) = psx_media::PsxExe::parse(&bytes) else { continue };
                let code = Code { base: 0x8000_0000 | (exe.text_addr & 0x1F_FFFF), bytes: exe.payload(&bytes) };
                for f in e["functions"].as_array().into_iter().flatten() {
                    if let Some(sha) = chunks(f).and_then(|c| code.sha(&c)) {
                        add(path, vec![base_name(path)], true, f, sha);
                    }
                }
            }
        }
        for o in inv["overlays"].as_array().into_iter().flatten() {
            let name = o["name"].as_str().unwrap_or_default();
            let mut labels = vec![name.to_lowercase()];
            for p in [&o["file"]["path"], &o["file"]["unpacked"]["source"]] {
                if let Some(p) = p.as_str() {
                    labels.push(base_name(p));
                }
            }
            for f in o["functions"].as_array().into_iter().flatten() {
                if let Some(sha) = f["sha256"].as_str() {
                    add(name, labels.clone(), false, f, sha.into());
                }
            }
        }
        Some(Inventory { by_entry, present: HashMap::new() })
    }

    /// The inventory's functions a name at `addr` (of `module`, or the
    /// executable's) may be: empty when the inventory has none there and no
    /// module was said, an error when the name cannot be placed.
    fn candidates(&self, addr: u32, module: Option<&str>) -> Result<Vec<&Known>, String> {
        let all = self.by_entry.get(&addr).map(Vec::as_slice).unwrap_or_default();
        let Some(m) = module else {
            let exe: Vec<_> = all.iter().filter(|k| k.executable).collect();
            let known: Vec<_> = if exe.is_empty() { all.iter().collect() } else { exe };
            let modules: BTreeSet<&str> = known.iter().map(|k| k.module.as_str()).collect();
            if modules.len() > 1 {
                return Err(format!(
                    "several overlays have a function at {addr:08X} ({}): say which with `in`",
                    modules.into_iter().collect::<Vec<_>>().join(", ")
                ));
            }
            return Ok(known);
        };
        let lower = m.to_lowercase();
        let known: Vec<_> = all.iter().filter(|k| k.labels.contains(&lower)).collect();
        if known.is_empty() {
            return Err(format!("the inventory has no function of {m} at {addr:08X}"));
        }
        Ok(known)
    }

    /// Whether a function name at `addr` (of `module`, or the executable's)
    /// applies to the analysed code.
    pub(crate) fn function(&self, code: &Code, addr: u32, module: Option<&str>) -> Bind {
        match self.candidates(addr, module) {
            Err(why) => Bind::Skip(why),
            // A function the inventory missed: by its address.
            Ok(known) if known.is_empty() => Bind::Apply,
            Ok(known) if known.iter().any(|k| code.sha(&k.chunks).is_some_and(|s| s == k.sha)) => Bind::Apply,
            Ok(_) => Bind::Elsewhere,
        }
    }

    /// The name of the inventory's function at `entry` in `module` (as the
    /// inventory names it), from the project's names.
    fn name_of(&self, names: &Names, module: &str, entry: u32) -> Option<String> {
        names.functions.iter().filter(|f| f.address == entry).find_map(|f| {
            let known = self.candidates(entry, f.module.as_deref()).ok()?;
            known.iter().any(|k| k.module == module).then(|| label(&f.name))
        })
    }

    /// Whether the analysis holds `module`'s code: most of its functions in
    /// the analysed memory are its own bytes.
    pub(crate) fn holds(&mut self, code: &Code, module: &str) -> bool {
        let m = module.to_lowercase();
        if let Some(&p) = self.present.get(&m) {
            return p;
        }
        let (mut same, mut other) = (0, 0);
        for k in self.by_entry.values().flatten().filter(|k| k.labels.contains(&m)) {
            match code.sha(&k.chunks) {
                Some(s) if s == k.sha => same += 1,
                Some(_) => other += 1,
                None => {}
            }
        }
        let p = same > 0 && same >= other;
        self.present.insert(m, p);
        p
    }
}

/// `addr` in main RAM as KSEG0 (0x80xxxxxx), whatever mirror it is given in.
pub(crate) fn canonical(addr: u32) -> Option<u32> {
    use psx_media::memmap::{Region, resolve};
    let r = resolve(addr);
    (r.region == Region::MainRam).then_some(0x8000_0000 | r.offset)
}

/// A name as views show it: a guess keeps its `?`.
pub(crate) fn label(n: &Name) -> String {
    if n.guess { format!("{}?", n.text) } else { n.text.clone() }
}

/// One code a named function may be: its chunks and their sha256. Bytes
/// that once hashed right are kept, so later checks compare instead.
struct Alt {
    name: String,
    entry: u32,
    chunks: Vec<(u32, u32)>,
    sha: String,
    seen: OnceLock<Vec<u8>>,
}

impl Alt {
    fn holds(&self, image_base: u32, image: &[u8]) -> bool {
        let part = |&(s, e): &(u32, u32)| -> Option<&[u8]> {
            let o = s.checked_sub(image_base)? as usize;
            image.get(o..o + e.checked_sub(s)? as usize)
        };
        if let Some(seen) = self.seen.get() {
            let mut at = 0;
            let same = self.chunks.iter().all(|c| {
                part(c).is_some_and(|b| {
                    at += b.len();
                    seen.get(at - b.len()..at) == Some(b)
                })
            });
            if same {
                return true;
            }
        }
        let Some(bytes) = self.chunks.iter().map(part).collect::<Option<Vec<_>>>().map(|p| p.concat()) else {
            return false;
        };
        let ok = crate::stamp::sha256(&bytes) == self.sha;
        if ok {
            let _ = self.seen.set(bytes);
        }
        ok
    }
}

/// The project's function names as views use them: a name the inventory
/// places is given where its function's bytes are, others by address.
pub(crate) struct Scope {
    alts: Vec<Alt>,
    /// Chunk ranges by start: (start, end, alt).
    intervals: Vec<(u32, u32, usize)>,
    /// Names the inventory does not place, each up to the next function it knows.
    unplaced: psx_symbols::SymbolTable,
    /// Every name, for lookups without the code at hand.
    pub table: psx_symbols::SymbolTable,
}

impl Scope {
    pub(crate) fn new(names: &Names, inv: Option<&Inventory>) -> Option<Scope> {
        if names.functions.is_empty() {
            return None;
        }
        let mut alts = Vec::new();
        let mut unplaced = String::new();
        let mut all = String::new();
        let mut bodies = Vec::new();
        let mut unplaced_bodies = Vec::new();
        for f in &names.functions {
            let name = label(&f.name);
            all.push_str(&format!("{:08X} {name}\n", f.address));
            let known = inv.map(|i| i.candidates(f.address, f.module.as_deref()));
            match known {
                Some(Ok(k)) if !k.is_empty() => {
                    bodies.push((f.address, k[0].chunks.clone()));
                    for k in k {
                        alts.push(Alt {
                            name: name.clone(),
                            entry: f.address,
                            chunks: k.chunks.clone(),
                            sha: k.sha.clone(),
                            seen: OnceLock::new(),
                        });
                    }
                }
                // A name the inventory cannot place is the decompile's to report.
                Some(Err(_)) => {}
                _ => {
                    unplaced.push_str(&format!("{:08X} {name}\n", f.address));
                    let next = inv.and_then(|i| i.by_entry.range(f.address + 1..).next().map(|(&a, _)| a));
                    if let Some(next) = next {
                        unplaced_bodies.push((f.address, vec![(f.address, next)]));
                    }
                }
            }
        }
        let mut intervals: Vec<_> =
            alts.iter().enumerate().flat_map(|(i, a)| a.chunks.iter().map(move |&(s, e)| (s, e, i))).collect();
        intervals.sort_unstable();
        let mut table = psx_symbols::SymbolTable::from_sym_text(&all);
        for (entry, body) in bodies {
            table.set_body(entry, body);
        }
        let mut unplaced = psx_symbols::SymbolTable::from_sym_text(&unplaced);
        for (entry, body) in unplaced_bodies {
            unplaced.set_body(entry, body);
        }
        Some(Scope { alts, intervals, unplaced, table })
    }

    /// The project's names, read again when its names or inventory change.
    pub(crate) fn project() -> Option<Arc<Scope>> {
        type Key = (Arc<Names>, Option<Arc<Inventory>>);
        static SCOPE: Mutex<Option<(Key, Option<Arc<Scope>>)>> = Mutex::new(None);
        let key = (project_names(), project_inventory());
        let mut cache = SCOPE.lock().unwrap_or_else(|e| e.into_inner());
        let same =
            |k: &Key| Arc::ptr_eq(&k.0, &key.0) && k.1.as_ref().map(Arc::as_ptr) == key.1.as_ref().map(Arc::as_ptr);
        if let Some((_, scope)) = cache.as_ref().filter(|(k, _)| same(k)) {
            return scope.clone();
        }
        let scope = Scope::new(&key.0, key.1.as_deref()).map(Arc::new);
        *cache = Some((key, scope.clone()));
        scope
    }

    /// `name` or `name+0xOFF` for `pc` in `image` (loaded at `image_base`),
    /// and whether the name was checked against the function's bytes.
    pub(crate) fn describe(&self, pc: u32, image_base: u32, image: &[u8], distance: u32) -> Option<(String, bool)> {
        let pc = canonical(pc)?;
        let i = self.intervals.partition_point(|r| r.0 <= pc);
        for &(_, end, a) in self.intervals[..i].iter().rev() {
            let alt = &self.alts[a];
            if pc < end && alt.holds(image_base, image) {
                let off = pc.wrapping_sub(alt.entry);
                return Some((if off == 0 { alt.name.clone() } else { format!("{}+0x{off:X}", alt.name) }, true));
            }
        }
        self.unplaced.describe(pc, distance).map(|n| (n, false))
    }
}

/// What a cached read depends on: the project root and each file's size
/// and time, so an edit or another project reads again.
type Stamp = (std::path::PathBuf, Vec<(std::path::PathBuf, u64, Option<SystemTime>)>);

fn stamp(files: Vec<std::path::PathBuf>) -> Stamp {
    let cwd = std::env::current_dir().unwrap_or_default();
    let files = files
        .into_iter()
        .map(|p| {
            let m = std::fs::metadata(&p).ok();
            let len = m.as_ref().map_or(0, |m| m.len());
            let time = m.and_then(|m| m.modified().ok());
            (p, len, time)
        })
        .collect();
    (cwd, files)
}

/// `make`'s value, kept in `cache` until `key` changes.
fn fresh<T>(cache: &Mutex<Option<(Stamp, T)>>, key: Stamp, make: impl FnOnce() -> T) -> T
where
    T: Clone,
{
    let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, v)) = c.as_ref() {
        if *k == key {
            return v.clone();
        }
    }
    let v = make();
    *c = Some((key, v.clone()));
    v
}

/// The project's inventory, read again when it changes.
fn project_inventory() -> Option<Arc<Inventory>> {
    static INVENTORY: Mutex<Option<(Stamp, Option<Arc<Inventory>>)>> = Mutex::new(None);
    let key = stamp(vec![Path::new(".psx/inventory.json").to_path_buf()]);
    fresh(&INVENTORY, key, || Inventory::load(Path::new(".")).map(Arc::new))
}

/// The code of the inventory's function at `entry` (of `module`, or the
/// executable's), as its chunks.
pub(crate) fn body(entry: u32, module: Option<&str>) -> Option<Vec<(u32, u32)>> {
    project_inventory()?.candidates(entry, module).ok()?.first().map(|k| k.chunks.clone())
}

/// The project's name for the inventory's function at `entry` in `module`
/// (an executable's path or an overlay's name, as the inventory has them).
pub(crate) fn function_name(module: &str, entry: u32) -> Option<String> {
    project_inventory()?.name_of(&project_names(), module, entry)
}

/// `row` with the project's name for its function, when there is one.
pub(crate) fn name_row(row: &mut Value, module: &str, entry: u32) {
    if let Some(n) = function_name(module, entry) {
        row["name"] = Value::String(n);
    }
}

pub(crate) fn named(mut row: Value, module: &str, entry: u32) -> Value {
    name_row(&mut row, module, entry);
    row
}

/// The project's names (names/ and .psx/names/ under the current
/// directory, which psx makes the project root), read again when a file
/// there changes: one MCP process sees the agent's edits.
pub(crate) fn project_names() -> Arc<Names> {
    static NAMES: Mutex<Option<(Stamp, Arc<Names>)>> = Mutex::new(None);
    let key = stamp(psx_symbols::names::files(Path::new(".")));
    fresh(&NAMES, key, || Arc::new(psx_symbols::names::load(Path::new("."))))
}

/// An address argument: hex, or a name from the project's names files,
/// `[MODULE:]name[+OFFSET]` (a function's or a global's). Hex wins unless
/// a name is spelled the same and no `0x` is given.
pub(crate) fn address(spec: &str) -> Result<u32, String> {
    let spec = spec.trim();
    match NamePath::parse(spec) {
        Some(Ok(p)) => {
            return p.fixed().ok_or_else(|| {
                format!("{spec:?} reads a pointer: give it where memory is read (sample, until, watch, debug)")
            });
        }
        Some(Err(e)) => return Err(e),
        None => {}
    }
    let hex = psx_media::memmap::parse_hex(spec);
    let names = project_names();
    if let Some(h) = hex {
        if spec.starts_with("0x") || spec.starts_with("0X") || find(&names, spec).is_empty() {
            return Ok(h);
        }
    }
    let (base, off) = match spec.split_once('+') {
        Some((b, o)) => (
            b.trim(),
            parse_offset(o.trim()).ok_or_else(|| format!("bad offset {o:?} in {spec:?} (hex 0x.. or decimal)"))?,
        ),
        None => (spec, 0),
    };
    let found = find(&names, base);
    match found.as_slice() {
        [a] => Ok(a.wrapping_add(off)),
        [] => Err(format!("{base:?} is neither a hex address nor a name in names/")),
        many => Err(format!(
            "{base:?} names several addresses ({}): say which with MODULE:{base}",
            many.iter().map(|a| format!("{a:08X}")).collect::<Vec<_>>().join(", ")
        )),
    }
}

fn parse_offset(s: &str) -> Option<u32> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

/// The addresses `[MODULE:]name` names, without repeats.
fn find(names: &Names, spec: &str) -> Vec<u32> {
    let (module, name) = match spec.rsplit_once(':') {
        Some((m, n)) => (Some(m), n),
        None => (None, spec),
    };
    let name = name.trim_end_matches('?');
    let module_ok = |m: &Option<String>| module.is_none_or(|x| m.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(x)));
    let mut out: Vec<u32> = names
        .functions
        .iter()
        .filter(|f| f.name.text == name && module_ok(&f.module))
        .map(|f| f.address)
        .chain(names.globals.iter().filter(|g| g.name.text == name && module_ok(&g.module)).map(|g| g.address))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// A names path: a global of names/ that has a type, then `.field`,
/// `->field` and `[N]` steps (`car_0->wheels[2].load`). A step through a
/// pointer reads the pointer where the path is read.
#[derive(Clone)]
pub(crate) struct NamePath {
    pub text: String,
    base: u32,
    ty: psx_symbols::names::Type,
    steps: Vec<Step>,
    /// The type it ends at; it does not depend on memory.
    pub leaf: psx_symbols::names::Type,
    /// Whether a step reads a pointer.
    pub derefs: bool,
    names: Arc<Names>,
}

impl std::fmt::Debug for NamePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

#[derive(Clone, Debug)]
enum Step {
    Field(String),
    Arrow(String),
    Index(u32),
}

impl NamePath {
    /// `spec` as a path, by the project's names: None where it is none (hex,
    /// a function, `name+OFF`, a global without a type and no steps).
    pub(crate) fn parse(spec: &str) -> Option<Result<NamePath, String>> {
        NamePath::parse_in(project_names(), spec)
    }

    pub(crate) fn parse_in(names: Arc<Names>, spec: &str) -> Option<Result<NamePath, String>> {
        let text = spec.trim();
        let (module, body) = match text.rsplit_once(':') {
            Some((m, b)) => (Some(m), b),
            None => (None, text),
        };
        let end = [body.find('.'), body.find('['), body.find("->")].into_iter().flatten().min().unwrap_or(body.len());
        let (name, rest) = body.split_at(end);
        let name = name.trim().trim_end_matches('?');
        if name.is_empty() || name.contains(['+', '-', ' ']) {
            return None;
        }
        let module_ok =
            |m: &Option<String>| module.is_none_or(|x| m.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(x)));
        let globals: Vec<&psx_symbols::names::Global> =
            names.globals.iter().filter(|g| g.name.text == name && module_ok(&g.module)).collect();
        let fail = |e: String| Some(Err(format!("{text:?}: {e}")));
        let g = match globals.as_slice() {
            [] if rest.is_empty() => return None,
            [] => return fail(format!("{name} is neither a hex address nor a global of names/")),
            [g] => g,
            _ => return fail(format!("{name} is several globals: say which with MODULE:{name}")),
        };
        let Some(ty) = g.ty.clone() else {
            return if rest.is_empty() { None } else { fail(format!("{name} has no type in names/")) };
        };
        let steps = match steps(rest) {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        let mut path =
            NamePath { text: text.to_string(), base: g.address, ty: ty.clone(), steps, leaf: ty, derefs: false, names };
        // Walked once with every pointer at the start of RAM, for the type
        // it ends at and the steps' errors.
        let reads = std::cell::Cell::new(0);
        match path.resolve(&|_| {
            reads.set(reads.get() + 1);
            Some(0x8000_0000)
        }) {
            Ok(Some((_, leaf))) => {
                path.leaf = leaf;
                path.derefs = reads.get() > 0;
                Some(Ok(path))
            }
            Ok(None) => unreachable!("every read answers"),
            Err(e) => fail(e),
        }
    }

    /// Where the path leads now and its type: none where a pointer on the
    /// way cannot be read or is null.
    pub(crate) fn resolve(
        &self,
        read: &dyn Fn(u32) -> Option<u32>,
    ) -> Result<Option<(u32, psx_symbols::names::Type)>, String> {
        use psx_symbols::names::Type;
        let n = &*self.names;
        let (mut addr, mut ty) = (self.base, self.ty.clone());
        let size = |t: &Type| n.size_of(t).ok_or_else(|| format!("the size of {} is not known", type_text(t)));
        for step in &self.steps {
            match step {
                Step::Field(f) => (addr, ty) = field(n, addr, &ty, f)?,
                Step::Arrow(f) => {
                    let Type::Pointer { to, offset } = plain(n, &ty) else {
                        return Err(format!("->{f}: {} is not a pointer", type_text(&ty)));
                    };
                    let Some(p) = read(addr).filter(|&p| p != 0) else { return Ok(None) };
                    (addr, ty) = field(n, p.wrapping_sub(offset), &to, f)?;
                }
                &Step::Index(i) => match plain(n, &ty) {
                    Type::Array { of, len } => {
                        if i >= len {
                            return Err(format!("[{i}]: {} has {len}", type_text(&ty)));
                        }
                        addr = addr.wrapping_add(i * size(&of)?);
                        ty = *of;
                    }
                    Type::Pointer { to, offset } => {
                        let Some(p) = read(addr).filter(|&p| p != 0) else { return Ok(None) };
                        addr = p.wrapping_sub(offset).wrapping_add(i * size(&to)?);
                        ty = *to;
                    }
                    _ => return Err(format!("[{i}]: {} is not an array or a pointer", type_text(&ty))),
                },
            }
        }
        Ok(Some((addr, ty)))
    }

    /// The address, for a path that reads no pointer.
    pub(crate) fn fixed(&self) -> Option<u32> {
        if self.derefs { None } else { self.resolve(&|_| None).ok().flatten().map(|(a, _)| a) }
    }

    /// The leaf type's size in bytes, when known.
    pub(crate) fn size(&self) -> Option<u32> {
        self.names.size_of(&self.leaf)
    }

    /// The leaf type with typedefs seen through.
    pub(crate) fn plain_leaf(&self) -> psx_symbols::names::Type {
        plain(&self.names, &self.leaf)
    }

    /// Whether the leaf is an enum (an unsigned number of its size).
    pub(crate) fn leaf_is_enum(&self) -> bool {
        use psx_symbols::names::{Type, TypeBody};
        matches!(plain(&self.names, &self.leaf), Type::Named { name }
            if matches!(self.names.type_named(&name).map(|t| &t.body), Some(TypeBody::Enum { .. })))
    }
}

/// `.a`, `->b` and `[N]` steps.
fn steps(mut s: &str) -> Result<Vec<Step>, String> {
    let ident = |s: &str| -> (String, usize) {
        let n = s.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '?')).unwrap_or(s.len());
        (s[..n].trim_end_matches('?').to_string(), n)
    };
    let mut out = Vec::new();
    while !s.is_empty() {
        if let Some(r) = s.strip_prefix("->").or_else(|| s.strip_prefix('.')) {
            let arrow = s.starts_with("->");
            let (f, n) = ident(r);
            if f.is_empty() {
                return Err(format!("a field's name after {}", if arrow { "->" } else { "." }));
            }
            out.push(if arrow { Step::Arrow(f) } else { Step::Field(f) });
            s = &r[n..];
        } else if let Some(r) = s.strip_prefix('[') {
            let (i, r) = r.split_once(']').ok_or("[ without ]")?;
            out.push(Step::Index(parse_offset(i.trim()).ok_or_else(|| format!("[{i}]: an index, decimal or 0x hex"))?));
            s = r;
        } else {
            return Err(format!("{s:?}: steps are .field, ->field and [N]"));
        }
    }
    Ok(out)
}

/// `ty` with typedefs seen through.
fn plain(n: &Names, ty: &psx_symbols::names::Type) -> psx_symbols::names::Type {
    use psx_symbols::names::{Type, TypeBody};
    let mut ty = ty.clone();
    for _ in 0..16 {
        let Type::Named { name } = &ty else { break };
        match n.type_named(name).map(|t| &t.body) {
            Some(TypeBody::Typedef { ty: t }) => ty = t.clone(),
            _ => break,
        }
    }
    ty
}

/// The field `f` of a struct or union at `addr`.
fn field(
    n: &Names,
    addr: u32,
    ty: &psx_symbols::names::Type,
    f: &str,
) -> Result<(u32, psx_symbols::names::Type), String> {
    use psx_symbols::names::{Type, TypeBody};
    let fields = match plain(n, ty) {
        Type::Named { name } => match n.type_named(&name).map(|t| &t.body) {
            Some(TypeBody::Struct { fields } | TypeBody::Union { fields }) => fields.clone(),
            _ => return Err(format!(".{f}: {name} is not a struct or union")),
        },
        t => return Err(format!(".{f}: {} is not a struct or union", type_text(&t))),
    };
    let found = fields.iter().find(|x| x.name.text == f).ok_or_else(|| {
        let known: Vec<&str> = fields.iter().map(|x| x.name.text.as_str()).collect();
        format!("{} has no field {f} (it has {})", type_text(ty), known.join(", "))
    })?;
    Ok((addr.wrapping_add(found.offset), found.ty.clone()))
}

fn type_text(ty: &psx_symbols::names::Type) -> String {
    use psx_symbols::names::Type;
    match ty {
        Type::Named { name } => name.clone(),
        Type::Pointer { to, .. } => format!("{} *", type_text(to)),
        Type::Array { of, len } => format!("{}[{len}]", type_text(of)),
        Type::Function(_) => "a function".into(),
    }
}

/// The global holding `addr`, as `name` or `name+0xOFF` within its type's size.
/// Where `off` falls within a `ty`: `.field`, `[k]` and `+0xN` steps
/// (empty at the start of a base type).
pub(crate) fn path_in(names: &Names, ty: &psx_symbols::names::Type, off: u32) -> String {
    use psx_symbols::names::{Type, TypeBody};
    let rest = |off: u32| if off == 0 { String::new() } else { format!("+0x{off:X}") };
    match ty {
        Type::Named { name } => match names.type_named(name).map(|t| &t.body) {
            Some(TypeBody::Struct { fields } | TypeBody::Union { fields }) => fields
                .iter()
                .find(|f| f.offset <= off && off < f.offset + names.size_of(&f.ty).unwrap_or(1).max(1))
                .map_or_else(|| rest(off), |f| format!(".{}{}", label(&f.name), path_in(names, &f.ty, off - f.offset))),
            Some(TypeBody::Typedef { ty }) => path_in(names, ty, off),
            _ => rest(off),
        },
        Type::Array { of, .. } => {
            let size = names.size_of(of).unwrap_or(1).max(1);
            format!("[{}]{}", off / size, path_in(names, of, off % size))
        }
        _ => rest(off),
    }
}

/// `addr` named through a pointer argument: `params` are each pointer
/// parameter's name, type and value at the call.
pub(crate) fn through_params(
    names: &Names,
    params: &[(String, psx_symbols::names::Type, u32)],
    addr: u32,
) -> Option<String> {
    use psx_symbols::names::Type;
    let addr = canonical(addr)?;
    params.iter().find_map(|(name, ty, value)| {
        let Type::Pointer { to, offset } = ty else { return None };
        let base = canonical(value.wrapping_sub(*offset))?;
        let size = names.size_of(to)?.max(1);
        let off = addr.checked_sub(base).filter(|o| *o < size)?;
        let path = path_in(names, to, off);
        Some(match path.strip_prefix('.') {
            Some(p) => format!("{name}->{p}"),
            None if path.is_empty() => format!("*{name}"),
            None => format!("(*{name}){path}"),
        })
    })
}

pub(crate) fn global_at(names: &Names, addr: u32) -> Option<String> {
    let addr = canonical(addr).unwrap_or(addr);
    names
        .globals
        .iter()
        .filter(|g| g.address <= addr)
        .filter_map(|g| {
            let size = g.ty.as_ref().and_then(|t| names.size_of(t)).unwrap_or(1).max(1);
            let off = addr - g.address;
            (off < size).then(|| if off == 0 { label(&g.name) } else { format!("{}+0x{off:X}", label(&g.name)) })
        })
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_names_file_edited_is_read_again() {
        let dir = std::env::temp_dir().join(format!("psx-names-fresh-{}", std::process::id()));
        let file = dir.join("names/a.names");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "function 0x80010000 one\n").unwrap();
        let cache = Mutex::new(None);
        let read = || {
            let key = stamp(psx_symbols::names::files(&dir));
            fresh(&cache, key, || Arc::new(psx_symbols::names::load(&dir)))
        };
        let first = read();
        assert!(Arc::ptr_eq(&first, &read()), "unchanged files are not read again");
        std::fs::write(&file, "function 0x80010000 one\nfunction 0x80020000 two\n").unwrap();
        std::fs::write(dir.join("names/b.names"), "global 0x80030000 u32 three\n").unwrap();
        let second = read();
        assert_eq!(first.functions.len(), 1);
        assert_eq!(second.functions.len(), 2);
        assert_eq!(second.globals.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn inventory(overlays: Value) -> Inventory {
        let mut by_entry: BTreeMap<u32, Vec<Known>> = BTreeMap::new();
        for o in overlays.as_array().unwrap() {
            let name = o["name"].as_str().unwrap();
            for f in o["functions"].as_array().unwrap() {
                by_entry.entry(hex(&f["entry"]).unwrap()).or_default().push(Known {
                    module: name.into(),
                    labels: vec![name.to_lowercase(), base_name(o["file"].as_str().unwrap())],
                    executable: o["exe"] == true,
                    chunks: chunks(f).unwrap(),
                    sha: f["sha256"].as_str().unwrap().into(),
                });
            }
        }
        Inventory { by_entry, present: HashMap::new() }
    }

    #[test]
    fn overlays_at_one_address_are_told_apart_by_their_bytes() {
        let a = [1u8, 0, 0, 0, 2, 0, 0, 0];
        let b = [3u8, 0, 0, 0, 4, 0, 0, 0];
        let f = |sha: &[u8]| json!([{"entry": "0x80100000", "end": "0x80100008", "sha256": crate::stamp::sha256(sha)}]);
        let mut inv = inventory(json!([
            {"name": "0x80100000 /RACE.OVL", "file": "/RACE.OVL", "functions": f(&a)},
            {"name": "0x80100000 /MENU.OVL", "file": "/MENU.OVL", "functions": f(&b)},
        ]));
        let race = Code { base: 0x8010_0000, bytes: &a };
        assert!(matches!(inv.function(&race, 0x8010_0000, Some("race.ovl")), Bind::Apply));
        assert!(matches!(inv.function(&race, 0x8010_0000, Some("MENU.OVL")), Bind::Elsewhere));
        assert!(matches!(inv.function(&race, 0x8010_0000, None), Bind::Skip(m) if m.contains("say which")));
        assert!(
            matches!(inv.function(&race, 0x8010_0000, Some("CAR.OVL")), Bind::Skip(m) if m.contains("no function of CAR.OVL"))
        );
        // A function the inventory does not know applies by its address.
        assert!(matches!(inv.function(&race, 0x8010_0004, None), Bind::Apply));
        let elsewhere = Code { base: 0x8020_0000, bytes: &a };
        assert!(matches!(inv.function(&elsewhere, 0x8010_0000, Some("RACE.OVL")), Bind::Elsewhere));
        assert!(inv.holds(&race, "RACE.OVL"));
        assert!(!inv.holds(&race, "MENU.OVL"));
    }

    #[test]
    fn a_name_without_a_module_is_the_executables() {
        let a = [1u8, 0, 0, 0];
        let b = [3u8, 0, 0, 0];
        let f = |sha: &[u8]| json!([{"entry": "0x80010000", "end": "0x80010004", "sha256": crate::stamp::sha256(sha)}]);
        let inv = inventory(json!([
            {"name": "/SCUS_944.88", "file": "/SCUS_944.88", "exe": true, "functions": f(&a)},
            {"name": "0x80010000 /GT2.OVL @ 30 (unpacked)", "file": "/GT2.OVL @ 30 (unpacked)", "functions": f(&b)},
        ]));
        assert!(matches!(inv.function(&Code { base: 0x8001_0000, bytes: &a }, 0x8001_0000, None), Bind::Apply));
        let ovl = Code { base: 0x8001_0000, bytes: &b };
        assert!(matches!(inv.function(&ovl, 0x8001_0000, None), Bind::Elsewhere));
        assert!(matches!(inv.function(&ovl, 0x8001_0000, Some("gt2.ovl")), Bind::Apply));
    }
    fn names(text: &str) -> Names {
        psx_symbols::names::from_texts(&[("names/a.names".into(), text.into())], &[])
    }

    #[test]
    fn views_name_a_function_where_its_bytes_are() {
        let a = [1u8, 0, 0, 0, 2, 0, 0, 0];
        let b = [3u8, 0, 0, 0, 4, 0, 0, 0];
        let f = |sha: &[u8]| json!([{"entry": "0x80100000", "end": "0x80100008", "sha256": crate::stamp::sha256(sha)}]);
        let mut inv = inventory(json!([
            {"name": "0x80100000 /RACE.OVL", "file": "/RACE.OVL", "functions": f(&a)},
            {"name": "0x80100000 /MENU.OVL", "file": "/MENU.OVL", "functions": f(&b)},
        ]));
        inv.by_entry.entry(0x8020_0000).or_default();
        let n = names("function 0x80100000 in RACE.OVL race_update?\nfunction 0x80180000 loose\n");
        let scope = Scope::new(&n, Some(&inv)).unwrap();
        assert_eq!(scope.describe(0x8010_0004, 0x8010_0000, &a, 0x1000), Some(("race_update?+0x4".into(), true)));
        // Twice, the second from the kept bytes.
        assert_eq!(scope.describe(0xA010_0000, 0x8010_0000, &a, 0x1000), Some(("race_update?".into(), true)));
        assert_eq!(scope.describe(0x8010_0004, 0x8010_0000, &b, 0x1000), None);
        // A name the inventory does not place reaches up to its next function.
        assert_eq!(scope.describe(0x8018_0010, 0x8010_0000, &a, 0x1000), Some(("loose+0x10".into(), false)));
        assert_eq!(scope.describe(0x8020_0000, 0x8010_0000, &a, 0x10_0000), None);
    }

    #[test]
    fn names_resolve_to_addresses_and_globals_cover_their_type() {
        let n = names(
            "struct Pos {\n 0x0 int x\n 0x4 int y\n}\nglobal 0x80075000 Pos croc_pos\nglobal 0x80075010 STEP\n\
             function 0x80031F64 world_collide\nfunction 0x80100000 in RACE.OVL update\nfunction 0x80100000 in MENU.OVL update\n",
        );
        assert_eq!(find(&n, "world_collide"), vec![0x8003_1F64]);
        assert_eq!(find(&n, "croc_pos"), vec![0x8007_5000]);
        assert_eq!(find(&n, "update"), vec![0x8010_0000], "one address, though two modules");
        assert_eq!(find(&n, "race.ovl:update"), vec![0x8010_0000]);
        assert!(find(&n, "CAR.OVL:update").is_empty());
        assert_eq!((parse_offset("0x370"), parse_offset("16"), parse_offset("x")), (Some(0x370), Some(16), None));
        assert_eq!(global_at(&n, 0x8007_5000).as_deref(), Some("croc_pos"));
        assert_eq!(global_at(&n, 0xA007_5004).as_deref(), Some("croc_pos+0x4"));
        assert_eq!(global_at(&n, 0x8007_5008), None);
        // An untyped global covers its first byte.
        assert_eq!(global_at(&n, 0x8007_5010).as_deref(), Some("STEP"));
        assert_eq!(global_at(&n, 0x8007_5011), None);
    }

    #[test]
    fn a_names_path_follows_fields_indexes_and_pointers() {
        let n = Arc::new(names(
            "struct Wheel {\n 0x0 i16 load\n 0x2 u8 grip\n}\nstruct Car {\n 0x4 Wheel wheels[4]\n 0x14 Car *next\n}\n\
             global 0x80070000 Car car\nglobal 0x80070100 Car *car_ptr\nglobal 0x80070200 STEP\n",
        ));
        let path = |s: &str| NamePath::parse_in(n.clone(), s);
        let p = path("car.wheels[2].grip").unwrap().unwrap();
        assert_eq!((p.fixed(), p.derefs, p.size()), (Some(0x8007_000C), false, Some(1)));
        let mem: HashMap<u32, u32> = [(0x8007_0100, 0x8008_0000), (0x8008_0014, 0x8009_0000)].into();
        let read = |a: u32| mem.get(&a).copied();
        let p = path("car_ptr->next->wheels[1].load").unwrap().unwrap();
        assert!(p.derefs && p.fixed().is_none());
        let (at, leaf) = p.resolve(&read).unwrap().unwrap();
        assert_eq!((at, leaf), (0x8009_0007, psx_symbols::names::Type::Named { name: "i16".into() }));
        assert_eq!(path("car_ptr[1].wheels[0].load").unwrap().unwrap().resolve(&read).unwrap().unwrap().0, 0x8008_001C);
        // A pointer that cannot be read leads nowhere.
        assert_eq!(path("car.next->next").unwrap().unwrap().resolve(&read).unwrap(), None);
        // Not paths: hex, an untyped label, an offset; mistakes name themselves.
        assert!(path("80070000").is_none() && path("STEP").is_none() && path("car+4").is_none());
        assert!(path("car.wheel").unwrap().unwrap_err().contains("no field wheel (it has wheels, next)"));
        assert!(path("car.wheels[4]").unwrap().unwrap_err().contains("has 4"));
        assert!(path("car->wheels").unwrap().unwrap_err().contains("not a pointer"));
        assert!(path("nope.x").unwrap().unwrap_err().contains("nope is neither a hex address nor a global"));
        assert!(path("STEP.x").unwrap().unwrap_err().contains("no type"));
    }
}
