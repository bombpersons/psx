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
    Debug,
    Code,
    Assets,
    Setup,
    Arithmetic,
}

impl Topic {
    const ALL: [Self; 7] =
        [Self::Overview, Self::Run, Self::Debug, Self::Code, Self::Assets, Self::Setup, Self::Arithmetic];

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Port a game by recordings: the loop, recordings, status and next, assets, commands",
            Self::Run => "Run the game: input, memory formats, observations, the tick, asking the person",
            Self::Debug => "The debugger: stop at a PC, a write or the tick, look, change, go on or back",
            Self::Code => "Find and read the game's code: RAM search, disassembly, decompilation, coverage, unpacking",
            Self::Assets => "The disc and its data: standard formats, streams, archives, what the game built",
            Self::Setup => {
                "The project, BIOS, named states, the port's commands and protocol, the template, and results that look wrong"
            }
            Self::Arithmetic => "The original's integer arithmetic and the GTE, exactly, and test vectors for them",
        }
    }

    fn text(self) -> &'static str {
        match self {
            Self::Overview => include_str!("guide/overview.md"),
            Self::Run => include_str!("guide/run.md"),
            Self::Debug => include_str!("guide/debug.md"),
            Self::Code => include_str!("guide/code.md"),
            Self::Assets => include_str!("guide/assets.md"),
            Self::Setup => include_str!("guide/setup.md"),
            Self::Arithmetic => include_str!("guide/arithmetic.md"),
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
