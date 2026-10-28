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
//!
//! [subjects.enemy]          # found through a pointer: the address held at
//! struct = "object"         # pointer (plus offset, hex), read anew at each
//! pointer = "800A1234"      # sample; empty while null or not memory
//! offset = "0"              # optional; stride and count work here too
//!
//! [map]                     # optional: where this map is right
//! holds = ["level=1"]       # conditions (--until's syntax) true there; a
//!                           # sample row where one fails is reported
//! ```
//!
//! `--sample` then takes `camera_x`, `player` (every field, as `player.x`
//! ...), `player.x`, `placement[3]` or `placement[3].yaw`. FMT is a
//! `--sample` format (u8 i16 x32 fx16.16 u16[14] ...) or a width 1/2/4.

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
    /// The address, or (`pointer`) where the address is kept.
    at: u32,
    /// Through a pointer: the offset added to the address it holds.
    via: Option<u32>,
    /// Indexed subjects: the element size, and the index bound if given.
    stride: Option<(u32, Option<u32>)>,
}

pub struct FieldMap {
    pub path: PathBuf,
    pub tick: Option<Tick>,
    globals: Vec<(String, u32, String)>,
    structs: Vec<(String, Vec<Field>)>,
    subjects: Vec<Subject>,
    /// `[map] holds`: true wherever the map is right (the level it was made for).
    pub holds: Vec<crate::until::Until>,
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
        if let Some(k) = t.keys().find(|k| !["tick", "globals", "structs", "subjects", "map"].contains(&k.as_str())) {
            return Err(format!("unknown section [{k}] (tick, globals, structs, subjects, map)"));
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
            let keys = ["struct", "at", "pointer", "offset", "stride", "count"];
            if let Some(k) = s.keys().find(|k| !keys.contains(&k.as_str())) {
                return Err(format!("{what}: unknown key {k} ({})", keys.join(", ")));
            }
            let layout = text(s.get("struct").ok_or_else(|| format!("{what}: needs struct = \"NAME\""))?, &what)?;
            if !structs.iter().any(|(k, _)| k == layout) {
                return Err(format!("{what}: no [structs.{layout}]"));
            }
            let (at, via) = match (s.get("at"), s.get("pointer")) {
                (Some(a), None) => {
                    if s.contains_key("offset") {
                        return Err(format!("{what}: offset goes with pointer"));
                    }
                    (hex(text(a, &what)?, &what)?, None)
                }
                (None, Some(p)) => {
                    let p = hex(text(p, &what)?, &format!("{what}.pointer"))?;
                    if crate::ram::locate(p, 4).is_none() {
                        return Err(format!("{what}.pointer: 0x{p:08X} is not main RAM or scratchpad"));
                    }
                    let offset = s
                        .get("offset")
                        .map(|o| text(o, &what).and_then(|o| hex(o, &format!("{what}.offset"))))
                        .transpose()?;
                    (p, Some(offset.unwrap_or(0)))
                }
                _ => return Err(format!("{what}: needs at = \"ADDR\" or pointer = \"ADDR\", one of them")),
            };
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
            subjects.push(Subject { name: n.clone(), layout: layout.to_string(), at, via, stride });
        }
        let mut map = FieldMap { path, tick, globals, structs, subjects, holds: Vec::new() };
        if let Some(t) = table(&t, "map")? {
            if let Some(k) = t.keys().find(|k| *k != "holds") {
                return Err(format!("[map]: unknown key {k} (holds)"));
            }
            let conds = t.get("holds").map(|v| v.as_array().ok_or("[map] holds: expected a list of conditions"));
            for c in conds.transpose()?.into_iter().flatten() {
                let c = text(c, "[map] holds")?;
                let u = crate::until::parse(c, Some(&map)).map_err(|e| e.replacen("--until", "[map] holds", 1))?;
                map.holds.push(u);
            }
        }
        Ok(map)
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
        // From the subject's start: its address, or the pointer's offset.
        let start = s.via.unwrap_or(s.at);
        let base = match (s.stride, index) {
            (None, None) => start,
            (Some((stride, count)), Some(i)) => {
                if count.is_some_and(|c| i >= c) {
                    return Err(format!("{label}: {name} has {} elements", count.unwrap()));
                }
                start.wrapping_add(stride.wrapping_mul(i))
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
                let name = format!("{head}.{}", f.name);
                if s.via.is_none() {
                    let c = sample_at(base.wrapping_add(f.offset), &f.fmt, Some(&name), &label)?;
                    return Ok(Sample { mapped: true, ..c });
                }
                // Read through the pointer: mem is where the address is kept.
                let c = sample_at(s.at, &f.fmt, Some(&name), &label)?;
                let mem = crate::ram::locate(s.at, 4).ok_or_else(|| format!("{label}: the pointer is not memory"))?;
                Ok(Sample { mapped: true, mem, via: Some(base.wrapping_add(f.offset)), ..c })
            })
            .collect()
    }

    pub fn report(&self) -> Value {
        let mut v = json!({"path": self.path, "names": self.names()});
        if !self.holds.is_empty() {
            v["holds"] = json!(self.holds.iter().map(|u| &u.text).collect::<Vec<_>>());
        }
        v
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

[subjects.lara]
struct = "object"
pointer = "800A0000"

[subjects.items]
struct = "object"
pointer = "800A0004"
offset = "10"
stride = "4C"
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
        assert_eq!(m.names(), ["camera_x", "player", "placement[N]", "lara", "items[N]"]);
        assert!(m.knows("placement[1].x") && !m.knows("8008B258"));
    }

    #[test]
    fn pointer_subjects_follow_the_pointer_at_each_read() {
        let m = map();
        let mut ram = vec![0u8; crate::ram::RAM_SIZE];
        let scratch = vec![0u8; crate::ram::SCRATCH_SIZE];
        let put = |ram: &mut Vec<u8>, at: usize, v: u32| ram[at..at + 4].copy_from_slice(&v.to_le_bytes());
        let x = m.resolve("lara.x").unwrap().remove(0);
        let yaw = m.resolve("items[2].yaw").unwrap().remove(0);
        assert_eq!(x.name, "lara.x");
        // Null, then pointing at an object, then moved: read where it points now.
        assert_eq!((x.read(&ram, &scratch), yaw.read(&ram, &scratch)), (None, None));
        put(&mut ram, 0xA0000, 0x8010_0000);
        put(&mut ram, 0x10_0034, 7);
        assert_eq!(x.read(&ram, &scratch), Some(crate::ram::Val::Num(7)));
        put(&mut ram, 0xA0000, 0x8011_0000);
        put(&mut ram, 0x11_0034, 9);
        assert_eq!(x.read(&ram, &scratch), Some(crate::ram::Val::Num(9)));
        put(&mut ram, 0xA0004, 0x8012_0000);
        ram[0x12_0000 + 0x10 + 2 * 0x4C + 0x42] = 5;
        assert_eq!(yaw.read(&ram, &scratch), Some(crate::ram::Val::Num(5)));
        // A row: the absent value is an empty cell.
        put(&mut ram, 0xA0000, 0);
        let mut s = crate::ram::Sampler::new(vec![x, yaw], 1, false);
        s.row(0, 0, 0, &ram, &scratch, true);
        assert_eq!(s.out, "run_frame,frame,lara.x,items[2].yaw\n0,0,,5\n");
    }

    #[test]
    fn pokes_by_name_write_where_the_map_says() {
        use crate::ram::parse_poke;
        let m = map();
        let mut ram = vec![0u8; crate::ram::RAM_SIZE];
        let mut scratch = vec![0u8; crate::ram::SCRATCH_SIZE];
        // A field in its format: fixed, signed and through a pointer.
        let mut p = parse_poke("player.yaw=-2@1..", Some(&m)).unwrap();
        assert_eq!((p.width(), p.value.clone(), p.from, p.to), (2, crate::ram::Val::Num(0xFFFE), 1, None));
        p.apply(1, &mut ram, &mut scratch);
        assert_eq!(&ram[0x17_C8BE..0x17_C8C0], &[0xFE, 0xFF]);
        assert_eq!((p.report()["name"].clone(), p.report().get("address").is_none()), ("player.yaw".into(), true));
        let mut p = parse_poke("lara.x=7", Some(&m)).unwrap();
        p.apply(0, &mut ram, &mut scratch);
        assert_eq!((p.applied.len(), p.skipped.clone()), (0, vec![0]), "a null pointer writes nothing");
        ram[0xA0000..0xA0004].copy_from_slice(&0x8010_0000u32.to_le_bytes());
        p.apply(1, &mut ram, &mut scratch);
        assert_eq!((ram[0x10_0034], p.applied.clone()), (7, vec![1]));
        assert_eq!(p.report()["skipped"], 1);
        // Hex is still an address; a name the map has wins.
        assert!(!parse_poke("8008B258=1", Some(&m)).unwrap().at.mapped);
        assert_eq!(parse_poke("camera_x=-1", Some(&m)).unwrap().value, crate::ram::Val::Num(u32::MAX));
        // A byte string's text may hold a colon (not a format).
        let names = FieldMap::parse("[globals]\nname = \"80010000:str8\"\n", "samples.toml".into()).unwrap();
        let p = parse_poke("name=a:b@3", Some(&names)).unwrap();
        assert_eq!((p.value, p.from), (crate::ram::Val::Bytes(b"a:b\0\0\0\0\0".to_vec()), 3));
        for (spec, says) in [
            ("player=1", "names 2 fields"),
            ("player.x=1:2", "format comes from the map"),
            ("player.speed=1", "no field"),
            ("player.yaw=1.5", "bad value"),
        ] {
            let e = parse_poke(spec, Some(&m)).unwrap_err();
            assert!(e.contains(says), "{spec}: {e}");
        }
    }

    #[test]
    fn bit_fields_read_and_write_their_bits_alone() {
        use crate::ram::parse_poke;
        let m = FieldMap::parse(
            "[structs.o]\nflags = \"28:x16\"\nactive = \"28:u16[14]\"\nmode = \"28:u16[0..2]\"\n\
             [subjects.door]\nstruct = \"o\"\nat = \"80100000\"\n",
            "samples.toml".into(),
        )
        .unwrap();
        let mut ram = vec![0u8; crate::ram::RAM_SIZE];
        let mut scratch = vec![0u8; crate::ram::SCRATCH_SIZE];
        ram[0x10_0028..0x10_002A].copy_from_slice(&0xC005u16.to_le_bytes());
        let mut s = crate::ram::Sampler::new(m.resolve("door").unwrap(), 1, false);
        s.row(0, 0, 0, &ram, &scratch, true);
        assert_eq!(s.out, "run_frame,frame,door.flags,door.active,door.mode\n0,0,0xC005,1,5\n");
        // A poke changes its bits and keeps the word's others.
        let mut p = parse_poke("door.active=0", Some(&m)).unwrap();
        p.apply(0, &mut ram, &mut scratch);
        assert_eq!(u16::from_le_bytes([ram[0x10_0028], ram[0x10_0029]]), 0x8005);
        assert_eq!(p.report()["format"], "u16[14]");
        assert!(parse_poke("door.active=2", Some(&m)).unwrap_err().contains("bad value"));
        let until = crate::until::parse("door.mode=5", Some(&m)).unwrap();
        assert!(until.holds(&ram, &scratch));
        assert!(FieldMap::parse("[structs.o]\na = \"0:u8[8]\"", "x".into()).err().unwrap().contains("bits of a word"));
    }

    #[test]
    fn mistakes_are_named() {
        let m = map();
        for (sel, says) in [
            ("placement", "say which"),
            ("placement[4]", "has 4 elements"),
            ("player[0]", "not an array"),
            ("player.speed", "no field \"speed\" (x, yaw)"),
            ("enemy", "names: camera_x, player, placement[N], lara, items[N]"),
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
            ("[structs.o]\nx = \"0\"\n[subjects.a]\nstruct = \"o\"", "needs at = \"ADDR\" or pointer"),
            (
                "[structs.o]\nx = \"0\"\n[subjects.a]\nstruct = \"o\"\nat = \"80000000\"\npointer = \"80000000\"",
                "one of them",
            ),
            (
                "[structs.o]\nx = \"0\"\n[subjects.a]\nstruct = \"o\"\nat = \"80000000\"\noffset = \"4\"",
                "offset goes with pointer",
            ),
            ("[structs.o]\nx = \"0\"\n[subjects.a]\nstruct = \"o\"\npointer = \"1F000000\"", "not main RAM"),
            ("[map]\nwhere = 1", "[map]: unknown key where"),
            ("[globals]\nlevel = \"80000000:u8\"\n[map]\nholds = [\"lvl=1\"]", "[map] holds"),
        ] {
            let e = FieldMap::parse(toml, "m.toml".into()).err().unwrap();
            assert!(e.contains(says), "{toml}: {e}");
        }
    }

    #[test]
    fn a_map_says_where_it_is_right() {
        let m = FieldMap::parse("[globals]\nlevel = \"80000010:u8\"\n[map]\nholds = [\"level=3\"]", "m.toml".into())
            .unwrap();
        let (mut ram, scratch) = (vec![0u8; 0x20_0000], vec![0u8; 0x400]);
        assert!(!m.holds[0].holds(&ram, &scratch));
        ram[0x10] = 3;
        assert!(m.holds[0].holds(&ram, &scratch));
        assert_eq!(m.report()["holds"], json!(["level=3"]));
    }
}
