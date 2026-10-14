//! Software PlayStation GPU.
//!
//! [`Gpu`] consumes the same GP0/GP1 words the hardware does, so three things
//! go through one implementation:
//! - packets captured from an emulator (replay and compare),
//! - native reconstruction code using [`prim`] builders, which encode to GP0 words,
//! - raw VRAM uploads of extracted TIM textures and CLUTs.
//!
//! Optional logging records every completed GP0 command for later diffing.

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

/// Hardware rejects polygons and lines whose extent reaches 1024×512.
fn too_large(vs: &[Vtx]) -> bool {
    vs.iter().any(|a| vs.iter().any(|b| (a.x - b.x).abs() >= 1024 || (a.y - b.y).abs() >= 512))
}

/// Drawing environment set by GP0(E1h..E6h).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
        Raster { clip: self.area, check_mask: self.check_mask, set_mask: self.set_mask }
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
        if self.set_mask {
            0x8000
        } else {
            0
        }
    }
}

/// Display configuration set by GP1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Debug, Default)]
enum Transfer {
    #[default]
    None,
    /// CPU→VRAM: remaining halfwords go to `(x0 + i % w, y0 + i / w)`.
    ToVram { x: i32, y: i32, w: i32, h: i32, index: i32 },
}

#[derive(Debug, Default)]
pub struct Gpu {
    pub vram: Vram,
    pub state: DrawState,
    pub display: Display,
    pending: Vec<u32>,
    transfer: Transfer,
    /// Halfwords produced by GP0(C0h) VRAM→CPU, read with [`Gpu::gpuread`].
    read_queue: std::collections::VecDeque<u32>,
    /// When `Some`, every completed GP0 command is appended (A0h includes its image data).
    pub log: Option<Vec<Vec<u32>>>,
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
            if op & 0x10 != 0 {
                4
            } else {
                3
            }
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

    fn vertex(&self, w: u32) -> (i32, i32) {
        (sext11(w) + self.state.offset.0, sext11(w >> 16) + self.state.offset.1)
    }

    fn execute(&mut self, p: &[u32]) {
        let op = (p[0] >> 24) as u8;
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
        if too_large(&vs[..n]) {
            return;
        }
        let (r, tex) = (self.state.raster(), self.state.texture(clut));
        raster::triangle(&mut self.vram, &tex, &r, &sh, [vs[0], vs[1], vs[2]]);
        if quad {
            raster::triangle(&mut self.vram, &tex, &r, &sh, [vs[1], vs[2], vs[3]]);
        }
    }

    fn lines(&mut self, p: &[u32]) {
        let op = (p[0] >> 24) as u8;
        let gouraud = op & 0x10 != 0;
        let sh = Shade {
            semi: op & 0x02 != 0,
            gouraud,
            blend: ((self.state.texpage >> 5) & 3) as u8,
            dither: self.state.dither() && gouraud,
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
        let r = self.state.raster();
        for pair in pts.windows(2) {
            if !too_large(pair) {
                raster::line(&mut self.vram, &r, &sh, pair[0], pair[1]);
            }
        }
    }

    fn rectangle(&mut self, p: &[u32]) {
        let op = (p[0] >> 24) as u8;
        let textured = op & 0x04 != 0;
        let (r, g, b) = color(p[0]);
        let (x, y) = self.vertex(p[1]);
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
        let (ras, tex) = (self.state.raster(), self.state.texture(clut));
        let origin = Vtx { x, y, r, g, b, u, v };
        raster::rect(&mut self.vram, &tex, &ras, &sh, origin, w, h, self.state.rect_flip);
    }

    fn fill(&mut self, p: &[u32]) {
        let (r, g, b) = color(p[0]);
        let value = vram::rgb8_to_rgb15(r as u8, g as u8, b as u8);
        let x = (p[1] & 0x3F0) as i32;
        let y = ((p[1] >> 16) & 0x1FF) as i32;
        let w = (((p[2] & 0x3FF) + 0xF) & !0xF) as i32;
        let h = ((p[2] >> 16) & 0x1FF) as i32;
        for row in 0..h {
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
