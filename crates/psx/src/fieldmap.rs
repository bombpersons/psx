//! The project's field map, `samples.toml`: names for a game's memory, so
//! samples (and the cases made from them) carry names rather than addresses,
//! and the game's tick, counted into every samples.csv.
//!
//! ```toml
//! [tick]                    # a store here ends one tick of the game's logic
//! write = "80087AA4:4"      # ADDR[:WIDTH], width in bytes (default 4)
//! pc = "8001DD64"           # optional: count stores from this PC only
//!
//! [globals]                 # NAME = "ADDR:FMT"
//! camera_x = "8008B258:i32"
//!
//! [structs.object]          # a layout: FIELD = "OFFSET:FMT", offsets in hex
//! x = "34:i32"
//! yaw = "42:i16"
//!
//! [subjects.player]         # one struct at a fixed address
//! struct = "object"
//! at = "8017C87C"
//!
//! [subjects.placement]      # an array of them: placement[3]
//! struct = "object"
//! at = "8017C61C"
//! stride = "4C"
//! count = 256               # optional bound on the index
//! ```
//!
//! `--sample` then takes `camera_x`, `player` (every field, as `player.x`
//! ...), `player.x`, `placement[3]` or `placement[3].yaw`. FMT is a
//! `--sample` format (u8 i16 x32 fx16.16 ...) or a width 1/2/4.

use crate::ram::{Sample, sample_at};
use psx_media::memmap::parse_hex;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use toml::Table;

pub const DEFAULT: &str = "samples.toml";

#[derive(Clone, Debug)]
pub struct Tick {
    pub addr: u32,
    pub width: u32,
    pub pc: Option<u32>,
}

struct Field {
    name: String,
    offset: u32,
    fmt: String,
}

struct Subject {
    name: String,
    layout: String,
    at: u32,
    /// Indexed subjects: the element size, and the index bound if given.
    stride: Option<(u32, Option<u32>)>,
}

pub struct FieldMap {
    pub path: PathBuf,
    pub tick: Option<Tick>,
    globals: Vec<(String, u32, String)>,
    structs: Vec<(String, Vec<Field>)>,
    subjects: Vec<Subject>,
}

fn is_name(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn text<'a>(v: &'a toml::Value, what: &str) -> Result<&'a str, String> {
    v.as_str().ok_or_else(|| format!("{what}: expected a string"))
}

fn hex(s: &str, what: &str) -> Result<u32, String> {
    parse_hex(s).ok_or_else(|| format!("{what}: bad hex {s:?}"))
}

/// `"ADDR:FMT"` or `"OFFSET:FMT"`; the format defaults to u32, as in `--sample`.
fn place(s: &str, what: &str) -> Result<(u32, String), String> {
    let (a, f) = s.split_once(':').unwrap_or((s, "u32"));
    // Check the format now, so a bad map fails when loaded.
    sample_at(0x8000_0000, f, None, what)?;
    Ok((hex(a, what)?, f.trim().to_string()))
}

fn table<'a>(t: &'a Table, key: &str) -> Result<Option<&'a Table>, String> {
    t.get(key).map(|v| v.as_table().ok_or_else(|| format!("[{key}]: expected a table"))).transpose()
}

impl FieldMap {
    /// `path`, or `samples.toml` in the project root when it exists.
    pub fn load(path: Option<&Path>) -> Result<Option<FieldMap>, String> {
        let p = match path {
            Some(p) => p.to_path_buf(),
            None if Path::new(DEFAULT).is_file() => PathBuf::from(DEFAULT),
            None => return Ok(None),
        };
        let s = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        FieldMap::parse(&s, p.clone()).map(Some).map_err(|e| format!("{}: {e}", p.display()))
    }

    pub fn parse(s: &str, path: PathBuf) -> Result<FieldMap, String> {
        let t: Table = s.parse().map_err(|e: toml::de::Error| e.message().to_string())?;
        if let Some(k) = t.keys().find(|k| !["tick", "globals", "structs", "subjects"].contains(&k.as_str())) {
            return Err(format!("unknown section [{k}] (tick, globals, structs, subjects)"));
        }
        let tick = table(&t, "tick")?
            .map(|t| -> Result<Tick, String> {
                if let Some(k) = t.keys().find(|k| !["write", "pc"].contains(&k.as_str())) {
                    return Err(format!("[tick]: unknown key {k} (write, pc)"));
                }
                let w = text(t.get("write").ok_or("[tick]: needs write = \"ADDR[:WIDTH]\"")?, "[tick] write")?;
                let (a, width) = w.split_once(':').unwrap_or((w, "4"));
                let width = width
                    .trim()
                    .parse()
                    .ok()
                    .filter(|&w| w >= 1)
                    .ok_or_else(|| format!("[tick] write {w:?}: bad width"))?;
                let pc = t.get("pc").map(|v| text(v, "[tick] pc").and_then(|s| hex(s, "[tick] pc"))).transpose()?;
                Ok(Tick { addr: hex(a, "[tick] write")?, width, pc })
            })
            .transpose()?;
        let mut names: Vec<String> = Vec::new();
        let mut claim = |n: &str, what: &str| {
            if !is_name(n) || n == "tick" {
                return Err(format!(
                    "{what} {n:?}: names are letters, digits and _, not starting with a digit, and not \"tick\""
                ));
            }
            if names.iter().any(|m| m == n) {
                return Err(format!("{what} {n:?}: the name is used twice"));
            }
            names.push(n.to_string());
            Ok(())
        };
        let mut globals = Vec::new();
        for (n, v) in table(&t, "globals")?.into_iter().flatten() {
            claim(n, "global")?;
            let what = format!("globals.{n}");
            let (addr, fmt) = place(text(v, &what)?, &what)?;
            sample_at(addr, &fmt, Some(n), &what)?;
            globals.push((n.clone(), addr, fmt));
        }
        let mut structs = Vec::new();
        for (n, v) in table(&t, "structs")?.into_iter().flatten() {
            let fields = v.as_table().ok_or_else(|| format!("structs.{n}: expected a table"))?;
            let mut out: Vec<Field> = Vec::new();
            for (f, v) in fields {
                let what = format!("structs.{n}.{f}");
                if !is_name(f) || out.iter().any(|o| o.name == *f) {
                    return Err(format!("{what}: bad or repeated field name"));
                }
                let (offset, fmt) = place(text(v, &what)?, &what)?;
                out.push(Field { name: f.clone(), offset, fmt });
            }
            structs.push((n.clone(), out));
        }
        let mut subjects = Vec::new();
        for (n, v) in table(&t, "subjects")?.into_iter().flatten() {
            claim(n, "subject")?;
            let what = format!("subjects.{n}");
            let s = v.as_table().ok_or_else(|| format!("{what}: expected a table"))?;
            if let Some(k) = s.keys().find(|k| !["struct", "at", "stride", "count"].contains(&k.as_str())) {
                return Err(format!("{what}: unknown key {k} (struct, at, stride, count)"));
            }
            let layout = text(s.get("struct").ok_or_else(|| format!("{what}: needs struct = \"NAME\""))?, &what)?;
            if !structs.iter().any(|(k, _)| k == layout) {
                return Err(format!("{what}: no [structs.{layout}]"));
            }
            let at = hex(text(s.get("at").ok_or_else(|| format!("{what}: needs at = \"ADDR\""))?, &what)?, &what)?;
            let count = s
                .get("count")
                .map(|v| {
                    v.as_integer()
                        .and_then(|c| u32::try_from(c).ok())
                        .ok_or_else(|| format!("{what}.count: expected a number"))
                })
                .transpose()?;
            let stride = match s.get("stride") {
                Some(v) => Some((hex(text(v, &what)?, &format!("{what}.stride"))?, count)),
                None if count.is_some() => return Err(format!("{what}: count needs a stride")),
                None => None,
            };
            subjects.push(Subject { name: n.clone(), layout: layout.to_string(), at, stride });
        }
        Ok(FieldMap { path, tick, globals, structs, subjects })
    }

    pub fn names(&self) -> Vec<String> {
        let subjects =
            self.subjects.iter().map(|s| if s.stride.is_some() { format!("{}[N]", s.name) } else { s.name.clone() });
        self.globals.iter().map(|g| g.0.clone()).chain(subjects).collect()
    }

    /// Whether `sel` names something here (a global or a subject).
    pub fn knows(&self, sel: &str) -> bool {
        let head = sel.split(['[', '.']).next().unwrap_or("");
        self.globals.iter().any(|g| g.0 == sel) || self.subjects.iter().any(|s| s.name == head)
    }

    /// The columns `sel` names: `global`, `subject`, `subject.field`,
    /// `subject[N]` or `subject[N].field`.
    pub fn resolve(&self, sel: &str) -> Result<Vec<Sample>, String> {
        let label = format!("{sel:?}");
        if let Some((n, addr, fmt)) = self.globals.iter().find(|g| g.0 == sel) {
            return Ok(vec![Sample { mapped: true, ..sample_at(*addr, fmt, Some(n), &label)? }]);
        }
        let (head, field) = match sel.split_once('.') {
            Some((h, f)) => (h, Some(f)),
            None => (sel, None),
        };
        let (name, index) = match head.split_once('[') {
            Some((n, rest)) => {
                let i = rest.strip_suffix(']').and_then(|i| i.parse::<u32>().ok());
                (n, Some(i.ok_or_else(|| format!("{label}: expected {n}[N] with a decimal N"))?))
            }
            None => (head, None),
        };
        let s = self.subjects.iter().find(|s| s.name == name).ok_or_else(|| {
            format!("{label}: not a name in {} (names: {})", self.path.display(), self.names().join(", "))
        })?;
        let base = match (s.stride, index) {
            (None, None) => s.at,
            (Some((stride, count)), Some(i)) => {
                if count.is_some_and(|c| i >= c) {
                    return Err(format!("{label}: {name} has {} elements", count.unwrap()));
                }
                s.at.wrapping_add(stride.wrapping_mul(i))
            }
            (None, Some(_)) => return Err(format!("{label}: {name} is one struct, not an array")),
            (Some(_), None) => return Err(format!("{label}: {name} is an array; say which, as {name}[N]")),
        };
        let fields = &self.structs.iter().find(|(k, _)| *k == s.layout).unwrap().1;
        let pick: Vec<&Field> = match field {
            None => fields.iter().collect(),
            Some(f) => vec![fields.iter().find(|x| x.name == f).ok_or_else(|| {
                let all: Vec<&str> = fields.iter().map(|x| x.name.as_str()).collect();
                format!("{label}: {} has no field {f:?} ({})", s.layout, all.join(", "))
            })?],
        };
        pick.into_iter()
            .map(|f| {
                let s = sample_at(base.wrapping_add(f.offset), &f.fmt, Some(&format!("{head}.{}", f.name)), &label)?;
                Ok(Sample { mapped: true, ..s })
            })
            .collect()
    }

    pub fn report(&self) -> Value {
        json!({"path": self.path, "names": self.names()})
    }
}

/// The columns a text selector names: a map name, or else `ADDR[:FMT[:NAME]]`.
/// A bare token is a name when the map has it or it is not hex.
pub fn select(s: &str, map: Option<&FieldMap>) -> Result<Vec<Sample>, String> {
    let name = !s.contains(':') && (map.is_some_and(|f| f.knows(s)) || psx_media::memmap::parse_hex(s).is_none());
    match (name, map) {
        (true, Some(f)) => f.resolve(s),
        (true, None) => Err(format!(
            "{s:?}: names come from a field map (samples.toml in the project root, or --map; guide topic fields)"
        )),
        (false, _) => crate::ram::parse_sample(s).map(|c| vec![c]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAP: &str = r#"
[tick]
write = "80087AA4"
pc = "8001DD64"

[globals]
camera_x = "8008B258:i32"

[structs.object]
x = "34:i32"
yaw = "42:i16"

[subjects.player]
struct = "object"
at = "8017C87C"

[subjects.placement]
struct = "object"
at = "8017C61C"
stride = "4C"
count = 4
"#;

    fn map() -> FieldMap {
        FieldMap::parse(MAP, "samples.toml".into()).unwrap()
    }

    fn cols(sel: &str) -> Vec<(String, u32)> {
        map().resolve(sel).unwrap().into_iter().map(|s| (s.name, s.addr)).collect()
    }

    #[test]
    fn names_resolve_to_columns_in_the_map_order() {
        let m = map();
        let t = m.tick.as_ref().unwrap();
        assert_eq!((t.addr, t.width, t.pc), (0x8008_7AA4, 4, Some(0x8001_DD64)));
        assert_eq!(cols("camera_x"), [("camera_x".into(), 0x8008_B258)]);
        assert_eq!(cols("player"), [("player.x".into(), 0x8017_C8B0), ("player.yaw".into(), 0x8017_C8BE)]);
        assert_eq!(cols("placement[2].yaw"), [("placement[2].yaw".into(), 0x8017_C61C + 2 * 0x4C + 0x42)]);
        assert_eq!(m.names(), ["camera_x", "player", "placement[N]"]);
        assert!(m.knows("placement[1].x") && !m.knows("8008B258"));
    }

    #[test]
    fn mistakes_are_named() {
        let m = map();
        for (sel, says) in [
            ("placement", "say which"),
            ("placement[4]", "has 4 elements"),
            ("player[0]", "not an array"),
            ("player.speed", "no field \"speed\" (x, yaw)"),
            ("enemy", "names: camera_x, player, placement[N]"),
        ] {
            let e = m.resolve(sel).unwrap_err();
            assert!(e.contains(says), "{sel}: {e}");
        }
        for (toml, says) in [
            ("[globals]\ntick = \"80000000:u8\"", "not \"tick\""),
            ("[globals]\nx = \"80000000:q8\"", "unknown format"),
            ("[subjects.a]\nstruct = \"none\"\nat = \"80000000\"", "no [structs.none]"),
            ("[ticks]\nwrite = \"0\"", "unknown section [ticks]"),
            ("[globals]\nx = \"1F000000:u8\"", "not main RAM"),
        ] {
            let e = FieldMap::parse(toml, "m.toml".into()).err().unwrap();
            assert!(e.contains(says), "{toml}: {e}");
        }
    }
}
