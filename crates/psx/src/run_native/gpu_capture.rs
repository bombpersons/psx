//! Frame-boundary capture. Only one candidate's VRAM and two frames of GTE
//! projections are retained while searching; the selected snapshot is immutable.

use psx_core::Machine;
use psx_gpu::{vram::Vram, Display, DrawState};
use serde_json::{json, Value};

use crate::run::TraceArgs;

#[derive(Clone)]
pub(super) struct Projection {
    pub sx: i16,
    pub sy: i16,
    pub view: [i32; 3],
}

struct Candidate {
    run_frame: u64,
    absolute_frame: u64,
    state: DrawState,
    before: Vram,
    prior_projections: Vec<Projection>,
}

pub(super) struct CapturedFrame {
    pub state: DrawState,
    pub before: Vram,
    pub after: Vram,
    pub area: (i32, i32, i32, i32),
    pub commands: Vec<Vec<u32>>,
    pub projections: Vec<Projection>,
    pub display: Display,
    pub ctrl: [u32; 3], // OFX, OFY, H at the end of the captured frame
    pub metadata: Value,
}

pub(super) struct GpuCapture {
    from: u64,
    limit: u32,
    next_draw: bool,
    inspected: u32,
    active: Option<Candidate>,
    previous: Vec<Projection>,
    pub selected: Option<CapturedFrame>,
}

fn projections(m: &mut Machine) -> Vec<Projection> {
    m.cpu.gte.projections.take().unwrap_or_default().into_iter().map(|p| Projection { sx: p.sx, sy: p.sy, view: p.view }).collect()
}

fn draws(command: &[u32]) -> bool {
    command.first().is_some_and(|w| matches!(w >> 24, 0x02 | 0x20..=0x7f))
}

impl GpuCapture {
    pub fn new(args: &TraceArgs) -> Option<Self> {
        (args.gpu_frame.is_some() || args.gpu_next_draw).then(|| Self {
            from: args.gpu_frame.unwrap_or(0),
            limit: if args.gpu_next_draw { args.gpu_search_frames.unwrap_or(60) } else { 1 },
            next_draw: args.gpu_next_draw,
            inspected: 0,
            active: None,
            previous: Vec::new(),
            selected: None,
        })
    }

    fn metadata(&self) -> Value {
        json!({
            "mode": if self.next_draw { "next_draw" } else { "exact" },
            "search": {"from_run_frame": self.from, "max_frames": self.limit, "frames_inspected": self.inspected},
        })
    }

    /// Called once at every observed boundary, including the final run boundary.
    /// A timeout's incomplete frame is eligible only for legacy exact capture.
    pub fn boundary(&mut self, m: &mut Machine, start: u64, finish: bool) {
        if self.active.as_ref().is_some_and(|c| m.bus.frames > c.absolute_frame || finish) {
            let c = self.active.take().unwrap();
            let complete = m.bus.frames > c.absolute_frame;
            let commands = m.bus.gpu.log.take().unwrap_or_default();
            let current = projections(m);
            self.inspected += u32::from(complete);
            let draw_commands = commands.iter().filter(|c| draws(c)).count();
            if !self.next_draw || (complete && draw_commands > 0) {
                let mut all_projections = c.prior_projections;
                all_projections.extend(current);
                let mut metadata = self.metadata();
                metadata["status"] = json!("captured");
                // Preserve the historical relative `frame` field for textures.
                metadata["frame"] = json!(c.run_frame);
                metadata["run_frame"] = json!(c.run_frame);
                metadata["absolute_frame"] = json!(c.absolute_frame);
                metadata["complete"] = json!(complete);
                metadata["draw_commands"] = json!(draw_commands);
                self.selected = Some(CapturedFrame {
                    state: c.state,
                    before: c.before,
                    after: m.bus.gpu.vram.clone(),
                    area: m.bus.gpu.state.area,
                    commands,
                    projections: all_projections,
                    display: m.bus.gpu.display,
                    ctrl: [m.cpu.gte.read_ctrl(24), m.cpu.gte.read_ctrl(25), m.cpu.gte.read_ctrl(26)],
                    metadata,
                });
            } else {
                self.previous = current;
            }
        }
        let rel = m.bus.frames - start;
        let in_window = rel >= self.from && rel - self.from < self.limit as u64;
        if finish || self.selected.is_some() || self.inspected >= self.limit || (rel >= self.from && !in_window) {
            m.bus.gpu.log = None;
            m.cpu.gte.projections = None;
            self.previous.clear();
            return;
        }
        if in_window {
            // The first candidate takes the preceding frame's log from the GTE;
            // subsequent candidates use only the immediately preceding frame.
            if self.inspected == 0 {
                self.previous = projections(m);
            }
            self.active = Some(Candidate {
                run_frame: rel,
                absolute_frame: m.bus.frames,
                state: m.bus.gpu.state,
                before: m.bus.gpu.vram.clone(),
                prior_projections: std::mem::take(&mut self.previous),
            });
            m.bus.gpu.log = Some(Vec::new());
            m.cpu.gte.projections = Some(Vec::new());
        } else if rel.saturating_add(1) == self.from {
            m.cpu.gte.projections = Some(Vec::new());
        }
    }

    pub fn missing(&self, termination: &str) -> Value {
        let reason = if self.inspected >= self.limit {
            "search_limit"
        } else if termination == "timeout" {
            "timeout"
        } else {
            "run_end"
        };
        let mut v = self.metadata();
        v["status"] = json!("not_found");
        v["reason"] = json!(reason);
        v["error"] = json!(if self.next_draw {
            "no completed frame with draw commands in the searched window"
        } else {
            "the run ended before the requested frame"
        });
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(from: u64, limit: u32) -> GpuCapture {
        GpuCapture::new(&TraceArgs { gpu_frame: Some(from), gpu_next_draw: true, gpu_search_frames: Some(limit), ..Default::default() })
            .unwrap()
    }

    fn advance(c: &mut GpuCapture, m: &mut Machine, start: u64, finish: bool) {
        m.bus.frames += 1;
        c.boundary(m, start, finish);
    }

    #[test]
    fn skips_state_upload_and_copy_then_freezes_the_last_candidate() {
        let mut m = Machine::new(vec![]);
        m.bus.frames = 100;
        let mut c = search(0, 3);
        c.boundary(&mut m, 100, false);
        m.bus.gpu.gp0_all(&[0xe3000000, 0xa0000000, 0, 0x00010002, 0x12341234]);
        advance(&mut c, &mut m, 100, false);
        m.bus.gpu.gp0_all(&[0x80000000, 0, 0x00100010, 0x00010002]);
        advance(&mut c, &mut m, 100, false);
        m.bus.gpu.gp0_all(&[0x020000ff, 0, 0x00010010]);
        m.bus.gpu.display.start = (10, 20);
        m.cpu.gte.write_ctrl(26, 320);
        advance(&mut c, &mut m, 100, true);
        let frame = c.selected.as_ref().unwrap();
        assert_eq!(frame.metadata["run_frame"], 2);
        assert_eq!(frame.metadata["absolute_frame"], 102);
        assert_eq!(frame.metadata["search"]["frames_inspected"], 3);
        assert_eq!(frame.metadata["complete"], true);
        assert_eq!(frame.commands.len(), 1);
        assert_eq!(frame.before.to_bytes()[..2], 0x1234u16.to_le_bytes());
        let captured = frame.after.to_bytes();
        m.bus.gpu.gp0_all(&[0x0200ff00, 0, 0x00010010]);
        m.bus.gpu.display.start = (30, 40);
        m.cpu.gte.write_ctrl(26, 640);
        advance(&mut c, &mut m, 100, true);
        let frame = c.selected.as_ref().unwrap();
        assert_eq!(frame.after.to_bytes(), captured);
        assert_ne!(frame.after.to_bytes(), m.bus.gpu.vram.to_bytes());
        assert_eq!(frame.display.start, (10, 20));
        assert_eq!(frame.ctrl[2], 320);
        assert!(m.bus.gpu.log.is_none());
        assert!(m.cpu.gte.projections.is_none());
    }

    #[test]
    fn bounds_are_inclusive_at_start_and_exclusive_at_end() {
        let mut m = Machine::new(vec![]);
        let mut c = search(1, 1);
        c.boundary(&mut m, 0, false);
        m.bus.gpu.gp0_all(&[0x020000ff, 0, 0x00010010]); // before window
        advance(&mut c, &mut m, 0, false);
        advance(&mut c, &mut m, 0, false); // empty candidate exhausts limit
        m.bus.gpu.gp0_all(&[0x020000ff, 0, 0x00010010]); // after window
        advance(&mut c, &mut m, 0, true);
        assert!(c.selected.is_none());
        assert_eq!(c.missing("frames_reached")["reason"], "search_limit");
        assert_eq!(c.inspected, 1);
        assert!(m.bus.gpu.log.is_none());
    }

    #[test]
    fn exact_empty_and_partial_captures_remain_available() {
        for complete in [false, true] {
            let mut m = Machine::new(vec![]);
            let mut c = GpuCapture::new(&TraceArgs { gpu_frame: Some(0), ..Default::default() }).unwrap();
            c.boundary(&mut m, 0, false);
            m.bus.frames = u64::from(complete);
            c.boundary(&mut m, 0, true);
            let frame = c.selected.as_ref().unwrap();
            assert!(frame.commands.is_empty());
            assert_eq!(frame.metadata["complete"], complete);
            assert_eq!(frame.metadata["mode"], "exact");
        }
    }

    #[test]
    fn incomplete_draw_is_not_selected_and_misses_explain_the_bound() {
        let mut m = Machine::new(vec![]);
        let mut c = search(0, 2);
        c.boundary(&mut m, 0, false);
        m.bus.gpu.gp0_all(&[0x020000ff, 0, 0x00010010]);
        c.boundary(&mut m, 0, true);
        assert!(c.selected.is_none());
        assert_eq!(c.missing("timeout")["reason"], "timeout");
        assert_eq!(c.inspected, 0);
        let mut c = search(0, 2);
        c.boundary(&mut m, 0, false);
        advance(&mut c, &mut m, 0, true);
        assert_eq!(c.missing("frames_reached")["reason"], "run_end");
        assert_eq!(c.inspected, 1);
    }

    #[test]
    fn draw_detection_uses_commands_even_when_pixels_do_not_change() {
        let mut m = Machine::new(vec![]);
        let mut c = search(0, 1);
        c.boundary(&mut m, 0, false);
        let before = m.bus.gpu.vram.to_bytes();
        m.bus.gpu.gp0_all(&[0x02000000, 0, 0x00010010]); // black on black
        advance(&mut c, &mut m, 0, true);
        assert_eq!(c.selected.as_ref().unwrap().after.to_bytes(), before);
        assert_eq!(c.selected.as_ref().unwrap().metadata["draw_commands"], 1);
        for opcode in [0x20, 0x3f, 0x40, 0x5f, 0x60, 0x7f] {
            assert!(draws(&[opcode << 24]));
        }
        for opcode in [0, 1, 0x80, 0xa0, 0xc0, 0xe1, 0xe6] {
            assert!(!draws(&[opcode << 24]));
        }
    }

    #[test]
    fn projections_follow_the_selected_and_immediately_preceding_frames() {
        let mut m = Machine::new(vec![]);
        let mut c = search(1, 3);
        for frame in 0..=2 {
            c.boundary(&mut m, 0, false);
            m.cpu.gte.write_ctrl(5, 10 * (frame + 1)); // TRX labels the projection's frame
            m.cpu.gte.execute(0x00080001); // RTPS, sf=1
            if frame == 2 {
                m.bus.gpu.gp0_all(&[0x020000ff, 0, 0x00010010]);
            }
            m.bus.frames += 1;
        }
        c.boundary(&mut m, 0, false);
        let selected = c.selected.as_ref().unwrap();
        assert_eq!(selected.metadata["run_frame"], 2);
        assert_eq!(selected.projections.iter().map(|p| p.view[0]).collect::<Vec<_>>(), [20, 30]);
        assert!(m.cpu.gte.projections.is_none());
        m.cpu.gte.execute(0x00080001);
        advance(&mut c, &mut m, 0, true);
        assert_eq!(c.selected.as_ref().unwrap().projections.len(), 2);
    }
}
