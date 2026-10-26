//! Built-in workflow guidance, available without a checkout, network or skill loader.
use crate::commands::{Ctx, Op, Outcome};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Copy, Default, clap::ValueEnum, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    #[default]
    Overview,
    Setup,
    Input,
    Run,
    Fields,
    Cases,
    States,
    Barrier,
    Code,
    Assets,
    Bevy,
    Port,
    Troubleshooting,
}

impl Topic {
    const ALL: [Self; 13] = [
        Self::Overview,
        Self::Setup,
        Self::Input,
        Self::Run,
        Self::Fields,
        Self::Cases,
        Self::States,
        Self::Barrier,
        Self::Code,
        Self::Assets,
        Self::Bevy,
        Self::Port,
        Self::Troubleshooting,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Choose an investigation workflow",
            Self::Setup => "Resolve the project, media and BIOS",
            Self::Input => "Explore gameplay from checkpoints",
            Self::Run => "Run, observe and read run results",
            Self::Fields => "Name the game's memory and find its tick",
            Self::Cases => "Record cases and check them again",
            Self::States => "Keep start states that rebuild from power-on",
            Self::Barrier => "Check what crosses to the implementer",
            Self::Code => "Follow runtime evidence into code",
            Self::Assets => "Find, decode and validate assets",
            Self::Bevy => "Rebuild the game in Bevy",
            Self::Port => "Port a whole game as an engine and a game",
            Self::Troubleshooting => "Diagnose incomplete or unexpected results",
        }
    }

    fn text(self) -> &'static str {
        match self {
            Self::Overview => include_str!("guide/overview.md"),
            Self::Setup => include_str!("guide/setup.md"),
            Self::Input => include_str!("guide/input.md"),
            Self::Run => include_str!("guide/run.md"),
            Self::Fields => include_str!("guide/fields.md"),
            Self::Cases => include_str!("guide/cases.md"),
            Self::States => include_str!("guide/states.md"),
            Self::Barrier => include_str!("guide/barrier.md"),
            Self::Code => include_str!("guide/code.md"),
            Self::Assets => include_str!("guide/assets.md"),
            Self::Bevy => include_str!("guide/bevy.md"),
            Self::Port => include_str!("guide/port.md"),
            Self::Troubleshooting => include_str!("guide/troubleshooting.md"),
        }
    }
}

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct GuideArgs {
    /// Workflow to read. Omit for an overview and the available topics.
    #[arg(long, value_enum, default_value = "overview")]
    #[serde(default)]
    pub topic: Topic,
}

impl Op for GuideArgs {
    fn exec(self, ctx: &Ctx) -> Outcome {
        Outcome::ok(json!({
            "topic": self.topic,
            "title": self.topic.title(),
            "text": self.topic.text().trim(),
            "project_root": ctx.root,
            "build": crate::context::build(),
            "topics": Topic::ALL.into_iter().map(|topic| json!({
                "topic":topic, "title":topic.title(),
                "next_call":{"tool":"guide","arguments":{"topic":topic}}
            })).collect::<Vec<_>>(),
            "next_calls": [{"tool":"context","arguments":{}}],
        }))
    }
}
