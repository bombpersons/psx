//! PS1 pixel rules, generic over where pixels go and where texels come from.
//!
//! The faithful [`crate::Gpu`] instantiates these with VRAM as both surface and
//! texture memory. A native game renderer can use its own framebuffer and
//! free-standing textures of any size while keeping the same look:
//! top-left fill convention, affine (non perspective-correct) UVs, texture
//! modulation `texel * color / 128`, 4×4 ordered dither, 15-bit output, the
//! four semi-transparency equations and mask bits (psx-spx "GPU").
//!
//! Triangle interpolation and line stepping reproduce pixel values captured on
//! real consoles (PCSX-Redux's MIT hardware suites, checked in
//! `tests/hardware.rs`) and match Redux's software GPU on gouraud and textured
//! output; research/native-core.md "GPU accuracy" lists the known differences.

pub const DITHER: [[i32; 4]; 4] = [
    [-4, 0, -3, 1],
    [2, -2, 3, -1],
    [-3, 1, -4, 0],
    [3, -1, 2, -2],
];

/// A 1555 BGR pixel store (VRAM or a native framebuffer).
pub trait Surface {
    fn get(&self, x: i32, y: i32) -> u16;
    fn set(&mut self, x: i32, y: i32, value: u16);
}

/// Texel lookup. Returns a 1555 value where `0x0000` means fully transparent
/// and bit 15 marks a semi-transparent texel. `surface` is the render target,
/// for sources (like VRAM) that read textures from the same memory.
pub trait TexelSource<S: ?Sized> {
    fn texel(&self, surface: &S, u: i32, v: i32) -> u16;
}

/// For untextured primitives.
pub struct NoTexture;

impl<S: ?Sized> TexelSource<S> for NoTexture {
    fn texel(&self, _: &S, _: i32, _: i32) -> u16 {
        0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Vtx {
    pub x: i32,
    pub y: i32,
    pub r: i32,
    pub g: i32,
    pub b: i32,
    pub u: i32,
    pub v: i32,
}

/// Target-wide state: clip rectangle (inclusive) and mask bit handling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Raster {
    pub clip: (i32, i32, i32, i32),
    pub check_mask: bool,
    pub set_mask: bool,
}

impl Raster {
    pub fn full(width: u32, height: u32) -> Self {
        Raster {
            clip: (0, 0, width as i32 - 1, height as i32 - 1),
            check_mask: false,
            set_mask: false,
        }
    }
}

/// Per-primitive shading parameters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shade {
    pub textured: bool,
    /// Texture without colour modulation.
    pub raw: bool,
    pub semi: bool,
    pub gouraud: bool,
    /// Semi-transparency equation: 0 B/2+F/2, 1 B+F, 2 B-F, 3 B+F/4.
    pub blend: u8,
    pub dither: bool,
}

/// Shade and write one pixel. `rgb` is the 8-bit interpolated or flat colour.
#[inline]
#[allow(clippy::too_many_arguments)]
pub fn plot<S: Surface + ?Sized, T: TexelSource<S> + ?Sized>(
    surface: &mut S,
    tex: &T,
    r: &Raster,
    sh: &Shade,
    x: i32,
    y: i32,
    rgb: [i32; 3],
    uv: Option<(i32, i32)>,
) {
    if x < r.clip.0 || x > r.clip.2 || y < r.clip.1 || y > r.clip.3 {
        return;
    }
    let dst = surface.get(x, y);
    if r.check_mask && dst & 0x8000 != 0 {
        return;
    }
    let d = if sh.dither {
        DITHER[(y & 3) as usize][(x & 3) as usize]
    } else {
        0
    };
    let mut mask_bit = 0u16;
    let mut semi = sh.semi;
    let (cr, cg, cb);
    if let (true, Some((u, v))) = (sh.textured, uv) {
        let t = tex.texel(surface, u, v);
        if t == 0 {
            return;
        }
        mask_bit = t & 0x8000;
        semi &= mask_bit != 0;
        let tc = [
            (t & 31) as i32,
            ((t >> 5) & 31) as i32,
            ((t >> 10) & 31) as i32,
        ];
        if sh.raw {
            (cr, cg, cb) = (tc[0], tc[1], tc[2]);
        } else {
            // 5-bit texel × 8-bit colour / 128, at 8-bit precision for dithering.
            let m = |t: i32, c: i32| (((t * c) >> 4) + d).clamp(0, 255) >> 3;
            (cr, cg, cb) = (m(tc[0], rgb[0]), m(tc[1], rgb[1]), m(tc[2], rgb[2]));
        }
    } else {
        let q = |c: i32| (c + d).clamp(0, 255) >> 3;
        (cr, cg, cb) = (q(rgb[0]), q(rgb[1]), q(rgb[2]));
    }
    let (cr, cg, cb) = if semi {
        let bc = [
            (dst & 31) as i32,
            ((dst >> 5) & 31) as i32,
            ((dst >> 10) & 31) as i32,
        ];
        let f = |bk: i32, fg: i32| {
            match sh.blend {
                0 => (bk + fg) >> 1,
                1 => bk + fg,
                2 => bk - fg,
                _ => bk + (fg >> 2),
            }
            .clamp(0, 31)
        };
        (f(bc[0], cr), f(bc[1], cg), f(bc[2], cb))
    } else {
        (cr, cg, cb)
    };
    let set = if r.set_mask { 0x8000 } else { 0 };
    surface.set(
        x,
        y,
        (cr as u16) | ((cg as u16) << 5) | ((cb as u16) << 10) | mask_bit | set,
    );
}

/// Per-primitive constants of the pixel loop.
#[derive(Clone, Copy)]
struct Pipe {
    /// Per-pixel change of r, g, b, u, v (16.16).
    grad: [i32; 5],
    /// Colour of flat-shaded primitives.
    flat: [i32; 3],
    blend: u8,
    /// Mask bit forced on by GP0(E6h).
    set: u16,
}

/// Pick the pixel loop specialised for a primitive's flags, once per primitive:
/// `consts!(f [types] [fixed leading consts] [runtime flags] [fixed trailing consts] (args))`
/// expands to a branch tree calling `f::<types, consts>(args)` for each flag combination.
macro_rules! consts {
    ($f:ident [$($g:ty),*] [$($c:tt)*] [] [$($t:tt)*] $args:tt) => {
        $f::<$($g,)* $($c)* $($t)*> $args
    };
    ($f:ident [$($g:ty),*] [$($c:tt)*] [$flag:expr $(, $rest:expr)*] [$($t:tt)*] $args:tt) => {
        if $flag {
            consts!($f [$($g),*] [$($c)* true,] [$($rest),*] [$($t)*] $args)
        } else {
            consts!($f [$($g),*] [$($c)* false,] [$($rest),*] [$($t)*] $args)
        }
    };
}

/// Shade and write pixels `x0..=x1` of row `y`, which the caller has already
/// clipped. `s` holds r, g, b, u, v (16.16, rounding bias included) at `x0`;
/// each advances by `p.grad` per pixel. The per-pixel arithmetic is [`plot`]'s,
/// with the flags as constants so each primitive kind gets its own loop.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn span<
    S: Surface + ?Sized,
    T: TexelSource<S> + ?Sized,
    const TEX: bool,
    const RAW: bool,
    const SEMI: bool,
    const MASK: bool,
    const GOURAUD: bool,
    const DITHERED: bool,
>(
    surface: &mut S,
    tex: &T,
    p: &Pipe,
    y: i32,
    x0: i32,
    x1: i32,
    mut s: [i32; 5],
) {
    let drow = DITHER[(y & 3) as usize];
    // Half-open (x1 is a clipped surface coordinate): an inclusive range costs
    // an extra check per pixel.
    for x in x0..x1 + 1 {
        'px: {
            let dst = if MASK || SEMI { surface.get(x, y) } else { 0 };
            if MASK && dst & 0x8000 != 0 {
                break 'px;
            }
            let d = if DITHERED { drow[(x & 3) as usize] } else { 0 };
            let rgb = if GOURAUD {
                [s[0] >> 16, s[1] >> 16, s[2] >> 16]
            } else {
                p.flat
            };
            let mut mask_bit = 0u16;
            let mut semi = SEMI;
            let (cr, cg, cb);
            if TEX {
                let t = tex.texel(surface, s[3] >> 16, s[4] >> 16);
                if t == 0 {
                    break 'px;
                }
                mask_bit = t & 0x8000;
                semi &= mask_bit != 0;
                let tc = [
                    (t & 31) as i32,
                    ((t >> 5) & 31) as i32,
                    ((t >> 10) & 31) as i32,
                ];
                if RAW {
                    (cr, cg, cb) = (tc[0], tc[1], tc[2]);
                } else {
                    let m = |t: i32, c: i32| (((t * c) >> 4) + d).clamp(0, 255) >> 3;
                    (cr, cg, cb) = (m(tc[0], rgb[0]), m(tc[1], rgb[1]), m(tc[2], rgb[2]));
                }
            } else {
                let q = |c: i32| (c + d).clamp(0, 255) >> 3;
                (cr, cg, cb) = (q(rgb[0]), q(rgb[1]), q(rgb[2]));
            }
            let (cr, cg, cb) = if semi {
                let bc = [
                    (dst & 31) as i32,
                    ((dst >> 5) & 31) as i32,
                    ((dst >> 10) & 31) as i32,
                ];
                let f = |bk: i32, fg: i32| {
                    match p.blend {
                        0 => (bk + fg) >> 1,
                        1 => bk + fg,
                        2 => bk - fg,
                        _ => bk + (fg >> 2),
                    }
                    .clamp(0, 31)
                };
                (f(bc[0], cr), f(bc[1], cg), f(bc[2], cb))
            } else {
                (cr, cg, cb)
            };
            surface.set(
                x,
                y,
                (cr as u16) | ((cg as u16) << 5) | ((cb as u16) << 10) | mask_bit | p.set,
            );
        }
        if GOURAUD {
            for (c, g) in s.iter_mut().zip(p.grad).take(3) {
                *c = c.wrapping_add(g);
            }
        }
        if TEX {
            s[3] = s[3].wrapping_add(p.grad[3]);
            s[4] = s[4].wrapping_add(p.grad[4]);
        }
    }
}

/// One edge of a triangle, walked top to bottom in 16.16 fixed point.
#[derive(Clone, Copy)]
struct Edge {
    top: i32,
    height: i32,
    x: i64,
    dx: i64,
    /// Attributes r, g, b, u, v at the top vertex and their change along the edge, 16.16.
    a: [i64; 5],
    da: [i64; 5],
}

impl Edge {
    fn new(p: &Vtx, q: &Vtx) -> Self {
        let at = |v: &Vtx| [v.r, v.g, v.b, v.u, v.v].map(|c| (c as i64) << 16);
        let (ap, aq) = (at(p), at(q));
        Edge {
            top: p.y,
            height: q.y - p.y,
            x: (p.x as i64) << 16,
            dx: ((q.x - p.x) as i64) << 16,
            a: ap,
            da: std::array::from_fn(|k| aq[k] - ap[k]),
        }
    }
}

/// `base + d * row / h` (truncating division, `h > 0`) for consecutive rows,
/// kept as an exact quotient and remainder so each row is an add instead of a
/// division.
#[derive(Clone, Copy, Default)]
struct Steps {
    base: i64,
    q: i64,
    r: i64,
    dq: i64,
    dr: i64,
    h: i64,
    neg: bool,
}

impl Steps {
    fn new(base: i64, d: i64, h: i64, row: i64) -> Self {
        // Truncation is symmetric about zero: d * row / h = sign(d) * (|d| * row / h).
        let n = d.abs();
        let (q, r) = if row == 0 {
            (0, 0)
        } else {
            (n * row / h, n * row % h)
        };
        Steps {
            base,
            q,
            r,
            dq: n / h,
            dr: n % h,
            h,
            neg: d < 0,
        }
    }

    #[inline(always)]
    fn value(&self) -> i64 {
        if self.neg {
            self.base - self.q
        } else {
            self.base + self.q
        }
    }

    #[inline(always)]
    fn step(&mut self) {
        self.q += self.dq;
        self.r += self.dr;
        if self.r >= self.h {
            self.r -= self.h;
            self.q += 1;
        }
    }
}

/// An edge's X and attributes at the current row. Colours divide per row like
/// X; texture coordinates step by the per-row increment truncated once, so
/// they lag slightly on long edges.
#[derive(Clone, Copy)]
struct Walk {
    x: Steps,
    colour: [Steps; 3],
    uv: [i64; 2],
    duv: [i64; 2],
}

impl Walk {
    /// Start `e` at row `y`, with colours when `colour` and UVs when `uv`.
    fn new(e: &Edge, y: i32, colour: bool, uv: bool) -> Self {
        let (h, row) = (e.height as i64, (y - e.top) as i64);
        let mut w = Walk {
            x: Steps::new(e.x, e.dx, h, row),
            colour: [Steps::default(); 3],
            uv: [0; 2],
            duv: [0; 2],
        };
        if colour {
            w.colour = std::array::from_fn(|k| Steps::new(e.a[k], e.da[k], h, row));
        }
        if uv {
            w.duv = [e.da[3] / h, e.da[4] / h];
            w.uv = [e.a[3] + row * w.duv[0], e.a[4] + row * w.duv[1]];
        }
        w
    }

    #[inline(always)]
    fn step(&mut self, colour: bool, uv: bool) {
        self.x.step();
        if colour {
            for c in &mut self.colour {
                c.step();
            }
        }
        if uv {
            self.uv[0] += self.duv[0];
            self.uv[1] += self.duv[1];
        }
    }

    #[inline(always)]
    fn attr(&self, k: usize) -> i64 {
        if k < 3 {
            self.colour[k].value()
        } else {
            self.uv[k - 3]
        }
    }
}

/// A triangle after setup: sorted vertices, edges and horizontal gradients.
struct Setup {
    a: Vtx,
    b: Vtx,
    c: Vtx,
    /// The span through `b` is negative: `b` is on the right, the long edge on the left.
    b_right: bool,
    grad: [i64; 5],
}

/// Triangle scan conversion with PCSX-Redux's fixed-point rules (its software
/// GPU is the reference this crate is diffed against; see
/// research/native-core.md "GPU accuracy"):
///
/// - vertices are sorted by y; rows `top..bottom` are drawn (bottom excluded),
///   and on each row the pixels `ceil(left) ..= ceil(right) - 1` (left edge
///   included, right excluded);
/// - edge X and colours are evaluated per row in 16.16 with one truncating
///   division; texture coordinates advance by a truncated per-row step;
/// - horizontal gradients are a single 16.16 value per attribute, derived from
///   the span through the middle vertex (its width clamped to at least 1/16
///   pixel), truncated toward zero;
/// - each row starts from the left-edge value moved to the first pixel centre
///   plus a half (round to nearest), and adds the gradient per pixel.
///
/// Every pixel is shaded as [`plot`] would; the loop is specialised per
/// primitive kind (texture, modulation, blending, mask test, shading, dither).
pub fn triangle<S: Surface + ?Sized, T: TexelSource<S> + ?Sized>(
    surface: &mut S,
    tex: &T,
    ras: &Raster,
    sh: &Shade,
    v: [Vtx; 3],
) {
    // Sort by y with the same compare-and-swap sequence as the reference, so
    // ties pick the same middle vertex.
    let [mut a, mut b, mut c] = v;
    if a.y > b.y {
        std::mem::swap(&mut a, &mut b);
    }
    if a.y > c.y {
        std::mem::swap(&mut a, &mut c);
    }
    if b.y > c.y {
        std::mem::swap(&mut b, &mut c);
    }
    let height = c.y - a.y;
    if height == 0 {
        return;
    }
    // Fraction (16.16) of the long edge a→c above b, and the signed width of
    // the span through b: long edge X minus b's X. Negative means b is on the
    // right, so the right side has two sections.
    let t = (((b.y - a.y) as i64) << 16) / height as i64;
    let mut width = t * (c.x - a.x) as i64 + (((a.x - b.x) as i64) << 16);
    if width == 0 {
        return;
    }
    let n_attrs = if sh.textured {
        5
    } else if sh.gouraud {
        3
    } else {
        0
    };
    if n_attrs > 0 {
        width = if width < 0 {
            width.min(-0x1000)
        } else {
            width.max(0x1000)
        };
    }
    let attr = |v: &Vtx, k: usize| [v.r, v.g, v.b, v.u, v.v][k] as i64;
    let mut grad = [0i64; 5];
    for (k, g) in grad.iter_mut().enumerate().take(n_attrs) {
        // Attribute span width through b (scaled by 2^22), over the X span width (2^16), as 16.16.
        let span = (t * ((attr(&c, k) - attr(&a, k)) << 6) + ((attr(&a, k) - attr(&b, k)) << 22))
            as i32 as i64;
        *g = ((span << 10) / width) as i32 as i64;
    }
    let setup = Setup {
        a,
        b,
        c,
        b_right: width < 0,
        grad,
    };
    let p = Pipe {
        grad: grad.map(|g| g as i32),
        flat: [v[0].r, v[0].g, v[0].b],
        blend: sh.blend,
        set: if ras.set_mask { 0x8000 } else { 0 },
    };
    let mask = ras.check_mask;
    if !sh.textured {
        consts!(rows [S, NoTexture] [false, false,] [sh.semi, mask, sh.gouraud, sh.dither] [] (surface, &NoTexture, ras, &p, &setup));
    } else if sh.raw {
        // Raw texels ignore the colour, so neither shading nor dither matter.
        consts!(rows [S, T] [true, true,] [sh.semi, mask] [false, false] (surface, tex, ras, &p, &setup));
    } else {
        consts!(rows [S, T] [true, false,] [sh.semi, mask, sh.gouraud, sh.dither] [] (surface, tex, ras, &p, &setup));
    }
}

/// The row walk of [`triangle`] for one primitive kind.
fn rows<
    S: Surface + ?Sized,
    T: TexelSource<S> + ?Sized,
    const TEX: bool,
    const RAW: bool,
    const SEMI: bool,
    const MASK: bool,
    const GOURAUD: bool,
    const DITHERED: bool,
>(
    surface: &mut S,
    tex: &T,
    ras: &Raster,
    p: &Pipe,
    st: &Setup,
) {
    let (a, b, c) = (&st.a, &st.b, &st.c);
    let y_start = a.y.max(ras.clip.1);
    let y_end = (c.y - 1).min(ras.clip.3);
    if y_start > y_end {
        return;
    }
    // Only the left edge's attributes seed the rows.
    let (colour, uv) = (GOURAUD, TEX);
    let long_attrs = (st.b_right && colour, st.b_right && uv);
    let mut long = Walk::new(&Edge::new(a, c), y_start, long_attrs.0, long_attrs.1);
    let mut row = |y: i32, long: &Walk, short: &Walk| {
        let (left, right) = if st.b_right {
            (long, short)
        } else {
            (short, long)
        };
        let lx = left.x.value();
        let xmin = ((lx + 0xFFFF) >> 16) as i32;
        let xmax = (((right.x.value() - 1) >> 16) as i32).min(ras.clip.2);
        if xmax < xmin {
            return;
        }
        // Seed at the first pixel, then skip any left-clipped pixels.
        let x0 = xmin.max(ras.clip.0);
        let mut s = [0i32; 5];
        for (k, (sk, &g)) in s.iter_mut().zip(&st.grad).enumerate() {
            if (k < 3 && colour) || (k >= 3 && uv) {
                let sub = ((((xmin as i64) << 16) - lx) * g) >> 16;
                *sk = ((left.attr(k) + sub) as i32 + 0x8000)
                    .wrapping_add((x0 - xmin).wrapping_mul(g as i32));
            }
        }
        span::<S, T, TEX, RAW, SEMI, MASK, GOURAUD, DITHERED>(surface, tex, p, y, x0, xmax, s);
    };
    // Upper section (rows above b), then the lower one; the long edge runs through both.
    let short_attrs = (!st.b_right && colour, !st.b_right && uv);
    if y_start < b.y {
        let mut short = Walk::new(&Edge::new(a, b), y_start, short_attrs.0, short_attrs.1);
        for y in y_start..=y_end.min(b.y - 1) {
            row(y, &long, &short);
            long.step(long_attrs.0, long_attrs.1);
            short.step(short_attrs.0, short_attrs.1);
        }
    }
    let y_lower = y_start.max(b.y);
    if y_lower <= y_end {
        let mut short = Walk::new(&Edge::new(b, c), y_lower, short_attrs.0, short_attrs.1);
        for y in y_lower..=y_end {
            row(y, &long, &short);
            long.step(long_attrs.0, long_attrs.1);
            short.step(short_attrs.0, short_attrs.1);
        }
    }
}

/// Quad as the hardware draws it: (v0,v1,v2) then (v1,v2,v3).
pub fn quad<S: Surface + ?Sized, T: TexelSource<S> + ?Sized>(
    surface: &mut S,
    tex: &T,
    r: &Raster,
    sh: &Shade,
    v: [Vtx; 4],
) {
    triangle(surface, tex, r, sh, [v[0], v[1], v[2]]);
    triangle(surface, tex, r, sh, [v[1], v[2], v[3]]);
}

/// Line with both endpoints drawn. Positions follow the Bresenham variant
/// PCSX-Redux documents as hardware-verified (gpu-raster-phase2/10): lines are
/// walked left to right; x-major lines start from the error term `3dy - dx`
/// (a pixel-centre bias), y-major lines from the usual midpoint `2dx - dy`,
/// and the minor axis steps when the error is positive. Gouraud colours are
/// `c0 + (c1 - c0) * i / steps` in 16.16, truncated, with each colour kept on
/// its own vertex when the endpoints are swapped.
pub fn line<S: Surface + ?Sized>(surface: &mut S, r: &Raster, sh: &Shade, a: Vtx, b: Vtx) {
    let sh = Shade {
        textured: false,
        ..*sh
    };
    let (a, b) = if b.x < a.x { (b, a) } else { (a, b) };
    let (dx, dy) = (b.x - a.x, (b.y - a.y).abs());
    let sy = if b.y < a.y { -1 } else { 1 };
    let steps = dx.max(dy);
    let colour = |i: i32| {
        if !sh.gouraud || steps == 0 {
            return [a.r, a.g, a.b];
        }
        let at = |p: i32, q: i32| {
            ((((p as i64) << 16) + (((q - p) as i64) << 16) * i as i64 / steps as i64) >> 16) as i32
        };
        [at(a.r, b.r), at(a.g, b.g), at(a.b, b.b)]
    };
    let (mut x, mut y) = (a.x, a.y);
    // Error term and its increments for a straight and a diagonal step.
    let x_major = dx >= dy;
    let (mut err, straight, diag) = if x_major {
        (3 * dy - dx, 2 * dy, 2 * (dy - dx))
    } else {
        (2 * dx - dy, 2 * dx, 2 * (dx - dy))
    };
    for i in 0..=steps {
        plot(surface, &NoTexture, r, &sh, x, y, colour(i), None);
        if err <= 0 {
            err += straight;
        } else {
            err += diag;
            if x_major {
                y += sy;
            } else {
                x += 1;
            }
        }
        if x_major {
            x += 1;
        } else {
            y += sy;
        }
    }
}

/// Axis-aligned sprite: never dithered, UVs step one texel per pixel
/// (backwards when flipped).
#[allow(clippy::too_many_arguments)]
pub fn rect<S: Surface + ?Sized, T: TexelSource<S> + ?Sized>(
    surface: &mut S,
    tex: &T,
    r: &Raster,
    sh: &Shade,
    origin: Vtx,
    w: i32,
    h: i32,
    flip: (bool, bool),
) {
    let x0 = origin.x.max(r.clip.0);
    let x1 = (origin.x + w - 1).min(r.clip.2);
    let y0 = origin.y.max(r.clip.1);
    let y1 = (origin.y + h - 1).min(r.clip.3);
    if x0 > x1 || y0 > y1 {
        return;
    }
    // U as 16.16 so the triangle pixel loop can walk it; V is fixed per row.
    let du = if flip.0 { -1 } else { 1 };
    let p = Pipe {
        grad: [0, 0, 0, du << 16, 0],
        flat: [origin.r, origin.g, origin.b],
        blend: sh.blend,
        set: if r.set_mask { 0x8000 } else { 0 },
    };
    let area = (x0, y0, x1, y1);
    let mask = r.check_mask;
    if !sh.textured {
        consts!(rect_rows [S, NoTexture] [false, false,] [sh.semi, mask] [] (surface, &NoTexture, &p, origin, area, flip));
    } else {
        consts!(rect_rows [S, T] [true,] [sh.raw, sh.semi, mask] [] (surface, tex, &p, origin, area, flip));
    }
}

/// The rows of [`rect`] for one sprite kind (flat colour, no dither).
fn rect_rows<
    S: Surface + ?Sized,
    T: TexelSource<S> + ?Sized,
    const TEX: bool,
    const RAW: bool,
    const SEMI: bool,
    const MASK: bool,
>(
    surface: &mut S,
    tex: &T,
    p: &Pipe,
    origin: Vtx,
    (x0, y0, x1, y1): (i32, i32, i32, i32),
    flip: (bool, bool),
) {
    let col = x0 - origin.x;
    let u = if flip.0 {
        origin.u - col
    } else {
        origin.u + col
    };
    for y in y0..=y1 {
        let row = y - origin.y;
        let v = if flip.1 {
            origin.v - row
        } else {
            origin.v + row
        };
        span::<S, T, TEX, RAW, SEMI, MASK, false, false>(
            surface,
            tex,
            p,
            y,
            x0,
            x1,
            [0, 0, 0, u << 16, v << 16],
        );
    }
}

/// The per-pixel [`plot`] versions of [`triangle`] and [`rect`] that the
/// specialised loops replaced; tests check the two give identical VRAM.
#[cfg(test)]
pub(crate) mod reference {
    use super::*;

    /// One edge of a triangle, walked top to bottom in 16.16 fixed point.
    #[derive(Clone, Copy)]
    struct Edge {
        top: i32,
        height: i32,
        x: i64,
        dx: i64,
        a: [i64; 5],
        da: [i64; 5],
    }

    impl Edge {
        fn new(p: &Vtx, q: &Vtx) -> Self {
            let at = |v: &Vtx| [v.r, v.g, v.b, v.u, v.v].map(|c| (c as i64) << 16);
            let (ap, aq) = (at(p), at(q));
            Edge {
                top: p.y,
                height: q.y - p.y,
                x: (p.x as i64) << 16,
                dx: ((q.x - p.x) as i64) << 16,
                a: ap,
                da: std::array::from_fn(|k| aq[k] - ap[k]),
            }
        }

        fn x_at(&self, y: i32) -> i64 {
            self.x + self.dx * (y - self.top) as i64 / self.height as i64
        }

        fn attr_at(&self, k: usize, y: i32) -> i64 {
            let row = (y - self.top) as i64;
            if k < 3 {
                self.a[k] + self.da[k] * row / self.height as i64
            } else {
                self.a[k] + row * (self.da[k] / self.height as i64)
            }
        }
    }
    pub(crate) fn triangle<S: Surface + ?Sized, T: TexelSource<S> + ?Sized>(
        surface: &mut S,
        tex: &T,
        ras: &Raster,
        sh: &Shade,
        v: [Vtx; 3],
    ) {
        // Sort by y with the same compare-and-swap sequence as the reference, so
        // ties pick the same middle vertex.
        let [mut a, mut b, mut c] = v;
        if a.y > b.y {
            std::mem::swap(&mut a, &mut b);
        }
        if a.y > c.y {
            std::mem::swap(&mut a, &mut c);
        }
        if b.y > c.y {
            std::mem::swap(&mut b, &mut c);
        }
        let height = c.y - a.y;
        if height == 0 {
            return;
        }
        // Fraction (16.16) of the long edge a→c above b, and the signed width of
        // the span through b: long edge X minus b's X. Negative means b is on the
        // right, so the right side has two sections.
        let t = (((b.y - a.y) as i64) << 16) / height as i64;
        let mut width = t * (c.x - a.x) as i64 + (((a.x - b.x) as i64) << 16);
        if width == 0 {
            return;
        }
        let n_attrs = if sh.textured {
            5
        } else if sh.gouraud {
            3
        } else {
            0
        };
        if n_attrs > 0 {
            width = if width < 0 {
                width.min(-0x1000)
            } else {
                width.max(0x1000)
            };
        }
        let attr = |v: &Vtx, k: usize| [v.r, v.g, v.b, v.u, v.v][k] as i64;
        let mut grad = [0i64; 5];
        for (k, g) in grad.iter_mut().enumerate().take(n_attrs) {
            // Attribute span width through b (scaled by 2^22), over the X span width (2^16), as 16.16.
            let span = (t * ((attr(&c, k) - attr(&a, k)) << 6)
                + ((attr(&a, k) - attr(&b, k)) << 22)) as i32 as i64;
            *g = ((span << 10) / width) as i32 as i64;
        }
        let long = Edge::new(&a, &c);
        let upper = Edge::new(&a, &b);
        let lower = Edge::new(&b, &c);
        let b_right = width < 0;
        let y_end = (c.y - 1).min(ras.clip.3);
        for y in a.y.max(ras.clip.1)..=y_end {
            let short = if y < b.y { &upper } else { &lower };
            let (left, right) = if b_right {
                (&long, short)
            } else {
                (short, &long)
            };
            let lx = left.x_at(y);
            let xmin = ((lx + 0xFFFF) >> 16) as i32;
            let xmax = (((right.x_at(y) - 1) >> 16) as i32).min(ras.clip.2);
            if xmax < xmin {
                continue;
            }
            // Seed at the first pixel, then skip any left-clipped pixels.
            let mut s = [0i32; 5];
            for k in 0..n_attrs {
                let sub = ((((xmin as i64) << 16) - lx) * grad[k]) >> 16;
                s[k] = (left.attr_at(k, y) + sub) as i32 + 0x8000;
            }
            let x0 = xmin.max(ras.clip.0);
            for k in 0..n_attrs {
                s[k] = s[k].wrapping_add((x0 - xmin).wrapping_mul(grad[k] as i32));
            }
            for x in x0..=xmax {
                let rgb = if sh.gouraud {
                    [s[0] >> 16, s[1] >> 16, s[2] >> 16]
                } else {
                    [v[0].r, v[0].g, v[0].b]
                };
                let uv = sh.textured.then(|| (s[3] >> 16, s[4] >> 16));
                plot(surface, tex, ras, sh, x, y, rgb, uv);
                for k in 0..n_attrs {
                    s[k] = s[k].wrapping_add(grad[k] as i32);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn rect<S: Surface + ?Sized, T: TexelSource<S> + ?Sized>(
        surface: &mut S,
        tex: &T,
        r: &Raster,
        sh: &Shade,
        origin: Vtx,
        w: i32,
        h: i32,
        flip: (bool, bool),
    ) {
        let sh = Shade {
            dither: false,
            gouraud: false,
            ..*sh
        };
        let x0 = origin.x.max(r.clip.0);
        let x1 = (origin.x + w - 1).min(r.clip.2);
        let y0 = origin.y.max(r.clip.1);
        let y1 = (origin.y + h - 1).min(r.clip.3);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (col, row) = (x - origin.x, y - origin.y);
                let uv = sh.textured.then(|| {
                    (
                        if flip.0 {
                            origin.u - col
                        } else {
                            origin.u + col
                        },
                        if flip.1 {
                            origin.v - row
                        } else {
                            origin.v + row
                        },
                    )
                });
                plot(
                    surface,
                    tex,
                    r,
                    &sh,
                    x,
                    y,
                    [origin.r, origin.g, origin.b],
                    uv,
                );
            }
        }
    }
}
