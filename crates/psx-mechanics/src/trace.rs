//! `mechanics/<id>/traces/*.trace.jsonl`: a mechanic's steps, captured from
//! the original. The first line is the header; each later line is one step
//! and holds only what changed.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::interface::{CallKind, Interface};
use crate::mechanic::Mechanic;
use crate::value::{Type, Values, check_values};

pub const FORMAT: &str = "mechanic-trace";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Header {
    /// `mechanic-trace`.
    pub format: String,
    /// 1.
    pub format_version: u32,
    pub mechanic: String,
    pub mechanic_version: u32,
    pub source: Source,
    pub producer: Producer,
    /// The scenario the recording was made in, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario: Option<String>,
    /// Every param's value, so the trace stands alone.
    pub params: Values,
    /// The state at the first step's start: every field but the unobserved.
    pub initial_state: Values,
    /// State fields the binding marks unobserved; never compared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unobserved: Vec<String>,
    /// What the step read that the binding does not account for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit: Option<Audit>,
    /// Which of the step's branches the recording reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Coverage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

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

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Audit {
    /// Reads the binding neither binds nor explains. The audit is clear when
    /// this is empty.
    pub unaccounted: Vec<Finding>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    /// Branches inside the bracket the recording took.
    pub reached: u64,
    /// Branches inside the bracket.
    pub total: u64,
    /// The branches not reached.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missed: Vec<Finding>,
}

/// One place in the original, in the producer's words.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    /// The producer's location (for psx: pc and overlay, or an address).
    pub at: serde_json::Map<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One step line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// From 0, with no gaps.
    pub step: u64,
    /// Inputs that differ from the previous step's (all of them at step 0).
    #[serde(default, skip_serializing_if = "Values::is_empty")]
    pub inputs: Values,
    /// The world calls the step made, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<Call>,
    /// State fields changed by code outside the step since the previous
    /// step's end (an external change), with their values at this step's
    /// start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_before: Option<Values>,
    /// State fields the step changed, with their values at its end.
    #[serde(default, skip_serializing_if = "Values::is_empty")]
    pub state: Values,
    /// The producer's frame or field number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_frame: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Call {
    /// `<id>@<version>`.
    pub interface: String,
    pub call: String,
    pub request: Values,
    /// Queries only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<Values>,
}

/// One step with everything filled in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Full {
    pub inputs: Values,
    pub calls: Vec<Call>,
    /// The state at the step's start.
    pub before: Values,
    /// The state at its end.
    pub after: Values,
    /// Whether code outside the step changed the state since the last step.
    pub external: bool,
    pub source_frame: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Trace {
    pub header: Header,
    pub steps: Vec<Step>,
}

impl Trace {
    pub fn parse(text: &str) -> Result<Trace, String> {
        let mut lines = text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty());
        let (_, first) = lines.next().ok_or("empty trace")?;
        let header: Header = serde_json::from_str(first).map_err(|e| format!("line 1 (header): {e}"))?;
        let steps = lines
            .map(|(i, l)| serde_json::from_str(l).map_err(|e| format!("line {}: {e}", i + 1)))
            .collect::<Result<_, _>>()?;
        Ok(Trace { header, steps })
    }

    /// JSON Lines: the header, then one line per step.
    pub fn to_jsonl(&self) -> String {
        let mut out = serde_json::to_string(&self.header).unwrap();
        for s in &self.steps {
            out.push('\n');
            out.push_str(&serde_json::to_string(s).unwrap());
        }
        out.push('\n');
        out
    }

    /// The step lines for full steps: only what changed. `header`'s
    /// `initial_state` is the first step's `before`.
    pub fn encode(header: Header, full: &[Full]) -> Trace {
        let changed = |from: &Values, to: &Values| -> Values {
            to.iter().filter(|(k, v)| from.get(*k) != Some(v)).map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        let (mut inputs, mut state) = (Values::new(), header.initial_state.clone());
        let mut steps = Vec::new();
        for (i, f) in full.iter().enumerate() {
            let before = changed(&state, &f.before);
            let s = Step {
                step: i as u64,
                inputs: changed(&inputs, &f.inputs),
                calls: f.calls.clone(),
                state_before: (!before.is_empty()).then_some(before),
                state: changed(&f.before, &f.after),
                source_frame: f.source_frame,
                note: None,
            };
            steps.push(s);
            inputs = f.inputs.clone();
            state = f.after.clone();
        }
        Trace { header, steps }
    }

    /// Every step with its inputs and states filled in.
    pub fn expand(&self) -> Vec<Full> {
        let (mut inputs, mut state) = (Values::new(), self.header.initial_state.clone());
        let mut out = Vec::new();
        for s in &self.steps {
            inputs.extend(s.inputs.clone());
            if let Some(b) = &s.state_before {
                state.extend(b.clone());
            }
            let before = state.clone();
            state.extend(s.state.clone());
            out.push(Full {
                inputs: inputs.clone(),
                calls: s.calls.clone(),
                before,
                after: state.clone(),
                external: s.state_before.is_some(),
                source_frame: s.source_frame,
            });
        }
        out
    }

    /// What is wrong with the trace, for `mechanic` and its interfaces.
    pub fn check(&self, mechanic: &Mechanic, interfaces: &[Interface]) -> Vec<String> {
        let mut p = Vec::new();
        let h = &self.header;
        if h.format != FORMAT || h.format_version != FORMAT_VERSION {
            p.push(format!("header: format {}/{}, expected {FORMAT}/{FORMAT_VERSION}", h.format, h.format_version));
        }
        if h.mechanic != mechanic.id || h.mechanic_version != mechanic.version {
            p.push(format!(
                "header: traces {}@{}, the mechanic is {}@{}",
                h.mechanic, h.mechanic_version, mechanic.id, mechanic.version
            ));
        }
        if h.source.kind == SourceKind::Captured && (h.source.platform.is_none() || h.source.image_sha256.is_none()) {
            p.push("header: a captured trace names its platform and image_sha256".into());
        }
        let types = mechanic.types(&mut Vec::new());
        for u in h.unobserved.iter().filter(|u| !types.state.contains_key(*u)) {
            p.push(format!("header: unobserved {u} is not a state field"));
        }
        let observed: BTreeMap<String, Type> = types
            .state
            .iter()
            .filter(|(k, _)| !h.unobserved.contains(k))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        check_values("header: params", &h.params, &types.params, false, &[], &mut p);
        check_values("header: initial_state", &h.initial_state, &observed, false, &[], &mut p);
        let calls: BTreeMap<String, _> =
            mechanic.required(interfaces).into_iter().map(|i| (i.reference(), i.types(&mut Vec::new()))).collect();
        for (i, s) in self.steps.iter().enumerate() {
            let at = format!("step {i}");
            if s.step != i as u64 {
                p.push(format!("{at}: numbered {}", s.step));
            }
            check_values(&format!("{at}: inputs"), &s.inputs, &types.inputs, i > 0, &[], &mut p);
            if let Some(b) = &s.state_before {
                check_values(&format!("{at}: state_before"), b, &observed, true, &[], &mut p);
            }
            check_values(&format!("{at}: state"), &s.state, &observed, true, &[], &mut p);
            for (n, c) in s.calls.iter().enumerate() {
                let at = format!("{at}: calls[{n}]");
                let Some(ct) = calls.get(&c.interface).and_then(|i| i.get(&c.call)) else {
                    p.push(format!("{at}: {}.{} is not a call of a required interface", c.interface, c.call));
                    continue;
                };
                check_values(&format!("{at}: request"), &c.request, &ct.request, false, &[], &mut p);
                match (ct.kind, &c.response) {
                    (CallKind::Query, Some(r)) => {
                        check_values(&format!("{at}: response"), r, &ct.response, false, &[], &mut p)
                    }
                    (CallKind::Query, None) => p.push(format!("{at}: a query has a response")),
                    (CallKind::Command, Some(_)) => p.push(format!("{at}: a command has no response")),
                    (CallKind::Command, None) => {}
                }
            }
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::tests::FLOOR;
    use crate::mechanic::tests::WALK;
    use crate::value::Value;

    const TRACE: &str = r#"{"format":"mechanic-trace","format_version":1,"mechanic":"croc.walk","mechanic_version":1,"source":{"kind":"captured","platform":"psx","image_sha256":"00"},"producer":{"name":"psx","version":"0.8.0"},"params":{"curve":[1,2],"target":35},"initial_state":{"mode":"idle","speed":0}}
{"step":0,"inputs":{"forward":true},"calls":[{"interface":"world.floor-height@1","call":"height","request":{"x":0,"z":0},"response":{"y":-5}}],"state":{"mode":"walk","speed":35}}
{"step":1,"state":{"speed":61}}
{"step":2,"inputs":{"forward":false},"calls":[{"interface":"world.floor-height@1","call":"splash","request":{"x":3}}],"state_before":{"speed":0},"state":{"mode":"idle"}}
"#;

    #[test]
    fn a_trace_checks_and_expands() {
        let t = Trace::parse(TRACE).unwrap();
        let (m, i) = (Mechanic::parse(WALK).unwrap(), [Interface::parse(FLOOR).unwrap()]);
        assert_eq!(t.check(&m, &i), Vec::<String>::new());
        let full = t.expand();
        assert_eq!(full[1].inputs["forward"], Value::Bool(true));
        assert_eq!((full[1].before["speed"].clone(), full[1].after["speed"].clone()), (Value::Int(35), Value::Int(61)));
        assert!(full[2].external && !full[1].external);
        assert_eq!(full[2].before["speed"], Value::Int(0));
        assert_eq!(
            full[2].after,
            [("mode".into(), Value::Variant("idle".into())), ("speed".into(), Value::Int(0))].into()
        );
        assert_eq!(Trace::encode(t.header.clone(), &full).to_jsonl(), TRACE);
    }

    #[test]
    fn problems_name_the_step_and_field() {
        let (m, i) = (Mechanic::parse(WALK).unwrap(), [Interface::parse(FLOOR).unwrap()]);
        let bad = TRACE
            .replace(r#"{"step":1,"state":{"speed":61}}"#, r#"{"step":1,"state":{"speed":61,"mood":1}}"#)
            .replace(r#""response":{"y":-5}"#, r#""response":{"y":-5000000000}"#)
            .replace(r#""inputs":{"forward":true},"#, "");
        assert_eq!(
            Trace::parse(&bad).unwrap().check(&m, &i),
            vec![
                "step 0: inputs: forward is missing",
                "step 0: calls[0]: response: y: -5000000000 is outside -2147483648..=2147483647",
                "step 1: state: mood is not declared",
            ]
        );
        assert!(Trace::parse("{\"format\":1}").unwrap_err().starts_with("line 1 (header)"));
    }
}
