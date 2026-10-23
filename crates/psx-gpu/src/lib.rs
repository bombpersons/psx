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

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
enum Transfer {
    #[default]
    None,
    /// CPU→VRAM: remaining halfwords go to `(x0 + i % w, y0 + i / w)`.
    ToVram { x: i32, y: i32, w: i32, h: i32, index: i32 },
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Gpu {
    pub vram: Vram,
    pub state: DrawState,
    pub display: Display,
    pending: Vec<u32>,
    transfer: Transfer,
    /// Halfwords produced by GP0(C0h) VRAM→CPU, read with [`Gpu::gpuread`].
    read_queue: std::collections::VecDeque<u32>,
    /// When `Some`, every completed GP0 command is appended (A0h includes its image data).
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

    pub fn gp0_all(&mut self, words: &[u32]) {
        for &w in words {
            self.gp0(w);
        }
    }

    pub fn gp0(&mut self, word: u32) {
        if let Transfer::ToVram { .. } = self.transfer {
            // Image data belongs to the A0h command's log entry so replays are complete.
            if let Some(entry) = self.log.as_mut().and_then(|l| l.last_mut()) {
                entry.push(word);
            }
            self.upload_halfword(word as u16);
            self.upload_halfword((word >> 16) as u16);
            return;
        }
        self.pending.push(word);
        let cmd = self.pending[0];
        let complete = match command_len(cmd) {
            Some(n) => self.pending.len() >= n,
            None => self.polyline_complete(),
        };
        if complete {
            let words = std::mem::take(&mut self.pending);
            if let Some(log) = &mut self.log {
                log.push(words.clone());
            }
            self.execute(&words);
        }
    }

    fn polyline_complete(&self) -> bool {
        let gouraud = self.pending[0] & 0x1000_0000 != 0;
        let n = self.pending.len();
        // Positions: 0 cmd, then vertex(+color) groups; terminator may appear where
        // a vertex (flat) or a color (gouraud) is expected, after at least two vertices.
        let min = if gouraud { 4 } else { 3 };
        n > min && self.pending[n - 1] & 0xF000_F000 == 0x5000_5000 && {
            let group = if gouraud { 2 } else { 1 };
            (n - 1 - min).is_multiple_of(group)
        }
    }

    fn upload_halfword(&mut self, value: u16) {
        let Transfer::ToVram { x, y, w, h, index } = &mut self.transfer else { return };
        if *index >= *w * *h {
            return;
        }
        let (px, py) = (*x + *index % *w, *y + *index / *w);
        *index += 1;
        let done = *index >= *w * *h;
        if !(self.state.check_mask && self.vram.get(px, py) & 0x8000 != 0) {
            self.vram.set(px, py, value | self.state.set_mask_bit());
        }
        if done {
            self.transfer = Transfer::None;
        }
    }

    /// Vertex coordinates as the GPU parses them: 11-bit signed.
    fn vertex(&self, w: u32) -> (i32, i32) {
        (sext11(w), sext11(w >> 16))
    }

    /// Add the drawing offset to a polygon's or line's vertices. Positions wrap
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

    fn execute(&mut self, p: &[u32]) {
        let op = (p[0] >> 24) as u8;
        if matches!(op, 0x02 | 0x20..=0xBF) {
            self.draws += 1;
        }
        match op {
            0x02 => self.fill(p),
            0x20..=0x3F => self.polygon(p),
            0x40..=0x5F => self.lines(p),
            0x60..=0x7F => self.rectangle(p),
            0x80..=0x9F => self.copy(p),
            0xA0..=0xBF => {
                let (x, y) = ((p[1] & 0x3FF) as i32, ((p[1] >> 16) & 0x1FF) as i32);
                let w = (((p[2] & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1) as i32;
                let h = ((((p[2] >> 16) & 0xFFFF).wrapping_sub(1) & 0x1FF) + 1) as i32;
                self.transfer = Transfer::ToVram { x, y, w, h, index: 0 };
            }
            0xC0..=0xDF => {
                let (x, y) = ((p[1] & 0x3FF) as i32, ((p[1] >> 16) & 0x1FF) as i32);
                let w = (((p[2] & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1) as i32;
                let h = ((((p[2] >> 16) & 0xFFFF).wrapping_sub(1) & 0x1FF) + 1) as i32;
                let halves: Vec<u16> = (0..w * h).map(|i| self.vram.get(x + i % w, y + i / w)).collect();
                for pair in halves.chunks(2) {
                    let hi = pair.get(1).copied().unwrap_or(0) as u32;
                    self.read_queue.push_back(pair[0] as u32 | (hi << 16));
                }
            }
            0xE1 => {
                self.state.texpage = (p[0] & 0x3FFF) as u16;
                self.state.rect_flip = (p[0] & 0x1000 != 0, p[0] & 0x2000 != 0);
            }
            0xE2 => {
                let w = p[0];
                self.state.tex_window =
                    ((w & 31) as u8, ((w >> 5) & 31) as u8, ((w >> 10) & 31) as u8, ((w >> 15) & 31) as u8);
            }
            0xE3 => {
                self.state.area.0 = (p[0] & 0x3FF) as i32;
                self.state.area.1 = ((p[0] >> 10) & 0x1FF) as i32;
            }
            0xE4 => {
                self.state.area.2 = (p[0] & 0x3FF) as i32;
                self.state.area.3 = ((p[0] >> 10) & 0x1FF) as i32;
            }
            0xE5 => self.state.offset = (sext11(p[0]), sext11(p[0] >> 11)),
            0xE6 => {
                self.state.set_mask = p[0] & 1 != 0;
                self.state.check_mask = p[0] & 2 != 0;
            }
            _ => {} // NOP, cache clear, IRQ request
        }
    }

    fn polygon(&mut self, p: &[u32]) {
        let op = (p[0] >> 24) as u8;
        let gouraud = op & 0x10 != 0;
        let quad = op & 0x08 != 0;
        let textured = op & 0x04 != 0;
        let n = if quad { 4 } else { 3 };
        let mut vs = [Vtx::default(); 4];
        let mut i = 1;
        let mut clut = 0u32;
        let mut page = None;
        for (k, v) in vs.iter_mut().enumerate().take(n) {
            let c = if k > 0 && gouraud {
                i += 1;
                color(p[i - 1])
            } else {
                color(p[0])
            };
            let (x, y) = self.vertex(p[i]);
            i += 1;
            *v = Vtx { x, y, r: c.0, g: c.1, b: c.2, u: 0, v: 0 };
            if textured {
                v.u = (p[i] & 0xFF) as i32;
                v.v = ((p[i] >> 8) & 0xFF) as i32;
                match k {
                    0 => clut = p[i] >> 16,
                    1 => page = Some((p[i] >> 16) as u16),
                    _ => {}
                }
                i += 1;
            }
        }
        if let Some(pg) = page {
            // Polygons update the global texpage bits 0-8 and 11.
            self.state.texpage = (self.state.texpage & !0x9FF) | (pg & 0x9FF);
        }
        let sh = Shade {
            textured,
            raw: op & 0x01 != 0,
            semi: op & 0x02 != 0,
            gouraud,
            blend: ((self.state.texpage >> 5) & 3) as u8,
            dither: self.state.dither() && (gouraud || (textured && op & 0x01 == 0)),
        };
        // Mednafen: the offset is added without wrapping (rows wrap to 11
        // bits as they are drawn), and each triangle checks its own size.
        for v in &mut vs[..n] {
            v.x += self.state.offset.0;
            v.y += self.state.offset.1;
        }
        let (r, tex) = (self.raster(), self.state.texture(clut));
        #[cfg(test)]
        if tests::REFERENCE.get() {
            raster::reference::triangle(&mut self.vram, &tex, &r, &sh, [vs[0], vs[1], vs[2]]);
            if quad {
                raster::reference::triangle(&mut self.vram, &tex, &r, &sh, [vs[1], vs[2], vs[3]]);
            }
            return;
        }
        // psx-spx/Mednafen order: (v0,v1,v2) first. PCSX-Redux draws (v1,v3,v2)
        // first, which only differs where the halves overlap (self-intersecting quads).
        let vram = &mut self.vram;
        let draw = |vram: &mut Vram, t: &dyn Fn(&mut Vram, [Vtx; 3])| {
            t(vram, [vs[0], vs[1], vs[2]]);
            if quad {
                t(vram, [vs[1], vs[2], vs[3]]);
            }
        };
        if !textured {
            draw(vram, &|vram, v| raster::triangle(vram, &raster::NoTexture, &r, &sh, v));
        } else {
            with_page_texture!(tex, vram, r.clip, |t| draw(vram, &|vram, v| raster::triangle(vram, t, &r, &sh, v)));
        }
    }

    fn lines(&mut self, p: &[u32]) {
        let op = (p[0] >> 24) as u8;
        let gouraud = op & 0x10 != 0;
        let sh = Shade {
            semi: op & 0x02 != 0,
            gouraud,
            blend: ((self.state.texpage >> 5) & 3) as u8,
            // psx-spx: lines are dithered whenever dithering is enabled, mono or gouraud.
            dither: self.state.dither(),
            ..Shade::default()
        };
        let mut pts = Vec::new();
        let mut i = 1;
        let mut c = color(p[0]);
        while i < p.len() {
            if p[i] & 0xF000_F000 == 0x5000_5000 && pts.len() >= 2 {
                break;
            }
            if gouraud && !pts.is_empty() {
                c = color(p[i]);
                i += 1;
            }
            let Some(&vw) = p.get(i) else { break };
            let (x, y) = self.vertex(vw);
            pts.push(Vtx { x, y, r: c.0, g: c.1, b: c.2, u: 0, v: 0 });
            i += 1;
        }
        let r = self.raster();
        for pair in pts.windows(2) {
            if !too_large(pair) {
                let mut ab = [pair[0], pair[1]];
                self.offset(&mut ab);
                raster::line(&mut self.vram, &r, &sh, ab[0], ab[1]);
            }
        }
    }

    fn rectangle(&mut self, p: &[u32]) {
        let op = (p[0] >> 24) as u8;
        let textured = op & 0x04 != 0;
        let (r, g, b) = color(p[0]);
        let (x, y) = self.vertex(p[1]);
        let (x, y) = (x + self.state.offset.0, y + self.state.offset.1);
        let mut i = 2;
        let (mut u, mut v, mut clut) = (0, 0, 0);
        if textured {
            u = (p[i] & 0xFF) as i32;
            v = ((p[i] >> 8) & 0xFF) as i32;
            clut = p[i] >> 16;
            i += 1;
        }
        let (w, h) = match (op >> 3) & 3 {
            0 => ((p[i] & 0x3FF) as i32, ((p[i] >> 16) & 0x1FF) as i32),
            1 => (1, 1),
            2 => (8, 8),
            _ => (16, 16),
        };
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
        for row in (0..h).filter(|row| skip != Some((y + row) & 1)) {
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

    /// Next GPUREAD word from a pending VRAM→CPU transfer.
    pub fn gpuread(&mut self) -> Option<u32> {
        self.read_queue.pop_front()
    }

    pub fn gp1(&mut self, word: u32) {
        let arg = word & 0x00FF_FFFF;
        match (word >> 24) & 0x3F {
            0x00 => {
                self.state = DrawState::default();
                self.display = Display::default();
                self.pending.clear();
                self.transfer = Transfer::None;
            }
            0x01 => {
                self.pending.clear();
                self.transfer = Transfer::None;
            }
            0x03 => self.display.enabled = arg & 1 == 0,
            0x05 => self.display.start = ((arg & 0x3FE) as u16, ((arg >> 10) & 0x1FF) as u16),
            0x06 => self.display.h_range = ((arg & 0xFFF) as u16, ((arg >> 12) & 0xFFF) as u16),
            0x07 => self.display.v_range = ((arg & 0x3FF) as u16, ((arg >> 10) & 0x3FF) as u16),
            0x08 => self.display.mode = arg,
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
