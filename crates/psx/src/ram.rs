//! RAM analysis for gameplay reverse engineering: value formats, per-frame
//! samples and pokes for `psx run`, and `psx ram
//! diff/search/view` over `ram.bin` snapshots from several runs.
//!
//! Addresses are hex (`8015E8A8`, `0x8015E8A8`, any KUSEG/KSEG0/KSEG1
//! mirror); main RAM (2 MiB) and the scratchpad (0x1F800000, 1 KiB) are
//! readable. Values are little-endian and read byte by byte from the emulated
//! memory, so partial-width stores (a halfword into a word) are always seen.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use psx_media::memmap::parse_hex;
use psx_media::MediaError;
use serde_json::{json, Value};

pub const RAM_SIZE: usize = 2 * 1024 * 1024;
pub const SCRATCH_SIZE: usize = 1024;

type R = psx_media::Result<Value>;

fn bad(m: impl std::fmt::Display) -> MediaError {
    MediaError::Malformed(m.to_string())
}

/// Parse a number: decimal (optionally negative) or 0x-prefixed hex.
pub fn number(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, s),
    };
    let v = match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        Some(h) => i64::from_str_radix(h, 16).ok()?,
        None => body.parse::<i64>().ok()?,
    };
    Some(if neg { -v } else { v })
}

/// Which memory an address lands in, with the byte offset into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mem {
    Ram(usize),
    Scratch(usize),
}

pub fn locate(addr: u32, width: usize) -> Option<Mem> {
    let resolved = psx_media::memmap::resolve(addr);
    let offset = resolved.offset as usize;
    let (limit, memory) = match resolved.region {
        psx_media::memmap::Region::MainRam => (RAM_SIZE, Mem::Ram(offset)),
        psx_media::memmap::Region::Scratchpad => (SCRATCH_SIZE, Mem::Scratch(offset)),
        _ => return None,
    };
    offset.checked_add(width).filter(|&end| end <= limit).map(|_| memory)
}

/// Little-endian read of `width` (1..=4) bytes.
pub fn read_le(bytes: &[u8], off: usize, width: usize) -> u32 {
    bytes[off..off + width].iter().rev().fold(0u32, |acc, &b| (acc << 8) | b as u32)
}

pub fn write_le(bytes: &mut [u8], off: usize, width: usize, value: u32) {
    for (i, b) in bytes[off..off + width].iter_mut().enumerate() {
        *b = (value >> (8 * i)) as u8;
    }
}

/// Sign-extend the low `width` bytes of `v`.
pub fn signed(v: u32, width: usize) -> i64 {
    match width {
        1 => v as u8 as i8 as i64,
        2 => v as u16 as i16 as i64,
        _ => v as i32 as i64,
    }
}

/// How a sampled value is read and printed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fmt {
    Unsigned(u8),
    Signed(u8),
    Hex(u8),
    /// Signed fixed point: total bytes, fraction bits (`fx16.16` = (4, 16)).
    Fixed(u8, u8),
}

impl Fmt {
    pub fn parse(s: &str) -> Option<Fmt> {
        let s = s.trim().to_ascii_lowercase();
        let w = |n: &str| match n {
            "8" => Some(1),
            "16" => Some(2),
            "32" => Some(4),
            _ => None,
        };
        if let Some(rest) = s.strip_prefix("fx") {
            let (i, f) = rest.split_once('.')?;
            let (i, f): (u8, u8) = (i.parse().ok()?, f.parse().ok()?);
            let total = i as u16 + f as u16;
            return match total {
                16 => Some(Fmt::Fixed(2, f)),
                32 => Some(Fmt::Fixed(4, f)),
                _ => None,
            };
        }
        if let Some(n) = s.strip_prefix('u') {
            return w(n).map(Fmt::Unsigned);
        }
        if let Some(n) = s.strip_prefix('i').or_else(|| s.strip_prefix('s')) {
            return w(n).map(Fmt::Signed);
        }
        if let Some(n) = s.strip_prefix('x') {
            return w(n).map(Fmt::Hex);
        }
        None
    }

    pub fn width(self) -> usize {
        match self {
            Fmt::Unsigned(w) | Fmt::Signed(w) | Fmt::Hex(w) | Fmt::Fixed(w, _) => w as usize,
        }
    }

    pub fn name(self) -> String {
        match self {
            Fmt::Unsigned(w) => format!("u{}", w * 8),
            Fmt::Signed(w) => format!("i{}", w * 8),
            Fmt::Hex(w) => format!("x{}", w * 8),
            Fmt::Fixed(w, f) => format!("fx{}.{f}", w * 8 - f),
        }
    }

    /// Text form (CSV cells, tables).
    pub fn render(self, raw: u32) -> String {
        match self {
            Fmt::Unsigned(_) => raw.to_string(),
            Fmt::Signed(w) => signed(raw, w as usize).to_string(),
            Fmt::Hex(w) => format!("0x{raw:0width$X}", width = w as usize * 2),
            Fmt::Fixed(w, f) => {
                // Exact decimal: frac / 2^f = frac * 5^f / 10^f.
                let v = signed(raw, w as usize);
                let a = v.unsigned_abs() as u128;
                let (int, frac) = (a >> f, a & ((1u128 << f) - 1));
                let digits = format!("{:0n$}", frac * 5u128.pow(f as u32), n = f as usize);
                let digits = digits.trim_end_matches('0');
                format!("{}{int}.{}", if v < 0 { "-" } else { "" }, if digits.is_empty() { "0" } else { digits })
            }
        }
    }

    pub fn json(self, raw: u32) -> Value {
        match self {
            Fmt::Unsigned(_) => json!(raw),
            Fmt::Signed(w) => json!(signed(raw, w as usize)),
            Fmt::Hex(_) => json!(self.render(raw)),
            Fmt::Fixed(w, f) => json!(signed(raw, w as usize) as f64 / (1u64 << f) as f64),
        }
    }

    /// Encode a value written in this format (`1.5` for fx16.16, `-2`, `0x80`).
    pub fn encode(self, s: &str) -> Option<u32> {
        let w = self.width();
        let mask = if w == 4 { u32::MAX } else { (1u32 << (8 * w)) - 1 };
        let v = match self {
            Fmt::Fixed(_, f) if number(s).is_none() => {
                let x: f64 = s.trim().parse().ok()?;
                (x * (1u64 << f) as f64).round() as i64
            }
            Fmt::Fixed(_, f) => number(s)? << f,
            _ => number(s)?,
        };
        Some(v as u32 & mask)
    }
}

/// One `--sample ADDR:FMT[:NAME]` column.
#[derive(Clone, Debug)]
pub struct Sample {
    pub addr: u32,
    pub fmt: Fmt,
    pub name: String,
    pub mem: Mem,
}

pub fn parse_sample(s: &str) -> Result<Sample, String> {
    let mut it = s.splitn(3, ':');
    let (a, f, n) = (it.next().unwrap_or(""), it.next().unwrap_or("u32"), it.next());
    sample(a, f, n, &format!("--sample {s:?}"))
}

/// Shared by the `ADDR:FMT[:NAME]` text and typed forms. A bare byte width
/// (1, 2, 4, as in watches) reads unsigned.
fn sample(a: &str, f: &str, name: Option<&str>, label: &str) -> Result<Sample, String> {
    let addr = parse_hex(a).ok_or_else(|| format!("{label}: bad hex address {a:?}"))?;
    let fmt = match f.trim() {
        "1" => Fmt::Unsigned(1),
        "2" => Fmt::Unsigned(2),
        "4" => Fmt::Unsigned(4),
        f => Fmt::parse(f).ok_or_else(|| format!("{label}: unknown format {f:?} (u8 i8 x8 u16 i16 x16 u32 i32 x32 fxI.F, e.g. fx16.16 or fx4.12, or a width 1/2/4)"))?,
    };
    let mem = locate(addr, fmt.width()).ok_or_else(|| format!("{label}: 0x{addr:08X} is not main RAM or scratchpad"))?;
    let name = name.filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| format!("0x{addr:08X}:{}", fmt.name()));
    if name.contains([',', '"', '\n']) {
        return Err(format!("{label}: names cannot contain commas or quotes"));
    }
    Ok(Sample { addr, fmt, name, mem })
}

/// A `sample` column as given: `ADDR:FMT[:NAME]` text, or over MCP/JSON a
/// typed `{"addr", "format", "name"}` object.
#[derive(Clone, Debug, schemars::JsonSchema)]
#[serde(untagged)]
pub enum SampleSpec {
    /// `ADDR:FMT[:NAME]`, e.g. `8015E8AC:fx16.16:y`.
    Text(String),
    Typed(TypedSample),
}

#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TypedSample {
    /// Hex address in main RAM or the scratchpad, e.g. `8015E8AC`.
    pub addr: String,
    /// As in the text form (`i16`, `fx16.16`, width `4`); default u32.
    #[serde(default, deserialize_with = "format_or_width")]
    pub format: Option<String>,
    /// CSV column name (default `0xADDR:FMT`).
    #[serde(default)]
    pub name: Option<String>,
}

/// `"fx16.16"`, or a JSON number width as in watches (`4`).
fn format_or_width<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    match <Value as serde::Deserialize>::deserialize(d)? {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s)),
        Value::Number(n) => Ok(Some(n.to_string())),
        v => Err(serde::de::Error::custom(format!("format {v}: expected a string such as \"i16\" or a width 1/2/4"))),
    }
}

impl<'de> serde::Deserialize<'de> for SampleSpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // By hand rather than untagged, so a bad object names its field.
        match <Value as serde::Deserialize>::deserialize(d)? {
            Value::String(s) => Ok(SampleSpec::Text(s)),
            v @ Value::Object(_) => serde_json::from_value(v).map(SampleSpec::Typed).map_err(|e| serde::de::Error::custom(format!("sample: {e}"))),
            v => Err(serde::de::Error::custom(format!("sample {v}: expected \"ADDR:FMT[:NAME]\" or {{\"addr\", \"format\", \"name\"}}"))),
        }
    }
}

impl std::str::FromStr for SampleSpec {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Ok(SampleSpec::Text(s.to_string()))
    }
}

impl SampleSpec {
    pub fn parse(&self) -> Result<Sample, String> {
        match self {
            SampleSpec::Text(s) => parse_sample(s),
            SampleSpec::Typed(t) => sample(&t.addr, t.format.as_deref().unwrap_or("u32"), t.name.as_deref(), &format!("sample {:?}", t.addr)),
        }
    }
}

pub fn read_mem(ram: &[u8], scratch: &[u8], mem: Mem, width: usize) -> u32 {
    match mem {
        Mem::Ram(o) => read_le(ram, o, width),
        Mem::Scratch(o) => read_le(scratch, o, width),
    }
}

/// Per-frame samples, written as CSV: `run_frame,frame,<columns...>`.
pub struct Sampler {
    pub columns: Vec<Sample>,
    pub every: u64,
    pub out: String,
    pub rows: u64,
    last: Option<u64>,
}

impl Sampler {
    pub fn new(columns: Vec<Sample>, every: u64) -> Self {
        let mut out = String::from("run_frame,frame");
        for c in &columns {
            out.push(',');
            out.push_str(&c.name);
        }
        out.push('\n');
        Sampler { columns, every: every.max(1), out, rows: 0, last: None }
    }

    /// Record a row for `run_frame` if it is due (every Nth frame, or `force`).
    pub fn row(&mut self, run_frame: u64, frame: u64, ram: &[u8], scratch: &[u8], force: bool) {
        if self.last == Some(run_frame) || !(force || run_frame.is_multiple_of(self.every)) {
            return;
        }
        self.last = Some(run_frame);
        self.rows += 1;
        self.out += &format!("{run_frame},{frame}");
        for c in &self.columns {
            self.out.push(',');
            self.out += &c.fmt.render(read_mem(ram, scratch, c.mem, c.fmt.width()));
        }
        self.out.push('\n');
    }
}

/// One `--poke ADDR=VALUE[:WIDTH|:FMT][@FRAME|@A..B|@A..]`.
#[derive(Clone, Debug)]
pub struct Poke {
    pub spec: String,
    pub addr: u32,
    pub mem: Mem,
    pub width: usize,
    pub value: u32,
    pub from: u64,
    /// Last run frame (inclusive) to apply at; `None` = open-ended.
    pub to: Option<u64>,
    pub applied: Vec<u64>,
}

impl Poke {
    pub fn due(&self, run_frame: u64) -> bool {
        run_frame >= self.from && self.to.is_none_or(|t| run_frame <= t)
    }

    pub fn apply(&mut self, run_frame: u64, ram: &mut [u8], scratch: &mut [u8]) {
        match self.mem {
            Mem::Ram(o) => write_le(ram, o, self.width, self.value),
            Mem::Scratch(o) => write_le(scratch, o, self.width, self.value),
        }
        self.applied.push(run_frame);
    }

    pub fn report(&self) -> Value {
        let n = self.applied.len();
        json!({
            "spec": self.spec,
            "address": self.addr, "address_hex": format!("0x{:08X}", self.addr),
            "width": self.width, "value": self.value, "value_hex": format!("0x{:0w$X}", self.value, w = self.width * 2),
            "value_signed": signed(self.value, self.width),
            "run_frames": {"from": self.from, "to": self.to},
            "applied": n,
            "first_run_frame": self.applied.first(), "last_run_frame": self.applied.last(),
        })
    }
}

pub fn parse_poke(s: &str) -> Result<Poke, String> {
    let (body, when) = match s.split_once('@') {
        Some((b, w)) => (b, Some(w)),
        None => (s, None),
    };
    let (a, rest) = body.split_once('=').ok_or_else(|| format!("--poke {s:?}: expected ADDR=VALUE[:WIDTH][@FRAME]"))?;
    let addr = parse_hex(a).ok_or_else(|| format!("--poke {s:?}: bad hex address {a:?}"))?;
    let (val, ty) = match rest.split_once(':') {
        Some((v, t)) => (v, t),
        None => (rest, "4"),
    };
    let fmt = match ty.trim() {
        "1" => Fmt::Unsigned(1),
        "2" => Fmt::Unsigned(2),
        "4" => Fmt::Unsigned(4),
        t => Fmt::parse(t).ok_or_else(|| format!("--poke {s:?}: width {t:?} is not 1, 2, 4 or a format like i16/fx16.16"))?,
    };
    let width = fmt.width();
    let value = fmt.encode(val).ok_or_else(|| format!("--poke {s:?}: bad value {val:?} (decimal, -decimal, 0xHEX, or a decimal fraction for fx formats)"))?;
    let mem = locate(addr, width).ok_or_else(|| format!("--poke {s:?}: 0x{addr:08X} is not main RAM or scratchpad"))?;
    let frame = |t: &str| t.trim().parse::<u64>().map_err(|_| format!("--poke {s:?}: bad frame {t:?}"));
    let (from, to) = match when {
        None => (0, Some(0)),
        Some(w) => match w.split_once("..") {
            Some((a, "")) => (frame(a)?, None),
            Some((a, b)) => (frame(a)?, Some(frame(b)?)),
            None => {
                let f = frame(w)?;
                (f, Some(f))
            }
        },
    };
    if to.is_some_and(|t| t < from) {
        return Err(format!("--poke {s:?}: empty frame range"));
    }
    Ok(Poke { spec: s.to_string(), addr, mem, width, value, from, to, applied: Vec::new() })
}

// ---------------------------------------------------------------- snapshots

/// A `ram.bin` (2 MiB), from a run directory or the file itself.
pub struct Snapshot {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

pub fn load(p: &Path) -> psx_media::Result<Snapshot> {
    let path = if p.is_dir() { p.join("ram.bin") } else { p.to_path_buf() };
    let bytes = std::fs::read(&path).map_err(|e| MediaError::io(&path, e))?;
    if bytes.len() != RAM_SIZE {
        return Err(bad(format!("{}: {} bytes; a main RAM dump (ram.bin) is 2097152", path.display(), bytes.len())));
    }
    Ok(Snapshot { path, bytes })
}

fn parse_width(w: u8) -> psx_media::Result<usize> {
    match w {
        1 | 2 | 4 => Ok(w as usize),
        _ => Err(bad(format!("width {w}: use 1, 2 or 4"))),
    }
}

/// `LO-HI` (hex, HI exclusive) → RAM byte offsets.
fn parse_range(r: Option<&str>) -> psx_media::Result<(usize, usize)> {
    let Some(r) = r else { return Ok((0, RAM_SIZE)) };
    let (lo, hi) = r.split_once('-').ok_or_else(|| bad(format!("range {r:?}: expected LO-HI in hex")))?;
    let (lo, hi) = (parse_hex(lo).ok_or_else(|| bad(format!("bad range start {lo:?}")))?, parse_hex(hi).ok_or_else(|| bad(format!("bad range end {hi:?}")))?);
    let l = match locate(lo, 0) {
        Some(Mem::Ram(o)) => o,
        _ => return Err(bad(format!("range start 0x{lo:08X} is not main RAM"))),
    };
    let h = match hi.checked_sub(1).and_then(|last| locate(last, 1)) {
        Some(Mem::Ram(o)) => o + 1,
        _ => return Err(bad(format!("range end 0x{hi:08X} is not main RAM"))),
    };
    if h <= l {
        return Err(bad(format!("empty range {r:?}")));
    }
    Ok((l, h))
}

fn addr_of(off: usize) -> u32 {
    0x8000_0000 | off as u32
}

fn value_json(v: u32, width: usize, signed_view: bool) -> Value {
    if signed_view {
        json!(signed(v, width))
    } else {
        json!(v)
    }
}

pub struct DiffOpts<'a> {
    pub width: u8,
    pub range: Option<&'a str>,
    pub min_delta: Option<u64>,
    pub signed: bool,
    pub limit: usize,
    pub offset: usize,
}

/// Units (bytes/halfwords/words, aligned) whose value differs between two snapshots.
pub fn diff(a: &Path, b: &Path, o: DiffOpts) -> R {
    crate::page::validate(o.limit).map_err(MediaError::Malformed)?;
    let w = parse_width(o.width)?;
    let (sa, sb) = (load(a)?, load(b)?);
    let (lo, hi) = parse_range(o.range)?;
    let lo = lo & !(w - 1);
    let mut changes = Vec::new();
    let mut total = 0u64;
    // Clusters: runs of changed units no more than 16 bytes apart.
    let mut clusters: Vec<(usize, usize, u64)> = Vec::new();
    let mut off = lo;
    while off + w <= hi {
        let (x, y) = (read_le(&sa.bytes, off, w), read_le(&sb.bytes, off, w));
        if x != y {
            let delta = signed(y.wrapping_sub(x), w);
            if o.min_delta.is_none_or(|m| delta.unsigned_abs() >= m) {
                total += 1;
                match clusters.last_mut() {
                    Some(c) if off <= c.1 + 16 => {
                        c.1 = off + w;
                        c.2 += 1;
                    }
                    _ => clusters.push((off, off + w, 1)),
                }
                if total>o.offset as u64 && changes.len() < o.limit {
                    changes.push(json!({
                        "address": addr_of(off), "address_hex": format!("0x{:08X}", addr_of(off)),
                        "a": value_json(x, w, o.signed), "b": value_json(y, w, o.signed),
                        "a_hex": format!("0x{x:0n$X}", n = w * 2), "b_hex": format!("0x{y:0n$X}", n = w * 2),
                        "delta": delta,
                    }));
                }
            }
        }
        off += w;
    }
    let mut big = clusters.clone();
    big.sort_by_key(|c| (std::cmp::Reverse(c.2), c.0));
    Ok(json!({
        "a": sa.path, "b": sb.path, "width": w, "signed": o.signed,
        "range": [format!("0x{:08X}", addr_of(lo)), format!("0x{:08X}", addr_of(hi))],
        "differing": total, "shown": changes.len(),
        "page":crate::page::metadata(total as usize,o.offset,changes.len(),o.limit),
        "coverage":{"scope":"selected RAM range","complete":true},
        "changes": changes,
        "clusters": {"count": clusters.len(), "largest": big.iter().take(o.limit.min(32)).map(|&(s, e, n)| json!({
            "start_hex": format!("0x{:08X}", addr_of(s)), "end_hex": format!("0x{:08X}", addr_of(e)), "units": n,
        })).collect::<Vec<_>>()},
    }))
}

/// A predicate between consecutive snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pred {
    Any,
    Changed,
    Unchanged,
    Increased,
    Decreased,
    Delta(i64),
}

fn parse_pred(s: &str) -> psx_media::Result<Pred> {
    let s = s.trim().to_ascii_lowercase();
    Ok(match s.as_str() {
        "any" | "*" => Pred::Any,
        "changed" | "ne" | "!=" => Pred::Changed,
        "unchanged" | "eq" | "same" | "equal" | "==" => Pred::Unchanged,
        "increased" | "inc" | "gt" | ">" => Pred::Increased,
        "decreased" | "dec" | "lt" | "<" => Pred::Decreased,
        _ => match s.strip_prefix("delta=").or_else(|| s.strip_prefix("delta:")).and_then(number) {
            Some(d) => Pred::Delta(d),
            None => return Err(bad(format!("predicate {s:?}: use changed, unchanged, increased, decreased, delta=N or any"))),
        },
    })
}

impl Pred {
    fn holds(self, a: u32, b: u32, w: usize, signed_view: bool) -> bool {
        let (x, y) = if signed_view { (signed(a, w), signed(b, w)) } else { (a as i64, b as i64) };
        match self {
            Pred::Any => true,
            Pred::Changed => a != b,
            Pred::Unchanged => a == b,
            Pred::Increased => y > x,
            Pred::Decreased => y < x,
            Pred::Delta(d) => signed(b.wrapping_sub(a), w) == d,
        }
    }
}

pub struct SearchOpts<'a> {
    pub width: u8,
    pub signed: bool,
    pub preds: &'a [String],
    pub values: Option<&'a str>,
    pub between: Option<&'a str>,
    pub range: Option<&'a str>,
    pub align: Option<usize>,
    pub candidates: Option<&'a Path>,
    pub save: Option<&'a Path>,
    pub limit: usize,
    pub offset: usize,
}

/// Cheat-finder search: addresses whose values across the snapshots (in
/// order) satisfy every predicate.
pub fn search(paths: &[PathBuf], o: SearchOpts) -> R {
    crate::page::validate(o.limit).map_err(MediaError::Malformed)?;
    let w = parse_width(o.width)?;
    if paths.is_empty() {
        return Err(bad("give at least one snapshot (run directory or ram.bin)"));
    }
    let snaps = paths.iter().map(|p| load(p)).collect::<psx_media::Result<Vec<_>>>()?;
    let n = snaps.len();
    let preds: Vec<Pred> = o.preds.iter().flat_map(|p| p.split(',')).filter(|p| !p.trim().is_empty()).map(parse_pred).collect::<psx_media::Result<_>>()?;
    let pair_preds: Vec<Pred> = match preds.len() {
        0 => vec![Pred::Any; n.saturating_sub(1)],
        1 => vec![preds[0]; n.saturating_sub(1)],
        k if k == n - 1 => preds,
        k => return Err(bad(format!("{k} predicates for {n} snapshots: give one (applied to every consecutive pair) or {}", n - 1))),
    };
    let values: Option<Vec<Option<i64>>> = o
        .values
        .map(|v| {
            let list: Vec<Option<i64>> = v.split(',').map(|x| if x.trim() == "*" { Ok(None) } else { number(x).map(Some).ok_or_else(|| bad(format!("bad value {x:?}"))) }).collect::<psx_media::Result<_>>()?;
            if list.len() != n {
                return Err(bad(format!("--values has {} entries for {n} snapshots (use * to skip one)", list.len())));
            }
            Ok(list)
        })
        .transpose()?;
    let between = o
        .between
        .map(|b| {
            let (l, h) = b.split_once("..").ok_or_else(|| bad(format!("--between {b:?}: expected LO..HI")))?;
            Ok::<_, MediaError>((number(l).ok_or_else(|| bad(format!("bad bound {l:?}")))?, number(h).ok_or_else(|| bad(format!("bad bound {h:?}")))?))
        })
        .transpose()?;
    let view = |v: u32| if o.signed { signed(v, w) } else { v as i64 };
    // A value matches --values whether given signed (-2) or as raw bits (0xFFFE).
    let eq = |v: u32, want: i64| view(v) == want || (want >= 0 && v as i64 == want);
    let (lo, hi) = parse_range(o.range)?;
    let align = o.align.unwrap_or(w).max(1);
    let restrict: Option<BTreeSet<u32>> = match o.candidates {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| MediaError::io(p, e))?;
            let v: Value = serde_json::from_str(&text).map_err(|e| bad(format!("{}: {e}", p.display())))?;
            Some(v["addresses"].as_array().into_iter().flatten().filter_map(|a| a.as_str().and_then(parse_hex).or(a.as_u64().map(|x| x as u32))).map(|a| a & 0x1F_FFFF).collect())
        }
        None => None,
    };
    let mut found: Vec<usize> = Vec::new();
    let mut off = lo.div_ceil(align) * align;
    while off + w <= hi {
        if restrict.as_ref().is_none_or(|r| r.contains(&(off as u32))) {
            let vals: Vec<u32> = snaps.iter().map(|s| read_le(&s.bytes, off, w)).collect();
            let ok = pair_preds.iter().enumerate().all(|(i, p)| p.holds(vals[i], vals[i + 1], w, o.signed))
                && values.as_ref().is_none_or(|vs| vs.iter().zip(&vals).all(|(want, &v)| want.is_none_or(|x| eq(v, x))))
                && between.is_none_or(|(l, h)| vals.iter().all(|&v| (l..=h).contains(&view(v))));
            if ok {
                found.push(off);
            }
        }
        off += align;
    }
    let saved = match o.save {
        Some(p) => {
            if let Some(parent) = p.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).map_err(|e| MediaError::io(parent, e))?;
            }
            let doc = json!({"width": w, "addresses": found.iter().map(|&o| format!("0x{:08X}", addr_of(o))).collect::<Vec<_>>()});
            std::fs::write(p, serde_json::to_vec_pretty(&doc).unwrap()).map_err(|e| MediaError::io(p, e))?;
            Some(p.to_path_buf())
        }
        None => None,
    };
    let shown: Vec<Value> = found
        .iter()
        .skip(o.offset)
        .take(o.limit)
        .map(|&off| {
            let vals: Vec<u32> = snaps.iter().map(|s| read_le(&s.bytes, off, w)).collect();
            let mut v = json!({
                "address": addr_of(off), "address_hex": format!("0x{:08X}", addr_of(off)),
                "values": vals.iter().map(|&x| view(x)).collect::<Vec<_>>(),
                "hex": vals.iter().map(|&x| format!("0x{x:0n$X}", n = w * 2)).collect::<Vec<_>>(),
            });
            if w == 4 {
                v["fx16_16"] = json!(vals.iter().map(|&x| x as i32 as f64 / 65536.0).collect::<Vec<_>>());
            }
            v
        })
        .collect();
    Ok(json!({
        "snapshots": snaps.iter().map(|s| &s.path).collect::<Vec<_>>(),
        "width": w, "align": align, "signed": o.signed,
        "predicates": pair_preds.iter().map(|p| format!("{p:?}").to_lowercase()).collect::<Vec<_>>(),
        "range": [format!("0x{:08X}", addr_of(lo)), format!("0x{:08X}", addr_of(hi))],
        "restricted_to": restrict.as_ref().map(|r| r.len()),
        "candidates": found.len(), "shown": shown.len(),
        "page":crate::page::metadata(found.len(),o.offset,shown.len(),o.limit),
        "coverage":{"scope":"selected RAM range/alignment and candidate set","complete":true},
        "matches": shown,
        "saved": saved,
    }))
}

/// A memory window from one or more snapshots side by side, marking units that differ.
pub fn view(paths: &[PathBuf], addr: &str, len: usize, fmt: &str) -> psx_media::Result<(Value, String)> {
    let fmt = Fmt::parse(fmt).ok_or_else(|| bad(format!("unknown format {fmt:?}")))?;
    let w = fmt.width();
    let snaps = paths.iter().map(|p| load(p)).collect::<psx_media::Result<Vec<_>>>()?;
    if snaps.is_empty() {
        return Err(bad("give at least one snapshot"));
    }
    let a = parse_hex(addr).ok_or_else(|| bad(format!("bad hex address {addr:?}")))?;
    let Some(Mem::Ram(start)) = locate(a, 0) else { return Err(bad(format!("0x{a:08X} is not main RAM"))) };
    let end = (start + len.min(64 * 1024)).min(RAM_SIZE);
    let mut rows = Vec::new();
    let mut text = format!("{:<12}{:<8}", "address", "offset");
    for (i, _) in snaps.iter().enumerate() {
        text += &format!("{:>16}", format!("[{i}]"));
    }
    text += "\n";
    let mut off = start;
    while off + w <= end {
        let vals: Vec<u32> = snaps.iter().map(|s| read_le(&s.bytes, off, w)).collect();
        let differs = vals.iter().any(|&v| v != vals[0]);
        rows.push(json!({
            "address_hex": format!("0x{:08X}", addr_of(off)), "offset": format!("+0x{:X}", off - start),
            "values": vals.iter().map(|&v| fmt.json(v)).collect::<Vec<_>>(), "differs": differs,
        }));
        text += &format!("{:<12}{:<8}", format!("{:08X}", addr_of(off)), format!("+{:X}", off - start));
        for &v in &vals {
            text += &format!("{:>16}", fmt.render(v));
        }
        text += if differs { "  *\n" } else { "\n" };
        off += w;
    }
    let legend: Vec<String> = snaps.iter().enumerate().map(|(i, s)| format!("[{i}] {}", s.path.display())).collect();
    let text = format!("; {} as {}\n{text}", legend.join("  "), fmt.name());
    Ok((json!({"snapshots": snaps.iter().map(|s| &s.path).collect::<Vec<_>>(), "format": fmt.name(), "rows": rows}), text))
}

#[cfg(test)]
mod tests {
    #[test]
    fn address_validation_uses_the_shared_memory_map() {
        assert_eq!(super::locate(0xA0610000, 4), Some(super::Mem::Ram(0x10000)));
        assert_eq!(super::locate(0x1F800004, 4), Some(super::Mem::Scratch(4)));
        for addr in [0xC0010000, 0xBF800004, 0xBFC00000, 0x1F801000] {
            assert_eq!(super::locate(addr, 4), None);
        }
        assert_eq!(super::locate(0x801FFFFF, 2), None);
        assert_eq!(super::locate(0x80010000, usize::MAX), None);
        assert_eq!(super::parse_range(Some("801ffff0-80200000")).unwrap(), (0x1ffff0, 0x200000));
        assert!(super::parse_range(Some("80010000-c0200000")).is_err());
        assert!(super::parse_range(Some("c0010000-c0010004")).is_err());
    }
    use super::*;

    #[test]
    fn formats_parse_and_render() {
        assert_eq!(Fmt::parse("fx16.16"), Some(Fmt::Fixed(4, 16)));
        assert_eq!(Fmt::parse("fx4.12"), Some(Fmt::Fixed(2, 12)));
        assert_eq!(Fmt::parse("s16"), Some(Fmt::Signed(2)));
        assert_eq!(Fmt::parse("i8"), Some(Fmt::Signed(1)));
        assert_eq!(Fmt::parse("fx3.3"), None);
        assert_eq!(Fmt::Fixed(4, 16).render(0x01B9_0000), "441.0");
        assert_eq!(Fmt::Fixed(4, 16).render(0x0005_C900), "5.78515625");
        assert_eq!(Fmt::Fixed(4, 16).render(0xFFFA_0000), "-6.0");
        assert_eq!(Fmt::Fixed(4, 16).render(1), "0.0000152587890625", "exact, not rounded");
        assert_eq!(Fmt::Fixed(4, 16).render(0xFFFF_FFFF), "-0.0000152587890625");
        assert_eq!(Fmt::Fixed(2, 12).render(0x1800), "1.5");
        assert_eq!(Fmt::Signed(2).render(0xFFFE), "-2");
        assert_eq!(Fmt::Hex(2).render(0x2A), "0x002A");
        assert_eq!(Fmt::Fixed(4, 16).encode("1.5"), Some(0x0001_8000));
        assert_eq!(Fmt::Signed(2).encode("-1"), Some(0xFFFF));
    }

    #[test]
    fn samples_and_pokes_parse() {
        let s = parse_sample("8015E8A8:fx16.16:x").unwrap();
        assert_eq!((s.addr, s.mem, s.name.as_str()), (0x8015_E8A8, Mem::Ram(0x15E8A8), "x"));
        assert_eq!(parse_sample("1F800010:u8").unwrap().mem, Mem::Scratch(0x10));
        assert!(parse_sample("8015E8A8:f32").is_err());
        assert_eq!(parse_sample("8015E8A8:4").unwrap().fmt, Fmt::Unsigned(4), "watch widths read unsigned");
        assert_eq!(parse_sample("8015E8A8:2").unwrap().name, "0x8015E8A8:u16");
        let p = parse_poke("0x8015E926=1:1@10").unwrap();
        assert_eq!((p.width, p.value, p.from, p.to), (1, 1, 10, Some(10)));
        let p = parse_poke("8015E8AC=300.5:fx16.16@3..").unwrap();
        assert_eq!((p.width, p.value, p.from, p.to), (4, 0x012C_8000, 3, None));
        let p = parse_poke("8015E8AC=-2:2").unwrap();
        assert_eq!((p.width, p.value, p.from, p.to), (2, 0xFFFE, 0, Some(0)));
        assert!(parse_poke("8015E8AC=1@5..2").is_err());
        assert!(parse_poke("1F000000=1").is_err());
    }

    #[test]
    fn typed_samples_match_text() {
        let spec = |v: Value| serde_json::from_value::<SampleSpec>(v).map(|s| s.parse());
        let t = spec(json!({"addr": "8015E8A8", "format": "fx16.16", "name": "x"})).unwrap().unwrap();
        let s = spec(json!("8015E8A8:fx16.16:x")).unwrap().unwrap();
        assert_eq!((t.addr, t.fmt, t.mem, t.name), (s.addr, s.fmt, s.mem, s.name));
        let d = spec(json!({"addr": "0x1F800010"})).unwrap().unwrap();
        assert_eq!((d.fmt, d.mem, d.name.as_str()), (Fmt::Unsigned(4), Mem::Scratch(0x10), "0x1F800010:u32"));
        assert_eq!(spec(json!({"addr": "80010000", "format": 2})).unwrap().unwrap().fmt, Fmt::Unsigned(2));
        assert!(spec(json!({"addr": "80010000", "format": 3})).unwrap().unwrap_err().contains("unknown format"));
        assert!(spec(json!({"addr": "80010000", "format": true})).unwrap_err().to_string().contains("format true"));
        assert!(spec(json!({"addr": "80010000", "fmt": "u8"})).unwrap_err().to_string().contains("unknown field `fmt`"));
        assert!(spec(json!(5)).is_err());
        assert!(spec(json!({"addr": "80010000", "format": "f32"})).unwrap().unwrap_err().contains("unknown format"));
        assert!(spec(json!({"addr": "80010000", "name": "a,b"})).unwrap().is_err());
    }

    #[test]
    fn read_watch_ranges_fold_mirrors() {
        use crate::run::{fold, parse_range_spec};
        assert_eq!(parse_range_spec("800751CC:4"), Ok((0x751CC, 0x751D0)));
        assert_eq!(parse_range_spec("0x80100000-80100400"), Ok((0x10_0000, 0x10_0400)));
        assert_eq!(parse_range_spec("A0100000"), Ok((0x10_0000, 0x10_0004)));
        assert_eq!(fold(0x0020_0010), 0x10, "2 MiB mirrors fold");
        assert_eq!(fold(0x1F80_0004), 0x1F80_0004);
        assert!(parse_range_spec("80100400-80100000").is_err());
    }

    #[test]
    fn sampler_reads_bytes_little_endian() {
        let mut ram = vec![0u8; RAM_SIZE];
        let scratch = [0u8; SCRATCH_SIZE];
        let mut s = Sampler::new(vec![parse_sample("80000010:fx16.16:y").unwrap(), parse_sample("80000012:i16").unwrap()], 2);
        write_le(&mut ram, 0x10, 4, 0x01B9_8000);
        s.row(0, 100, &ram, &scratch, false);
        s.row(1, 101, &ram, &scratch, false); // not due
        // A halfword store into the high half is seen by the word view.
        write_le(&mut ram, 0x12, 2, 0x01BF);
        s.row(2, 102, &ram, &scratch, false);
        s.row(3, 103, &ram, &scratch, true);
        assert_eq!(s.out, "run_frame,frame,y,0x80000012:i16\n0,100,441.5,441\n2,102,447.5,447\n3,103,447.5,447\n");
        assert_eq!(s.rows, 3);
    }

    fn snap(dir: &Path, name: &str, words: &[(usize, u32)]) -> PathBuf {
        let mut ram = vec![0u8; RAM_SIZE];
        for &(o, v) in words {
            write_le(&mut ram, o, 4, v);
        }
        let p = dir.join(name);
        std::fs::write(&p, ram).unwrap();
        p
    }

    #[test]
    fn diff_and_search_find_the_ordered_variable() {
        let dir = std::env::temp_dir().join(format!("psx-ram-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 0x100 increases across runs (a position); 0x200 changes randomly; 0x300 is constant.
        let a = snap(&dir, "a.bin", &[(0x100, 10), (0x200, 5), (0x300, 7)]);
        let b = snap(&dir, "b.bin", &[(0x100, 20), (0x200, 1), (0x300, 7)]);
        let c = snap(&dir, "c.bin", &[(0x100, 35), (0x200, 9), (0x300, 7)]);
        let d = diff(&a, &b, DiffOpts { width: 4, range: None, min_delta: None, signed: true, limit: 10, offset:0 }).unwrap();
        assert_eq!(d["differing"], 2);
        assert_eq!(d["changes"][0]["address_hex"], "0x80000100");
        assert_eq!(d["changes"][1]["delta"], -4);
        let d = diff(&a, &b, DiffOpts { width: 4, range: None, min_delta: Some(5), signed: true, limit: 10, offset:0 }).unwrap();
        assert_eq!(d["differing"], 1);
        let paths = vec![a.clone(), b.clone(), c.clone()];
        let base = SearchOpts { width: 4, signed: false, preds: &[], values: None, between: None, range: None, align: None, candidates: None, save: None, limit: 10, offset:0 };
        let r = search(&paths, SearchOpts { preds: &["increased".into()], ..base }).unwrap();
        assert_eq!(r["candidates"], 1);
        assert_eq!(r["matches"][0]["values"], json!([10, 20, 35]));
        let r = search(&paths, SearchOpts { preds: &["dec,inc".into()], ..base }).unwrap();
        assert_eq!(r["matches"][0]["address_hex"], "0x80000200");
        let saved = dir.join("cands.json");
        let r = search(&paths, SearchOpts { preds: &["unchanged".into()], values: Some("7,*,7"), save: Some(&saved), ..base }).unwrap();
        assert_eq!(r["candidates"], 1);
        let r = search(&paths[..1], SearchOpts { candidates: Some(&saved), ..base }).unwrap();
        assert_eq!((r["candidates"].as_u64(), r["restricted_to"].as_u64()), (Some(1), Some(1)));
        let r = search(&paths, SearchOpts { preds: &["delta=10".into(), "delta=15".into()], ..base }).unwrap();
        assert_eq!(r["candidates"], 1);
        assert!(search(&paths, SearchOpts { preds: &["inc".into(), "inc".into(), "inc".into()], ..base }).is_err());
        let (v, text) = view(&paths, "800000FC", 12, "u32").unwrap();
        assert_eq!(v["rows"][1]["values"], json!([10, 20, 35]));
        assert!(text.contains("80000100") && text.contains('*'));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
