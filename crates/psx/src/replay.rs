//! Replay files for `psx run`: a JSON array of
//! `{"frame": n, "buttons": ["CROSS", ...], "sticks": [lx, ly, rx, ry]}`.
//! Each event is the complete pad state from that frame on; `sticks` is
//! optional (0-255, 128 = centred) and only read by a DualShock.

use psx_core::Machine;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum Button {
    Select,
    L3,
    R3,
    Start,
    Up,
    Right,
    Down,
    Left,
    L2,
    R2,
    L1,
    R1,
    Triangle,
    Circle,
    Cross,
    Square,
}
impl<'de> Deserialize<'de> for Button {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let bit = psx_core::sio0::button_from_name(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown button {s:?}")))?;
        Ok(BUTTONS[bit.trailing_zeros() as usize])
    }
}
const BUTTONS: [Button; 16] = [
    Button::Select,
    Button::L3,
    Button::R3,
    Button::Start,
    Button::Up,
    Button::Right,
    Button::Down,
    Button::Left,
    Button::L2,
    Button::R2,
    Button::L1,
    Button::R1,
    Button::Triangle,
    Button::Circle,
    Button::Cross,
    Button::Square,
];
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Event {
    /// Run-relative boundary where this complete pad state becomes active.
    pub frame: u64,
    /// Complete held button set; [] releases all buttons. Names are case insensitive.
    pub buttons: Vec<Button>,
    #[serde(default)]
    pub sticks: Option<[u8; 4]>,
    /// Optional exclusive release boundary; emits a neutral state there. Cannot overlap another event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(transparent)]
pub struct Events(pub Vec<Event>);
impl std::str::FromStr for Events {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Input {
    pub frame: u64,
    pub buttons: u16,
    /// Left x/y, right x/y.
    pub sticks: [u8; 4],
}

/// Events allowed inline (`--input`, MCP `input`).
pub const INLINE_EVENT_LIMIT: usize = 10_000;
/// Events allowed in a replay file: a handoff records every pad change of a
/// long play session (an analog stick changes nearly every frame).
pub const FILE_EVENT_LIMIT: usize = 1_000_000;
const FILE_BYTE_LIMIT: u64 = 64 * 1024 * 1024;

pub fn load(path: &std::path::Path) -> Result<Vec<Input>, String> {
    resolve(Some(path), None, u64::MAX)
}

pub fn resolve(path: Option<&std::path::Path>, inline: Option<&Events>, frames: u64) -> Result<Vec<Input>, String> {
    if path.is_some() && inline.is_some() {
        return Err("input and replay are mutually exclusive".into());
    }
    let owned;
    let events = if let Some(path) = path {
        if std::fs::metadata(path).map_err(|e| format!("replay {}: {e}", path.display()))?.len() > FILE_BYTE_LIMIT {
            return Err("replay exceeds 64 MiB".into());
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("replay {}: {e}", path.display()))?;
        owned = serde_json::from_str::<Events>(&text).map_err(|e| format!("replay {}: {e}", path.display()))?;
        &owned.0
    } else if let Some(events) = inline {
        &events.0
    } else {
        return Ok(Vec::new());
    };
    let limit = if path.is_some() { FILE_EVENT_LIMIT } else { INLINE_EVENT_LIMIT };
    if events.len() > limit {
        return Err(format!(
            "at most {limit} input events are allowed{}",
            if path.is_some() { "" } else { " inline (use a replay file)" }
        ));
    }
    let mut out = Vec::with_capacity(events.len());
    let mut previous = None;
    let mut held_until = 0;
    for e in events {
        if previous.is_some_and(|f| e.frame <= f) {
            return Err("input frames must be strictly increasing (no duplicates)".into());
        }
        if e.frame > frames || e.until.is_some_and(|end| end <= e.frame || end > frames) {
            return Err("input frame/until must be within 0..=frames, with until > frame".into());
        }
        if e.frame < held_until {
            return Err("input events cannot overlap an earlier event's until interval".into());
        }
        // An explicit state at a previous release boundary takes precedence over neutral.
        if out.last().is_some_and(|i: &Input| i.frame == e.frame) {
            out.pop();
        }
        let mut bits = 0u16;
        for b in &e.buttons {
            let bit = 1u16
                << BUTTONS
                    .iter()
                    .position(|candidate| std::mem::discriminant(candidate) == std::mem::discriminant(b))
                    .unwrap();
            if bits & bit != 0 {
                return Err("duplicate button in input state".into());
            }
            bits |= bit;
        }
        out.push(Input { frame: e.frame, buttons: bits, sticks: e.sticks.unwrap_or([0x80; 4]) });
        if let Some(end) = e.until {
            out.push(Input { frame: end, buttons: 0, sticks: [0x80; 4] });
            held_until = end;
        }
        previous = Some(e.frame);
    }
    Ok(out)
}

pub fn button_names(bits: u16) -> Vec<String> {
    BUTTONS
        .iter()
        .enumerate()
        .filter(|(i, _)| bits & (1 << i) != 0)
        .map(|(_, b)| serde_json::to_value(b).unwrap().as_str().unwrap().to_owned())
        .collect()
}
pub fn report(events: &[Input]) -> serde_json::Value {
    serde_json::json!({"frame_origin":"run_start","state_semantics":"complete held state from frame until next event; empty buttons releases all; until is exclusive",
        "events":events.iter().map(|e|serde_json::json!({"frame":e.frame,"buttons":button_names(e.buttons),"sticks":e.sticks})).collect::<Vec<_>>()})
}

/// True when any event moves a stick (needs a DualShock).
pub fn uses_sticks(inputs: &[Input]) -> bool {
    inputs.iter().any(|i| i.sticks != [0x80; 4])
}

pub fn apply(m: &mut Machine, i: &Input) {
    m.bus.sio0.buttons = i.buttons;
    let [lx, ly, rx, ry] = i.sticks;
    m.bus.sio0.pad.sticks = [rx, ry, lx, ly];
}
