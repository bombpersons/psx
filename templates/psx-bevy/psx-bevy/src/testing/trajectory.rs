//! Trajectory tests: replay the original's input in the rebuild and compare
//! recorded values tick by tick.
//!
//! Record the original once with the psx toolkit, from a checkpoint, with the
//! replay stored beside the result:
//!
//! ```text
//! psx run game.cue --from-state jump/state.bin --frames 60 \
//!     --replay tests/data/jump.input.json \
//!     --sample 8015E8A8:fx16.16:x --sample 8015E8AC:fx16.16:y --out .psx/jump
//! cp .psx/jump/samples.csv tests/data/jump.samples.csv
//! ```
//!
//! Then a test spawns the recorded starting state, replays the same input,
//! records the same values in the original's units and compares:
//!
//! ```
//! use bevy::prelude::*;
//! use psx_bevy::prelude::{Region, TickRate};
//! use psx_bevy::testing::{Alignment, Replay, Trajectory, compare, headless_app};
//!
//! # #[derive(Component)] struct Pos(f64);
//! # fn walk(pad: Res<psx_bevy::prelude::ControllerState>, mut q: Query<&mut Pos>) {
//! #     for mut p in &mut q { p.0 += pad.direction().x as f64 * 1.5; }
//! # }
//! // In a test: Replay::load("tests/data/walk.input.json") and
//! // Trajectory::load_psx_samples("tests/data/walk.samples.csv", align).
//! let align = Alignment::new(1, 0); // in a game: Alignment::new(game::FIELDS_PER_TICK, 0)
//! let replay = Replay::parse(r#"[{"frame":0,"buttons":["RIGHT"],"until":3}]"#).unwrap();
//! let original = Trajectory::parse_psx_samples(
//!     "run_frame,frame,x\n0,900,10.0\n1,901,11.5\n2,902,13.0\n3,903,14.5\n4,904,14.5\n",
//!     align,
//! )
//! .unwrap();
//!
//! let mut app = headless_app(TickRate::every_field(Region::Ntsc));
//! app.add_systems(FixedUpdate, walk.in_set(psx_bevy::prelude::GameSystems::Game));
//! let player = app.world_mut().spawn(Pos(original.get(0, "x").unwrap())).id();
//! let inputs = replay.tick_inputs(align, original.ticks() - 1).unwrap();
//! let rebuild = Trajectory::record(&mut app, &inputs, &["x"], |world| {
//!     vec![world.get::<Pos>(player).unwrap().0]
//! });
//! compare(&original, &rebuild, &[("x", 1e-9)]).unwrap().assert_passed();
//! ```
//!
//! Timing: sample row N is the original's memory after N video fields of the
//! run, before field N's input. Tick k of the rebuild therefore uses the input
//! held at field `start + k * fields_per_tick`, and the state after k ticks is
//! compared with that row ([`Alignment`]). Record the rebuild in the original's
//! units (fixed point decoded, original axes), so each value compares directly.
use crate::controller::Button;
use crate::shell::InjectedMouse;
use crate::testing::{TickApp, TickInput};
use bevy::prelude::*;
use std::fmt;
use std::path::Path;

/// How the rebuild's ticks line up with the original's video fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alignment {
    /// Fields per logic tick: the game's `FIELDS_PER_TICK`.
    pub fields_per_tick: u32,
    /// The sample row (run field) that is the state before the first tick.
    pub start: u32,
}

impl Alignment {
    pub const fn new(fields_per_tick: u32, start: u32) -> Self {
        assert!(fields_per_tick > 0, "fields_per_tick must be at least 1");
        Self { fields_per_tick, start }
    }

    /// The run field whose sample row is the state after `ticks` ticks, and
    /// whose held input drives tick `ticks`.
    pub fn field(self, ticks: usize) -> u64 {
        self.start as u64 + ticks as u64 * self.fields_per_tick as u64
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Event {
    frame: u64,
    buttons: Vec<Button>,
    mouse: Option<InjectedMouse>,
    until: Option<u64>,
}

/// A psx controller replay (`psx run --replay`/`--input`): each event is the
/// complete held state from its `frame`, until the next event or its exclusive
/// `until`. Frames count video fields from the start of the run.
///
/// The rebuild also reads an optional `"mouse"` per event, in
/// [`InjectedMouse::from_json`]'s form: its buttons and position are held like
/// pad buttons, and its motion and wheel apply on every tick of the event. The
/// original had no mouse, so `psx run` refuses replays that use it; they are
/// for the rebuild's own tests. Read them with [`Replay::ticks`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Replay {
    events: Vec<Event>,
}

impl Replay {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(json: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let items = value.as_array().ok_or("a replay is a JSON array of events")?;
        let mut events: Vec<Event> = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let object = item.as_object().ok_or(format!("event {i} is not an object"))?;
            if let Some(key) =
                object.keys().find(|k| !["frame", "buttons", "until", "sticks", "mouse"].contains(&k.as_str()))
            {
                return Err(format!("event {i}: unknown field {key:?}"));
            }
            let frame = object.get("frame").and_then(|v| v.as_u64()).ok_or(format!("event {i} needs a frame"))?;
            let until = match object.get("until") {
                None => None,
                Some(v) => {
                    Some(v.as_u64().filter(|&u| u > frame).ok_or(format!("event {i}: until must exceed frame"))?)
                }
            };
            if object.get("sticks").is_some_and(|s| *s != serde_json::json!([128, 128, 128, 128])) {
                return Err(format!("event {i}: analog sticks have no template controller equivalent"));
            }
            let mut buttons = Vec::new();
            for name in object.get("buttons").and_then(|v| v.as_array()).into_iter().flatten() {
                let name = name.as_str().ok_or(format!("event {i}: button names are strings"))?;
                let button = button(name).ok_or(format!("event {i}: no template button for {name:?}"))?;
                if !buttons.contains(&button) {
                    buttons.push(button);
                }
            }
            if let Some(previous) = events.last() {
                if frame <= previous.frame {
                    return Err(format!("event {i}: frames must increase ({frame} after {})", previous.frame));
                }
                if previous.until.is_some_and(|u| frame < u) {
                    return Err(format!("event {i}: frame {frame} starts inside the previous event's until"));
                }
            }
            let mouse =
                object.get("mouse").map(InjectedMouse::from_json).transpose().map_err(|e| format!("event {i}: {e}"))?;
            events.push(Event { frame, buttons, mouse, until });
        }
        Ok(Self { events })
    }

    /// The buttons held during `frame`. Before the first event nothing is held:
    /// start replays at frame 0 so the original's checkpoint pad state agrees.
    pub fn held(&self, frame: u64) -> &[Button] {
        self.active(frame).map_or(&[], |e| &e.buttons)
    }

    /// The injected mouse during `frame`; none before the first event.
    pub fn mouse(&self, frame: u64) -> InjectedMouse {
        self.active(frame).and_then(|e| e.mouse).unwrap_or_default()
    }

    /// Whether any event drives the mouse.
    pub fn uses_mouse(&self) -> bool {
        self.events.iter().any(|e| e.mouse.is_some())
    }

    fn active(&self, frame: u64) -> Option<&Event> {
        self.events.iter().rev().find(|e| e.frame <= frame).filter(|e| e.until.is_none_or(|u| frame < u))
    }

    /// The buttons for each of `ticks` ticks. Input may only change on a tick
    /// boundary; an edge inside a tick is an error rather than a guess about
    /// which field the original read the pad on.
    pub fn tick_inputs(&self, align: Alignment, ticks: usize) -> Result<Vec<Vec<Button>>, String> {
        if self.uses_mouse() {
            return Err("this replay drives the mouse; read it with Replay::ticks".into());
        }
        Ok(self.ticks(align, ticks)?.into_iter().map(|input| input.buttons).collect())
    }

    /// [`Replay::tick_inputs`] with the mouse: one [`TickInput`] per tick.
    pub fn ticks(&self, align: Alignment, ticks: usize) -> Result<Vec<TickInput>, String> {
        let (first, last) = (align.field(0), align.field(ticks));
        let edges = self.events.iter().flat_map(|e| std::iter::once(e.frame).chain(e.until));
        for edge in edges.filter(|&f| f > first && f < last) {
            if (edge - first) % align.fields_per_tick as u64 != 0 {
                let tick = (edge - first) / align.fields_per_tick as u64;
                return Err(format!(
                    "input changes at field {edge}, inside tick {tick} (fields {}..{}); with {} fields per tick, change input only at fields {first} + n*{}",
                    align.field(tick as usize),
                    align.field(tick as usize + 1),
                    align.fields_per_tick,
                    align.fields_per_tick,
                ));
            }
        }
        Ok((0..ticks)
            .map(|k| TickInput { buttons: self.held(align.field(k)).to_vec(), mouse: self.mouse(align.field(k)) })
            .collect())
    }
}

fn button(name: &str) -> Option<Button> {
    // Variant names are the psx names (`CROSS`, `L1`) in another case.
    Button::ALL.into_iter().find(|b| format!("{b:?}").eq_ignore_ascii_case(name))
}

/// Named values per tick: row k is the state after k ticks.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Trajectory {
    columns: Vec<String>,
    rows: Vec<Vec<f64>>,
}

impl Trajectory {
    pub fn new(columns: &[&str]) -> Self {
        Self { columns: columns.iter().map(|c| c.to_string()).collect(), rows: Vec::new() }
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Recorded rows, including the starting state.
    pub fn ticks(&self) -> usize {
        self.rows.len()
    }

    pub fn get(&self, tick: usize, column: &str) -> Option<f64> {
        let index = self.columns.iter().position(|c| c == column)?;
        self.rows.get(tick).map(|row| row[index])
    }

    pub fn push(&mut self, row: Vec<f64>) {
        assert_eq!(row.len(), self.columns.len(), "a row needs one value per column");
        self.rows.push(row);
    }

    /// Records row 0 before any tick, then one row after each input's tick.
    /// `inputs` are per-tick buttons ([`Replay::tick_inputs`]) or
    /// [`TickInput`]s with the mouse ([`Replay::ticks`]).
    pub fn record<I: Clone + Into<TickInput>>(
        app: &mut App,
        inputs: &[I],
        columns: &[&str],
        mut read: impl FnMut(&World) -> Vec<f64>,
    ) -> Self {
        let mut trajectory = Self::new(columns);
        trajectory.push(read(app.world()));
        for input in inputs {
            app.run_input(&input.clone().into());
            trajectory.push(read(app.world()));
        }
        trajectory
    }

    /// `psx run` `samples.csv`: keeps the rows at the aligned fields, from
    /// `align.start` until the first missing one.
    pub fn load_psx_samples(path: impl AsRef<Path>, align: Alignment) -> Result<Self, String> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse_psx_samples(&text, align).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse_psx_samples(text: &str, align: Alignment) -> Result<Self, String> {
        let mut lines = text.lines();
        let header: Vec<&str> = lines.next().ok_or("empty samples file")?.split(',').collect();
        if header.len() < 3 || header[0] != "run_frame" || header[1] != "frame" {
            return Err("expected a psx samples.csv header: run_frame,frame,<names>".into());
        }
        let mut trajectory = Self::new(&header[2..]);
        for (number, line) in lines.enumerate() {
            let cells: Vec<&str> = line.split(',').collect();
            let field: u64 = cells[0].parse().map_err(|_| format!("line {}: bad run_frame", number + 2))?;
            if field != align.field(trajectory.ticks()) {
                if field > align.field(trajectory.ticks()) {
                    break;
                }
                continue;
            }
            if cells.len() != header.len() {
                return Err(format!("line {}: {} cells for {} columns", number + 2, cells.len(), header.len()));
            }
            let row = cells[2..].iter().map(|c| number_cell(c)).collect::<Option<Vec<_>>>();
            trajectory.push(row.ok_or(format!("line {}: non-numeric value", number + 2))?);
        }
        if trajectory.rows.is_empty() {
            return Err(format!("no sample row for field {} (alignment start)", align.start));
        }
        Ok(trajectory)
    }

    /// `tick,<columns>` CSV, as [`Trajectory::to_csv`] writes it.
    pub fn parse_csv(text: &str) -> Result<Self, String> {
        let mut lines = text.lines().filter(|l| !l.starts_with('#'));
        let header: Vec<&str> = lines.next().ok_or("empty trajectory")?.split(',').collect();
        if header.first() != Some(&"tick") {
            return Err("expected a tick,<names> header".into());
        }
        let mut trajectory = Self::new(&header[1..]);
        for (tick, line) in lines.enumerate() {
            let cells: Vec<&str> = line.split(',').collect();
            let row = (cells.len() == header.len() && cells[0].parse() == Ok(tick))
                .then(|| cells[1..].iter().map(|c| number_cell(c)).collect::<Option<Vec<_>>>())
                .flatten();
            trajectory.push(row.ok_or(format!("row {tick}: expected tick {tick} and {} numbers", header.len() - 1))?);
        }
        Ok(trajectory)
    }

    pub fn to_csv(&self) -> String {
        let mut out = format!("tick,{}\n", self.columns.join(","));
        for (tick, row) in self.rows.iter().enumerate() {
            let cells: Vec<String> = row.iter().map(f64::to_string).collect();
            out += &format!("{tick},{}\n", cells.join(","));
        }
        out
    }
}

fn number_cell(cell: &str) -> Option<f64> {
    match cell.strip_prefix("0x").or_else(|| cell.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok().map(|v| v as f64),
        None => cell.parse().ok(),
    }
}

/// One compared column.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnComparison {
    pub name: String,
    pub tolerance: f64,
    pub max_error: f64,
    pub max_error_tick: usize,
    /// The first tick whose error exceeds the tolerance.
    pub first_divergence: Option<usize>,
}

/// The result of [`compare`]. Its `Display` is a report for test output.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    pub original_ticks: usize,
    pub rebuild_ticks: usize,
    pub columns: Vec<ColumnComparison>,
    /// If the rebuild fails as aligned but matches when shifted: the rebuild
    /// reaches each state this many ticks earlier (positive) or later (negative).
    pub shift: Option<i64>,
    original: Trajectory,
    rebuild: Trajectory,
}

fn error(a: f64, b: f64) -> f64 {
    if a.is_nan() && b.is_nan() {
        0.0
    } else if a.is_nan() || b.is_nan() {
        f64::INFINITY
    } else {
        (a - b).abs()
    }
}

/// Compares the listed columns tick by tick, each with its absolute tolerance
/// (0.0 for exact). Both trajectories must have the same number of ticks to pass.
pub fn compare(original: &Trajectory, rebuild: &Trajectory, tolerances: &[(&str, f64)]) -> Result<Comparison, String> {
    if tolerances.is_empty() {
        return Err("list at least one column to compare".into());
    }
    let index = |t: &Trajectory, name: &str, side: &str| {
        t.columns.iter().position(|c| c == name).ok_or(format!("{side} has no column {name:?} (has {:?})", t.columns))
    };
    let pairs = tolerances
        .iter()
        .map(|&(name, tolerance)| {
            Ok((name, tolerance, index(original, name, "original")?, index(rebuild, name, "rebuild")?))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let errors_at = |shift: i64| {
        let ticks = (0..rebuild.rows.len()).filter_map(move |k| {
            let o = usize::try_from(k as i64 + shift).ok()?;
            Some((k, original.rows.get(o)?, &rebuild.rows[k]))
        });
        ticks.map(|(k, o, r)| (k, pairs.iter().map(|&(_, _, oi, ri)| error(o[oi], r[ri])).collect::<Vec<_>>()))
    };
    let mut columns: Vec<ColumnComparison> = pairs
        .iter()
        .map(|&(name, tolerance, _, _)| ColumnComparison {
            name: name.into(),
            tolerance,
            max_error: 0.0,
            max_error_tick: 0,
            first_divergence: None,
        })
        .collect();
    for (tick, errors) in errors_at(0) {
        for (column, err) in columns.iter_mut().zip(errors) {
            if err > column.max_error {
                (column.max_error, column.max_error_tick) = (err, tick);
            }
            if err > column.tolerance && column.first_divergence.is_none() {
                column.first_divergence = Some(tick);
            }
        }
    }
    let diverged = columns.iter().any(|c| c.first_divergence.is_some());
    let fits = |shift: i64| {
        let mut compared = 0;
        let ok = errors_at(shift).all(|(_, errors)| {
            compared += 1;
            errors.iter().zip(&pairs).all(|(e, p)| *e <= p.1)
        });
        // A shift must still cover most of the run to mean anything.
        ok && compared * 2 > original.rows.len().max(1)
    };
    let shift = diverged.then(|| [1, -1, 2, -2].into_iter().find(|&s| fits(s))).flatten();
    Ok(Comparison {
        original_ticks: original.rows.len(),
        rebuild_ticks: rebuild.rows.len(),
        columns,
        shift,
        original: original.clone(),
        rebuild: rebuild.clone(),
    })
}

impl Comparison {
    pub fn passed(&self) -> bool {
        self.original_ticks == self.rebuild_ticks && self.columns.iter().all(|c| c.first_divergence.is_none())
    }

    /// Panics with the full report unless every column stays within tolerance.
    #[track_caller]
    pub fn assert_passed(&self) {
        assert!(self.passed(), "trajectory mismatch\n{self}");
    }

    /// The first tick where any column diverges.
    pub fn first_divergence(&self) -> Option<usize> {
        self.columns.iter().filter_map(|c| c.first_divergence).min()
    }
}

impl fmt::Display for Comparison {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verdict = if self.passed() { "match" } else { "MISMATCH" };
        writeln!(f, "{verdict}: original {} ticks, rebuild {} ticks", self.original_ticks, self.rebuild_ticks)?;
        for c in &self.columns {
            let first = c.first_divergence.map_or("-".to_string(), |t| t.to_string());
            writeln!(
                f,
                "  {:<16} tolerance {:<10} max error {:<12} at tick {:<5} first divergence {first}",
                c.name, c.tolerance, c.max_error, c.max_error_tick
            )?;
        }
        if let Some(tick) = self.first_divergence() {
            writeln!(f, "around tick {tick} (original | rebuild):")?;
            for k in tick.saturating_sub(3)..=tick + 3 {
                let mut line = format!("  {k:>5}");
                for c in &self.columns {
                    let (o, r) = (self.original.get(k, &c.name), self.rebuild.get(k, &c.name));
                    let outside = o.zip(r).is_some_and(|(o, r)| error(o, r) > c.tolerance);
                    let text = |v: Option<f64>| v.map_or("-".into(), |v| v.to_string());
                    line += &format!("  {}={} | {}{}", c.name, text(o), text(r), if outside { "*" } else { " " });
                }
                writeln!(f, "{line}")?;
            }
        }
        if let Some(shift) = self.shift {
            let (n, way) = (shift.unsigned_abs(), if shift > 0 { "earlier" } else { "later" });
            writeln!(
                f,
                "hint: the rebuild matches when shifted: it reaches each state {n} tick(s) {way} than the original. Check input latency (when the original reads the pad) and Alignment::start."
            )?;
        }
        if self.original_ticks != self.rebuild_ticks {
            writeln!(f, "hint: record as many ticks as the original: tick_inputs(align, original.ticks() - 1).")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(values: &[f64]) -> Trajectory {
        let mut t = Trajectory::new(&["x"]);
        values.iter().for_each(|&v| t.push(vec![v]));
        t
    }

    #[test]
    fn replays_use_psx_names_until_releases_and_reject_unrepresentable_input() {
        let replay =
            Replay::parse(r#"[{"frame":0,"buttons":["cross","RIGHT"],"until":2},{"frame":4,"buttons":["Triangle"]}]"#)
                .unwrap();
        assert_eq!(replay.held(1), &[Button::Cross, Button::Right]);
        assert!(replay.held(2).is_empty() && replay.held(3).is_empty());
        assert_eq!(replay.held(9), &[Button::Triangle]);
        for bad in [
            r#"[{"frame":0,"buttons":["L3"]}]"#,
            r#"[{"frame":0,"buttons":[],"sticks":[0,128,128,128]}]"#,
            r#"[{"frame":2,"buttons":[]},{"frame":2,"buttons":[]}]"#,
            r#"[{"frame":0,"buttons":[],"until":5},{"frame":3,"buttons":[]}]"#,
            r#"[{"frame":0,"buttons":[],"hold":5}]"#,
            r#"[{"frame":3,"buttons":[],"until":3}]"#,
        ] {
            assert!(Replay::parse(bad).is_err(), "{bad}");
        }
        assert!(Replay::parse(r#"[{"frame":0,"buttons":[],"sticks":[128,128,128,128]}]"#).is_ok());
    }

    #[test]
    fn tick_inputs_follow_the_alignment_and_refuse_edges_inside_a_tick() {
        let replay = Replay::parse(r#"[{"frame":0,"buttons":["UP"]},{"frame":5,"buttons":[]}]"#).unwrap();
        let inputs = replay.tick_inputs(Alignment::new(2, 1), 3).unwrap();
        assert_eq!(inputs, vec![vec![Button::Up], vec![Button::Up], vec![]]);
        let error = replay.tick_inputs(Alignment::new(2, 0), 4).unwrap_err();
        assert!(error.contains("field 5, inside tick 2 (fields 4..6)"), "{error}");
        // Edges outside the replayed ticks do not matter.
        assert_eq!(replay.tick_inputs(Alignment::new(2, 0), 2).unwrap().len(), 2);
    }

    /// Mouse events: buttons and position held, motion and wheel per tick,
    /// a bound mouse button pressing its action, and nothing left behind.
    #[test]
    fn replayed_mouse_reaches_controller_state_each_tick() {
        use crate::controller::{ConflictPolicy, ControllerBindings, ControllerState, PhysicalBinding};
        use crate::prelude::{Region, TickRate};
        let replay = Replay::parse(
            r#"[{"frame":0,"buttons":[],"mouse":{"buttons":["left"],"position":[10,20],"motion":[3,-1],"wheel":1},"until":2},
                {"frame":2,"buttons":["CROSS"]}]"#,
        )
        .unwrap();
        assert!(replay.uses_mouse());
        assert!(replay.tick_inputs(Alignment::new(1, 0), 3).unwrap_err().contains("Replay::ticks"));
        let inputs = replay.ticks(Alignment::new(1, 0), 3).unwrap();
        let mut app = crate::testing::headless_app(TickRate::every_field(Region::Ntsc));
        let mut bindings = ControllerBindings::default();
        bindings.assign(Button::Circle, PhysicalBinding::Mouse(MouseButton::Left), ConflictPolicy::Reject).unwrap();
        app.insert_resource(bindings);
        let read = |w: &World| {
            let pad = w.resource::<ControllerState>();
            let m = pad.mouse;
            vec![
                f64::from(u8::from(m.held(MouseButton::Left))),
                m.position.map_or(-1.0, |p| f64::from(p.x)),
                f64::from(m.motion.x),
                f64::from(m.wheel),
                f64::from(u8::from(pad.held(Button::Circle))),
            ]
        };
        let t = Trajectory::record(&mut app, &inputs, &["left", "x", "dx", "wheel", "circle"], read);
        // Row 0 is before any tick; rows 1 and 2 are the mouse event's ticks.
        for tick in [1, 2] {
            assert_eq!(
                ["left", "x", "dx", "wheel", "circle"].map(|c| t.get(tick, c).unwrap()),
                [1.0, 10.0, 3.0, 1.0, 1.0],
                "tick {tick}"
            );
        }
        assert_eq!(["left", "x", "dx", "circle"].map(|c| t.get(3, c).unwrap()), [0.0, -1.0, 0.0, 0.0]);
        for bad in [
            r#"[{"frame":0,"buttons":[],"mouse":{"buttons":["thumb"]}}]"#,
            r#"[{"frame":0,"buttons":[],"mouse":{"position":[1]}}]"#,
            r#"[{"frame":0,"buttons":[],"mouse":{"speed":2}}]"#,
        ] {
            assert!(Replay::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn psx_samples_are_decimated_from_the_start_row() {
        let csv = "run_frame,frame,x,flags\n0,10,1.5,0x01\n1,11,2.0,0x02\n2,12,2.5,0x0A\n3,13,3,0x10\n4,14,-1,0x00\n";
        let t = Trajectory::parse_psx_samples(csv, Alignment::new(2, 1)).unwrap();
        assert_eq!(t.columns(), ["x", "flags"]);
        assert_eq!(t.rows, vec![vec![2.0, 2.0], vec![3.0, 16.0]]);
        assert!(Trajectory::parse_psx_samples(csv, Alignment::new(1, 9)).is_err());
        assert!(Trajectory::parse_psx_samples("tick,x\n0,1\n", Alignment::new(1, 0)).is_err());
        let round = Trajectory::parse_csv(&t.to_csv()).unwrap();
        assert_eq!(round, t);
    }

    #[test]
    fn comparisons_report_first_divergence_and_detect_shifts() {
        let original = line(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
        let same = compare(&original, &line(&[0.0, 1.0, 2.0, 3.05, 4.0, 5.0, 6.0, 7.0]), &[("x", 0.1)]).unwrap();
        assert!(same.passed(), "{same}");
        assert_eq!(same.columns[0].max_error_tick, 3);

        let off = compare(&original, &line(&[0.0, 1.0, 2.0, 3.5, 4.0, 5.0, 6.0, 7.0]), &[("x", 0.1)]).unwrap();
        assert!(!off.passed());
        assert_eq!(off.first_divergence(), Some(3));
        assert_eq!(off.shift, None);
        assert!(off.to_string().contains("x=3 | 3.5*") && off.to_string().contains("x=4 | 4 "), "{off}");

        // The rebuild is one tick behind the original throughout.
        let late = compare(&original, &line(&[0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), &[("x", 0.0)]).unwrap();
        assert_eq!(late.shift, Some(-1), "{late}");
        assert!(late.to_string().contains("1 tick(s) later"));

        let short = compare(&original, &line(&[0.0, 1.0]), &[("x", 0.0)]).unwrap();
        assert!(!short.passed() && short.to_string().contains("as many ticks"));
        assert!(compare(&original, &original, &[("y", 0.0)]).is_err());
    }
}
