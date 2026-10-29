//! `mechanics/<id>/mechanic.toml`: what one step owns, reads and needs.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::interface::Interface;
use crate::value::{Field, Type, Value, is_id, is_name, parse_ref};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mechanic {
    /// `<game>.<name>`, also the directory's name: `croc.walk`.
    pub id: String,
    /// Bumped on any change to the fields or `requires`; bindings and traces
    /// name the version they were made for.
    pub version: u32,
    pub title: String,
    /// One line, for listings.
    pub summary: String,
    /// `exact` (the default): the port's step reproduces every field of every
    /// step. `faithful`: each step starts from the trace's state and is
    /// compared within `tolerance`.
    #[serde(default)]
    pub exactness: Exactness,
    /// How often the original runs the step.
    pub rate: Rate,
    /// Interfaces the step calls, as `<id>@<version>`.
    #[serde(default)]
    pub requires: Vec<String>,
    /// What the step owns and carries from one step to the next.
    #[serde(default)]
    pub state: BTreeMap<String, Field>,
    /// What the step reads each step but does not own, as the step sees it.
    #[serde(default)]
    pub inputs: BTreeMap<String, Field>,
    /// Constants for the length of a trace; the binding gives their values.
    #[serde(default)]
    pub params: BTreeMap<String, Field>,
    /// `faithful` only: the largest difference allowed per state field, in
    /// raw units (int and fixed fields).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tolerance: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Exactness {
    #[default]
    Exact,
    Faithful,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rate {
    /// `tick`: once per game tick; `vblank`: once per display refresh;
    /// `event`: when something happens (a button press, a hit).
    pub per: Per,
    /// Nominal steps per second (`tick` and `vblank`): 25 for a PAL game
    /// that ticks every second field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hz: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
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
    pub fn parse(text: &str) -> Result<Mechanic, String> {
        toml::from_str(text).map_err(|e| e.message().to_string())
    }

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
                    Ok(Type::Table { .. }) if what != "params" => {
                        problems.push(format!("{what}.{name}: tables are for params"))
                    }
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
        for (i, r) in self.requires.iter().enumerate() {
            if self.requires[..i].contains(r) {
                p.push(format!("requires: {r} is listed twice"));
            }
            match parse_ref(r) {
                None => p.push(format!("requires: {r:?} is not <interface-id>@<version>")),
                Some((id, v)) => match interfaces.iter().find(|i| i.id == id) {
                    None => p.push(format!("requires: {r}: no interfaces/{id}.toml")),
                    Some(i) if i.version != v => {
                        p.push(format!("requires: {r}: interfaces/{id}.toml is version {}", i.version))
                    }
                    Some(_) => {}
                },
            }
        }
        let types = self.types(&mut p);
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

    /// The interfaces this mechanic requires, found among `interfaces`.
    pub fn required<'a>(&self, interfaces: &'a [Interface]) -> Vec<&'a Interface> {
        let refs: Vec<_> = self.requires.iter().filter_map(|r| parse_ref(r)).collect();
        interfaces.iter().filter(|i| refs.contains(&(i.id.as_str(), i.version))).collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const WALK: &str = r#"
id = "croc.walk"
version = 1
title = "Walking"
summary = "Croc's speed closes on the stick's target each tick."
rate = { per = "tick", hz = 25 }
requires = ["world.floor-height@1"]

[state.speed]
type = "fixed"
signed = true
int_bits = 20
frac_bits = 12
unit = "units/tick"

[state.mode]
type = "enum"
variants = ["idle", "walk"]

[inputs.forward]
type = "bool"

[params.target]
type = "int"
bits = 16
signed = false

[params.curve]
type = "table"
len = 2
of = { type = "int", bits = 8, signed = false }
"#;

    #[test]
    fn a_mechanic_checks_against_its_interfaces() {
        let m = Mechanic::parse(WALK).unwrap();
        let floor = Interface::parse(crate::interface::tests::FLOOR).unwrap();
        assert_eq!(m.check(std::slice::from_ref(&floor)), Vec::<String>::new());
        assert_eq!(m.check(&[]), vec!["requires: world.floor-height@1: no interfaces/world.floor-height.toml"]);
        assert_eq!(m.exactness, Exactness::Exact);
        assert_eq!(m.required(std::slice::from_ref(&floor)).len(), 1);
    }

    #[test]
    fn problems_are_named() {
        let mut m = Mechanic::parse(WALK).unwrap();
        m.requires.clear();
        m.rate.hz = None;
        m.inputs.insert("curve".into(), m.params["curve"].clone());
        m.tolerance.insert("speed".into(), Value::Int(1));
        assert_eq!(
            m.check(&[]),
            vec![
                "rate: hz is needed (nominal steps per second)",
                "inputs.curve: tables are for params",
                "tolerance: an exact mechanic has none (make it faithful)",
            ]
        );
        m.exactness = Exactness::Faithful;
        m.tolerance.insert("mode".into(), Value::Int(-1));
        assert_eq!(
            m.check(&[])[2..],
            [
                "tolerance.mode: only int and fixed fields have a tolerance",
                "tolerance.mode: a non-negative integer in raw units"
            ]
        );
        assert!(Mechanic::parse(&WALK.replace("summary", "sumary")).is_err());
    }
}
