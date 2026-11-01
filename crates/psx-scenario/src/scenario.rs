//! Scenarios: the glossary (the names quantities go by, the `glossary`
//! crate's `Quantities`, read by [`crate::rust::glossary`]),
//! `scenarios/<id>.toml` (a situation both the original and the port can
//! reach) and the scenario's recordings, `scenarios/<id>/<name>.jsonl` (the
//! original's quantities, tick by tick).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::value::{Field, Type, Value, Values, check_values, is_name};

pub const RECORDING_FORMAT: &str = "scenario-recording";
pub const RECORDING_FORMAT_VERSION: u32 = 1;

/// The fields the determinism check idles for when a scenario gives none.
pub const IDLE: u32 = 60;

/// The glossary: the quantities scenarios compare, each with its type,
/// unit and meaning in game terms.
#[derive(Clone, Debug, PartialEq)]
pub struct Glossary {
    /// The crate documentation's first paragraph: the release, in words.
    pub title: String,
    pub quantities: BTreeMap<String, Field>,
}

impl Glossary {
    /// Each quantity's checked type; a quantity whose type has a problem is
    /// left out.
    pub fn types(&self) -> BTreeMap<String, Type> {
        self.quantities
            .iter()
            .filter_map(|(n, f)| f.ty().ok().filter(|t| !t.is_data()).map(|t| (n.clone(), t)))
            .collect()
    }

    pub fn check(&self) -> Vec<String> {
        let mut p = Vec::new();
        for (n, f) in &self.quantities {
            if !is_name(n) {
                p.push(format!("Quantities.{n}: not a name ([a-z][a-z0-9_]*)"));
            }
            match f.ty() {
                Ok(t) if t.is_data() => p.push(format!(
                    "Quantities.{n}: a quantity is a number, a bool or an enum, not a table, list or record"
                )),
                Ok(_) => {}
                Err(e) => p.push(format!("Quantities.{n}: {e}")),
            }
        }
        p
    }

    /// A hash of the quantities' names and types (not their documentation):
    /// the glossary's binding was generated from the same ones.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let bare: BTreeMap<&String, Field> =
            self.quantities.iter().map(|(k, f)| (k, Field { unit: None, description: None, ..f.clone() })).collect();
        let hash = Sha256::digest(serde_json::json!({"quantities": bare}).to_string().as_bytes());
        hash[..8].iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// `scenarios/<id>.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// The moment it starts at, in game terms.
    pub moment: String,
    /// How the original gets there: the producer's state chain, relative to
    /// the workspace (for psx, `notes/states/<name>.json`).
    pub chain: String,
    /// The port's launcher for it.
    pub kind: String,
    /// The determinism check's idle fields, added before the chain's last
    /// run (60 when left out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle: Option<u32>,
    /// The glossary quantities compared, each with its tolerance in raw
    /// units (0: exactly; bool and enum quantities only exactly).
    pub compare: BTreeMap<String, Value>,
    /// How the port gets there: what its launcher builds the world from.
    #[serde(default)]
    pub parameters: serde_json::Map<String, serde_json::Value>,
}

impl Scenario {
    pub fn parse(text: &str) -> Result<Scenario, String> {
        toml::from_str(text).map_err(|e| e.message().to_string())
    }

    /// What is wrong with it, given the glossary's quantity types.
    pub fn check(&self, quantities: &BTreeMap<String, Type>) -> Vec<String> {
        let mut p = Vec::new();
        for (what, v) in [("moment", &self.moment), ("chain", &self.chain)] {
            if v.trim().is_empty() {
                p.push(format!("{what}: empty"));
            }
        }
        if !is_name(&self.kind) {
            p.push(format!("kind {:?}: not a name ([a-z][a-z0-9_]*)", self.kind));
        }
        if self.idle == Some(0) {
            p.push("idle: at least 1 field".into());
        }
        if self.compare.is_empty() {
            p.push("compare: at least one quantity".into());
        }
        for (n, tol) in &self.compare {
            let Some(ty) = quantities.get(n) else {
                p.push(format!("compare.{n}: not a glossary quantity"));
                continue;
            };
            match (ty, tol) {
                (Type::Bool | Type::Enum(_), Value::Int(0)) => {}
                (Type::Bool | Type::Enum(_), _) => {
                    p.push(format!("compare.{n}: a {} is compared exactly: 0", kind(ty)))
                }
                (_, Value::Int(t)) if *t >= 0 => {}
                _ => p.push(format!("compare.{n}: a tolerance is an integer, 0 or more, in raw units")),
            }
        }
        p
    }

    pub fn idle(&self) -> u32 {
        self.idle.unwrap_or(IDLE)
    }
}

fn kind(ty: &Type) -> &'static str {
    match ty {
        Type::Bool => "bool",
        _ => "enum",
    }
}

/// Where a recording came from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// `captured` (from the original: evidence) or `synthetic` (written by
    /// hand or by code: examples and tests only).
    pub kind: SourceKind,
    /// Captured: the producer's platform and the image's identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Captured,
    Synthetic,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Producer {
    pub name: String,
    pub version: String,
    /// The producer's own: how to make the recording again.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub details: serde_json::Map<String, serde_json::Value>,
}

/// A recording's first line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Header {
    /// `scenario-recording`.
    pub format: String,
    /// 1.
    pub format_version: u32,
    pub scenario: String,
    pub source: Source,
    pub producer: Producer,
    /// The compared quantities' declarations, so the recording stands alone.
    pub quantities: BTreeMap<String, Field>,
    /// Their values at the end of the recording's first tick: the moment the
    /// port's launcher builds.
    pub initial: Values,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One tick line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Row {
    /// Ticks since the initial moment: 1, 2, ... with no gaps.
    pub tick: u64,
    /// The field the tick ended in, counted from the recording's start.
    /// Two ticks in one field, or a field with none, is the original
    /// falling behind.
    pub field: u64,
    /// The buttons held during that field, when they differ from the
    /// previous tick's (none are held at the initial moment).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad: Option<Vec<String>>,
    /// Whether the pad is in analog mode (a DualShock answering with its
    /// sticks, ID 73h), when it differs from the previous tick's (digital at
    /// the initial moment). It is what the game read, whatever switched it:
    /// the game itself or the player's ANALOG button.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analog: Option<bool>,
    /// The analog sticks' axes that differ from the previous tick's (all
    /// centred at the initial moment, and on a pad not in analog mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sticks: Option<Sticks>,
    /// The quantities that changed, with their values at the tick's end.
    #[serde(default, skip_serializing_if = "Values::is_empty")]
    pub values: Values,
}

/// The analog sticks' raw bytes, as the game read them in the field: 0
/// is left or up, 0xFF right or down, [`CENTRED`] the stick at rest. In a
/// row, only the axes that changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Sticks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lx: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ly: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rx: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ry: Option<u8>,
}

impl Sticks {
    /// The axes of `now` that differ from `before` (lx, ly, rx, ry).
    pub fn changed(before: [u8; 4], now: [u8; 4]) -> Option<Sticks> {
        let axis = |i: usize| (before[i] != now[i]).then_some(now[i]);
        let s = Sticks { lx: axis(0), ly: axis(1), rx: axis(2), ry: axis(3) };
        (s != Sticks::default()).then_some(s)
    }

    /// `before` with these axes set.
    pub fn apply(&self, before: [u8; 4]) -> [u8; 4] {
        let [lx, ly, rx, ry] = before;
        [self.lx.unwrap_or(lx), self.ly.unwrap_or(ly), self.rx.unwrap_or(rx), self.ry.unwrap_or(ry)]
    }
}

/// Both sticks at rest (lx, ly, rx, ry).
pub const CENTRED: [u8; 4] = [0x80; 4];

/// One tick with everything filled in.
#[derive(Clone, Debug, PartialEq)]
pub struct Full {
    pub tick: u64,
    pub field: u64,
    pub pad: Vec<String>,
    /// The pad in analog mode.
    pub analog: bool,
    /// lx, ly, rx, ry.
    pub sticks: [u8; 4],
    pub values: Values,
}

impl Default for Full {
    fn default() -> Full {
        Full { tick: 0, field: 0, pad: Vec::new(), analog: false, sticks: CENTRED, values: Values::new() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Recording {
    pub header: Header,
    pub rows: Vec<Row>,
}

impl Recording {
    pub fn parse(text: &str) -> Result<Recording, String> {
        let mut lines = text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty());
        let (_, first) = lines.next().ok_or("empty recording")?;
        let header: Header = serde_json::from_str(first).map_err(|e| format!("line 1 (header): {e}"))?;
        let rows = lines
            .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("line {}: {e}", i + 1)))
            .collect::<Result<_, _>>()?;
        Ok(Recording { header, rows })
    }

    /// JSON Lines: the header, then one line per tick.
    pub fn to_jsonl(&self) -> String {
        let mut out = serde_json::to_string(&self.header).unwrap();
        for r in &self.rows {
            out.push('\n');
            out.push_str(&serde_json::to_string(r).unwrap());
        }
        out.push('\n');
        out
    }

    /// The lines for full ticks: only what changed since the header's
    /// `initial` and the ticks before.
    pub fn encode(header: Header, full: &[Full]) -> Recording {
        let (mut pad, mut sticks, mut values) = (Vec::<String>::new(), CENTRED, header.initial.clone());
        let mut analog = false;
        let mut rows = Vec::new();
        for f in full {
            rows.push(Row {
                tick: f.tick,
                field: f.field,
                pad: (f.pad != pad).then(|| f.pad.clone()),
                analog: (f.analog != analog).then_some(f.analog),
                sticks: Sticks::changed(sticks, f.sticks),
                values: f
                    .values
                    .iter()
                    .filter(|(k, v)| values.get(*k) != Some(v))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            });
            pad = f.pad.clone();
            analog = f.analog;
            sticks = f.sticks;
            values = f.values.clone();
        }
        Recording { header, rows }
    }

    /// Every tick with its pad, mode, sticks and values filled in.
    pub fn expand(&self) -> Vec<Full> {
        let (mut pad, mut sticks, mut values) = (Vec::new(), CENTRED, self.header.initial.clone());
        let mut analog = false;
        self.rows
            .iter()
            .map(|r| {
                if let Some(p) = &r.pad {
                    pad = p.clone();
                }
                analog = r.analog.unwrap_or(analog);
                if let Some(s) = &r.sticks {
                    sticks = s.apply(sticks);
                }
                values.extend(r.values.clone());
                Full { tick: r.tick, field: r.field, pad: pad.clone(), analog, sticks, values: values.clone() }
            })
            .collect()
    }

    /// What is wrong with the recording, for the scenario `id`.
    pub fn check(&self, id: &str) -> Vec<String> {
        let mut p = Vec::new();
        let h = &self.header;
        if h.format != RECORDING_FORMAT || h.format_version != RECORDING_FORMAT_VERSION {
            p.push(format!(
                "header: format {}/{}, expected {RECORDING_FORMAT}/{RECORDING_FORMAT_VERSION}",
                h.format, h.format_version
            ));
        }
        if h.scenario != id {
            p.push(format!("header: records scenario {}, its file is under {id}", h.scenario));
        }
        if h.source.kind == SourceKind::Captured && (h.source.platform.is_none() || h.source.image_sha256.is_none()) {
            p.push("header: a captured recording names its platform and image_sha256".into());
        }
        let mut types = BTreeMap::new();
        for (n, f) in &h.quantities {
            match f.ty() {
                Ok(t) if t.is_data() => {
                    p.push(format!("header: quantities.{n}: tables, lists and records are for params"))
                }
                Ok(t) => {
                    types.insert(n.clone(), t);
                }
                Err(e) => p.push(format!("header: quantities.{n}: {e}")),
            }
        }
        check_values("header: initial", &h.initial, &types, false, &[], &mut p);
        let mut field = 0;
        let (mut analog, mut sticks, mut unread) = (false, CENTRED, false);
        for (i, r) in self.rows.iter().enumerate() {
            let at = format!("tick {}", i + 1);
            if r.tick != i as u64 + 1 {
                p.push(format!("{at}: numbered {}", r.tick));
            }
            if r.field < field {
                p.push(format!("{at}: field {} is before the previous tick's {field}", r.field));
            }
            field = r.field;
            analog = r.analog.unwrap_or(analog);
            sticks = r.sticks.map_or(sticks, |s| s.apply(sticks));
            // Once for each stretch of ticks it lasts.
            if !analog && sticks != CENTRED && !unread {
                p.push(format!("{at}: sticks off centre on a pad not in analog mode, which does not send them"));
            }
            unread = !analog && sticks != CENTRED;
            check_values(&format!("{at}: values"), &r.values, &types, true, &[], &mut p);
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rust::{Crate, LIB};

    pub(crate) const GLOSSARY: &str = r#"
//! Croc (Europe) SLES-00593: the quantities scenarios compare.

/// Fixed point: 20 integer bits, 12 fraction bits.
pub type Fx20_12 = i32;

pub enum Mode {
    Idle,
    Walk,
}

pub struct Quantities {
    /// Croc's position across the level
    /// Unit: world units
    pub croc_x: Fx20_12,
    pub croc_mode: Mode,
}
"#;

    const SCENARIO: &str = r#"
moment = "the first tick Croc can move, standing at the start of level 1-1"
chain = "notes/states/level_1_1.json"
kind = "level"
[compare]
croc_x = 0
croc_mode = 0
[parameters]
level = { asset = "level/1-1" }
"#;

    const RECORDING: &str = r#"{"format":"scenario-recording","format_version":1,"scenario":"level_start","source":{"kind":"captured","platform":"psx","image_sha256":"00"},"producer":{"name":"psx","version":"0.8.0"},"quantities":{"croc_mode":{"type":"enum","variants":["idle","walk"]},"croc_x":{"type":"fixed","signed":true,"int_bits":20,"frac_bits":12}},"initial":{"croc_mode":"idle","croc_x":4096}}
{"tick":1,"field":2,"pad":["RIGHT"],"values":{"croc_mode":"walk","croc_x":4196}}
{"tick":2,"field":4,"analog":true,"sticks":{"lx":143},"values":{"croc_x":4296}}
{"tick":3,"field":4,"pad":[],"sticks":{"lx":128,"ry":0}}
"#;

    fn glossary() -> Glossary {
        let files = [(LIB.to_string(), GLOSSARY.to_string())];
        let (g, problems) =
            crate::rust::glossary(&Crate { cargo_toml: "[package]\nname = \"glossary\"\n", files: &files }).unwrap();
        assert_eq!(problems, Vec::<String>::new());
        g
    }

    #[test]
    fn a_glossary_and_scenario_check() {
        let g = glossary();
        assert_eq!(g.title, "Croc (Europe) SLES-00593: the quantities scenarios compare");
        assert_eq!(g.quantities["croc_x"].unit.as_deref(), Some("world units"));
        let s = Scenario::parse(SCENARIO).unwrap();
        assert_eq!(s.check(&g.types()), Vec::<String>::new());
        assert_eq!((s.idle(), s.parameters["level"]["asset"].as_str()), (60, Some("level/1-1")));
    }

    #[test]
    fn glossary_and_scenario_mistakes_are_named() {
        let files = [(
            LIB.to_string(),
            GLOSSARY.replace("pub croc_mode: Mode,", "pub croc_mode: Mode,\n    pub path: Vec<i32>,"),
        )];
        let (_, problems) =
            crate::rust::glossary(&Crate { cargo_toml: "[package]\nname = \"glossary\"\n", files: &files }).unwrap();
        assert_eq!(
            problems,
            ["src/lib.rs: Quantities.path: a quantity is a number, a bool or an enum, not a table, list or record"]
        );
        let s = Scenario::parse(
            &SCENARIO.replace("croc_mode = 0", "croc_mode = 1\nspeed = 0").replace("croc_x = 0", "croc_x = -1"),
        )
        .unwrap();
        assert_eq!(
            s.check(&glossary().types()),
            vec![
                "compare.croc_mode: a enum is compared exactly: 0",
                "compare.croc_x: a tolerance is an integer, 0 or more, in raw units",
                "compare.speed: not a glossary quantity",
            ]
        );
    }

    #[test]
    fn a_recording_checks_and_expands() {
        let r = Recording::parse(RECORDING).unwrap();
        assert_eq!(r.check("level_start"), Vec::<String>::new());
        let full = r.expand();
        assert_eq!(full.len(), 3);
        assert_eq!(
            (full[1].pad.clone(), full[1].values["croc_x"].clone()),
            (vec!["RIGHT".to_string()], Value::Int(4296))
        );
        assert_eq!((full[2].field, full[2].pad.len()), (4, 0));
        assert_eq!((full[0].analog, full[1].analog, full[2].analog), (false, true, true));
        // Sticks: the axes that changed, centred before.
        assert_eq!(
            (full[0].sticks, full[1].sticks, full[2].sticks),
            ([128; 4], [143, 128, 128, 128], [128, 128, 128, 0])
        );
        assert_eq!(Recording::encode(r.header.clone(), &full).to_jsonl(), RECORDING);
        let bad = RECORDING.replace(r#""tick":2,"field":4"#, r#""tick":5,"field":1"#).replace("4296", "\"walk\"");
        assert_eq!(
            Recording::parse(&bad).unwrap().check("other"),
            vec![
                "header: records scenario level_start, its file is under other",
                "tick 2: numbered 5",
                "tick 2: field 1 is before the previous tick's 2",
                "tick 2: values: croc_x: expected an integer, got \"walk\"",
            ]
        );
        let digital = RECORDING.replace(r#""analog":true,"#, "");
        assert_eq!(
            Recording::parse(&digital).unwrap().check("level_start"),
            vec!["tick 2: sticks off centre on a pad not in analog mode, which does not send them"]
        );
    }
}
