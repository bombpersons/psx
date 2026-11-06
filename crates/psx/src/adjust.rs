//! Adjustments: changes psx makes to how the original meets the machine
//! while a recording runs, never to the game's rules, so the original runs
//! to the port's model (docs/master-plan.md). A recording names the ones it
//! was made with (`producer.details.adjust`; none means stock), the
//! determinism check runs with them, and `scenario adjust-check` measures
//! them against the stock game.
//!
//! **latch**: the game's pad changes only where a tick ends, to that tick's
//! input. The controller takes new input only there (input that arrives
//! mid-tick waits), and where psx has found the pad buffer
//! ([`psx_core::padflow`]) it writes the tick's input into the buffer at
//! once, through each byte's mask, and puts the buffer back whenever the
//! interrupt handler stores into it before the next tick ends. Every read
//! the game makes during a tick then sees that tick's input, whatever its
//! loop does between polls. Ticks in which no game code read the pad are
//! counted as lag frames; a tick ending inside an exception handler is
//! counted too (the binding's tick is then not the game's loop).
//!
//! **steady**: no tick overruns its fields. The CPU's own work (instruction
//! issue and fetch) costs a quarter of its clocks ([`STEADY_OVERCLOCK`]),
//! and the GPU draws without taking time, so the game's work for a tick
//! fits in its fields where the console slowed down. Timers, the CD, the
//! SPU and video timing keep the real clock. Emulators offer the same to
//! players (DuckStation's CPU overclock), and it breaks some games, which
//! is why it is measured per game.
//!
//! **off**: functions the scenario names (`[off]` in its `.toml`) return at
//! once, with the value given, when the CPU reaches their entry: systems
//! the port does not have yet (enemies, a cutscene trigger), so a feature's
//! recordings hold only what it ports. Unlike latch and steady it changes
//! the game itself, which the recording says (`producer.details.off`).
//!
//! **fix**: the agent's replacements (`fixes/`, built for the R3000) of the
//! game's functions that keep the tick's input from the game, such as a pad
//! driver that skips its poll: a port is told each tick's input and never
//! copies such a delay (docs/master-plan.md, A9). The recording keeps the
//! build (`producer.details.fix`), so a check runs what it was made with.

use psx_core::Machine;
use psx_core::cpu::StepInfo;
use psx_core::padflow::{BufferByte, PadFlow};
use serde_json::{Value as Json, json};

/// The adjustments a run is made with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Adjustments {
    /// The pad changes only where a tick ends.
    pub latch: bool,
    /// No tick overruns its fields.
    pub steady: bool,
    /// Functions that return at once.
    pub off: Vec<Off>,
    /// The agent's replacements of functions that delay input.
    pub fix: Option<Fixes>,
    /// Test adjustments: one that does nothing, and one that adds 1 to the
    /// word at an address when a tick ends.
    #[cfg(test)]
    pub test: Option<TestAdjust>,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TestAdjust {
    Nothing,
    /// When tick `tick` ends, the word at `addr` (in main RAM) goes up by 1.
    Nudge {
        tick: u64,
        addr: u32,
    },
}

/// fixes/ built (`fix`): the functions replaced and the build they go to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fixes {
    pub functions: Vec<Fixed>,
    /// Loaded at [`crate::decomp::FIX_BASE`].
    pub image: Vec<u8>,
    /// fixes/'s hash when it was built.
    pub sources: String,
}

/// A game function the fixes replace: its entry, and where its
/// replacement is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fixed {
    pub name: String,
    pub address: u32,
    pub at: u32,
}

/// A function that returns at once (`off`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Off {
    /// As the scenario names it.
    pub name: String,
    /// Its entry.
    pub address: u32,
    /// What it returns, in v0.
    pub returns: u32,
}

impl Off {
    /// A scenario's `[off]` table, each name resolved by the project's names
    /// (or hex): what does not resolve, by name, with a spelling the names
    /// files have when one is close.
    pub fn resolve(table: &std::collections::BTreeMap<String, i64>) -> Result<Vec<Off>, Vec<String>> {
        let (mut out, mut problems) = (Vec::new(), Vec::new());
        for (name, v) in table {
            match crate::names_bind::address(name) {
                Ok(address) => out.push(Off { name: name.clone(), address, returns: *v as u32 }),
                Err(e) => problems.push(match spelling(&crate::names_bind::project_names(), name) {
                    Some(k) => format!("off {name:?}: {e} ({k}?)"),
                    None => format!("off {name:?}: {e}"),
                }),
            }
        }
        if problems.is_empty() { Ok(out) } else { Err(problems) }
    }
}

/// A function in `names` spelled as `name` is, but for case and
/// underscores.
fn spelling(names: &psx_symbols::names::Names, name: &str) -> Option<String> {
    let key = |s: &str| s.to_ascii_lowercase().replace('_', "");
    let (module, bare) = name.rsplit_once(':').map_or((None, name), |(m, n)| (Some(m), n));
    let near = names.functions.iter().find(|f| key(&f.name.text) == key(bare))?;
    Some(match module {
        Some(m) => format!("{m}:{}", near.name.text),
        None => near.name.text.clone(),
    })
}

/// The names of the adjustments this psx knows.
const KNOWN: &[&str] = &["latch", "steady", "off", "fix"];

/// `steady`'s clock multiplier for the CPU's own work.
pub(crate) const STEADY_OVERCLOCK: u32 = 4;

impl Adjustments {
    /// None: the stock game.
    pub fn stock() -> Adjustments {
        Adjustments::default()
    }

    /// What `scenario record` and `scenario play` use unless `stock`: every
    /// adjustment this psx has.
    pub fn default_set() -> Adjustments {
        Adjustments { latch: true, steady: true, ..Adjustments::default() }
    }

    pub fn is_stock(&self) -> bool {
        self.names().is_empty()
    }

    /// Their names, as recordings keep them.
    pub fn names(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.latch {
            out.push("latch");
        }
        if self.steady {
            out.push("steady");
        }
        if !self.off.is_empty() {
            out.push("off");
        }
        if self.fix.is_some() {
            out.push("fix");
        }
        #[cfg(test)]
        match self.test {
            Some(TestAdjust::Nothing) => out.push("test_nothing"),
            Some(TestAdjust::Nudge { .. }) => out.push("test_nudge"),
            None => {}
        }
        out
    }

    /// The adjustments a recording names in its details (`adjust`); none
    /// named is stock. A name this psx does not know is an error.
    pub fn from_details(details: &serde_json::Map<String, Json>) -> Result<Adjustments, String> {
        let names: Vec<&str> = match details.get("adjust") {
            None | Some(Json::Null) => return Ok(Adjustments::stock()),
            Some(Json::Array(a)) => {
                a.iter().map(|v| v.as_str().ok_or("adjust: a list of names")).collect::<Result<_, _>>()?
            }
            Some(_) => return Err("adjust: a list of names".into()),
        };
        let mut a = Adjustments::stock();
        for n in names {
            match n {
                "latch" => a.latch = true,
                "steady" => a.steady = true,
                "off" => {
                    let bad = || "off: a list of {name, address, returns}".to_string();
                    let list = details.get("off").and_then(Json::as_array).ok_or_else(bad)?;
                    for o in list {
                        let address = o["address"].as_str().and_then(psx_media::memmap::parse_hex).ok_or_else(bad)?;
                        let returns = o["returns"].as_u64().ok_or_else(bad)? as u32;
                        let name = o["name"].as_str().ok_or_else(bad)?.to_string();
                        a.off.push(Off { name, address, returns });
                    }
                }
                "fix" => {
                    let bad = || "fix: {functions: [{name, address, at}], image, sources}".to_string();
                    let d = details.get("fix").ok_or_else(bad)?;
                    let hex = |v: &Json| v.as_str().and_then(psx_media::memmap::parse_hex);
                    let mut functions = Vec::new();
                    for f in d["functions"].as_array().ok_or_else(bad)? {
                        let name = f["name"].as_str().ok_or_else(bad)?.to_string();
                        functions.push(Fixed { name, address: hex(&f["address"]).ok_or_else(bad)?, at: hex(&f["at"]).ok_or_else(bad)? });
                    }
                    let text = d["image"].as_str().ok_or_else(bad)?;
                    let image = (0..text.len() / 2)
                        .map(|i| text.get(2 * i..2 * i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
                        .collect::<Option<Vec<u8>>>()
                        .ok_or_else(bad)?;
                    let sources = d["sources"].as_str().unwrap_or_default().to_string();
                    a.fix = Some(Fixes { functions, image, sources });
                }
                #[cfg(test)]
                "test_nothing" => a.test = Some(TestAdjust::Nothing),
                _ => {
                    return Err(format!(
                        "adjust: this psx does not know the adjustment {n:?} (it knows {}): update psx, or record again",
                        KNOWN.join(", ")
                    ));
                }
            }
        }
        Ok(a)
    }

    /// Their entry in a recording's details, when there are any.
    pub fn detail(&self) -> Option<Json> {
        (!self.is_stock()).then(|| json!(self.names()))
    }

    /// The functions turned off, for the recording's details (`off`), by
    /// address as well as name, so a check later runs the same ones.
    pub fn off_detail(&self) -> Option<Json> {
        (!self.off.is_empty()).then(|| {
            json!(
                self.off
                    .iter()
                    .map(|o| json!({"name": o.name, "address": format!("0x{:08X}", o.address),
                                                  "returns": o.returns}))
                    .collect::<Vec<_>>()
            )
        })
    }

    /// The fixes, for the recording's details (`fix`): the build itself,
    /// so a check runs what the recording was made with.
    pub fn fix_detail(&self) -> Option<Json> {
        self.fix.as_ref().map(|f| {
            json!({
                "functions": f.functions.iter().map(|x| json!({"name": x.name, "address": format!("0x{:08X}", x.address),
                    "at": format!("0x{:08X}", x.at)})).collect::<Vec<_>>(),
                "image": f.image.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "sources": f.sources,
            })
        })
    }

    /// These without latch, steady and the fixes: `--stock` keeps what the
    /// scenario turns off, which is its scope, not its timing.
    pub fn without_timing(&self) -> Adjustments {
        Adjustments { latch: false, steady: false, fix: None, ..self.clone() }
    }

    /// What they keep while a run goes, made where the recording starts;
    /// `steady` takes effect on `m` from here.
    pub fn start(&self, m: &mut Machine) -> Running {
        m.cpu.overclock = if self.steady { STEADY_OVERCLOCK } else { 0 };
        m.bus.gpu.timed = !self.steady;
        m.redirects.clear_returns();
        for o in &self.off {
            m.redirects.insert(o.address, psx_core::Redirect::Return(o.returns));
        }
        if let Some(f) = &self.fix {
            crate::decomp::load_hidden(m, crate::decomp::FIX_BASE, &f.image);
            for x in &f.functions {
                m.redirects.insert(x.address, psx_core::Redirect::To(x.at));
            }
        }
        Running {
            a: self.clone(),
            flow: self.latch.then(PadFlow::new),
            held: None,
            latched: Vec::new(),
            latched_pad: None,
            read: false,
            ticks: 0,
            lag: Vec::new(),
            in_handler: 0,
            unmasked: false,
            buffer_reads: 0,
            stale_reads: 0,
            pending: Pending::No,
            field: 0,
            waited: 0,
            late_fields: 0,
            late_ticks: Vec::new(),
        }
    }
}

/// Adjustments while a run goes.
pub(crate) struct Running {
    a: Adjustments,
    /// `latch`: the pad buffer's search, and the game's reads.
    flow: Option<PadFlow>,
    /// `latch`: input that arrived during the tick, for where it ends.
    held: Option<crate::replay::Input>,
    /// `latch`: the buffer bytes written where the last tick ended, and
    /// their values, to put back over the interrupt handler's stores.
    latched: Vec<(u32, u8)>,
    /// `latch`: the controller's state written there (buttons, sticks,
    /// analog), to tell a change from the buffer the game is seen to keep.
    latched_pad: Option<(u16, [u8; 4], bool)>,
    /// `latch`: some game code read the pad during this tick.
    read: bool,
    ticks: u64,
    /// `latch`: the ticks in which no game code read the pad.
    lag: Vec<u64>,
    /// Ticks that ended inside an exception handler.
    in_handler: u64,
    /// A buffer byte the game stores other than as a copy (or an inverted
    /// one) of the controller's: the latch could not write it.
    unmasked: bool,
    /// `latch`: loads by game code (outside a handler) of a latched byte,
    /// and those that read anything but the tick's latched value.
    buffer_reads: u64,
    stale_reads: u64,
    /// `latch`: whether the game has taken in the buttons latched where the
    /// last tick ended, by a poll (the handler storing into the buffer) or
    /// a load of them (its code's, or a handler's copying them).
    pending: Pending,
    /// The field the last step ran in, to see each one end.
    field: u64,
    /// `latch`: whole fields of this tick that passed with its latched
    /// change not taken in; late unless game code reads the buffer before
    /// the tick ends (its reads see the latch).
    waited: u64,
    /// `latch`: the fields of ticks that ended late, and those ticks.
    late_fields: u64,
    late_ticks: Vec<u64>,
}

/// Where a latched change stands (`latch`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    /// Taken in, or nothing changed.
    No,
    /// Latched in the field now running: a poll may yet come in the next.
    Latched,
    /// Not taken in through a whole field.
    Waiting,
}

/// The reply byte at `pos` for buttons `held` and `sticks` (as psx-core's
/// pad keeps them: rx, ry, lx, ly), in analog mode or not.
fn reply_byte(pos: u8, held: u16, sticks: [u8; 4], analog: bool) -> Option<u8> {
    match pos {
        3 => Some(!(held as u8)),
        4 => Some(!((held >> 8) as u8)),
        5..=8 if analog => Some(sticks[pos as usize - 5]),
        _ => None,
    }
}

fn poke(m: &mut Machine, addr: u32, v: u8) {
    let p = addr & 0x1FFF_FFFF;
    if p < 0x0080_0000 {
        m.bus.ram[(p & 0x1F_FFFF) as usize] = v;
    } else if (0x1F80_0000..0x1F80_0400).contains(&p) {
        m.bus.scratch[(p - 0x1F80_0000) as usize] = v;
    }
}

impl Running {
    /// Input from the recording's events or ticks: on the controller at
    /// once, or with `latch`, where the tick ends.
    pub fn input(&mut self, m: &mut Machine, i: &crate::replay::Input) {
        if self.a.latch {
            self.held = Some(*i);
        } else {
            crate::replay::apply(m, i);
        }
    }

    /// `latch` where the game's tick is not running (a menu, the boot):
    /// input reaches the controller at once, as on the stock game, and the
    /// buffer latched where the last tick ended is no longer put back over
    /// the game's own polls. The next tick to end latches again. Only for a
    /// person's play: a recording refuses a tick that does not end instead.
    pub fn pass_through(&mut self, m: &mut Machine, i: Option<&crate::replay::Input>) {
        if let Some(i) = i.or(self.held.as_ref()).copied() {
            crate::replay::apply(m, &i);
        }
        self.held = None;
        self.latched.clear();
        self.latched_pad = None;
        self.pending = Pending::No;
        self.waited = 0;
    }

    /// The step `info` just ran on `m`.
    pub fn step(&mut self, info: &StepInfo, m: &mut Machine) {
        let Some(f) = &mut self.flow else { return };
        f.step(info, &m.bus.sio0);
        self.read |= f.take_reads().iter().any(|r| !r.in_handler);
        if m.bus.frames != self.field {
            self.field = m.bus.frames;
            // A field ended: a change latched before it began and still
            // not taken in waited a whole field for the game.
            match self.pending {
                Pending::Latched => self.pending = Pending::Waiting,
                Pending::Waiting => self.waited += 1,
                Pending::No => {}
            }
        }
        let Some(acc) = info.mem else { return };
        let p = acc.addr & 0x1FFF_FFFF;
        let hit = |a: u32| (p..p + acc.width as u32).contains(&(a & 0x1FFF_FFFF));
        if acc.store && f.in_handler() {
            // The interrupt handler stored over the latched buffer: put it back.
            for (a, v) in self.latched.iter().filter(|(a, _)| hit(*a)) {
                poke(m, *a, *v);
                self.pending = Pending::No;
            }
        } else if !acc.store && self.latched.iter().any(|(a, _)| hit(*a)) {
            // A load of the latched buttons takes them in, by a handler handing
            // them to the game (as a poll does) or by the game's code, whose
            // read before the tick ends also sees them in time.
            self.pending = Pending::No;
            if !f.in_handler() {
                self.waited = 0;
                self.buffer_reads += 1;
                let byte = |a: u32| (acc.value >> (8 * ((a & 0x1FFF_FFFF) - p))) as u8;
                if self.latched.iter().any(|(a, v)| hit(*a) && byte(*a) != *v) {
                    self.stale_reads += 1;
                }
            }
        }
    }

    /// Tick `n` ended (its quantities read) on `m`.
    pub fn tick_ended(&mut self, n: u64, m: &mut Machine) {
        if let Some(f) = &self.flow {
            if f.in_handler() {
                self.in_handler += 1;
            }
            if n > 0 {
                self.ticks += 1;
                if !self.read {
                    self.lag.push(n);
                }
            }
            self.read = false;
            if self.waited > 0 {
                self.late_fields += std::mem::take(&mut self.waited);
                self.late_ticks.push(self.ticks);
            }
            if let Some(i) = self.held.take() {
                crate::replay::apply(m, &i);
            }
            self.latch(f.buffer(), m);
        }
        #[cfg(test)]
        if let Some(TestAdjust::Nudge { tick, addr }) = self.a.test {
            if n == tick {
                let a = (addr & 0x1F_FFFF) as usize;
                let ram = &mut m.bus.ram;
                let v = u32::from_le_bytes(ram[a..a + 4].try_into().unwrap()).wrapping_add(1);
                ram[a..a + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
    }

    /// Writes the controller's state into the pad buffer, through each
    /// byte's mask.
    fn latch(&mut self, buffer: Vec<BufferByte>, m: &mut Machine) {
        let (held, sticks, analog) = (m.bus.sio0.buttons, m.bus.sio0.pad.sticks, m.bus.sio0.pad.analog);
        let before = std::mem::take(&mut self.latched);
        for b in buffer {
            let Some(v) = reply_byte(b.pos, held, sticks, analog) else { continue };
            match b.mask {
                Some(mask) => {
                    poke(m, b.addr, v ^ mask);
                    self.latched.push((b.addr, v ^ mask));
                }
                None => self.unmasked = true,
            }
        }
        let pad = Some((held, if analog { sticks } else { [0; 4] }, analog));
        if !before.is_empty() && !self.latched.is_empty() && self.latched_pad != pad {
            self.pending = Pending::Latched;
        }
        if !self.latched.is_empty() {
            self.latched_pad = pad;
        }
    }

    /// What they did on `m`, for the recording's details; None for a stock run.
    pub fn report(&self, m: &Machine) -> Option<Json> {
        let mut r = self.latch_report().unwrap_or_else(|| json!({}));
        if !self.a.off.is_empty() {
            let calls: Vec<Json> =
                self.a.off.iter().map(|o| json!({"name": o.name, "calls": m.redirects.hits(o.address)})).collect();
            r["off_calls"] = json!(calls);
        }
        if let Some(f) = &self.a.fix {
            let calls: Vec<Json> =
                f.functions.iter().map(|x| json!({"name": x.name, "calls": m.redirects.hits(x.address)})).collect();
            r["fix_calls"] = json!(calls);
        }
        (r != json!({})).then_some(r)
    }

    fn latch_report(&self) -> Option<Json> {
        let f = self.flow.as_ref()?;
        let buffer: Vec<Json> = f
            .buffer()
            .iter()
            .map(|b| {
                json!({"address": format!("0x{:08X}", b.addr), "byte": crate::pad_reads::byte_name(b.pos),
                       "mask": b.mask.map(|m| format!("0x{m:02X}"))})
            })
            .collect();
        let mut r = json!({
            "buffer": buffer,
            "by": if buffer.is_empty() { "the controller, set where each tick ends: no pad buffer found" }
                  else if self.unmasked { "the controller, and the buffer bytes the game stores as the controller sent them" }
                  else { "the pad buffer and the controller" },
            "ticks": self.ticks,
            "lag_frames": self.lag.len(),
            "buffer_reads": self.buffer_reads,
            "late_fields": self.late_fields,
        });
        if self.stale_reads > 0 {
            r["stale_reads"] = json!(self.stale_reads);
        }
        if self.late_fields > 0 {
            r["late_input"] = json!({
                "fields": self.late_fields,
                "ticks": self.late_ticks.iter().take(20).collect::<Vec<_>>(),
                "tick_count": self.late_ticks.len(),
                "note": "ticks in which a whole field passed with the tick's latched input not taken in (no poll into the pad buffer) and no code read the buttons from it before the tick ended: the game ran that field on the last poll's buttons, so a change can reach its rules a tick late. The original's driver delays it; a port never copies that. Find what stops the poll and replace that function in fixes/ (guide topic code) so the tick's input arrives every field",
            });
        }
        if !self.lag.is_empty() {
            r["lag_frame_ticks"] = json!(self.lag.iter().take(20).collect::<Vec<_>>());
        }
        if self.in_handler > 0 {
            r["ticks_ended_in_handler"] = json!(self.in_handler);
            r["note"] = json!(
                "the binding's tick store ran inside an exception handler: its ticks are the handler's (vblank's), not the game's loop; move the tick to the store that ends the game's logic step"
            );
        }
        Some(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn details(v: Json) -> serde_json::Map<String, Json> {
        json!({ "adjust": v }).as_object().unwrap().clone()
    }

    #[test]
    fn a_recording_names_its_adjustments_and_unknown_ones_are_refused() {
        assert_eq!(Adjustments::from_details(&serde_json::Map::new()), Ok(Adjustments::stock()));
        let a = Adjustments { test: Some(TestAdjust::Nothing), ..Adjustments::stock() };
        assert_eq!(a.detail(), Some(json!(["test_nothing"])));
        assert_eq!(Adjustments::from_details(&details(a.detail().unwrap())), Ok(a));
        assert_eq!(Adjustments::stock().detail(), None);
        let d = Adjustments::default_set();
        assert_eq!(d.detail(), Some(json!(["latch", "steady"])));
        assert_eq!(Adjustments::from_details(&details(d.detail().unwrap())), Ok(d));
        let e = Adjustments::from_details(&details(json!(["latch_v9"]))).unwrap_err();
        assert!(e.contains("\"latch_v9\"") && e.contains("knows latch, steady"), "{e}");
        assert!(Adjustments::from_details(&details(json!("latch"))).is_err());
    }

    #[test]
    fn a_fix_is_kept_with_its_build_and_stock_leaves_it_out() {
        let a = Adjustments {
            latch: true,
            fix: Some(Fixes {
                functions: vec![Fixed { name: "pad_poll".into(), address: 0x8004_1200, at: crate::decomp::FIX_BASE }],
                image: vec![0x08, 0x00, 0xE0, 0x03, 0x00, 0x00, 0x00, 0x00],
                sources: "0123456789abcdef".into(),
            }),
            ..Adjustments::stock()
        };
        assert_eq!(a.detail(), Some(json!(["latch", "fix"])));
        let mut d = details(a.detail().unwrap());
        d.insert("fix".into(), a.fix_detail().unwrap());
        assert_eq!(Adjustments::from_details(&d), Ok(a.clone()));
        assert_eq!(a.without_timing(), Adjustments::stock());
        d.insert("fix".into(), json!({"functions": [], "image": "0g"}));
        assert!(Adjustments::from_details(&d).is_err());
        d.remove("fix");
        assert!(Adjustments::from_details(&d).is_err());
    }

    #[test]
    fn off_is_kept_by_address_and_a_misspelt_name_gets_a_spelling() {
        let a = Adjustments {
            off: vec![Off { name: "enemy_update".into(), address: 0x8003_1F64, returns: 0 }],
            ..Adjustments::stock()
        };
        assert_eq!(a.detail(), Some(json!(["off"])));
        let mut d = details(a.detail().unwrap());
        d.insert("off".into(), a.off_detail().unwrap());
        assert_eq!(Adjustments::from_details(&d), Ok(a.clone()));
        assert_eq!(a.without_timing(), a);
        let dir = std::env::temp_dir().join(format!("psx-off-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("names")).unwrap();
        std::fs::write(dir.join("names/a.names"), "function 0x80031F64 void enemy_update()\n").unwrap();
        let names = psx_symbols::names::load(&dir);
        assert_eq!(spelling(&names, "EnemyUpdate").as_deref(), Some("enemy_update"));
        assert_eq!(spelling(&names, "ENEMY:enemyupdate").as_deref(), Some("ENEMY:enemy_update"));
        assert_eq!(spelling(&names, "player_update"), None);
        let _ = std::fs::remove_dir_all(dir);
        let table = [("0x80010030".to_string(), 42i64)].into();
        assert_eq!(
            Off::resolve(&table),
            Ok(vec![Off { name: "0x80010030".into(), address: 0x8001_0030, returns: 42 }])
        );
    }

    #[test]
    fn reply_bytes_are_the_wire_s() {
        let sticks = [1, 2, 3, 4];
        assert_eq!(reply_byte(3, 0x4000, sticks, false), Some(0xFF));
        assert_eq!(reply_byte(4, 0x4000, sticks, false), Some(0xBF));
        assert_eq!(reply_byte(5, 0, sticks, false), None);
        assert_eq!(reply_byte(8, 0, sticks, true), Some(4));
        assert_eq!(reply_byte(1, 0, sticks, true), None);
    }
}
