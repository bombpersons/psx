//! Scenarios: `glossary.toml` (the names quantities go by, and where the
//! original keeps them), `scenarios/<id>.toml` (a situation both the
//! original and the port can reach) and the scenario's recordings,
//! `scenarios/<id>/<name>.jsonl` (the original's quantities, tick by tick).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::binding::{Platform, Read, Role, Source, check_read};
use crate::trace::{Producer, SourceKind};
use crate::value::{Field, Type, Value, Values, check_values, is_name};

pub const RECORDING_FORMAT: &str = "scenario-recording";
pub const RECORDING_FORMAT_VERSION: u32 = 1;

/// The fields the determinism check idles for when a scenario gives none.
pub const IDLE: u32 = 60;

/// `glossary.toml`, generic over the platform's location (`L`) and tick
/// (`T`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, bound(deserialize = "L: Deserialize<'de>, T: Deserialize<'de>"))]
pub struct Glossary<L, T> {
    /// The release the bindings are for.
    pub source: Source,
    /// What ends one tick of the game's logic: quantities are read there.
    pub tick: T,
    /// Each quantity's type, unit and meaning, in game terms.
    #[serde(default)]
    pub quantities: BTreeMap<String, Field>,
    /// Where the original keeps each quantity.
    #[serde(default)]
    pub bindings: BTreeMap<String, Read<L>>,
}

/// The glossary of a platform.
pub type GlossaryFor<P> = Glossary<<P as Platform>::Location, <P as Platform>::Tick>;

impl<L, T> Glossary<L, T> {
    /// Each quantity's checked type; a quantity whose type has a problem is
    /// left out.
    pub fn types(&self) -> BTreeMap<String, Type> {
        self.quantities
            .iter()
            .filter_map(|(n, f)| f.ty().ok().filter(|t| !matches!(t, Type::Table { .. })).map(|t| (n.clone(), t)))
            .collect()
    }

    pub fn check<P>(&self) -> Vec<String>
    where
        P: Platform<Location = L, Tick = T>,
    {
        let mut p = Vec::new();
        self.source.check(P::NAME, &mut p);
        P::check_tick(&self.tick, &mut p);
        for (n, f) in &self.quantities {
            if !is_name(n) {
                p.push(format!("quantities.{n}: not a name ([a-z][a-z0-9_]*)"));
            }
            match f.ty() {
                Ok(Type::Table { .. }) => p.push(format!("quantities.{n}: tables are for params")),
                Ok(_) => {}
                Err(e) => p.push(format!("quantities.{n}: {e}")),
            }
            if !self.bindings.contains_key(n) {
                p.push(format!("quantities.{n}: not bound"));
            }
        }
        let types = self.types();
        for (n, r) in &self.bindings {
            match types.get(n) {
                Some(ty) => check_read::<P>(&format!("bindings.{n}"), r, ty, Role::Quantity, &mut p),
                None if !self.quantities.contains_key(n) => p.push(format!("bindings.{n}: no such quantity")),
                None => {}
            }
        }
        p
    }
}

/// `scenarios/<id>.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// The feature whose integration test it is.
    pub feature: String,
    /// The moment it starts at, in game terms.
    pub moment: String,
    /// How the original gets there: the producer's state chain, relative to
    /// the workspace (for psx, `notes/states/<name>.json`).
    pub chain: String,
    /// The port's launcher for it, registered by the feature's plugin.
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

/// A recording's first line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Header {
    /// `scenario-recording`.
    pub format: String,
    /// 1.
    pub format_version: u32,
    pub scenario: String,
    pub source: crate::trace::Source,
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
    /// The quantities that changed, with their values at the tick's end.
    #[serde(default, skip_serializing_if = "Values::is_empty")]
    pub values: Values,
}

/// One tick with everything filled in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Full {
    pub tick: u64,
    pub field: u64,
    pub pad: Vec<String>,
    pub values: Values,
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
        let (mut pad, mut values) = (Vec::<String>::new(), header.initial.clone());
        let mut rows = Vec::new();
        for f in full {
            rows.push(Row {
                tick: f.tick,
                field: f.field,
                pad: (f.pad != pad).then(|| f.pad.clone()),
                values: f
                    .values
                    .iter()
                    .filter(|(k, v)| values.get(*k) != Some(v))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            });
            pad = f.pad.clone();
            values = f.values.clone();
        }
        Recording { header, rows }
    }

    /// Every tick with its pad and values filled in.
    pub fn expand(&self) -> Vec<Full> {
        let (mut pad, mut values) = (Vec::new(), self.header.initial.clone());
        self.rows
            .iter()
            .map(|r| {
                if let Some(p) = &r.pad {
                    pad = p.clone();
                }
                values.extend(r.values.clone());
                Full { tick: r.tick, field: r.field, pad: pad.clone(), values: values.clone() }
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
                Ok(Type::Table { .. }) => p.push(format!("header: quantities.{n}: tables are for params")),
                Ok(t) => {
                    types.insert(n.clone(), t);
                }
                Err(e) => p.push(format!("header: quantities.{n}: {e}")),
            }
        }
        check_values("header: initial", &h.initial, &types, false, &[], &mut p);
        let mut field = 0;
        for (i, r) in self.rows.iter().enumerate() {
            let at = format!("tick {}", i + 1);
            if r.tick != i as u64 + 1 {
                p.push(format!("{at}: numbered {}", r.tick));
            }
            if r.field < field {
                p.push(format!("{at}: field {} is before the previous tick's {field}", r.field));
            }
            field = r.field;
            check_values(&format!("{at}: values"), &r.values, &types, true, &[], &mut p);
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::tests::Flat;

    const GLOSSARY: &str = r#"
source = { platform = "flat", title = "Croc (Europe)", image_sha256 = "…" }
tick = { address = 0x80087AA4 }

[quantities.croc_x]
type = "fixed"
signed = true
int_bits = 20
frac_bits = 12
unit = "world units"
description = "Croc's position across the level"

[quantities.croc_mode]
type = "enum"
variants = ["idle", "walk"]

[bindings.croc_x]
parts = [{ at = { address = 0x8017C8B0 }, bytes = 4 }]

[bindings.croc_mode]
parts = [{ at = { address = 0x8017C8C0 }, bytes = 1 }]
map = { 0 = "idle", 1 = "walk" }
"#;

    const SCENARIO: &str = r#"
feature = "F-0001"
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
{"tick":2,"field":4,"values":{"croc_x":4296}}
{"tick":3,"field":4,"pad":[]}
"#;

    fn glossary() -> GlossaryFor<Flat> {
        toml::from_str(&GLOSSARY.replace('…', &"0".repeat(64))).unwrap()
    }

    #[test]
    fn a_glossary_and_scenario_check() {
        let g = glossary();
        assert_eq!(g.check::<Flat>(), Vec::<String>::new());
        let s = Scenario::parse(SCENARIO).unwrap();
        assert_eq!(s.check(&g.types()), Vec::<String>::new());
        assert_eq!((s.idle(), s.parameters["level"]["asset"].as_str()), (60, Some("level/1-1")));
    }

    #[test]
    fn glossary_and_scenario_mistakes_are_named() {
        let mut g = glossary();
        g.bindings.remove("croc_mode");
        g.bindings.insert("speed".into(), g.bindings["croc_x"].clone());
        assert_eq!(g.check::<Flat>(), vec!["quantities.croc_mode: not bound", "bindings.speed: no such quantity"]);
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
    }
}
