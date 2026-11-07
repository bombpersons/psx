//! run's observers and its input driver, each with its own files and report
//! section (crate::session).

use std::path::PathBuf;
use std::rc::Rc;

use psx_core::Machine;
use psx_core::cpu::StepInfo;
use psx_core::trace::Tracer;
use psx_media::memmap::{hex, region};
use serde_json::{Value, json};

use super::{GpuCapture, sha256};
use crate::Error;
use crate::commands::addr_value;
use crate::run::write_png;
use crate::session::{Driver, Flow, Observer, Out, Pad};

type Scopes = Rc<Vec<crate::ScopedSymbols>>;

/// The report sections an observer gives, null when it was not attached.
pub(super) const SECTIONS: [&str; 10] =
    ["audio", "pad_reads", "profile", "read_watch", "samples", "screens", "spu", "tick_finder", "trace", "until"];

pub(super) fn jsonl(items: &[Value]) -> String {
    items.iter().map(|v| v.to_string() + "\n").collect()
}

/// Every log carries `frame` (absolute, from power-on, as the core counts) and
/// `run_frame` (from the run start, as --frames and --replay count).
pub(super) fn with_run_frame(mut v: Value, start: u64) -> Value {
    if let Some(f) = v["frame"].as_u64() {
        v["run_frame"] = json!(f as i64 - start as i64);
    }
    v
}

/// Replay events: pad state from each run frame, each application logged.
pub(super) struct Events {
    events: Vec<crate::replay::Input>,
    next: usize,
    applied: Vec<Value>,
}

impl Events {
    pub fn new(events: Vec<crate::replay::Input>) -> Events {
        Events { events, next: 0, applied: Vec::new() }
    }
}

impl Driver for Events {
    fn field(&mut self, rel: u64, _ticks: u64, pad: &mut Pad) {
        while self.next < self.events.len() && self.events[self.next].frame <= rel {
            let e = &self.events[self.next];
            pad.apply(e);
            self.applied.push(json!({"event_index":self.next,"scheduled_run_frame":e.frame,
                "run_frame":rel,"frame":pad.m.bus.frames,"buttons":crate::replay::button_names(e.buttons),
                "sticks":e.sticks}));
            self.next += 1;
        }
    }

    fn finish(self: Box<Self>, _m: &Machine, out: &mut Out) -> crate::Result<()> {
        let n = self.events.len();
        if n > 0 {
            let path = out.dir.join("input.jsonl");
            let bytes = jsonl(&self.applied);
            std::fs::write(&path, bytes.as_bytes()).map_err(|e| Error::io(&path, e))?;
            out.files.insert("input.jsonl".into(), json!({"path":path,"sha256":sha256(bytes.as_bytes())}));
        }
        let file = (n > 0).then(|| out.dir.join("input.jsonl"));
        out.section(
            "input",
            json!({"application":{"observed":true,"scheduled_events":n,"applied_events":self.next,
                "pending_events":n-self.next,"file":file,
                "note":"Actual scheduler applications, including boundary releases; does not prove the game polled the controller."}}),
        );
        Ok(())
    }
}

/// `until`: the first boundary where every condition holds ends the run.
pub(super) struct Until {
    pub conds: Vec<crate::until::Until>,
    pub met: Option<(u64, u64)>,
}

impl Observer for Until {
    fn field(&mut self, rel: u64, m: &mut Machine) -> crate::Result<Flow> {
        if self.conds.iter().all(|u| u.holds(&m.bus.ram, &m.bus.scratch)) {
            self.met = Some((rel, m.bus.frames));
            return Ok(Flow::Stop("until_met"));
        }
        Ok(Flow::Go)
    }

    fn finish(self: Box<Self>, m: &Machine, out: &mut Out) -> crate::Result<()> {
        out.section("until", crate::until::report(&self.conds, self.met, &m.bus.ram, &m.bus.scratch));
        Ok(())
    }
}

/// `audio`: the mixed sound output to audio.wav.
pub(super) struct Audio {
    pub wav: crate::native::WavStream,
    pub path: PathBuf,
}

impl Audio {
    pub fn new(m: &mut Machine, path: PathBuf) -> crate::Result<Audio> {
        let wav = crate::native::WavStream::create(&path).map_err(|e| Error::io(&path, e))?;
        m.bus.spu.capture = true;
        m.bus.spu.take_samples();
        Ok(Audio { wav, path })
    }
}

impl Observer for Audio {
    fn field(&mut self, _rel: u64, m: &mut Machine) -> crate::Result<Flow> {
        self.wav.push(&m.bus.spu.take_samples()).map_err(|e| Error::io(&self.path, e))?;
        Ok(Flow::Go)
    }

    fn last_field(&mut self, _rel: u64, m: &mut Machine, _complete: bool) -> crate::Result<()> {
        self.wav.push(&m.bus.spu.take_samples()).map_err(|e| Error::io(&self.path, e))
    }

    fn finish(self: Box<Self>, _m: &Machine, out: &mut Out) -> crate::Result<()> {
        let Audio { wav, path } = *self;
        let (count, peak) = (wav.samples, wav.peak);
        wav.finish().map_err(|e| Error::io(&path, e))?;
        out.files.insert("audio.wav".into(), json!({"path": path}));
        out.section(
            "audio",
            json!({"file": path, "seconds": count as f64 / 88_200.0, "peak": peak, "silent": peak == 0,
                   "format": "44.1 kHz stereo 16-bit PCM, the SPU's mixed output (voices, reverb, CD audio) after the main volume"}),
        );
        Ok(())
    }
}

/// debug_go gpu and draw: one field's GPU commands, counted from the
/// first boundary it sees; `stop` ends the go at the field draw found.
pub(crate) struct Gpu {
    pub capture: GpuCapture,
    pub start: Option<u64>,
    pub stop: bool,
}

impl Observer for Gpu {
    fn field(&mut self, _rel: u64, m: &mut Machine) -> crate::Result<Flow> {
        let start = *self.start.get_or_insert(m.bus.frames);
        self.capture.boundary(m, start, false);
        Ok(match self.stop && self.capture.selected.is_some() {
            true => Flow::Stop("draw"),
            false => Flow::Go,
        })
    }

    fn last_field(&mut self, _rel: u64, m: &mut Machine, _complete: bool) -> crate::Result<()> {
        let start = *self.start.get_or_insert(m.bus.frames);
        self.capture.boundary(m, start, true);
        Ok(())
    }

    fn finish(self: Box<Self>, _m: &Machine, out: &mut Out) -> crate::Result<()> {
        let v = match &self.capture.selected {
            Some(frame) => super::gpu_frame_outputs(frame, out.dir, out.files)?,
            None => self.capture.missing(out.termination),
        };
        out.section("gpu_frame", v);
        Ok(())
    }
}

/// `find_tick`: every CPU store grouped by the fields it fell in.
pub(super) struct Census;

impl Census {
    pub fn new(m: &mut Machine) -> Census {
        m.bus.obs.census = Some(Default::default());
        Census
    }
}

impl Observer for Census {
    fn field(&mut self, rel: u64, m: &mut Machine) -> crate::Result<Flow> {
        if let Some(c) = m.bus.obs.census.as_mut().filter(|_| rel > 0) {
            c.end_field();
        }
        Ok(Flow::Go)
    }

    fn last_field(&mut self, rel: u64, m: &mut Machine, complete: bool) -> crate::Result<()> {
        if complete {
            self.field(rel, m)?;
        }
        Ok(())
    }

    fn finish(self: Box<Self>, m: &Machine, out: &mut Out) -> crate::Result<()> {
        if let Some(c) = &m.bus.obs.census {
            let (mut section, file) = crate::tickfind::report(c);
            out.put("tick_finder.json", serde_json::to_string_pretty(&file).unwrap_or_default().as_bytes());
            section["file"] = json!(out.dir.join("tick_finder.json"));
            out.section("tick_finder", section);
        }
        Ok(())
    }
}

/// `pad_reads`: the controller's reply followed into RAM.
pub(super) struct PadFlow {
    pub flow: crate::pad_reads::PadReads,
    pub scopes: Scopes,
}

impl Observer for PadFlow {
    fn per_step(&self) -> bool {
        true
    }

    fn step(&mut self, info: &StepInfo, m: &mut Machine) -> Flow {
        self.flow.step(info, m);
        Flow::Go
    }

    fn finish(self: Box<Self>, m: &Machine, out: &mut Out) -> crate::Result<()> {
        out.section("pad_reads", self.flow.report(&self.scopes, m));
        Ok(())
    }
}

/// `sample`: samples.csv, a row per field or per tick.
pub(crate) struct Samples(pub crate::ram::Sampler);

impl Observer for Samples {
    fn field(&mut self, rel: u64, m: &mut Machine) -> crate::Result<Flow> {
        self.0.field(rel, m, false);
        Ok(Flow::Go)
    }

    fn tick(&mut self, rel: u64, m: &mut Machine) -> Flow {
        self.0.tick(rel, m);
        Flow::Go
    }

    fn last_field(&mut self, rel: u64, m: &mut Machine, complete: bool) -> crate::Result<()> {
        if complete {
            self.0.field(rel, m, true);
        }
        Ok(())
    }

    fn finish(self: Box<Self>, _m: &Machine, out: &mut Out) -> crate::Result<()> {
        let s = &self.0;
        out.put("samples.csv", s.out.as_bytes());
        let tick = s.per == crate::ram::SamplePer::Tick;
        let mut r = json!({
            "file": out.dir.join("samples.csv"), "rows": s.rows, "every": s.every,
            "per": if tick { "tick" } else { "field" },
            "columns": s.columns.iter().map(|c| match c {
                crate::ram::Column::Memory(c) => match &c.path {
                    Some(p) if p.derefs => json!({"name": c.name, "path": p.text, "format": c.fmt.name(), "note": "read through a pointer each row: an empty cell where one is null or leads outside memory"}),
                    p => {
                        let mut v = json!({"name": c.name, "address": hex(c.addr), "region": region(c.addr), "format": c.fmt.name()});
                        if let Some(p) = p { v["path"] = json!(p.text) }
                        v
                    }
                },
                crate::ram::Column::Quantity(n) => json!({"name": n, "source": "glossary"}),
            }).collect::<Vec<_>>(),
            "timing": if tick {
                "a row at the end of every tick of the game's logic (the glossary binding's tick store), as that store left memory; tick counts the ticks ended since the run start"
            } else {
                "row N = memory at the Nth vblank of the run (row 0 = the starting memory), before that frame's replay input"
            },
        });
        if tick {
            r["ticks"] = json!(s.ticks);
            if s.ticks == 0 {
                r["note"] = json!(
                    "the glossary binding's tick store never ran: check its address, bytes and pc, and that the run reaches the game's logic"
                );
            }
        }
        if let Some(q) = &s.quantities {
            if s.columns.iter().any(|c| matches!(c, crate::ram::Column::Quantity(_))) {
                r["quantities"] = json!(
                    "raw, as recordings hold them: a fixed's underlying integer, a float's bit pattern; an empty cell where the binding refused"
                );
            }
            if q.refusal_count > 0 {
                r["refusal_count"] = json!(q.refusal_count);
                r["refusals"] = json!(q.refusals);
            }
        }
        out.section("samples", r);
        Ok(())
    }
}

/// `profile`: the CPU's cycles by function.
pub(super) struct Profile {
    pub p: psx_core::profile::Profiler,
    pub start_pc: u32,
    pub scopes: Scopes,
}

impl Profile {
    pub fn new(m: &Machine, scopes: Scopes) -> Profile {
        Profile { p: psx_core::profile::Profiler::new(m.cpu.cycles), start_pc: m.cpu.pc, scopes }
    }
}

impl Observer for Profile {
    fn per_step(&self) -> bool {
        true
    }

    fn step(&mut self, info: &StepInfo, m: &mut Machine) -> Flow {
        self.p.after_step(info, m.cpu.cycles);
        Flow::Go
    }

    fn finish(self: Box<Self>, m: &Machine, out: &mut Out) -> crate::Result<()> {
        let v = super::profile_outputs(&self.p, self.start_pc, m, &self.scopes, out.dir, out.files)?;
        out.section("profile", v);
        Ok(())
    }
}

/// `trace`: instructions, coverage, calls and code pages.
pub(crate) struct Trace {
    pub t: Tracer,
    pub scopes: Scopes,
}

impl Trace {
    pub fn new(m: &mut Machine, t: Tracer, scopes: Scopes) -> Trace {
        // The tracer copies a code page again when a store changed it.
        m.bus.page_writes = vec![0; psx_core::bus::RAM_SIZE / psx_core::trace::CODE_PAGE];
        Trace { t, scopes }
    }
}

impl Observer for Trace {
    fn per_step(&self) -> bool {
        true
    }

    fn step(&mut self, info: &StepInfo, m: &mut Machine) -> Flow {
        self.t.after_step(info, &m.cpu, &m.bus);
        Flow::Go
    }

    fn finish(mut self: Box<Self>, m: &Machine, out: &mut Out) -> crate::Result<()> {
        let v = super::trace_outputs(&mut self.t, m, &self.scopes, out.dir, out.start, out.files)?;
        out.section("trace", v);
        Ok(())
    }
}

/// `watch_reads`: CPU loads from the ranges, to reads.jsonl.
pub(crate) struct Reads {
    pub ranges: Vec<(u32, u32)>,
    pub max: usize,
    pub start: u64,
    pub scopes: Scopes,
    pub reads: Vec<Value>,
    pub hits: u64,
    pub readers: std::collections::HashMap<u32, u64>,
}

impl Observer for Reads {
    fn per_step(&self) -> bool {
        true
    }

    fn step(&mut self, info: &StepInfo, m: &mut Machine) -> Flow {
        let Some(acc) = info.mem.filter(|acc| !acc.store) else { return Flow::Go };
        let p = crate::run::fold(acc.addr);
        if !self.ranges.iter().any(|&(lo, hi)| p < hi && p + acc.width as u32 > lo) {
            return Flow::Go;
        }
        self.hits += 1;
        *self.readers.entry(info.pc).or_default() += 1;
        if self.reads.len() < self.max {
            let function = crate::name_pc(&self.scopes, info.pc, Some(&m.bus.ram))
                .map(|(name, check, source)| json!({"name":name,"exe":source,"check":check.as_str()}))
                .unwrap_or(Value::Null);
            self.reads.push(json!({
                "frame": m.bus.frames, "run_frame": m.bus.frames - self.start, "cycle": m.bus.now,
                "pc": hex(info.pc),
                "function": function,
                "address": hex(acc.addr), "region": region(acc.addr), "width": acc.width,
                "value": acc.value, "value_hex": format!("0x{:0n$X}", acc.value, n = acc.width as usize * 2),
            }));
        }
        Flow::Go
    }

    fn finish(mut self: Box<Self>, m: &Machine, out: &mut Out) -> crate::Result<()> {
        for r in &mut self.reads {
            let global = r["address"]
                .as_str()
                .and_then(psx_media::memmap::parse_hex)
                .and_then(|a| crate::names_bind::global_at(&crate::names_bind::project_names(), a));
            if let Some(g) = global {
                r["global"] = json!(g);
            }
        }
        out.put("reads.jsonl", jsonl(&self.reads).as_bytes());
        let mut top: Vec<(u32, u64)> = std::mem::take(&mut self.readers).into_iter().collect();
        top.sort_by_key(|&(pc, n)| (std::cmp::Reverse(n), pc));
        let (hits, max) = (self.hits, self.max as u64);
        out.section(
            "read_watch",
            json!({
                "enabled":true,"hits": hits, "recorded": hits.min(max), "file": out.dir.join("reads.jsonl"),
                "complete":hits<=max,"truncated":hits>max,
                "readers_page":crate::page::metadata(top.len(),0,top.len().min(16),16),
                "readers": top.iter().take(16).map(|&(pc, n)| {
                    let mut v = json!({"pc": hex(pc), "count": n});
                    v["function"] = crate::final_name(&self.scopes, pc, Some(&m.bus.ram));
                    v
                }).collect::<Vec<_>>(),
            }),
        );
        Ok(())
    }
}

/// `snapshot_every`: the screen every N run frames.
pub(super) struct Screens {
    pub every: u64,
    pub dir: PathBuf,
    pub shots: Vec<Value>,
}

impl Observer for Screens {
    fn field(&mut self, rel: u64, m: &mut Machine) -> crate::Result<Flow> {
        if rel.is_multiple_of(self.every) {
            let sd = self.dir.join("screens");
            std::fs::create_dir_all(&sd).map_err(|e| Error::io(&sd, e))?;
            let (w, h, rgba) = m.bus.gpu.display_rgba();
            let p = sd.join(format!("f{rel:06}.png"));
            write_png(&p, w, h, &rgba, png::ColorType::Rgba).map_err(|e| Error::io(&p, e))?;
            self.shots.push(json!({"run_frame": rel, "frame": m.bus.frames, "path": p}));
        }
        Ok(Flow::Go)
    }

    fn last_field(&mut self, rel: u64, m: &mut Machine, complete: bool) -> crate::Result<()> {
        if complete {
            self.field(rel, m)?;
        }
        Ok(())
    }

    fn finish(self: Box<Self>, _m: &Machine, out: &mut Out) -> crate::Result<()> {
        out.section(
            "screens",
            json!({"dir": out.dir.join("screens"), "count": self.shots.len(), "first": self.shots.first(), "last": self.shots.last()}),
        );
        Ok(())
    }
}

/// `watch`: the core's write watches, to writes.jsonl; the section is there
/// without any.
pub(crate) struct Writes {
    pub enabled: bool,
    pub changes_only: bool,
    pub scopes: Scopes,
}

impl Observer for Writes {
    fn finish(self: Box<Self>, m: &Machine, out: &mut Out) -> crate::Result<()> {
        let hits: Vec<Value> = m
            .bus
            .obs
            .hits
            .iter()
            .filter(|h| !(self.changes_only && h.old.is_some() && h.old == h.value))
            .map(|h| with_run_frame(super::annotate_hit(h), out.start))
            .collect();
        if self.enabled {
            out.put("writes.jsonl", jsonl(&hits).as_bytes());
        }
        let shown: Vec<Value> = hits
            .iter()
            .take(64)
            .map(|v| {
                let mut v = v.clone();
                if let Some(pc) = addr_value(&v["pc"]) {
                    if v["function"].is_null() {
                        v["function"] = crate::final_name(&self.scopes, pc, Some(&m.bus.ram));
                    }
                }
                v
            })
            .collect();
        // Every recorded store by its writer: the PC, or DMA without one.
        let mut by: std::collections::HashMap<Option<u32>, (u64, &psx_core::observe::Writer)> = Default::default();
        for h in m.bus.obs.hits.iter().filter(|h| !(self.changes_only && h.old.is_some() && h.old == h.value)) {
            by.entry(h.pc).or_insert((0, &h.writer)).0 += 1;
        }
        let mut writers: Vec<_> = by.into_iter().collect();
        writers.sort_by_key(|&(pc, (n, _))| (std::cmp::Reverse(n), pc));
        let writers: Vec<Value> = writers
            .iter()
            .take(16)
            .map(|&(pc, (n, w))| {
                let mut v = json!({"count": n, "writer": w});
                if let Some(pc) = pc {
                    v["pc"] = json!(hex(pc));
                    v["function"] = crate::final_name(&self.scopes, pc, Some(&m.bus.ram));
                }
                v
            })
            .collect();
        let obs = &m.bus.obs;
        out.section(
            "watch",
            json!({"enabled":self.enabled,"hits": obs.hit_count, "recorded": obs.hits.len(), "shown": shown.len(), "first": shown,
                "writers": writers,
                "emitted":hits.len(),"complete":obs.hit_count==obs.hits.len() as u64,
                "truncated":obs.hit_count>obs.hits.len() as u64,
                "file":self.enabled.then(||out.dir.join("writes.jsonl")),"changes_only":self.changes_only}),
        );
        Ok(())
    }
}

/// `trace_cd`, `trace_bios`, `trace_spu`: the core's event logs.
pub(super) struct Logs {
    pub scopes: Scopes,
}

impl Observer for Logs {
    fn finish(self: Box<Self>, m: &Machine, out: &mut Out) -> crate::Result<()> {
        let start = out.start;
        let mut event_logs = serde_json::Map::new();
        if let Some(cd) = &m.bus.obs.cd {
            let lines: Vec<Value> =
                cd.iter().map(|t| with_run_frame(serde_json::to_value(t).unwrap(), start)).collect();
            out.put("cd_native.jsonl", jsonl(&lines).as_bytes());
            event_logs.insert("cd_native".into(), json!({"events": cd.len(), "recorded": cd.len()}));
        }
        if let Some(b) = &m.bus.obs.bios {
            let lines: Vec<Value> = b.iter().map(|t| with_run_frame(serde_json::to_value(t).unwrap(), start)).collect();
            out.put("bios.jsonl", jsonl(&lines).as_bytes());
            event_logs.insert("bios".into(), json!({"events": b.len(), "recorded": b.len()}));
        }
        out.section("event_logs", Value::Object(event_logs));
        if let Some(log) = &m.bus.obs.spu {
            let events: Vec<Value> = log
                .events
                .iter()
                .map(|e| {
                    let mut v = with_run_frame(serde_json::to_value(e).unwrap(), start);
                    if let Some(pc) = e.pc {
                        let name = crate::final_name(&self.scopes, pc, Some(&m.bus.ram));
                        if !name.is_null() {
                            v["function"] = name;
                        }
                    }
                    v
                })
                .collect();
            out.put("spu.jsonl", jsonl(&events).as_bytes());
            let v = super::spu_summary(log, &events, out.dir);
            out.section("spu", v);
        }
        Ok(())
    }
}
