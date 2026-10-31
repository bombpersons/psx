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
    Run,
    Code,
    Assets,
    Setup,
}

impl Topic {
    const ALL: [Self; 5] = [Self::Overview, Self::Run, Self::Code, Self::Assets, Self::Setup];

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Port a game by recordings: the loop, recordings, status and next, assets, commands",
            Self::Run => "Run the game: input, memory formats, observations, the tick, asking the person",
            Self::Code => "Find and read the game's code: RAM search, disassembly, decompilation, coverage, unpacking",
            Self::Assets => "The disc and its data: standard formats, streams, archives, what the game built",
            Self::Setup => "The project, BIOS, named states, template updates, and results that look wrong",
        }
    }

    fn text(self) -> &'static str {
        match self {
            Self::Overview => include_str!("guide/overview.md"),
            Self::Run => include_str!("guide/run.md"),
            Self::Code => include_str!("guide/code.md"),
            Self::Assets => include_str!("guide/assets.md"),
            Self::Setup => include_str!("guide/setup.md"),
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
        let (topic, title, text) = (self.topic, self.topic.title(), self.topic.text().trim());
        // A topic is its text: the index, the project and the build come
        // with the overview, which a session reads once if at all.
        if !matches!(topic, Topic::Overview) {
            return Outcome::ok(json!({"topic": topic, "title": title, "text": text}));
        }
        Outcome::ok(json!({
            "topic": topic,
            "title": title,
            "text": text,
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
