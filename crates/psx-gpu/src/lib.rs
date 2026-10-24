//! Software PlayStation GPU.
//!
//! [`Gpu`] consumes the same GP0/GP1 words the hardware does, so three things
//! go through one implementation:
//! - packets captured from an emulator (replay and compare),
//! - native reconstruction code using [`prim`] builders, which encode to GP0 words,
//! - raw VRAM uploads of extracted TIM textures and CLUTs.
//!
//! Optional logging records every completed GP0 command for later diffing.

pub mod capture;
pub mod ot;
pub mod prim;
pub mod raster;
pub mod vram;

use raster::{Raster, Shade, Surface, TexelSource, Vtx};
pub use vram::Vram;

impl Surface for Vram {
    fn get(&self, x: i32, y: i32) -> u16 {
        Vram::get(self, x, y)
    }
    fn set(&mut self, x: i32, y: i32, value: u16) {
        Vram::set(self, x, y, value)
    }
}

/// Texels read from VRAM through a texture page, CLUT and texture window.
#[derive(Clone, Copy, Debug)]
pub struct VramTexture {
    /// Texpage bits 0-8 (base x/y, blend, depth).
    pub texpage: u16,
    pub clut_x: i32,
    pub clut_y: i32,
    /// `(mask_x, mask_y, offset_x, offset_y)` in 8-texel units.
    pub window: (u8, u8, u8, u8),
}

impl TexelSource<Vram> for VramTexture {
    #[inline(always)]
    fn texel(&self, vram: &Vram, u: i32, v: i32) -> u16 {
        let (mx, my, ox, oy) = self.window;
        let u = (u & 0xFF & !((mx as i32) * 8)) | (((ox & mx) as i32) * 8);
        let v = (v & 0xFF & !((my as i32) * 8)) | (((oy & my) as i32) * 8);
        let bx = ((self.texpage & 0xF) as i32) * 64;
        let by = (((self.texpage >> 4) & 1) as i32) * 256;
        match (self.texpage >> 7) & 3 {
            0 => {
                let idx = (vram.get(bx + u / 4, by + v) >> ((u & 3) * 4)) & 0xF;
                vram.get(self.clut_x + idx as i32, self.clut_y)
            }
            1 => {
                let idx = (vram.get(bx + u / 2, by + v) >> ((u & 1) * 8)) & 0xFF;
                vram.get(self.clut_x + idx as i32, self.clut_y)
            }
            _ => vram.get(bx + u, by + v),
        }
    }
}

/// [`VramTexture`] with the colour depth fixed (0 = 4-bit CLUT, 1 = 8-bit
/// CLUT, 2 = 15-bit direct), the page base and window folded into masks and
/// the CLUT copied out of VRAM, so the specialised pixel loops do no per-texel
/// decoding. Only used for primitives that cannot draw over their own CLUT
/// (see `with_page_texture!`), so every texel equals [`VramTexture::texel`].
#[derive(Clone, Copy, Debug)]
struct PageTexture<const DEPTH: u8> {
    u_and: i32,
    u_or: i32,
    v_and: i32,
    v_or: i32,
    base_x: i32,
    base_y: i32,
    clut: [u16; 256],
}

impl<const DEPTH: u8> PageTexture<DEPTH> {
    const CLUT_LEN: i32 = match DEPTH {
        0 => 16,
        1 => 256,
        _ => 0,
    };

    fn new(t: &VramTexture, vram: &Vram) -> Self {
        let (mx, my, ox, oy) = t.window;
        let mut clut = [0; 256];
        for (i, c) in clut.iter_mut().enumerate().take(Self::CLUT_LEN as usize) {
            *c = vram.get(t.clut_x + i as i32, t.clut_y);
        }
        PageTexture {
            u_and: 0xFF & !((mx as i32) * 8),
            u_or: ((ox & mx) as i32) * 8,
            v_and: 0xFF & !((my as i32) * 8),
            v_or: ((oy & my) as i32) * 8,
            base_x: ((t.texpage & 0xF) as i32) * 64,
            base_y: (((t.texpage >> 4) & 1) as i32) * 256,
            clut,
        }
    }

    /// Whether drawing inside `clip` (inclusive, within VRAM) leaves the CLUT
    /// of `t` untouched, so it can be read once per primitive.
    fn clut_outside(t: &VramTexture, clip: (i32, i32, i32, i32)) -> bool {
        let (x0, x1) = (t.clut_x, t.clut_x + Self::CLUT_LEN - 1);
        t.clut_y < clip.1 || t.clut_y > clip.3 || (x1 < 1024 && (x1 < clip.0 || x0 > clip.2))
    }
}

impl<const DEPTH: u8> TexelSource<Vram> for PageTexture<DEPTH> {
    #[inline(always)]
    fn texel(&self, vram: &Vram, u: i32, v: i32) -> u16 {
        // Never negative: u_and <= 0xFF, u_or >= 0.
        let u = (u & self.u_and) | self.u_or;
        let y = self.base_y + ((v & self.v_and) | self.v_or);
        match DEPTH {
            0 => self.clut[((vram.get(self.base_x + (u >> 2), y) >> ((u & 3) * 4)) & 0xF) as usize],
            1 => self.clut[((vram.get(self.base_x + (u >> 1), y) >> ((u & 1) * 8)) & 0xFF) as usize],
            _ => vram.get(self.base_x + u, y),
        }
    }
}

/// Evaluate `$body` with `$t` bound to `$tex` as a [`PageTexture`] of its
/// colour depth, or as the plain [`VramTexture`] when the drawing area `$clip`
/// covers part of the CLUT (the primitive could overwrite entries it reads
/// later).
macro_rules! with_page_texture {
    ($tex:expr, $vram:expr, $clip:expr, |$t:ident| $body:expr) => {{
        let tex: &VramTexture = &$tex;
        let clip = $clip;
        match (tex.texpage >> 7) & 3 {
            0 if PageTexture::<0>::clut_outside(tex, clip) => {
                let $t = &PageTexture::<0>::new(tex, $vram);
                $body
            }
            1 if PageTexture::<1>::clut_outside(tex, clip) => {
                let $t = &PageTexture::<1>::new(tex, $vram);
                $body
            }
            2 | 3 => {
                let $t = &PageTexture::<2>::new(tex, $vram);
                $body
            }
            _ => {
                let $t = tex;
                $body
            }
        }
    }};
}

/// Hardware drops a primitive when any edge it draws spans 1024+ pixels
/// horizontally or 512+ vertically. For a quad these are the edges of its two
/// triangles: the perimeter plus the shared v1-v2 diagonal, but not v0-v3
/// (PCSX-Redux gpu-raster-phase14/23, run on SCPH-1000/5501/7001).
fn too_large(vs: &[Vtx]) -> bool {
    let over = |a: &Vtx, b: &Vtx| (a.x - b.x).abs() >= 1024 || (a.y - b.y).abs() >= 512;
    match vs {
        [a, b] => over(a, b),
        [a, b, c] => over(a, b) || over(b, c) || over(c, a),
        [a, b, c, d] => over(a, b) || over(b, d) || over(d, c) || over(c, a) || over(b, c),
        _ => false,
    }
}

/// Drawing environment set by GP0(E1h..E6h).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DrawState {
    /// E1 bits 0-13: page x/y, blend mode, depth, dither, draw-to-display, texture disable, rect flips.
    pub texpage: u16,
    /// `(mask_x, mask_y, offset_x, offset_y)` in 8-pixel units (E2).
    pub tex_window: (u8, u8, u8, u8),
    /// Inclusive drawing area `(x1, y1, x2, y2)` (E3/E4).
    pub area: (i32, i32, i32, i32),
    /// Drawing offset added to every vertex (E5).
    pub offset: (i32, i32),
    pub set_mask: bool,
    pub check_mask: bool,
    /// Rectangle texture flips (E1 bits 12/13).
    pub rect_flip: (bool, bool),
}

impl DrawState {
    pub fn dither(&self) -> bool {
        self.texpage & 0x200 != 0
    }
    pub fn raster(&self) -> Raster {
        Raster { clip: self.area, check_mask: self.check_mask, set_mask: self.set_mask, skip_parity: None }
    }

    /// Drawing to the displayed area is allowed (E1 bit 10).
    pub fn draw_to_display(&self) -> bool {
        self.texpage & 0x400 != 0
    }

    fn texture(&self, clut: u32) -> VramTexture {
        VramTexture {
            texpage: self.texpage & 0x1FF,
            clut_x: ((clut & 0x3F) * 16) as i32,
            clut_y: ((clut >> 6) & 0x1FF) as i32,
            window: self.tex_window,
        }
    }

    pub(crate) fn set_mask_bit(&self) -> u16 {
        if self.set_mask { 0x8000 } else { 0 }
    }
}

/// Display configuration set by GP1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Display {
    pub enabled: bool,
    pub start: (u16, u16),
    pub h_range: (u16, u16),
    pub v_range: (u16, u16),
    /// Raw GP1(08h) bits.
    pub mode: u32,
}

impl Default for Display {
    fn default() -> Self {
        Display { enabled: false, start: (0, 0), h_range: (0x200, 0xC00), v_range: (0x10, 0x100), mode: 0 }
    }
}

impl Display {
    pub fn width(&self) -> u32 {
        if self.mode & 0x40 != 0 {
            return 368;
        }
        [256, 320, 512, 640][(self.mode & 3) as usize]
    }
    pub fn pal(&self) -> bool {
        self.mode & 0x08 != 0
    }
    pub fn interlaced_480(&self) -> bool {
        self.mode & 0x24 == 0x24
    }
    pub fn rgb24(&self) -> bool {
        self.mode & 0x10 != 0
    }
    /// Visible lines: the vertical display range, doubled for 480-line interlace.
    pub fn height(&self) -> u32 {
        let lines = self.v_range.1.saturating_sub(self.v_range.0) as u32;
        let lines = if lines == 0 { if self.pal() { 256 } else { 240 } } else { lines };
        (if self.interlaced_480() { lines * 2 } else { lines }).min(512)
    }
}

/// What the GPU is in the middle of (Mednafen's `InCmd`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum InCmd {
    #[default]
    None,
    /// GP0(A0h): image words follow through the FIFO.
    FbWrite,
    /// GP0(C0h): GPUREAD returns VRAM.
    FbRead,
    /// A quad's first triangle is drawn; the fourth vertex follows.
    Quad,
    /// A polyline's first segment is drawn; vertices follow until the terminator.
    Pline,
}

/// The VRAM rectangle of a GP0(A0h) write or GP0(C0h) read, and the position in it.
#[derive(Clone, Copy, Debug, Default, serde::Serialize, serde::Deserialize)]
struct FbRw {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    cur_x: u32,
    cur_y: u32,
}

/// A GP0 command's words (Mednafen's `len`; a quad's or polyline's first
/// triangle or segment), the FIFO fill at which it makes GPUSTAT bit 28
/// report full (`fifo_fb_len`), and whether it runs without drawing time
/// (`ss_cmd`).
fn command_info(cc: u8) -> (usize, usize, bool) {
    match cc {
        0x01 => (1, 2, false),
        0x02 => (3, 3, false),
        0x1F => (1, 1, false),
        0x20..=0x3F => {
            let (tex, gouraud) = ((cc >> 2) & 1, (cc >> 4) & 1);
            ((1 + 3 * (1 + tex + gouraud) - gouraud) as usize, 1, false)
        }
        0x40..=0x5F => (3 + ((cc >> 4) & 1) as usize, 1, false),
        0x60..=0x7F => {
            let (tex, sized) = ((cc >> 2) & 1, (cc & 0x18 == 0) as u8);
            ((2 + tex + sized) as usize, (2 | tex | sized) as usize, false)
        }
        0x80..=0x9F => (4, 2, false),
        0xA0..=0xDF => (3, 2, false),
        0xE1 | 0xE2 | 0xE6 => (1, 2, false),
        0xE3..=0xE5 => (1, 1, true),
        _ => (1, 1, true),
    }
}

/// Texel addressing for Mednafen's texture cache (`GetTexel`), from the
/// texture page and window.
#[derive(Clone, Copy)]
struct TexAddr {
    depth: u32,
    u_and: u32,
    u_add: u32,
    v_and: u32,
    v_add: u32,
}

impl TexAddr {
    fn new(st: &DrawState) -> Self {
        let tp = st.texpage as u32;
        let depth = ((tp >> 7) & 3).min(2);
        let (tww, twh, twx, twy) =
            (st.tex_window.0 as u32, st.tex_window.1 as u32, st.tex_window.2 as u32, st.tex_window.3 as u32);
        TexAddr {
            depth,
            u_and: !(tww << 3),
            u_add: ((twx & tww) << 3) + (((tp & 0xF) * 64) << (2 - depth)),
            v_and: !(twh << 3),
            v_add: ((twy & twh) << 3) + (tp & 0x10) * 16,
        }
    }

    /// Drawing time of a texel fetch: 4 when it misses the cache, which
    /// holds 256 blocks of 4 halfwords (64×64 texels at 4 bits, 64×32 at 8
    /// bits, 32×32 at 15 bits).
    fn fetch(&self, tags: &mut [u32], u: u32, v: u32) -> i32 {
        self.fetch_block(tags, self.block(u, v))
    }

    /// The cache block (its first halfword's VRAM address) holding texel `(u, v)`.
    #[inline(always)]
    fn block(&self, u: u32, v: u32) -> u32 {
        let u_ext = (u & self.u_and).wrapping_add(self.u_add);
        let x = (u_ext >> (2 - self.depth)) & 1023;
        let y = (v & self.v_and).wrapping_add(self.v_add);
        y.wrapping_mul(1024).wrapping_add(x) & !3
    }

    #[inline(always)]
    fn fetch_block(&self, tags: &mut [u32], block: u32) -> i32 {
        let i = if self.depth == 0 {
            ((block >> 2) & 3) | ((block >> 8) & 0xFC)
        } else {
            ((block >> 2) & 7) | ((block >> 7) & 0xF8)
        };
        let tag = &mut tags[i as usize];
        if *tag != block {
            *tag = block;
            4
        } else {
            0
        }
    }
}

/// A software GPU fed GP0/GP1 words, with Mednafen's command processing
/// (`gpu.cpp`): GP0 words go through a FIFO, and a command runs when it is
/// complete and, in timed mode, when drawing time is left.
///
/// Drawing time ([`Gpu::draw_time`], Mednafen's `DrawTimeAvail`) is in
/// half system clocks: [`Gpu::credit`] adds two per clock up to 256, and each
/// command takes Mednafen's estimate of its cost (per triangle, span, sprite
/// row, line step, fill row, copied pixel, CLUT load and texture cache miss).
/// While it is negative, drawing commands wait in the FIFO, GPUSTAT reports
/// busy and, once the FIFO fills, the GPU's DMA waits. Untimed (the default,
/// for replays and tools), commands run as soon as they are complete.
///
/// The pixels are psx-gpu's own (`raster`); only the time is Mednafen's.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Gpu {
    pub vram: Vram,
    pub state: DrawState,
    pub display: Display,
    /// Mednafen's `BlitterFIFO`: up to 16 words on the console, 32 here as in
    /// Mednafen (a command's last words may arrive when 16 are queued).
    fifo: std::collections::VecDeque<u32>,
    in_cmd: InCmd,
    in_cmd_cc: u8,
    /// A quad's first triangle (drawing offset added) and its CLUT.
    quad: [Vtx; 3],
    quad_clut: u32,
    /// A polyline's last vertex (before the drawing offset).
    pline_prev: Vtx,
    fb: FbRw,
    /// GPUREAD outside a VRAM read: the last GP1(10h) answer.
    data_read: u32,
    data_read_ex: u32,
    /// Drawing time left, in half system clocks (Mednafen's `DrawTimeAvail`).
    pub draw_time: i32,
    /// Commands wait for drawing time (the console core); see [`Gpu`].
    pub timed: bool,
    /// Texture cache tags (block addresses, `!0` when invalid).
    tex_tags: Vec<u32>,
    /// The CLUT cached, with its depth (`!0` when invalid).
    clut_vb: u32,
    /// GP0(1Fh) requested an interrupt; GP1(02h) acknowledges it.
    pub irq: bool,
    /// GP1(04h): DMA direction.
    pub dma_control: u32,
    tex_disable: bool,
    tex_disable_allow: bool,
    /// GP0(E3h)/GP0(E4h) parameters, for GP1(10h).
    clip_raw: (u32, u32),
    /// When `Some`, every GP0 command run is appended (a quad's fourth vertex,
    /// a polyline's vertices and terminator, and A0h's image data with it).
    #[serde(skip)]
    pub log: Option<Vec<Vec<u32>>>,
    /// GP0 commands that touch VRAM (fills, primitives, copies, CPU uploads)
    /// since power-on or a state restore. Analysis only, not saved.
    #[serde(skip)]
    pub draws: u64,
    /// The field being read out for display, latched at vblank by the video
    /// timing (`psx-core`): see [`Gpu::skip_parity`].
    #[serde(default)]
    pub readout_field: bool,
}

impl Default for Gpu {
    fn default() -> Self {
        Gpu {
            vram: Vram::default(),
            state: DrawState::default(),
            display: Display::default(),
            fifo: Default::default(),
            in_cmd: InCmd::None,
            in_cmd_cc: 0,
            quad: [Vtx::default(); 3],
            quad_clut: 0,
            pline_prev: Vtx::default(),
            fb: FbRw::default(),
            data_read: 0,
            data_read_ex: 0,
            draw_time: 0,
            timed: false,
            tex_tags: vec![!0; 256],
            clut_vb: !0,
            irq: false,
            dma_control: 0,
            tex_disable: false,
            tex_disable_allow: false,
            clip_raw: (0, 0),
            log: None,
            draws: 0,
            readout_field: false,
        }
    }
}

fn sext11(v: u32) -> i32 {
    ((v as i32) << 21) >> 21
}

fn color(w: u32) -> (i32, i32, i32) {
    ((w & 0xFF) as i32, ((w >> 8) & 0xFF) as i32, ((w >> 16) & 0xFF) as i32)
}

/// Number of words for a fixed-length GP0 command, `None` for polylines.
pub fn command_len(cmd: u32) -> Option<usize> {
    let op = (cmd >> 24) as u8;
    Some(match op {
        0x02 => 3,
        0x20..=0x3F => {
            let verts = if op & 0x08 != 0 { 4 } else { 3 };
            let gouraud = op & 0x10 != 0;
            let tex = op & 0x04 != 0;
            1 + verts * (1 + tex as usize) + if gouraud { verts - 1 } else { 0 }
        }
        0x40..=0x5F => {
            if op & 0x08 != 0 {
                return None;
            }
            if op & 0x10 != 0 { 4 } else { 3 }
        }
        0x60..=0x7F => 2 + ((op & 0x04 != 0) as usize) + ((op & 0x18 == 0) as usize),
        0x80..=0x9F => 4,
        0xA0..=0xDF => 3,
        _ => 1,
    })
}

impl Gpu {
    pub fn new() -> Self {
        Self::default()
    }

    /// A GPU whose commands wait for drawing time, as the console's (see [`Gpu`]).
    pub fn timed() -> Self {
        Gpu { timed: true, ..Self::default() }
    }

    pub fn gp0_all(&mut self, words: &[u32]) {
        for &w in words {
            self.gp0(w);
        }
    }

    /// A GP0 word (Mednafen's `WriteCB`): queued, then the FIFO runs once.
    /// A word arriving with 16 queued is lost, unless the command at the head
    /// still needs it.
    pub fn gp0(&mut self, word: u32) {
        let n = self.fifo.len();
        if n >= 0x10 && (self.in_cmd != InCmd::None || n - 0x10 >= command_info((self.fifo[0] >> 24) as u8).1) {
            return;
        }
        self.fifo.push_back(word);
        self.process_fifo();
    }

    /// Credit `clocks` system clocks of drawing time and run the FIFO once
    /// (Mednafen's `GPU_Update`).
    /// Nothing queued and drawing time full: crediting more time changes
    /// nothing, however it is split.
    pub fn idle(&self) -> bool {
        self.fifo.is_empty() && self.draw_time == 256
    }

    pub fn credit(&mut self, clocks: u64) {
        if clocks == 0 {
            return;
        }
        self.draw_time = (self.draw_time as i64 + 2 * clocks as i64).min(256) as i32;
        self.process_fifo();
    }

    fn blocked(&self) -> bool {
        self.timed && self.draw_time < 0
    }

    /// GPUSTAT bit 28, which also lets the GPU's DMA move a word (Mednafen's
    /// `GPU_CalcFIFOReadyBit`, `GPU_DMACanWrite`).
    pub fn ready(&self) -> bool {
        match self.in_cmd {
            InCmd::Pline | InCmd::Quad => false,
            _ if self.fifo.is_empty() => true,
            InCmd::FbRead | InCmd::FbWrite => false,
            InCmd::None => self.fifo.len() < command_info((self.fifo[0] >> 24) as u8).1,
        }
    }

    /// GPUSTAT bits the GPU sets: 15 (texture disable), 24 (interrupt), 25
    /// (DMA request), 26 (idle), 27 (VRAM read ready), 28 (ready) and 29-30
    /// (DMA direction).
    pub fn status_bits(&self) -> u32 {
        let mut v = (self.tex_disable as u32) << 15 | (self.irq as u32) << 24 | self.dma_control << 29;
        v |= ((self.dma_control & 2) != 0) as u32 * (1 << 25);
        if self.in_cmd == InCmd::None && !self.blocked() && self.fifo.is_empty() {
            v |= 1 << 26;
        }
        if self.in_cmd == InCmd::FbRead {
            v |= 1 << 27;
        }
        v | (self.ready() as u32) << 28
    }

    /// GPUREAD (Mednafen's `ReadData`): VRAM during a GP0(C0h) read,
    /// otherwise the last GP1(10h) answer.
    pub fn read_data(&mut self) -> u32 {
        if self.in_cmd != InCmd::FbRead {
            return self.data_read;
        }
        let f = &mut self.fb;
        let mut ex = 0;
        for i in 0..2 {
            ex |= (self.vram.get(f.cur_x as i32, f.cur_y as i32) as u32) << (i * 16);
            f.cur_x += 1;
            if f.cur_x == f.x + f.w {
                if f.cur_y + 1 == f.y + f.h {
                    self.in_cmd = InCmd::None;
                } else {
                    f.cur_y += 1;
                    f.cur_x = f.x;
                }
            }
        }
        self.data_read_ex = ex;
        ex
    }

    /// Next GPUREAD word of a VRAM→CPU transfer, `None` when none is pending.
    pub fn gpuread(&mut self) -> Option<u32> {
        (self.in_cmd == InCmd::FbRead).then(|| self.read_data())
    }

    fn log_more(&mut self, words: &[u32]) {
        if let Some(entry) = self.log.as_mut().and_then(|l| l.last_mut()) {
            entry.extend_from_slice(words);
        }
    }

    /// Run at most one command, or one word of image data (Mednafen's `ProcessFIFO`).
    fn process_fifo(&mut self) {
        let Some(&head) = self.fifo.front() else { return };
        match self.in_cmd {
            InCmd::None => {}
            InCmd::FbRead => return,
            InCmd::FbWrite => {
                let w = self.fifo.pop_front().unwrap();
                self.log_more(&[w]);
                self.fb_write_word(w);
                return;
            }
            InCmd::Quad | InCmd::Pline => {
                if self.blocked() {
                    return;
                }
                let cc = self.in_cmd_cc;
                if self.in_cmd == InCmd::Pline && head & 0xF000_F000 == 0x5000_5000 {
                    self.fifo.pop_front();
                    self.log_more(&[head]);
                    self.in_cmd = InCmd::None;
                    return;
                }
                let vl = if self.in_cmd == InCmd::Quad {
                    1 + ((cc >> 2) & 1) as usize + ((cc >> 4) & 1) as usize
                } else {
                    1 + ((cc >> 4) & 1) as usize
                };
                if self.fifo.len() >= vl {
                    let cb: Vec<u32> = self.fifo.drain(..vl).collect();
                    self.log_more(&cb);
                    if self.in_cmd == InCmd::Quad { self.polygon(cc, &cb) } else { self.line(cc, &cb) }
                }
                return;
            }
        }
        let cc = (head >> 24) as u8;
        let (len, _, ss) = command_info(cc);
        if self.blocked() && !ss {
            return;
        }
        if self.fifo.len() < len {
            return;
        }
        let mut cb = [0u32; 16];
        for (d, s) in cb.iter_mut().zip(self.fifo.drain(..len)) {
            *d = s;
        }
        let cb = &cb[..len];
        if let Some(log) = &mut self.log {
            log.push(cb.to_vec());
        }
        if !ss {
            self.draw_time -= 2;
        }
        if (0x20..=0x3F).contains(&cc) && cc & 4 != 0 {
            // Textured polygons set the page (bits 0-8 and 11) from vertex 1.
            let pg = cb[4 + ((cc as usize >> 4) & 1)] >> 16;
            self.set_tpage(pg);
            self.state.texpage = (self.state.texpage & !0x9FF) | (pg as u16 & 0x9FF);
        }
        self.execute(cc, cb);
    }

    fn execute(&mut self, cc: u8, p: &[u32]) {
        if matches!(cc, 0x02 | 0x20..=0xBF) {
            self.draws += 1;
        }
        match cc {
            0x01 => self.invalidate_cache(),
            0x02 => self.fill(p),
            0x1F => self.irq = true,
            0x20..=0x3F => self.polygon(cc, p),
            0x40..=0x5F => self.line(cc, p),
            0x60..=0x7F => self.rectangle(p),
            0x80..=0x9F => self.copy(p),
            0xA0..=0xBF => {
                self.fb = self.fb_rect(p, (p[2] >> 16) & 0x1FF);
                if self.fb.h == 0 {
                    self.fb.h = 0x200;
                }
                self.invalidate_tex_cache();
                self.in_cmd = InCmd::FbWrite;
            }
            0xC0..=0xDF => {
                let h = (p[2] >> 16) & 0x3FF;
                self.fb = self.fb_rect(p, if h > 0x200 { h & 0x1FF } else { h });
                self.invalidate_tex_cache();
                if self.fb.h != 0 {
                    self.in_cmd = InCmd::FbRead;
                }
            }
            0xE1 => {
                self.set_tpage(p[0]);
                self.state.texpage = (p[0] & 0x3FFF) as u16;
                self.state.rect_flip = (p[0] & 0x1000 != 0, p[0] & 0x2000 != 0);
            }
            0xE2 => {
                let w = p[0];
                self.state.tex_window =
                    ((w & 31) as u8, ((w >> 5) & 31) as u8, ((w >> 10) & 31) as u8, ((w >> 15) & 31) as u8);
            }
            0xE3 => {
                self.clip_raw.0 = p[0] & 0xF_FFFF;
                self.state.area.0 = (p[0] & 0x3FF) as i32;
                self.state.area.1 = ((p[0] >> 10) & 0x1FF) as i32;
            }
            0xE4 => {
                self.clip_raw.1 = p[0] & 0xF_FFFF;
                self.state.area.2 = (p[0] & 0x3FF) as i32;
                self.state.area.3 = ((p[0] >> 10) & 0x1FF) as i32;
            }
            0xE5 => self.state.offset = (sext11(p[0]), sext11(p[0] >> 11)),
            0xE6 => {
                self.state.set_mask = p[0] & 1 != 0;
                self.state.check_mask = p[0] & 2 != 0;
            }
            _ => {}
        }
    }

    /// A VRAM rectangle from GP0(A0h)/GP0(C0h) parameters, with height `h`.
    fn fb_rect(&self, p: &[u32], h: u32) -> FbRw {
        let (x, y) = (p[1] & 0x3FF, (p[1] >> 16) & 0x3FF);
        let w = match p[2] & 0x3FF {
            0 => 0x400,
            w => w,
        };
        FbRw { x, y, w, h, cur_x: x, cur_y: y }
    }

    fn fb_write_word(&mut self, mut data: u32) {
        for _ in 0..2 {
            let f = self.fb;
            let (x, y) = (f.cur_x as i32, f.cur_y as i32);
            if !(self.state.check_mask && self.vram.get(x, y) & 0x8000 != 0) {
                self.vram.set(x, y, data as u16 | self.state.set_mask_bit());
            }
            self.fb.cur_x += 1;
            if self.fb.cur_x == f.x + f.w {
                self.fb.cur_x = f.x;
                self.fb.cur_y += 1;
                if self.fb.cur_y == f.y + f.h {
                    self.in_cmd = InCmd::None;
                    break;
                }
            }
            data >>= 16;
        }
    }

    fn invalidate_tex_cache(&mut self) {
        self.tex_tags.fill(!0);
    }

    fn invalidate_cache(&mut self) {
        self.clut_vb = !0;
        self.invalidate_tex_cache();
    }

    /// Mednafen's `SetTPage`: a new page or a change between CLUT and direct
    /// colour invalidates the texture cache.
    fn set_tpage(&mut self, w: u32) {
        let old = self.state.texpage as u32;
        if ((w >> 7) & 3 == 0) != ((old >> 7) & 3 == 0) || (w & 0x1F) != (old & 0x1F) {
            self.invalidate_tex_cache();
        }
        if self.tex_disable_allow {
            let disable = (w >> 11) & 1 != 0;
            if disable != self.tex_disable {
                self.invalidate_tex_cache();
            }
            self.tex_disable = disable;
        }
    }

    /// Drawing time of loading a CLUT (Mednafen's `Update_CLUT_Cache`): one
    /// per entry, unless it is the one cached.
    fn clut_time(&mut self, raw: u32) -> i32 {
        let depth = ((self.state.texpage as u32 >> 7) & 3).min(2);
        if depth < 2 {
            let vb = (raw & 0x7FFF) | (depth << 16);
            if self.clut_vb != vb {
                self.clut_vb = vb;
                return if depth == 1 { 256 } else { 16 };
            }
        }
        0
    }

    /// Vertex coordinates as the GPU parses them: 11-bit signed.
    fn vertex(&self, w: u32) -> (i32, i32) {
        (sext11(w), sext11(w >> 16))
    }

    /// Add the drawing offset to a line's vertices. Positions wrap
    /// to 11 bits signed after the add (PCSX-Redux gpu-raster-phase23 O3: vertex
    /// 1500 with offset -1000 draws at 500, on SCPH-1000/5501/7001), but the
    /// primitive wraps as a whole, as on a 2048-wide ring: its leftmost (topmost)
    /// vertex wraps and the others keep their distance from it. The size check
    /// has already bounded those distances below 1024 (512), so a primitive that
    /// crosses x=1023 just runs off the drawing area instead of splitting into a
    /// screen-wide sliver (Crash Bandicoot's beach, drawn at offset 768).
    fn offset(&self, vs: &mut [Vtx]) {
        let wrap = |p: i32, off: i32| sext11((p + off) as u32) - p;
        let (min_x, min_y) = (vs.iter().map(|v| v.x).min().unwrap_or(0), vs.iter().map(|v| v.y).min().unwrap_or(0));
        let (dx, dy) = (wrap(min_x, self.state.offset.0), wrap(min_y, self.state.offset.1));
        for v in vs {
            v.x += dx;
            v.y += dy;
        }
    }

    /// A triangle, or one half of a quad (Mednafen's `Command_DrawPolygon`):
    /// a quad's first three vertices draw (v0,v1,v2), and its fourth, arriving
    /// later, draws (v1,v2,v3).
    fn polygon(&mut self, cc: u8, cb: &[u32]) {
        let (gouraud, textured, quad) = (cc & 0x10 != 0, cc & 0x04 != 0, cc & 0x08 != 0);
        let second = quad && self.in_cmd == InCmd::Quad;
        self.draw_time -= if second { 28 + 18 } else { 64 + 18 };
        self.draw_time -= match (gouraud, textured) {
            (true, true) => 150 * 3,
            (true, false) => 96 * 3,
            (false, true) => 60 * 3,
            _ => 0,
        };
        let mut vs = [Vtx::default(); 3];
        let mut clut = self.quad_clut;
        let first = if second {
            vs[0] = self.quad[1];
            vs[1] = self.quad[2];
            2
        } else {
            0
        };
        let mut i = 0;
        for k in first..3 {
            let (r, g, b) = if k == 0 || gouraud {
                i += 1;
                color(cb[i - 1])
            } else {
                (vs[0].r, vs[0].g, vs[0].b)
            };
            let (x, y) = self.vertex(cb[i]);
            i += 1;
            // Mednafen: the offset is added without wrapping (rows wrap to 11
            // bits as they are drawn), and each triangle checks its own size.
            vs[k] = Vtx { x: x + self.state.offset.0, y: y + self.state.offset.1, r, g, b, u: 0, v: 0 };
            if textured {
                vs[k].u = (cb[i] & 0xFF) as i32;
                vs[k].v = ((cb[i] >> 8) & 0xFF) as i32;
                if k == 0 {
                    clut = cb[i] >> 16;
                    self.draw_time -= self.clut_time(clut);
                }
                i += 1;
            }
        }
        if quad {
            if second {
                self.in_cmd = InCmd::None;
            } else {
                self.in_cmd = InCmd::Quad;
                self.in_cmd_cc = cc;
                self.quad = vs;
                self.quad_clut = clut;
            }
        }
        let sh = Shade {
            textured,
            raw: cc & 0x01 != 0,
            semi: cc & 0x02 != 0,
            gouraud,
            blend: ((self.state.texpage >> 5) & 3) as u8,
            dither: self.state.dither() && (gouraud || (textured && cc & 0x01 == 0)),
        };
        let (r, tex) = (self.raster(), self.state.texture(clut));
        if let Some(tri) = self.timed.then(|| raster::Tri::new(vs, gouraud, textured)).flatten() {
            let slow = sh.semi || r.check_mask;
            let span = |w: i32| {
                if gouraud || textured {
                    w * 2
                } else if slow {
                    w + ((w + 1) >> 1)
                } else {
                    w
                }
            };
            let t = if textured {
                let (a, tags) = (TexAddr::new(&self.state), &mut self.tex_tags);
                // A texel in the block the last one was in hits the cache.
                let mut last = !0;
                tri.time(
                    &r,
                    span,
                    Some(|u, v| {
                        let b = a.block(u, v);
                        if b == last {
                            return 0;
                        }
                        last = b;
                        a.fetch_block(tags, b)
                    }),
                )
            } else {
                tri.time(&r, span, None::<fn(u32, u32) -> i32>)
            };
            self.draw_time -= t;
        }
        #[cfg(test)]
        if tests::REFERENCE.get() {
            raster::reference::triangle(&mut self.vram, &tex, &r, &sh, vs);
            return;
        }
        let vram = &mut self.vram;
        if !textured {
            raster::triangle(vram, &raster::NoTexture, &r, &sh, vs);
        } else {
            with_page_texture!(tex, vram, r.clip, |t| raster::triangle(vram, t, &r, &sh, vs));
        }
    }

    /// A line, or a polyline's segment (Mednafen's `Command_DrawLine`).
    fn line(&mut self, cc: u8, cb: &[u32]) {
        let (gouraud, poly) = (cc & 0x10 != 0, cc & 0x08 != 0);
        self.draw_time -= 16;
        let mut i = 0;
        let a = if poly && self.in_cmd == InCmd::Pline {
            self.pline_prev
        } else {
            let c = color(cb[0]);
            let (x, y) = self.vertex(cb[1]);
            i = 2;
            Vtx { x, y, r: c.0, g: c.1, b: c.2, u: 0, v: 0 }
        };
        let c = if gouraud {
            i += 1;
            color(cb[i - 1])
        } else {
            (a.r, a.g, a.b)
        };
        let (x, y) = self.vertex(cb[i]);
        let b = Vtx { x, y, r: c.0, g: c.1, b: c.2, u: 0, v: 0 };
        if poly {
            self.pline_prev = b;
            if self.in_cmd != InCmd::Pline {
                self.in_cmd = InCmd::Pline;
                self.in_cmd_cc = cc;
            }
        }
        // Mednafen's time: 2 per step along the longer axis.
        let (dx, dy) = ((b.x - a.x).abs(), (b.y - a.y).abs());
        if dx < 1024 && dy < 512 {
            self.draw_time -= dx.max(dy) * 2;
        }
        if too_large(&[a, b]) {
            return;
        }
        let sh = Shade {
            semi: cc & 0x02 != 0,
            gouraud,
            blend: ((self.state.texpage >> 5) & 3) as u8,
            // psx-spx: lines are dithered whenever dithering is enabled, mono or gouraud.
            dither: self.state.dither(),
            ..Shade::default()
        };
        let mut ab = [a, b];
        self.offset(&mut ab);
        let r = self.raster();
        raster::line(&mut self.vram, &r, &sh, ab[0], ab[1]);
    }

    fn rectangle(&mut self, p: &[u32]) {
        let op = (p[0] >> 24) as u8;
        let textured = op & 0x04 != 0;
        self.draw_time -= 16;
        let (r, g, b) = color(p[0]);
        let (x, y) = self.vertex(p[1]);
        let mut i = 2;
        let (mut u, mut v, mut clut) = (0, 0, 0);
        if textured {
            u = (p[i] & 0xFF) as i32;
            v = ((p[i] >> 8) & 0xFF) as i32;
            clut = p[i] >> 16;
            self.draw_time -= self.clut_time(clut);
            i += 1;
        }
        let (w, h) = match (op >> 3) & 3 {
            0 => ((p[i] & 0x3FF) as i32, ((p[i] >> 16) & 0x1FF) as i32),
            1 => (1, 1),
            2 => (8, 8),
            _ => (16, 16),
        };
        // Mednafen: the position wraps to 11 bits after the offset is added.
        let (x, y) = (sext11((x + self.state.offset.0) as u32), sext11((y + self.state.offset.1) as u32));
        let sh = Shade {
            textured,
            raw: op & 0x01 != 0,
            semi: op & 0x02 != 0,
            blend: ((self.state.texpage >> 5) & 3) as u8,
            ..Shade::default()
        };
        let (ras, tex) = (self.raster(), self.state.texture(clut));
        let origin = Vtx { x, y, r, g, b, u, v };
        let flip = self.state.rect_flip;
        if self.timed {
            self.draw_time -= self.sprite_time(&ras, &sh, origin, w, h);
        }
        #[cfg(test)]
        if tests::REFERENCE.get() {
            raster::reference::rect(&mut self.vram, &tex, &ras, &sh, origin, w, h, flip);
            return;
        }
        let vram = &mut self.vram;
        if !textured {
            raster::rect(vram, &raster::NoTexture, &ras, &sh, origin, w, h, flip);
        } else {
            with_page_texture!(tex, vram, ras.clip, |t| raster::rect(vram, t, &ras, &sh, origin, w, h, flip));
        }
    }

    /// Mednafen's drawing time for a sprite (`DrawSprite`): per row drawn,
    /// its width (and half again, rounded out to pixel pairs, when it blends
    /// or tests the mask), and each texel fetch's cache misses.
    fn sprite_time(&mut self, ras: &Raster, sh: &Shade, o: Vtx, w: i32, h: i32) -> i32 {
        let (cx0, cy0, cx1, cy1) = ras.clip;
        let (mut xs, mut ys) = (o.x, o.y);
        let (xb, yb) = ((o.x + w).min(cx1 + 1), (o.y + h).min(cy1 + 1));
        let (mut u, mut v) = (o.u as u8, o.v as u8);
        let (fx, fy) = self.state.rect_flip;
        let (du, dv) = (if fx { 0xFF } else { 1 }, if fy { 0xFF } else { 1 });
        if fx {
            u |= 1;
        }
        if xs < cx0 {
            u = u.wrapping_add(((cx0 - xs) as u8).wrapping_mul(du));
            xs = cx0;
        }
        if ys < cy0 {
            v = v.wrapping_add(((cy0 - ys) as u8).wrapping_mul(dv));
            ys = cy0;
        }
        let slow = sh.semi || ras.check_mask;
        let a = TexAddr::new(&self.state);
        let mut t = 0;
        for y in ys..yb {
            if ras.skip_parity != Some(y & 1) {
                if xb > xs {
                    t += xb - xs;
                    if slow {
                        t += (((xb + 1) & !1) - (xs & !1)) >> 1;
                    }
                }
                if sh.textured {
                    let mut ur = u;
                    for _ in xs..xb {
                        t += a.fetch(&mut self.tex_tags, ur as u32, v as u32);
                        ur = ur.wrapping_add(du);
                    }
                }
            }
            v = v.wrapping_add(dv);
        }
        t
    }

    /// The drawing state's raster, with the rows the GPU skips: in 480-line
    /// interlace, unless drawing to the display is allowed, the lines of the
    /// field being displayed (Mednafen's `LineSkipTest`).
    pub fn raster(&self) -> Raster {
        Raster { skip_parity: self.skip_parity(), ..self.state.raster() }
    }

    pub fn skip_parity(&self) -> Option<i32> {
        (self.display.interlaced_480() && !self.state.draw_to_display())
            .then(|| ((self.display.start.1 as i32) + self.readout_field as i32) & 1)
    }

    fn fill(&mut self, p: &[u32]) {
        let (r, g, b) = color(p[0]);
        let value = vram::rgb8_to_rgb15(r as u8, g as u8, b as u8);
        let x = (p[1] & 0x3F0) as i32;
        let y = ((p[1] >> 16) & 0x1FF) as i32;
        let w = (((p[2] & 0x3FF) + 0xF) & !0xF) as i32;
        let h = ((p[2] >> 16) & 0x1FF) as i32;
        let skip = self.skip_parity();
        self.draw_time -= 46;
        for row in (0..h).filter(|row| skip != Some((y + row) & 1)) {
            self.draw_time -= (w >> 3) + 9;
            for col in 0..w {
                self.vram.set(x + col, y + row, value);
            }
        }
    }

    fn copy(&mut self, p: &[u32]) {
        let (sx, sy) = ((p[1] & 0x3FF) as i32, ((p[1] >> 16) & 0x1FF) as i32);
        let (dx, dy) = ((p[2] & 0x3FF) as i32, ((p[2] >> 16) & 0x1FF) as i32);
        let w = (((p[3] & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1) as i32;
        let h = ((((p[3] >> 16) & 0xFFFF).wrapping_sub(1) & 0x1FF) + 1) as i32;
        self.invalidate_tex_cache();
        self.draw_time -= w * h * 2;
        for row in 0..h {
            let line: Vec<u16> = (0..w).map(|c| self.vram.get(sx + c, sy + row)).collect();
            for (c, v) in line.into_iter().enumerate() {
                let (x, y) = (dx + c as i32, dy + row);
                if self.state.check_mask && self.vram.get(x, y) & 0x8000 != 0 {
                    continue;
                }
                self.vram.set(x, y, v | self.state.set_mask_bit());
            }
        }
    }

    /// GP1(00h) (Mednafen's `SoftReset`).
    fn soft_reset(&mut self) {
        self.irq = false;
        self.invalidate_cache();
        self.dma_control = 0;
        if self.draw_time < 0 {
            self.draw_time = 0;
        }
        self.fifo.clear();
        self.data_read_ex = 0;
        self.in_cmd = InCmd::None;
        self.state = DrawState::default();
        self.display = Display::default();
        self.tex_disable = false;
        self.tex_disable_allow = false;
        self.clip_raw = (0, 0);
    }

    pub fn gp1(&mut self, word: u32) {
        let arg = word & 0x00FF_FFFF;
        match word >> 24 {
            0x00 => self.soft_reset(),
            0x01 => {
                if self.draw_time < 0 {
                    self.draw_time = 0;
                }
                self.fifo.clear();
                self.in_cmd = InCmd::None;
            }
            0x02 => self.irq = false,
            0x03 => self.display.enabled = arg & 1 == 0,
            0x04 => self.dma_control = arg & 3,
            0x05 => self.display.start = ((arg & 0x3FE) as u16, ((arg >> 10) & 0x1FF) as u16),
            0x06 => self.display.h_range = ((arg & 0xFFF) as u16, ((arg >> 12) & 0xFFF) as u16),
            0x07 => self.display.v_range = ((arg & 0x3FF) as u16, ((arg >> 10) & 0x3FF) as u16),
            0x08 => self.display.mode = arg & 0xFF,
            0x09 => self.tex_disable_allow = arg & 1 != 0,
            0x10 => {
                // GPU info; other indices leave GPUREAD unchanged.
                let ex = self.data_read_ex;
                let (tww, twh, twx, twy) = self.state.tex_window;
                let (ox, oy) = self.state.offset;
                let v = match arg & 0xF {
                    2 => (ex & 0xFFF0_0000) | tww as u32 | (twh as u32) << 5 | (twx as u32) << 10 | (twy as u32) << 15,
                    3 => (ex & 0xFFF0_0000) | self.clip_raw.0,
                    4 => (ex & 0xFFF0_0000) | self.clip_raw.1,
                    5 => (ex & 0xFFC0_0000) | (ox as u32 & 2047) | ((oy as u32 & 2047) << 11),
                    7 => 2,
                    8 => 0,
                    _ => return,
                };
                self.data_read_ex = v;
                self.data_read = v;
            }
            _ => {}
        }
    }

    /// Displayed image as RGBA8 `(width, height, pixels)` from the display start.
    /// Horizontal range cropping is not applied; the full mode width is returned.
    pub fn display_rgba(&self) -> (u32, u32, Vec<u8>) {
        let (w, h) = (self.display.width(), self.display.height());
        let (x, y) = (self.display.start.0 as i32, self.display.start.1 as i32);
        let rgba = if self.display.rgb24() {
            self.vram.rgb24(x, y, w, h).chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect()
        } else {
            let mut px = self.vram.rgba15(x, y, w, h);
            px.chunks_mut(4).for_each(|c| c[3] = 255);
            px
        };
        (w, h, rgba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        /// Draw polygons and rectangles with `raster::reference` (per-pixel `plot`).
        pub(crate) static REFERENCE: Cell<bool> = const { Cell::new(false) };
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u32 {
            // xorshift64*
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
        }
        fn below(&mut self, n: u32) -> u32 {
            self.next() % n
        }
    }

    fn timed_with_area() -> Gpu {
        let mut g = Gpu::timed();
        g.gp0_all(&[0xE300_0000, 0xE400_0000 | 1023 | (511 << 10)]);
        g
    }

    /// A fill costs Mednafen's estimate (2 + 46 + per row (w / 8) + 9);
    /// meanwhile GPUSTAT reports busy and the next primitive waits in the
    /// FIFO until enough drawing time is credited.
    #[test]
    fn drawing_time_holds_commands_in_the_fifo() {
        let mut g = timed_with_area();
        g.gp0_all(&[0x0200_00FF, 0, 64 | (10 << 16)]);
        assert_eq!(g.draw_time, -(2 + 46 + 10 * (8 + 9)));
        assert_eq!(g.status_bits() & (1 << 26), 0, "busy");
        g.gp0_all(&[0x6000_FF00, 200 | (200 << 16), 4 | (4 << 16)]);
        assert_eq!(g.vram.get(201, 201), 0, "the sprite waits");
        g.credit(108);
        assert_eq!(g.vram.get(201, 201), 0);
        g.credit(1);
        assert_ne!(g.vram.get(201, 201), 0, "drawn once the time is back to 0");
        assert_eq!(g.draw_time, -(2 + 16 + 4 * 4));
        g.credit(1000);
        assert_eq!(g.draw_time, 256, "credit is capped");
        assert_ne!(g.status_bits() & (1 << 26), 0, "idle");
    }

    /// A quad's fourth vertex is a separate step: until it arrives the FIFO
    /// isn't ready (GPUSTAT bit 28, the GPU's DMA request).
    #[test]
    fn quads_draw_in_two_halves() {
        let mut g = timed_with_area();
        g.gp0_all(&[0x2800_00FF, 0, 8, 8 << 16]);
        assert!(!g.ready());
        // 2 per command, 82 for a first triangle, then its 36 pixels.
        assert_eq!(g.draw_time, -(2 + 82 + 36));
        g.gp0(8 | (8 << 16));
        assert!(!g.ready(), "the second triangle waits for drawing time");
        g.credit(60);
        assert!(g.ready());
        // 46 for a second triangle and its 28 pixels.
        assert_eq!(g.draw_time, -(46 + 28));
        assert_ne!(g.vram.get(7, 7), 0);
    }

    /// GP1(10h) answers with drawing state on GPUREAD; GP0(C0h) reads VRAM as
    /// it is when read.
    #[test]
    fn gpuread_returns_gpu_info_and_vram() {
        let mut g = Gpu::new();
        g.gp0_all(&[0xE300_0000 | 5 | (600 << 10), 0xE500_0000 | 0x7FF]);
        g.gp1(0x1000_0003);
        assert_eq!(g.read_data(), 5 | (600 << 10));
        g.gp1(0x1000_0005);
        assert_eq!(g.read_data(), 0x7FF);
        g.gp0_all(&[0xC000_0000, 0, 2 | (1 << 16)]);
        assert_ne!(g.status_bits() & (1 << 27), 0);
        g.vram.set(1, 0, 0x1234);
        assert_eq!(g.read_data(), 0x1234 << 16);
        assert_eq!(g.read_data(), 0x7FF, "back to the info answer");
    }

    /// Texel fetches charge 4 per texture cache miss: a 15-bit row of 16
    /// texels touches 4 blocks of 4 halfwords, a 4-bit row 1; a second
    /// identical sprite hits; a new page invalidates.
    #[test]
    fn texture_cache_misses_cost_four() {
        let sprite = |g: &mut Gpu| {
            let before = g.draw_time;
            g.gp0_all(&[0x6400_0000, 0, 0, 16 | (1 << 16)]);
            before - g.draw_time
        };
        let mut g = timed_with_area();
        g.gp0(0xE100_0100);
        g.credit(1000);
        assert_eq!(sprite(&mut g), 2 + 16 + 16 + 4 * 4);
        g.credit(1000);
        assert_eq!(sprite(&mut g), 2 + 16 + 16);
        g.gp0(0xE100_0000);
        g.credit(1000);
        assert_eq!(sprite(&mut g), 2 + 16 + 16 + 16 + 4, "CLUT load and one block");
    }

    /// A random vertex word near (or sometimes far from) the drawing area.
    fn vertex(rng: &mut Rng, area: (i32, i32)) -> u32 {
        let (x, y) = match rng.below(8) {
            0 => (rng.next() as i32 % 1100, rng.next() as i32 % 600),
            _ => (area.0 - 40 + rng.below(220) as i32, area.1 - 40 + rng.below(200) as i32),
        };
        (x as u32 & 0x7FF) | ((y as u32 & 0x7FF) << 16)
    }

    /// A triangle that crosses x=1023 after the drawing offset is cut off at the
    /// drawing area, not wrapped apart into a full-width sliver (Crash
    /// Bandicoot draws its right-hand buffer at offset 768 with geometry past it).
    #[test]
    fn triangle_crossing_the_wrap_stays_whole() {
        let mut g = Gpu::new();
        let xy = |x: i32, y: i32| (x as u32 & 0x7FF) | ((y as u32 & 0x7FF) << 16);
        g.gp0_all(&[0xE300_0000 | 512, 0xE400_0000 | 1023 | (255 << 10), 0xE500_0000 | 768 | (128 << 11)]);
        // x 200..300 relative, so 968..1068 in VRAM: only 968..1023 is in the area.
        g.gp0_all(&[0x2000_00FF, xy(200, -100), xy(300, -100), xy(200, -60)]);
        let row = |y: usize| &g.vram.as_slice()[y * 1024..(y + 1) * 1024];
        let drawn: Vec<usize> = row(30).iter().enumerate().filter(|(_, &p)| p != 0).map(|(x, _)| x).collect();
        assert_eq!((drawn.first(), drawn.last()), (Some(&968), Some(&1023)));
        assert!(row(30)[512..968].iter().all(|&p| p == 0));
    }

    /// The specialised triangle and sprite loops write exactly the VRAM that the
    /// per-pixel `plot` path writes, over random state, VRAM and primitives
    /// (every polygon and rectangle opcode, depths, windows, blending, masks,
    /// dither, clipping, offsets).
    #[test]
    fn specialised_loops_match_plot_reference() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let sequences = if cfg!(debug_assertions) { 40 } else { 400 };
        for seq in 0..sequences {
            let mut fast = Gpu::new();
            for p in fast.vram.as_mut_slice() {
                *p = rng.next() as u16;
            }
            let mut reference = Gpu::new();
            reference.vram = fast.vram.clone();
            for cmd in 0..40 {
                let mut words = Vec::new();
                // New drawing state every few commands.
                if cmd % 8 == 0 {
                    let (x1, y1) = (rng.below(1024), rng.below(512));
                    let (x2, y2) = ((x1 + rng.below(300)).min(1023), (y1 + rng.below(260)).min(511));
                    let (x2, y2) = if rng.below(16) == 0 { (rng.below(1024), rng.below(512)) } else { (x2, y2) };
                    words.push(0xE100_0000 | (rng.next() & 0x3FFF));
                    words.push(0xE200_0000 | if rng.below(3) == 0 { rng.next() & 0xF_FFFF } else { 0 });
                    words.push(0xE300_0000 | x1 | (y1 << 10));
                    words.push(0xE400_0000 | x2 | (y2 << 10));
                    let (ox, oy) = (rng.below(64) as i32 - 32 + x1 as i32, rng.below(64) as i32 - 32 + y1 as i32);
                    words.push(0xE500_0000 | (ox as u32 & 0x7FF) | ((oy as u32 & 0x7FF) << 11));
                    words.push(0xE600_0000 | (rng.next() & 3));
                    fast.gp0_all(&words);
                    reference.gp0_all(&words);
                    words.clear();
                }
                // Vertices are relative to the drawing offset.
                let near = (fast.state.area.0 - fast.state.offset.0, fast.state.area.1 - fast.state.offset.1);
                let op = if rng.below(4) == 0 { 0x60 + rng.below(32) } else { 0x20 + rng.below(32) } as u8;
                words.push(((op as u32) << 24) | (rng.next() & 0xFF_FFFF));
                if op < 0x40 {
                    let n = if op & 0x08 != 0 { 4 } else { 3 };
                    for k in 0..n {
                        if k > 0 && op & 0x10 != 0 {
                            words.push(rng.next() & 0xFF_FFFF);
                        }
                        words.push(vertex(&mut rng, near));
                        if op & 0x04 != 0 {
                            let hi = match k {
                                0 => rng.below(64) | (rng.below(512) << 6),
                                1 => (rng.next() & 0x1FF) | (rng.below(2) << 11),
                                _ => 0,
                            };
                            words.push((rng.next() & 0xFFFF) | (hi << 16));
                        }
                    }
                } else {
                    words.push(vertex(&mut rng, near));
                    if op & 0x04 != 0 {
                        words.push((rng.next() & 0xFFFF) | ((rng.below(64) | (rng.below(512) << 6)) << 16));
                    }
                    if op & 0x18 == 0 {
                        words.push(rng.below(300) | (rng.below(260) << 16));
                    }
                }
                fast.gp0_all(&words);
                REFERENCE.set(true);
                reference.gp0_all(&words);
                REFERENCE.set(false);
                if fast.vram.as_slice() != reference.vram.as_slice() {
                    let i =
                        fast.vram.as_slice().iter().zip(reference.vram.as_slice()).position(|(a, b)| a != b).unwrap();
                    panic!(
                        "sequence {seq} command {cmd} {words:08X?} state {:?}: first difference at ({}, {}): {:04X} vs reference {:04X}",
                        fast.state,
                        i % 1024,
                        i / 1024,
                        fast.vram.as_slice()[i],
                        reference.vram.as_slice()[i]
                    );
                }
            }
        }
    }
}
