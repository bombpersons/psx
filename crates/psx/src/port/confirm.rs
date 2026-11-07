//! `port confirm`: the person's approval of the goal: its description,
//! assets, held-out plays and the branches no recording can reach.

use serde_json::{Value, json};

use super::next::Facts;
use super::{Confirmed, GOAL, Goal};
use crate::Error;
use crate::commands::{Ctx, Op, Outcome};

/// `goal.toml`'s text with `confirmed` set to `c`: an earlier confirmation
/// (a line, or a `[confirmed]` table) replaced, everything else kept.
pub(super) fn with_confirmation(text: &str, c: &Confirmed) -> String {
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
    let mut line = format!("confirmed = {{ at = \"{}\"", c.at);
    if let Some(d) = &c.description {
        line += &format!(", description = \"{d}\"");
    }
    if !c.assets.is_none() {
        line += &format!(", assets = {}", c.assets.to_toml());
    }
    if let Some(h) = &c.held_out {
        line += &format!(", held_out = {}", toml::Value::from(h.clone()));
    }
    if !c.wont_reach.is_empty() {
        let v = toml::Value::try_from(&c.wont_reach).expect("strings serialise");
        line += &format!(", wont_reach = {v}");
    }
    line += " }";
    // A top-level key goes before the first table.
    let at = lines.iter().position(|l| l.trim_start().starts_with('[')).unwrap_or(lines.len());
    lines.insert(at, &line);
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

#[derive(clap::Args, Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PortConfirmArgs {
    /// Record the confirmation. Without it, show the goal, the person's
    /// held-out plays, the wont_reach branches and any problems: what
    /// would be confirmed.
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
            "assets": port.goal.assets.to_json(),
            "recordings": {"work": port.recordings.work.len(), "held_out": port.goal.held_out},
            "problems": port.problems,
            "confirmation": port.confirmation_json(),
            "wont_reach": port.wont_reach,
        });
        let ranges = facts.wont_reach_ranges();
        if !ranges.is_empty() {
            // What each range claims: the branches no recording ran in it.
            out["wont_reach_range_branches"] = json!(ranges);
        }
        if !self.yes {
            out["next"] = json!(if port.problems.is_empty() {
                "if this is the goal the person wants, the held-out plays theirs, and each wont_reach branch is one no recording can reach for its reason: psx port confirm --yes (the person runs it)"
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
        let c = Confirmed {
            at: crate::stamp::utc_now(),
            description: Some(super::description_stamp(&port.goal.description)),
            assets: port.goal.assets.clone(),
            wont_reach: port.wont_reach.clone(),
            held_out: Some(port.goal.held_out.clone()),
            features: None,
        };
        let path = ctx.root.join(GOAL);
        let text = std::fs::read_to_string(&path).map_err(|e| Error::io(&path, e))?;
        let new = with_confirmation(&text, &c);
        // What was written must read back as this confirmation.
        let check: Goal = toml::from_str(&new)
            .map_err(|e| Error::invalid(format!("{GOAL} rewritten unreadably: {}", e.message())))?;
        if check.confirmed.as_ref() != Some(&c) {
            return Err(Error::invalid(format!("{GOAL} rewritten without the confirmation; nothing written")));
        }
        crate::write_atomic(&path, new.as_bytes()).map_err(|e| Error::io(&path, e))?;
        out["confirmation"] = json!({"state": "covers", "at": c.at});
        out["confirmed"] =
            json!({"at": c.at, "assets": c.assets.to_json(), "held_out": c.held_out, "wont_reach": c.wont_reach});
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
    use super::super::{Assets, GOAL_TOML};
    use super::*;

    fn confirmed(at: &str) -> Confirmed {
        Confirmed {
            at: at.into(),
            description: None,
            assets: Default::default(),
            wont_reach: Default::default(),
            held_out: None,
            features: None,
        }
    }

    #[test]
    fn confirming_rewrites_only_the_confirmation() {
        let c = confirmed("2026-10-29T10:00:00Z");
        let text = "# The goal.\ndescription = \"x\"   # one\nconfirmed = { at = \"old\", features = [] }\n";
        assert_eq!(
            with_confirmation(text, &c),
            "# The goal.\ndescription = \"x\"   # one\nconfirmed = { at = \"2026-10-29T10:00:00Z\" }\n"
        );
        let table = "description = \"x\"\n\n[confirmed]\nat = \"old\"\nfeatures = []\n";
        assert_eq!(with_confirmation(table, &confirmed("t")), "description = \"x\"\n\nconfirmed = { at = \"t\" }\n");
        let goal: Goal = toml::from_str(&with_confirmation(GOAL_TOML, &c)).unwrap();
        assert_eq!(goal.confirmed.unwrap(), c);
        // The won't-reach branches the person saw, reasons and all, and their plays.
        let mut w = c.clone();
        w.assets = Assets::Names(vec!["format:level".into(), "/CROCFILE.1:CROC.*".into()]);
        w.wont_reach.insert("0x8002D820".into(), "under \"water\"".into());
        w.held_out = Some(vec!["walk/free".into()]);
        let text = with_confirmation(GOAL_TOML, &w);
        let goal: Goal = toml::from_str(&text).unwrap();
        assert_eq!(goal.confirmed.unwrap(), w, "{text}");
    }
}
