//! `port confirm`: the person's approval of the goal's list.

use serde_json::{Value, json};

use super::next::Facts;
use super::{Confirmed, GOAL, Goal, Stage};
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};

/// `goal.toml`'s text with `confirmed` set to `c`: an earlier confirmation
/// (a line, or a `[confirmed]` table) replaced, everything else kept.
fn with_confirmation(text: &str, c: &Confirmed) -> String {
    let mut lines: Vec<&str> = Vec::new();
    let mut in_table = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with('[') {
            in_table = t.starts_with("[confirmed]");
            if in_table {
                continue;
            }
        }
        let key = t.split(['=', ' ', '\t']).next().unwrap_or("");
        if in_table || key == "confirmed" {
            continue;
        }
        lines.push(line);
    }
    let line = format!("confirmed = {{ at = \"{}\", features = {} }}", c.at, c.features.to_toml());
    // A top-level key goes before the first table.
    let at = lines.iter().position(|l| l.trim_start().starts_with('[')).unwrap_or(lines.len());
    lines.insert(at, &line);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortConfirmArgs {
    /// Record the confirmation. Without it, show the goal's features, the
    /// closure and any problems: what would be confirmed.
    #[arg(long)]
    #[serde(default)]
    pub yes: bool,
}

impl PortConfirmArgs {
    fn run(&self, ctx: &Ctx) -> crate::Result<Value> {
        let facts = Facts::gather(&ctx.root)?;
        let port = &facts.port;
        let mut out = json!({
            "description": port.goal.description,
            "features": port.goal.features.to_json(),
            "closure": port.closure_json(&|id| facts.stage(id)),
            "problems": port.problems,
            "stages": Stage::ALL.map(Stage::name),
            "confirmation": port.confirmation_json(),
        });
        if !self.yes {
            out["next"] = json!(if port.problems.is_empty() {
                "if this is the list the person wants: psx port confirm --yes (the person runs it)"
            } else {
                "the agent fixes the problems first"
            });
            return Ok(out);
        }
        if !port.problems.is_empty() {
            return Err(Error::invalid(format!(
                "the goal or catalogue has {} problems; run psx port confirm without --yes to see them",
                port.problems.len()
            )));
        }
        if port.goal_ids().is_empty() {
            return Err(Error::invalid("the goal names no features, and no feature is catalogued"));
        }
        let c = Confirmed { at: crate::publish::utc_now(), features: port.goal.features.clone() };
        let path = ctx.root.join(GOAL);
        let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        let new = with_confirmation(&text, &c);
        // What was written must read back as this confirmation.
        let check: Goal = toml::from_str(&new)
            .map_err(|e| Error::invalid(format!("{GOAL} rewritten unreadably: {}", e.message())))?;
        if check.confirmed.as_ref().map(|k| (&k.at, &k.features)) != Some((&c.at, &c.features)) {
            return Err(Error::invalid(format!("{GOAL} rewritten without the confirmation; nothing written")));
        }
        crate::write_atomic(&path, new.as_bytes()).map_err(|e| Error::io(&path, e))?;
        out["confirmation"] = json!({"state": "covers", "at": c.at});
        out["confirmed"] = json!({"at": c.at, "features": c.features.to_json()});
        Ok(out)
    }
}

impl Op for PortConfirmArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::result(self.run(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Features, GOAL_TOML};
    use super::*;

    #[test]
    fn confirming_rewrites_only_the_confirmation() {
        let c = Confirmed { at: "2026-10-29T10:00:00Z".into(), features: Features::List(vec!["F-0001".into()]) };
        let text = "# The goal.\ndescription = \"x\"\nfeatures = [\"F-0001\"]   # one\nconfirmed = { at = \"old\", features = [] }\n";
        assert_eq!(
            with_confirmation(text, &c),
            "# The goal.\ndescription = \"x\"\nfeatures = [\"F-0001\"]   # one\nconfirmed = { at = \"2026-10-29T10:00:00Z\", features = [\"F-0001\"] }\n"
        );
        let table = "features = \"all\"\n\n[confirmed]\nat = \"old\"\nfeatures = []\n";
        let all = Confirmed { at: "t".into(), features: Features::All };
        assert_eq!(
            with_confirmation(table, &all),
            "features = \"all\"\n\nconfirmed = { at = \"t\", features = \"all\" }\n"
        );
        let goal: Goal = toml::from_str(&with_confirmation(GOAL_TOML, &c)).unwrap();
        assert_eq!(goal.confirmed.unwrap().features, c.features);
    }
}
