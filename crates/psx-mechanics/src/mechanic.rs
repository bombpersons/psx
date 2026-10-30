//! A mechanic: what one step owns, reads and needs, as psx reads it from
//! the mechanic's Rust crate (`mechanics/<id>/`, [`crate::rust::mechanic`]).

use std::collections::BTreeMap;

use crate::interface::{Call, CallKind, Interface};

/// The call a used mechanic's step is recorded as.
pub const STEP: &str = "step";
use crate::value::{Field, Type, Value, is_id, is_name, parse_ref};

#[derive(Clone, Debug, PartialEq)]
pub struct Mechanic {
    /// `<game>.<name>`, also the directory's name: `croc.walk`.
    pub id: String,
    /// Bumped on any change to the fields, `requires` or `uses`; bindings and traces
    /// name the version they were made for.
    pub version: u32,
    pub title: String,
    /// One line, for listings.
    pub summary: String,
    /// `exact` (the default): the port's step reproduces every field of every
    /// step. `faithful`: each step starts from the trace's state and is
    /// compared within `tolerance`.
    pub exactness: Exactness,
    /// How often the original runs the step.
    pub rate: Rate,
    /// Interfaces the step calls, as `<id>@<version>`.
    pub requires: Vec<String>,
    /// Mechanics whose steps run inside this one's, as `<id>@<version>`.
    /// Each is recorded as a call of its [`Mechanic::step_interface`].
    pub uses: Vec<String>,
    /// What the step owns and carries from one step to the next.
    pub state: BTreeMap<String, Field>,
    /// What the step reads each step but does not own, as the step sees it.
    pub inputs: BTreeMap<String, Field>,
    /// Constants for the length of a trace; the binding gives their values.
    pub params: BTreeMap<String, Field>,
    /// Params fields holding a used mechanic's params, each by the
    /// `<id>@<version>` it uses: that mechanic's binding gives their values.
    pub used_params: BTreeMap<String, String>,
    /// `faithful` only: the largest difference allowed per state field, in
    /// raw units (int and fixed fields).
    pub tolerance: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Exactness {
    #[default]
    Exact,
    Faithful,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Rate {
    /// `tick`: once per game tick; `vblank`: once per display refresh;
    /// `event`: when something happens (a button press, a hit).
    pub per: Per,
    /// Nominal steps per second (`tick` and `vblank`): 25 for a PAL game
    /// that ticks every second field.
    pub hz: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Per {
    Tick,
    Vblank,
    Event,
}

/// A mechanic's checked field types.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Types {
    pub state: BTreeMap<String, Type>,
    pub inputs: BTreeMap<String, Type>,
    pub params: BTreeMap<String, Type>,
}

impl Mechanic {
    /// The fields' types; problems go to `problems`, and the field is left out.
    pub fn types(&self, problems: &mut Vec<String>) -> Types {
        let mut out = Types::default();
        for (what, fields, into) in [
            ("state", &self.state, &mut out.state),
            ("inputs", &self.inputs, &mut out.inputs),
            ("params", &self.params, &mut out.params),
        ] {
            for (name, f) in fields {
                if !is_name(name) {
                    problems.push(format!("{what}.{name}: not a name ([a-z][a-z0-9_]*)"));
                }
                match f.ty() {
                    Ok(t) => {
                        into.insert(name.clone(), t);
                    }
                    Err(e) => problems.push(format!("{what}.{name}: {e}")),
                }
            }
        }
        out
    }

    /// What is wrong with the entry, given the workspace's interfaces.
    pub fn check(&self, interfaces: &[Interface]) -> Vec<String> {
        let mut p = Vec::new();
        if !is_id(&self.id) || !self.id.contains('.') {
            p.push(format!("id {:?}: <game>.<name>, lowercase words joined by hyphens", self.id));
        }
        if self.version == 0 {
            p.push("version starts at 1".into());
        }
        match (self.rate.per, self.rate.hz) {
            (Per::Event, Some(_)) => p.push("rate: an event-driven step has no hz".into()),
            (Per::Tick | Per::Vblank, None) => p.push("rate: hz is needed (nominal steps per second)".into()),
            (_, Some(hz)) if !(hz.is_finite() && hz > 0.0) => p.push(format!("rate: hz {hz} must be positive")),
            _ => {}
        }
        for (what, refs, file) in
            [("requires", &self.requires, "interfaces/{id}/"), ("uses", &self.uses, "mechanics/{id}/")]
        {
            for (i, r) in refs.iter().enumerate() {
                if refs[..i].contains(r) {
                    p.push(format!("{what}: {r} is listed twice"));
                }
                let Some((id, v)) = parse_ref(r) else {
                    let kind = if what == "uses" { "mechanic" } else { "interface" };
                    p.push(format!("{what}: {r:?} is not <{kind}-id>@<version>"));
                    continue;
                };
                let file = file.replace("{id}", id);
                if what == "uses" && id == self.id {
                    p.push(format!("uses: {r}: a mechanic cannot use itself"));
                    continue;
                }
                match interfaces.iter().find(|i| i.id == id) {
                    None => p.push(format!("{what}: {r}: no {file}")),
                    Some(i) if i.version != v => p.push(format!("{what}: {r}: {file} is version {}", i.version)),
                    Some(_) => {}
                }
            }
        }
        for (name, r) in &self.used_params {
            if !self.uses.contains(r) {
                p.push(format!("params.{name}: {r}'s params, which it does not use"));
            }
            if self.params.contains_key(name) {
                p.push(format!("params.{name}: declared twice"));
            }
        }
        let types = self.types(&mut p);
        for name in types.inputs.keys().filter(|n| types.state.contains_key(*n)) {
            p.push(format!("inputs.{name}: also a state field"));
        }
        for (name, n) in &self.tolerance {
            if self.exactness == Exactness::Exact {
                p.push("tolerance: an exact mechanic has none (make it faithful)".into());
                break;
            }
            match types.state.get(name) {
                Some(Type::Int { .. } | Type::Fixed { .. }) => {}
                Some(_) => p.push(format!("tolerance.{name}: only int and fixed fields have a tolerance")),
                None => p.push(format!("tolerance.{name}: no such state field")),
            }
            if !matches!(n, Value::Int(n) if *n >= 0) {
                p.push(format!("tolerance.{name}: a non-negative integer in raw units"));
            }
        }
        p
    }

    /// A hash of what its version covers: the id, each field's type, and
    /// `requires` and `uses`. Titles, summaries, units and descriptions are
    /// left out, so rewording them needs no new version. A trace records the
    /// fingerprint it was captured with.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        fn bare(f: &Field) -> Field {
            Field {
                unit: None,
                description: None,
                of: f.of.as_deref().map(|o| Box::new(bare(o))),
                fields: f.fields.as_ref().map(|fs| fs.iter().map(|(k, v)| (k.clone(), bare(v))).collect()),
                ..f.clone()
            }
        }
        let fields = |m: &BTreeMap<String, Field>| -> BTreeMap<String, Field> {
            m.iter().map(|(k, v)| (k.clone(), bare(v))).collect()
        };
        let mut covered = serde_json::json!({
            "id": self.id,
            "state": fields(&self.state),
            "inputs": fields(&self.inputs),
            "params": fields(&self.params),
            "requires": self.requires,
            "uses": self.uses,
        });
        // Only when there are some, so a mechanic using none keeps the
        // fingerprint it had before used params were covered.
        if !self.used_params.is_empty() {
            covered["used_params"] = serde_json::json!(self.used_params);
        }
        let hash = Sha256::digest(covered.to_string().as_bytes());
        hash[..8].iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The interfaces this mechanic requires, found among `interfaces`.
    pub fn required<'a>(&self, interfaces: &'a [Interface]) -> Vec<&'a Interface> {
        let refs: Vec<_> = self.requires.iter().filter_map(|r| parse_ref(r)).collect();
        interfaces.iter().filter(|i| refs.contains(&(i.id.as_str(), i.version))).collect()
    }

    /// Everything its traces may record as calls: the interfaces it
    /// requires and the steps of the mechanics it uses.
    pub fn called<'a>(&self, interfaces: &'a [Interface]) -> Vec<&'a Interface> {
        let refs: Vec<_> = self.requires.iter().chain(&self.uses).filter_map(|r| parse_ref(r)).collect();
        interfaces.iter().filter(|i| refs.contains(&(i.id.as_str(), i.version))).collect()
    }

    /// Its step as another mechanic's trace records it: the query `step`,
    /// whose request is the inputs and state as the step begins, and whose
    /// response is the state as it ends.
    pub fn step_interface(&self) -> Interface {
        let mut request = self.inputs.clone();
        request.extend(self.state.clone());
        let call = Call {
            kind: CallKind::Query,
            description: Some(format!("a step of {}", self.id)),
            request,
            response: self.state.clone(),
        };
        Interface {
            id: self.id.clone(),
            version: self.version,
            title: self.title.clone(),
            description: Some(format!("the step of mechanic {}, for the mechanics that use it", self.id)),
            calls: [(STEP.to_string(), call)].into(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const WALK: &str = r#"
//! Walking
//!
//! Croc's speed closes on the stick's target each tick.

pub const ID: &str = "croc.walk";
pub const VERSION: u32 = 1;
pub const RATE: &str = "tick";
pub const RATE_HZ: u32 = 25;
pub const REQUIRES: &[&str] = &["world.floor-height@1"];

/// Fixed point: 20 integer bits, 12 fraction bits.
pub type Fx20_12 = i32;

pub enum Mode {
    Idle,
    Walk,
}

pub struct State {
    /// Unit: units/tick
    pub speed: Fx20_12,
    pub mode: Mode,
}

pub struct Inputs {
    pub forward: bool,
}

pub struct Params {
    pub target: u16,
    pub curve: [u8; 2],
}
"#;

    /// The mechanic `lib` declares.
    pub(crate) fn read(lib: &str) -> Mechanic {
        let files = [(crate::rust::LIB.to_string(), lib.to_string())];
        let c = crate::rust::Crate { cargo_toml: "[package]\nname = \"croc-walk\"\n", files: &files };
        let (m, problems) = crate::rust::mechanic(&c).unwrap();
        assert_eq!(problems, Vec::<String>::new());
        m
    }

    #[test]
    fn a_mechanic_checks_against_its_interfaces() {
        let m = read(WALK);
        let floor = crate::interface::tests::floor();
        assert_eq!(m.check(std::slice::from_ref(&floor)), Vec::<String>::new());
        assert_eq!(m.check(&[]), vec!["requires: world.floor-height@1: no interfaces/world.floor-height/"]);
        assert_eq!(m.exactness, Exactness::Exact);
        assert_eq!(m.required(std::slice::from_ref(&floor)).len(), 1);
    }

    #[test]
    fn a_used_mechanic_is_called_as_its_step() {
        let walk = read(WALK);
        let mut tick = walk.clone();
        tick.id = "croc.tick".into();
        tick.requires.clear();
        tick.uses = vec!["croc.walk@1".into(), "croc.walk@2".into(), "croc.tick@1".into(), "walk".into()];
        let step = walk.step_interface();
        assert_eq!(step.reference(), "croc.walk@1");
        assert_eq!(step.check(), Vec::<String>::new());
        let t = step.types(&mut Vec::new());
        assert_eq!(t[STEP].request.keys().collect::<Vec<_>>(), ["forward", "mode", "speed"]);
        assert_eq!(t[STEP].response.keys().collect::<Vec<_>>(), ["mode", "speed"]);
        let all = [step];
        assert_eq!(
            tick.check(&all),
            vec![
                "uses: croc.walk@2: mechanics/croc.walk/ is version 1",
                "uses: croc.tick@1: a mechanic cannot use itself",
                "uses: \"walk\" is not <mechanic-id>@<version>",
            ]
        );
        assert_eq!((tick.required(&all).len(), tick.called(&all).len()), (0, 1));
        tick.inputs.insert("speed".into(), walk.state["speed"].clone());
        assert!(tick.check(&all).contains(&"inputs.speed: also a state field".to_string()));
    }

    #[test]
    fn problems_are_named() {
        let mut m = read(WALK);
        m.requires.clear();
        m.rate.hz = None;
        m.tolerance.insert("speed".into(), Value::Int(1));
        assert_eq!(
            m.check(&[]),
            vec![
                "rate: hz is needed (nominal steps per second)",
                "tolerance: an exact mechanic has none (make it faithful)",
            ]
        );
        m.exactness = Exactness::Faithful;
        m.tolerance.insert("mode".into(), Value::Int(-1));
        assert_eq!(
            m.check(&[])[1..],
            [
                "tolerance.mode: only int and fixed fields have a tolerance",
                "tolerance.mode: a non-negative integer in raw units"
            ]
        );
    }
}
