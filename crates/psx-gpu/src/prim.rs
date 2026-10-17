//! Typed primitives for native code. Each encodes to the exact GP0 words a
//! PS1 program would submit, so native rendering and captured packets share
//! one decoder and can be compared word for word.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Neutral colour for texture modulation (texel × 128 / 128).
    pub const NEUTRAL: Rgb = Rgb(128, 128, 128);
    fn word(self) -> u32 {
        self.0 as u32 | (self.1 as u32) << 8 | (self.2 as u32) << 16
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Xy(pub i16, pub i16);

impl Xy {
    fn word(self) -> u32 {
        (self.0 as u16 as u32 & 0x7FF) | ((self.1 as u16 as u32 & 0x7FF) << 16)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TexDepth {
    Clut4 = 0,
    Clut8 = 1,
    Direct15 = 2,
}

/// Semi-transparency equation (B = background, F = foreground).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    /// B/2 + F/2
    Half = 0,
    /// B + F
    Add = 1,
    /// B - F
    Sub = 2,
    /// B + F/4
    AddQuarter = 3,
}

/// Texture page attribute (same layout as GP0(E1h) bits 0-8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TexPage(pub u16);

impl TexPage {
    /// `x` in 64-halfword units (0-15), `y` in 256-line units (0-1).
    pub fn new(x: u16, y: u16, depth: TexDepth, blend: Blend) -> Self {
        TexPage((x & 0xF) | (y & 1) << 4 | (blend as u16) << 5 | (depth as u16) << 7)
    }
    /// Page containing VRAM position `(x, y)`.
    pub fn at(x: u16, y: u16, depth: TexDepth, blend: Blend) -> Self {
        Self::new(x / 64, y / 256, depth, blend)
    }
}

/// CLUT position; `x` must be a multiple of 16.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Clut(pub u16);

impl Clut {
    pub fn at(x: u16, y: u16) -> Self {
        Clut((x / 16) & 0x3F | (y & 0x1FF) << 6)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Uv(pub u8, pub u8);

#[derive(Clone, Copy, Debug)]
pub struct Texture {
    pub page: TexPage,
    pub clut: Clut,
    /// Skip colour modulation.
    pub raw: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PolyVertex {
    pub xy: Xy,
    pub color: Rgb,
    pub uv: Uv,
}

/// Triangle or quad. Quads are drawn as (v0,v1,v2)+(v1,v2,v3): order vertices
/// as a "Z" (top-left, top-right, bottom-left, bottom-right), not a ring.
#[derive(Clone, Debug)]
pub struct Poly {
    pub verts: Vec<PolyVertex>,
    pub gouraud: bool,
    pub texture: Option<Texture>,
    pub semi: bool,
}

impl Poly {
    pub fn encode(&self, out: &mut Vec<u32>) {
        let n = self.verts.len();
        assert!(n == 3 || n == 4, "polygons have 3 or 4 vertices");
        let mut op = 0x20u32;
        if self.gouraud {
            op |= 0x10;
        }
        if n == 4 {
            op |= 0x08;
        }
        if self.texture.is_some() {
            op |= 0x04;
        }
        if self.semi {
            op |= 0x02;
        }
        if self.texture.is_some_and(|t| t.raw) {
            op |= 0x01;
        }
        out.push(op << 24 | self.verts[0].color.word());
        for (i, v) in self.verts.iter().enumerate() {
            if i > 0 && self.gouraud {
                out.push(v.color.word());
            }
            out.push(v.xy.word());
            if let Some(t) = self.texture {
                let hi = match i {
                    0 => t.clut.0 as u32,
                    1 => t.page.0 as u32,
                    _ => 0,
                };
                out.push(v.uv.0 as u32 | (v.uv.1 as u32) << 8 | hi << 16);
            }
        }
    }
}

/// Sprite. Uses the *global* texture page (set it with [`Env::TexPage`]).
#[derive(Clone, Copy, Debug)]
pub struct Sprite {
    pub xy: Xy,
    pub size: (u16, u16),
    pub color: Rgb,
    pub texture: Option<(Clut, Uv, bool)>,
    pub semi: bool,
}

impl Sprite {
    pub fn encode(&self, out: &mut Vec<u32>) {
        let size_bits = match self.size {
            (1, 1) => 1,
            (8, 8) => 2,
            (16, 16) => 3,
            _ => 0,
        };
        let mut op = 0x60 | size_bits << 3;
        if self.texture.is_some() {
            op |= 0x04;
        }
        if self.semi {
            op |= 0x02;
        }
        if self.texture.is_some_and(|t| t.2) {
            op |= 0x01;
        }
        out.push(op << 24 | self.color.word());
        out.push(self.xy.word());
        if let Some((clut, uv, _)) = self.texture {
            out.push(uv.0 as u32 | (uv.1 as u32) << 8 | (clut.0 as u32) << 16);
        }
        if size_bits == 0 {
            out.push(self.size.0 as u32 | (self.size.1 as u32) << 16);
        }
    }
}

#[derive(Clone, Debug)]
pub struct Line {
    pub points: Vec<(Xy, Rgb)>,
    pub gouraud: bool,
    pub semi: bool,
}

impl Line {
    pub fn encode(&self, out: &mut Vec<u32>) {
        assert!(self.points.len() >= 2);
        let poly = self.points.len() > 2;
        let mut op = 0x40u32;
        if self.gouraud {
            op |= 0x10;
        }
        if poly {
            op |= 0x08;
        }
        if self.semi {
            op |= 0x02;
        }
        out.push(op << 24 | self.points[0].1.word());
        for (i, (xy, c)) in self.points.iter().enumerate() {
            if i > 0 && self.gouraud {
                out.push(c.word());
            }
            out.push(xy.word());
        }
        if poly {
            out.push(0x5555_5555);
        }
    }
}

/// Drawing-environment and VRAM commands.
#[derive(Clone, Debug)]
pub enum Env {
    /// GP0(E1h): page, dither, allow drawing to display area.
    TexPage { page: TexPage, dither: bool, draw_to_display: bool },
    /// GP0(E2h) in 8-pixel units.
    TexWindow { mask: (u8, u8), offset: (u8, u8) },
    /// GP0(E3h/E4h): inclusive drawing area.
    DrawArea { x1: u16, y1: u16, x2: u16, y2: u16 },
    /// GP0(E5h)
    DrawOffset { x: i16, y: i16 },
    /// GP0(E6h)
    Mask { set: bool, check: bool },
    /// GP0(02h): ignores drawing area and mask; x/w round to 16.
    Fill { x: u16, y: u16, w: u16, h: u16, color: Rgb },
    /// GP0(A0h): upload halfwords (row-major) to a VRAM rectangle.
    Upload { x: u16, y: u16, w: u16, h: u16, data: Vec<u16> },
}

impl Env {
    pub fn encode(&self, out: &mut Vec<u32>) {
        match self {
            Env::TexPage { page, dither, draw_to_display } => {
                out.push(0xE1 << 24 | page.0 as u32 | (*dither as u32) << 9 | (*draw_to_display as u32) << 10)
            }
            Env::TexWindow { mask, offset } => out.push(
                0xE2 << 24
                    | (mask.0 as u32 & 31)
                    | (mask.1 as u32 & 31) << 5
                    | (offset.0 as u32 & 31) << 10
                    | (offset.1 as u32 & 31) << 15,
            ),
            Env::DrawArea { x1, y1, x2, y2 } => {
                out.push(0xE3 << 24 | (*x1 as u32 & 0x3FF) | (*y1 as u32 & 0x1FF) << 10);
                out.push(0xE4 << 24 | (*x2 as u32 & 0x3FF) | (*y2 as u32 & 0x1FF) << 10);
            }
            Env::DrawOffset { x, y } => {
                out.push(0xE5 << 24 | (*x as u16 as u32 & 0x7FF) | (*y as u16 as u32 & 0x7FF) << 11)
            }
            Env::Mask { set, check } => out.push(0xE6 << 24 | *set as u32 | (*check as u32) << 1),
            Env::Fill { x, y, w, h, color } => {
                out.push(0x02 << 24 | color.word());
                out.push(*x as u32 | (*y as u32) << 16);
                out.push(*w as u32 | (*h as u32) << 16);
            }
            Env::Upload { x, y, w, h, data } => {
                assert_eq!(data.len(), *w as usize * *h as usize, "upload size mismatch");
                out.push(0xA0 << 24);
                out.push(*x as u32 | (*y as u32) << 16);
                out.push(*w as u32 | (*h as u32) << 16);
                for pair in data.chunks(2) {
                    out.push(pair[0] as u32 | (pair.get(1).copied().unwrap_or(0) as u32) << 16);
                }
            }
        }
    }
}

/// Anything that can be put in an ordering table.
#[derive(Clone, Debug)]
pub enum Prim {
    Poly(Poly),
    Sprite(Sprite),
    Line(Line),
    Env(Env),
}

impl Prim {
    pub fn encode(&self, out: &mut Vec<u32>) {
        match self {
            Prim::Poly(p) => p.encode(out),
            Prim::Sprite(s) => s.encode(out),
            Prim::Line(l) => l.encode(out),
            Prim::Env(e) => e.encode(out),
        }
    }
}
