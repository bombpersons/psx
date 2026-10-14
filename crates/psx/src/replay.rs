//! Replay files for the native backend: a JSON array of
//! `{"frame": n, "buttons": ["CROSS", ...], "sticks": [lx, ly, rx, ry]}`.
//! Each event is the complete pad state from that frame on; `sticks` is
//! optional (0-255, 128 = centred) and only read by a DualShock.

use psx_core::Machine;

#[derive(serde::Deserialize)]
struct Event {
    frame: u64,
    buttons: Vec<String>,
    #[serde(default)]
    sticks: Option<[u8; 4]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Input {
    pub frame: u64,
    pub buttons: u16,
    /// Left x/y, right x/y.
    pub sticks: [u8; 4],
}

pub fn load(path: &std::path::Path) -> Result<Vec<Input>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("replay {}: {e}", path.display()))?;
    let events: Vec<Event> = serde_json::from_str(&text).map_err(|e| format!("replay {}: {e}", path.display()))?;
    let mut out = Vec::with_capacity(events.len());
    for e in events {
        let mut bits = 0u16;
        for b in &e.buttons {
            bits |= psx_core::sio0::button_from_name(b).ok_or_else(|| format!("unknown button {b}"))?;
        }
        out.push(Input { frame: e.frame, buttons: bits, sticks: e.sticks.unwrap_or([0x80; 4]) });
    }
    out.sort_by_key(|e| e.frame);
    Ok(out)
}

/// True when any event moves a stick (needs a DualShock, and the native backend).
pub fn uses_sticks(inputs: &[Input]) -> bool {
    inputs.iter().any(|i| i.sticks != [0x80; 4])
}

pub fn apply(m: &mut Machine, i: &Input) {
    m.bus.sio0.buttons = i.buttons;
    let [lx, ly, rx, ry] = i.sticks;
    m.bus.sio0.pad.sticks = [rx, ry, lx, ly];
}
