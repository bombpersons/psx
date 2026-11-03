//! Adjustments: changes psx makes to how the original meets the machine
//! while a recording runs, never to the game's rules, so the original runs
//! to the port's model (docs/master-plan.md). A recording names the ones it
//! was made with (`producer.details.adjust`; none means stock), the
//! determinism check runs with them, and `scenario adjust-check` measures
//! them against the stock game.

use psx_core::Machine;
use serde_json::{Value as Json, json};

/// The adjustments a run is made with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Adjustments {
    /// Test adjustments: one that does nothing, and one that adds 1 to the
    /// word at an address when a tick ends.
    #[cfg(test)]
    pub test: Option<TestAdjust>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestAdjust {
    Nothing,
    /// When tick `tick` ends, the word at `addr` (in main RAM) goes up by 1.
    Nudge {
        tick: u64,
        addr: u32,
    },
}

impl Adjustments {
    /// None: the stock game.
    pub fn stock() -> Adjustments {
        Adjustments::default()
    }

    /// What `scenario record` and `scenario play` use unless `stock`: every
    /// adjustment this psx has.
    pub fn default_set() -> Adjustments {
        Adjustments::default()
    }

    pub fn is_stock(&self) -> bool {
        self.names().is_empty()
    }

    /// Their names, as recordings keep them.
    pub fn names(&self) -> Vec<&'static str> {
        #[allow(unused_mut)]
        let mut out = Vec::new();
        #[cfg(test)]
        match self.test {
            Some(TestAdjust::Nothing) => out.push("test_nothing"),
            Some(TestAdjust::Nudge { .. }) => out.push("test_nudge"),
            None => {}
        }
        out
    }

    /// The adjustments a recording names in its details (`adjust`); none
    /// named is stock. A name this psx does not know is an error.
    pub fn from_details(details: &serde_json::Map<String, Json>) -> Result<Adjustments, String> {
        let names: Vec<&str> = match details.get("adjust") {
            None | Some(Json::Null) => return Ok(Adjustments::stock()),
            Some(Json::Array(a)) => {
                a.iter().map(|v| v.as_str().ok_or("adjust: a list of names")).collect::<Result<_, _>>()?
            }
            Some(_) => return Err("adjust: a list of names".into()),
        };
        #[allow(unused_mut)]
        let mut a = Adjustments::stock();
        for n in names {
            match n {
                #[cfg(test)]
                "test_nothing" => a.test = Some(TestAdjust::Nothing),
                _ => {
                    return Err(format!(
                        "adjust: this psx does not know the adjustment {n:?}{}: update psx, or record again",
                        known()
                    ));
                }
            }
        }
        Ok(a)
    }

    /// Their entry in a recording's details, when there are any.
    pub fn detail(&self) -> Option<Json> {
        (!self.is_stock()).then(|| json!(self.names()))
    }

    /// What they keep while a run goes, made where the recording starts.
    pub fn start(&self, _m: &mut Machine) -> Running {
        Running { a: *self }
    }
}

fn known() -> String {
    let names: Vec<&str> = Vec::new();
    if names.is_empty() { String::new() } else { format!(" (it knows {})", names.join(", ")) }
}

/// Adjustments while a run goes.
pub(crate) struct Running {
    #[allow(dead_code)]
    a: Adjustments,
}

impl Running {
    /// Tick `n` ended (its quantities read) on `m`.
    pub fn tick_ended(&mut self, _n: u64, _m: &mut Machine) {
        #[cfg(test)]
        if let Some(TestAdjust::Nudge { tick, addr }) = self.a.test {
            if _n == tick {
                let a = (addr & 0x1F_FFFF) as usize;
                let ram = &mut _m.bus.ram;
                let v = u32::from_le_bytes(ram[a..a + 4].try_into().unwrap()).wrapping_add(1);
                ram[a..a + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details(v: Json) -> serde_json::Map<String, Json> {
        json!({ "adjust": v }).as_object().unwrap().clone()
    }

    #[test]
    fn a_recording_names_its_adjustments_and_unknown_ones_are_refused() {
        assert_eq!(Adjustments::from_details(&serde_json::Map::new()), Ok(Adjustments::stock()));
        let a = Adjustments { test: Some(TestAdjust::Nothing) };
        assert_eq!(a.detail(), Some(json!(["test_nothing"])));
        assert_eq!(Adjustments::from_details(&details(a.detail().unwrap())), Ok(a));
        assert_eq!(Adjustments::stock().detail(), None);
        let e = Adjustments::from_details(&details(json!(["latch_v9"]))).unwrap_err();
        assert!(e.contains("\"latch_v9\""), "{e}");
        assert!(Adjustments::from_details(&details(json!("latch"))).is_err());
    }
}
