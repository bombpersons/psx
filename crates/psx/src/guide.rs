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
    Code,
    Assets,
    Bevy,
    Troubleshooting,
}

impl Topic {
    const ALL: [Self; 7] =
        [Self::Overview, Self::Setup, Self::Input, Self::Code, Self::Assets, Self::Bevy, Self::Troubleshooting];

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Choose an investigation workflow",
            Self::Setup => "Resolve the project, media and BIOS",
            Self::Input => "Explore gameplay from checkpoints",
            Self::Code => "Follow runtime evidence into code",
            Self::Assets => "Find, decode and validate assets",
            Self::Bevy => "Rebuild the game in Bevy",
            Self::Troubleshooting => "Diagnose incomplete or unexpected results",
        }
    }

    fn text(self) -> &'static str {
        match self {
            Self::Overview => include_str!("guide/overview.md"),
            Self::Setup => include_str!("guide/setup.md"),
            Self::Input => include_str!("guide/input.md"),
            Self::Code => include_str!("guide/code.md"),
            Self::Assets => include_str!("guide/assets.md"),
            Self::Bevy => include_str!("guide/bevy.md"),
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
        Outcome::media(Ok(json!({
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
        })))
    }
}
