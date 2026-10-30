//! An interface: the calls a mechanic makes to the world, as psx reads them
//! from the interface's Rust crate (`interfaces/<id>/`,
//! [`crate::rust::interface`]).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::value::{Field, Type, is_id, is_name};

#[derive(Clone, Debug, PartialEq)]
pub struct Interface {
    /// Broad to narrow: `world.floor-height`; also the directory's name.
    pub id: String,
    /// Bumped on any change to the calls; mechanics require `<id>@<version>`.
    pub version: u32,
    pub title: String,
    pub description: Option<String>,
    pub calls: BTreeMap<String, Call>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    /// `query`: asks the world and gets a response. `command`: acts on the
    /// world, no response. Both are checked in order.
    pub kind: CallKind,
    pub description: Option<String>,
    pub request: BTreeMap<String, Field>,
    /// Queries only.
    pub response: BTreeMap<String, Field>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CallKind {
    Query,
    Command,
}

/// A call's checked types.
#[derive(Clone, Debug, PartialEq)]
pub struct CallTypes {
    pub kind: CallKind,
    pub request: BTreeMap<String, Type>,
    pub response: BTreeMap<String, Type>,
}

impl Interface {
    /// `<id>@<version>`, as mechanics and traces name it.
    pub fn reference(&self) -> String {
        format!("{}@{}", self.id, self.version)
    }

    /// The calls' types; problems go to `problems`.
    pub fn types(&self, problems: &mut Vec<String>) -> BTreeMap<String, CallTypes> {
        let mut out = BTreeMap::new();
        for (name, c) in &self.calls {
            let at = format!("calls.{name}");
            if !is_name(name) {
                problems.push(format!("{at}: not a name ([a-z][a-z0-9_]*)"));
            }
            if c.kind == CallKind::Command && !c.response.is_empty() {
                problems.push(format!("{at}: a command has no response"));
            }
            let mut ty = |side: &str, fields: &BTreeMap<String, Field>| {
                let mut m = BTreeMap::new();
                for (f, field) in fields {
                    if !is_name(f) {
                        problems.push(format!("{at}.{side}.{f}: not a name ([a-z][a-z0-9_]*)"));
                    }
                    match field.ty() {
                        Ok(t) => {
                            m.insert(f.clone(), t);
                        }
                        Err(e) => problems.push(format!("{at}.{side}.{f}: {e}")),
                    }
                }
                m
            };
            let (request, response) = (ty("request", &c.request), ty("response", &c.response));
            out.insert(name.clone(), CallTypes { kind: c.kind, request, response });
        }
        out
    }

    pub fn check(&self) -> Vec<String> {
        let mut p = Vec::new();
        if !is_id(&self.id) {
            p.push(format!("id {:?}: lowercase words joined by hyphens, segments by dots", self.id));
        }
        if self.version == 0 {
            p.push("version starts at 1".into());
        }
        if self.calls.is_empty() {
            p.push("calls: an interface has at least one call".into());
        }
        self.types(&mut p);
        p
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const FLOOR: &str = r#"
//! Height of the floor under a point

pub const ID: &str = "world.floor-height";
pub const VERSION: u32 = 1;

pub struct Floor {
    pub y: i32,
}

pub trait FloorHeight {
    fn height(&mut self, x: i32, z: i32) -> Floor;
    fn splash(&mut self, x: i32);
}
"#;

    pub(crate) fn floor() -> Interface {
        let files = [(crate::rust::LIB.to_string(), FLOOR.to_string())];
        let c = crate::rust::Crate { cargo_toml: "[package]\nname = \"world-floor-height\"\n", files: &files };
        let (i, problems) = crate::rust::interface(&c).unwrap();
        assert_eq!(problems, Vec::<String>::new());
        i
    }

    #[test]
    fn calls_are_queries_or_commands() {
        let i = floor();
        assert_eq!(i.check(), Vec::<String>::new());
        assert_eq!(i.reference(), "world.floor-height@1");
        let mut bad = i.clone();
        bad.calls.get_mut("splash").unwrap().response = bad.calls["height"].response.clone();
        assert_eq!(bad.check(), vec!["calls.splash: a command has no response"]);
    }
}
