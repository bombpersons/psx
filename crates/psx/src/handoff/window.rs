//! The handoff window: the game at full speed, and beside it (never on top of
//! it, so no game UI is hidden) a panel with the agent's request, the keys,
//! the controls menu and the hand-back note.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use minifb::{InputCallback, Key, KeyRepeat, Scale, ScaleMode, Window, WindowOptions};

use super::bindings::{self, Bindings};
use super::menu::{Menu, Outcome};
use super::{End, Session};

/// The game area (4:3, the displayed picture stretched into it).
const GW: usize = 640;
const GH: usize = 480;
/// The panel to its right.
const PX: usize = GW;
const PW: usize = 384;
const W: usize = GW + PW;
const H: usize = GH;
/// Text columns in the panel (8 px glyphs, 8 px margins).
const COLS: usize = (PW - 16) / 8;

const PANEL: u32 = 0x1A1D24;
const WHITE: u32 = 0xFFFFFF;
const YELLOW: u32 = 0xFFD84A;
const GREY: u32 = 0xA0A0A0;
const SELECTED: u32 = 0x2F4F8F;

/// How long a second F8 press still confirms starting over.
const RESTART_CONFIRM: Duration = Duration::from_secs(3);

/// Typed characters, for the note.
struct Chars(Rc<RefCell<Vec<char>>>);
impl InputCallback for Chars {
    fn add_char(&mut self, c: u32) {
        if let Some(ch) = char::from_u32(c).filter(|c| !c.is_control()) {
            self.0.borrow_mut().push(ch);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Play,
    Menu,
    /// Writing the note before handing back with this ending.
    Note(End),
}

pub fn play(s: &mut Session, request: &str, dir: &Path) -> Result<(End, String), String> {
    let title: String = request.chars().take(60).collect();
    let opts = WindowOptions { resize: true, scale: Scale::X1, scale_mode: ScaleMode::AspectRatioStretch, ..WindowOptions::default() };
    let mut win = Window::new(&format!("psx handoff: {title}"), W, H, opts).map_err(|e| format!("could not open a window: {e}"))?;
    // Pacing is ours (the console's field rate), not minifb's.
    win.set_target_fps(0);
    let typed = Rc::new(RefCell::new(Vec::new()));
    win.set_input_callback(Box::new(Chars(typed.clone())));

    let path = bindings::default_path();
    let mut b = Bindings::default();
    let mut msg = Message::default();
    match path.as_deref().map(bindings::load) {
        Some(Ok(Some(saved))) => b = saved,
        Some(Err(e)) => msg.show(format!("Controls file ignored: {e}")),
        _ => {}
    }
    let mut pads = Pads::new();
    if let Some(e) = &pads.error {
        msg.show(format!("No gamepads: {e}"));
    }
    let mut menu = Menu::default();
    let mut screen = Screen::Play;
    let mut paused = false;
    let mut restart_asked: Option<Instant> = None;
    let mut note = String::new();
    let mut buf = vec![0u32; W * H];
    let mut next = Instant::now();
    let mut live_at = Instant::now() - Duration::from_secs(5);

    loop {
        if !win.is_open() {
            return Ok((End::Closed, note));
        }
        pads.poll();
        let pressed: Vec<String> = win.get_keys_pressed(KeyRepeat::No).iter().map(key_name).collect();
        let held: Vec<String> = win.get_keys().iter().map(key_name).collect();
        let pad_pressed = std::mem::take(&mut pads.pressed);
        let pad_held = pads.held();
        let has = |k: &str| pressed.iter().any(|p| p == k);
        let typed_now: Vec<char> = std::mem::take(&mut *typed.borrow_mut());

        match screen {
            Screen::Play => {
                let pad_menu = pad_held.iter().any(|p| p == "Start")
                    && pad_held.iter().any(|p| p == "Select")
                    && pad_pressed.iter().any(|p| p == "Start" || p == "Select");
                if has("F1") || pad_menu {
                    screen = Screen::Menu;
                    menu.set_status("");
                } else if has("F12") {
                    screen = Screen::Note(End::Done);
                } else if has("F10") {
                    screen = Screen::Note(End::Declined);
                } else {
                    if has("F3") {
                        paused = !paused;
                    }
                    if has("F5") {
                        s.quick_save();
                        msg.show("Quick saved. F7 comes back here.");
                    }
                    if has("F7") {
                        match s.quick_load() {
                            Ok(true) => msg.show("Back at the quick save."),
                            Ok(false) => msg.show("No quick save yet (F5 makes one)."),
                            Err(e) => msg.show(format!("Quick load failed: {e}")),
                        }
                    }
                    if has("F8") {
                        if restart_asked.is_some_and(|t| t.elapsed() < RESTART_CONFIRM) {
                            restart_asked = None;
                            match s.restart() {
                                Ok(()) => msg.show("Started over from the beginning of the handoff."),
                                Err(e) => msg.show(format!("Could not start over: {e}")),
                            }
                        } else {
                            restart_asked = Some(Instant::now());
                            msg.show("Press F8 again to start over from the beginning of the handoff. Progress since then is lost (the quick save is kept).");
                        }
                    }
                    if !paused {
                        let fast = held.iter().any(|k| k == "Tab");
                        for _ in 0..if fast { 4 } else { 1 } {
                            let (buttons, sticks) = pad_state(s, &b, &held, &pad_held, &pads);
                            s.frame(buttons, sticks);
                        }
                    }
                }
            }
            Screen::Menu => {
                let keys: Vec<&str> = pressed.iter().map(String::as_str).collect();
                let pad: Vec<&str> = pad_pressed.iter().map(String::as_str).collect();
                match menu.tick(&mut b, &keys, &pad, !held.is_empty() || !pad_held.is_empty()) {
                    Outcome::Stay => {}
                    Outcome::Close => screen = Screen::Play,
                    Outcome::Save => match &path {
                        Some(p) => match bindings::save(p, &b) {
                            Ok(()) => menu.set_status(format!("Saved to {}", p.display())),
                            Err(e) => menu.set_status(format!("Not saved: {e}")),
                        },
                        None => menu.set_status("Not saved: no home directory"),
                    },
                }
            }
            Screen::Note(end) => {
                if has("Escape") {
                    screen = Screen::Play;
                } else if has("Enter") || has("NumPadEnter") {
                    return Ok((end, note.trim().to_string()));
                } else {
                    if has("Backspace") {
                        note.pop();
                    }
                    note.extend(typed_now.into_iter().take(2000usize.saturating_sub(note.chars().count())));
                }
            }
        }

        draw_game(&mut buf, s);
        fill(&mut buf, PX, 0, PW, H, PANEL);
        let mut p = Panel { buf: &mut buf, y: 8 };
        match screen {
            Screen::Play => {
                p.wrapped("THE AGENT ASKS:", YELLOW);
                for l in wrap(request, COLS).iter().take(14) {
                    p.line(l, WHITE);
                }
                p.gap();
                for (keys, what) in [
                    ("F12", "Done: hand back, with a note"),
                    ("F10", "Can't do it: tell the agent why"),
                    ("F8", "Start over (press twice)"),
                    ("F5/F7", "Quick save / quick load"),
                    ("F3", "Pause"),
                    ("Tab", "Fast forward (hold)"),
                    ("F1", "Controls (pad: Start+Select)"),
                ] {
                    p.line(&format!("{keys:<6}{what}"), GREY);
                }
                p.gap();
                let mode = match (s.dualshock(), s.analog()) {
                    (false, _) => "digital pad",
                    (true, false) => "DualShock, digital mode",
                    (true, true) => "DualShock, analog mode",
                };
                let n = pads.names().len();
                p.line(&format!("{}frame {}", if paused { "PAUSED  " } else { "" }, s.rel()), if paused { YELLOW } else { GREY });
                p.line(&format!("{mode}; {n} gamepad{}", if n == 1 { "" } else { "s" }), GREY);
                if let Some(text) = msg.current() {
                    p.gap();
                    p.wrapped(text, YELLOW);
                }
            }
            Screen::Menu => {
                for (i, (line, selected)) in menu.lines(&b, &pads.names()).iter().enumerate() {
                    if *selected {
                        fill(p.buf, PX + 4, p.y - 2, PW - 8, 11, SELECTED);
                    }
                    p.wrapped(line, if i == 0 { YELLOW } else { WHITE });
                }
            }
            Screen::Note(end) => {
                let (head, prompt) = match end {
                    End::Done => ("HAND BACK TO THE AGENT", "Anything the agent should know? (optional)"),
                    _ => ("CAN'T DO IT", "Tell the agent why, or what you tried (optional)"),
                };
                p.line(head, YELLOW);
                p.gap();
                p.wrapped(prompt, WHITE);
                p.line("Enter: hand back now", GREY);
                p.line("Esc: keep playing", GREY);
                p.gap();
                p.wrapped(&format!("{note}_"), WHITE);
            }
        }
        win.update_with_buffer(&buf, W, H).map_err(|e| e.to_string())?;

        if live_at.elapsed() >= Duration::from_secs(1) {
            live_at = Instant::now();
            let state = match screen {
                Screen::Play if paused => "paused",
                Screen::Play => "playing",
                Screen::Menu => "controls_menu",
                Screen::Note(_) => "writing_note",
            };
            s.write_live(dir, state);
        }

        // NTSC fields come at 59.94 Hz, PAL at 50.
        let period = Duration::from_secs_f64(if s.m.bus.pal() { 1.0 / 50.0 } else { 1001.0 / 60000.0 });
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > Duration::from_millis(250) {
            // Too slow to keep up (or a debugger stop): do not race to catch up.
            next = now;
        }
    }
}

/// Lines of text down the panel.
struct Panel<'a> {
    buf: &'a mut [u32],
    y: usize,
}

impl Panel<'_> {
    fn line(&mut self, s: &str, c: u32) {
        if self.y + 8 <= H {
            text(self.buf, PX + 8, self.y, s, c);
        }
        self.y += 11;
    }

    fn wrapped(&mut self, s: &str, c: u32) {
        for l in wrap(s, COLS) {
            self.line(&l, c);
        }
    }

    fn gap(&mut self) {
        self.y += 6;
    }
}

/// This frame's pad: buttons from both devices, sticks from the active gamepad.
/// A digital pad (or a DualShock the game has not switched to analog) gets the
/// left stick as the d-pad instead.
fn pad_state(s: &Session, b: &Bindings, keys: &[String], pad: &[String], pads: &Pads) -> (u16, [u8; 4]) {
    let mut buttons = b.buttons(keys.iter().map(String::as_str), pad.iter().map(String::as_str));
    let [lx, ly, rx, ry] = pads.sticks();
    let dz = b.stick_deadzone;
    let sticks = [bindings::stick_byte(lx, dz, false), bindings::stick_byte(ly, dz, true), bindings::stick_byte(rx, dz, false), bindings::stick_byte(ry, dz, true)];
    if s.analog() {
        (buttons, sticks)
    } else {
        buttons |= bindings::stick_dpad(sticks[0], sticks[1]);
        (buttons, [0x80; 4])
    }
}

fn key_name(k: &Key) -> String {
    format!("{k:?}")
}

/// Gamepads through gilrs, merged: any pad's buttons press, the most recently used pad's sticks steer.
struct Pads {
    gilrs: Option<gilrs::Gilrs>,
    error: Option<String>,
    held: Vec<(gilrs::GamepadId, String)>,
    pressed: Vec<String>,
    active: Option<gilrs::GamepadId>,
}

impl Pads {
    fn new() -> Self {
        let (gilrs, error) = match gilrs::Gilrs::new() {
            Ok(g) => (Some(g), None),
            Err(gilrs::Error::NotImplemented(g)) => (Some(g), Some("gamepads are not supported on this platform".to_string())),
            Err(e) => (None, Some(e.to_string())),
        };
        Pads { gilrs, error, held: Vec::new(), pressed: Vec::new(), active: None }
    }

    /// `South`, `DPadUp`...; `code:N` for a button the mapping database does not name.
    fn name(b: gilrs::Button, code: gilrs::ev::Code) -> String {
        match b {
            gilrs::Button::Unknown => format!("code:{}", code.into_u32()),
            b => format!("{b:?}"),
        }
    }

    fn press(&mut self, id: gilrs::GamepadId, name: String) {
        self.active = Some(id);
        if !self.held.iter().any(|(i, n)| *i == id && *n == name) {
            self.held.push((id, name.clone()));
            self.pressed.push(name);
        }
    }

    fn release(&mut self, id: gilrs::GamepadId, name: &str) {
        self.held.retain(|(i, n)| !(*i == id && n == name));
    }

    fn poll(&mut self) {
        use gilrs::{Axis, EventType};
        let Some(g) = &mut self.gilrs else { return };
        let mut events = Vec::new();
        while let Some(ev) = g.next_event() {
            events.push((ev.id, ev.event));
        }
        for (id, event) in events {
            match event {
                EventType::ButtonPressed(b, c) => self.press(id, Self::name(b, c)),
                EventType::ButtonReleased(b, c) => self.release(id, &Self::name(b, c)),
                // Some pads report the d-pad as a hat axis rather than buttons.
                EventType::AxisChanged(axis @ (Axis::DPadX | Axis::DPadY), v, _) => {
                    let (neg, pos) = if axis == Axis::DPadX { ("DPadLeft", "DPadRight") } else { ("DPadDown", "DPadUp") };
                    for (name, on) in [(neg, v < -0.5), (pos, v > 0.5)] {
                        if on {
                            self.press(id, name.into());
                        } else {
                            self.release(id, name);
                        }
                    }
                }
                EventType::AxisChanged(_, v, _) if v.abs() > 0.5 => self.active = Some(id),
                EventType::Disconnected => {
                    self.held.retain(|(i, _)| *i != id);
                    if self.active == Some(id) {
                        self.active = None;
                    }
                }
                _ => {}
            }
        }
    }

    fn held(&self) -> Vec<String> {
        self.held.iter().map(|(_, n)| n.clone()).collect()
    }

    /// Left x/y, right x/y of the active gamepad (-1..1, +y up).
    fn sticks(&self) -> [f32; 4] {
        use gilrs::Axis::*;
        let (Some(g), Some(id)) = (&self.gilrs, self.active) else { return [0.0; 4] };
        let Some(pad) = g.connected_gamepad(id) else { return [0.0; 4] };
        [LeftStickX, LeftStickY, RightStickX, RightStickY].map(|a| pad.value(a))
    }

    fn names(&self) -> Vec<String> {
        self.gilrs.as_ref().map(|g| g.gamepads().map(|(_, p)| p.name().to_string()).collect()).unwrap_or_default()
    }
}

/// The latest message (shown in the panel for a while).
#[derive(Default)]
struct Message {
    text: String,
    until: Option<Instant>,
}

impl Message {
    fn show(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.until = Some(Instant::now() + Duration::from_secs(6));
    }

    fn current(&self) -> Option<&str> {
        self.until.is_some_and(|t| Instant::now() < t).then_some(self.text.as_str())
    }
}

/// The displayed area stretched to the 4:3 game area.
fn draw_game(buf: &mut [u32], s: &Session) {
    let (w, h, rgba) = s.m.bus.gpu.display_rgba();
    let (w, h) = (w as usize, h as usize);
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        fill(buf, 0, 0, GW, GH, 0);
        return;
    }
    for y in 0..GH {
        let row = (y * h / GH) * w;
        for x in 0..GW {
            let p = (row + x * w / GW) * 4;
            buf[y * W + x] = (rgba[p] as u32) << 16 | (rgba[p + 1] as u32) << 8 | rgba[p + 2] as u32;
        }
    }
}

fn fill(buf: &mut [u32], x: usize, y: usize, w: usize, h: usize, c: u32) {
    for yy in y..(y + h).min(H) {
        buf[yy * W + x..yy * W + (x + w).min(W)].fill(c);
    }
}

fn text(buf: &mut [u32], x: usize, y: usize, s: &str, c: u32) {
    use font8x8::UnicodeFonts;
    for (i, ch) in s.chars().enumerate() {
        let gx = x + i * 8;
        if gx + 8 > W || y + 8 > H {
            break;
        }
        let glyph = font8x8::BASIC_FONTS.get(ch).or_else(|| font8x8::BASIC_FONTS.get('?')).unwrap_or([0; 8]);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8 {
                if bits & (1 << col) != 0 {
                    buf[(y + row) * W + gx + col] = c;
                }
            }
        }
    }
}

/// Word-wrap to `cols` characters (long words are split).
fn wrap(s: &str, cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in s.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let mut word = word.to_string();
            while word.chars().count() > cols {
                if !line.is_empty() {
                    out.push(std::mem::take(&mut line));
                }
                let head: String = word.chars().take(cols).collect();
                word = word.chars().skip(cols).collect();
                out.push(head);
            }
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > cols {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&word);
        }
        out.push(line);
    }
    out
}
