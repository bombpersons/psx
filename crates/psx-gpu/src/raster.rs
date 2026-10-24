//! PS1 pixel rules, generic over where pixels go and where texels come from.
//!
//! The faithful [`crate::Gpu`] instantiates these with VRAM as both surface and
//! texture memory. A native game renderer can use its own framebuffer and
//! free-standing textures of any size while keeping the same look:
//! top-left fill convention, affine (non perspective-correct) UVs, texture
//! modulation `texel * color / 128`, 4×4 ordered dither, 15-bit output, the
//! four semi-transparency equations and mask bits (psx-spx "GPU").
//!
//! Triangles are Mednafen's (`crates/psx-oracle/mednafen/src/psx/gpu_polygon.cpp`),
//! which psx takes as its reference; `tests/hardware.rs` checks them against
//! pixel values captured on real consoles (PCSX-Redux's MIT hardware suites).
//! Line stepping follows those suites; research/native-core.md "GPU accuracy"
//! lists the known differences.

pub const DITHER: [[i32; 4]; 4] = [[-4, 0, -3, 1], [2, -2, 3, -1], [-3, 1, -4, 0], [3, -1, 2, -2]];

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

/// Target-wide state: clip rectangle (inclusive), mask bit handling, and
/// rows left alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Raster {
    pub clip: (i32, i32, i32, i32),
    pub check_mask: bool,
    pub set_mask: bool,
    /// Rows whose `y & 1` is this aren't drawn: in 480-line interlace the GPU
    /// skips the field being displayed (Mednafen's `LineSkipTest`).
    pub skip_parity: Option<i32>,
}

impl Raster {
    pub fn full(width: u32, height: u32) -> Self {
        Raster {
            clip: (0, 0, width as i32 - 1, height as i32 - 1),
            check_mask: false,
            set_mask: false,
            skip_parity: None,
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
    if x < r.clip.0 || x > r.clip.2 || y < r.clip.1 || y > r.clip.3 || r.skip_parity == Some(y & 1) {
        return;
    }
    let dst = surface.get(x, y);
    if r.check_mask && dst & 0x8000 != 0 {
        return;
    }
    let d = if sh.dither { DITHER[(y & 3) as usize][(x & 3) as usize] } else { 0 };
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
        let tc = [(t & 31) as i32, ((t >> 5) & 31) as i32, ((t >> 10) & 31) as i32];
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
        let bc = [(dst & 31) as i32, ((dst >> 5) & 31) as i32, ((dst >> 10) & 31) as i32];
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
    surface.set(x, y, (cr as u16) | ((cg as u16) << 5) | ((cb as u16) << 10) | mask_bit | set);
}

/// Per-primitive constants of the pixel loop.
#[derive(Clone, Copy)]
struct Pipe {
    /// Per-pixel change of r, g, b, u, v (8.24, wrapping).
    grad: [u32; 5],
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
/// clipped. `s` holds r, g, b, u, v (8.24, rounding bias included) at `x0`;
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
    mut s: [u32; 5],
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
            let rgb = if GOURAUD { [s[0] >> 24, s[1] >> 24, s[2] >> 24].map(|c| c as i32) } else { p.flat };
            let mut mask_bit = 0u16;
            let mut semi = SEMI;
            let (cr, cg, cb);
            if TEX {
                let t = tex.texel(surface, (s[3] >> 24) as i32, (s[4] >> 24) as i32);
                if t == 0 {
                    break 'px;
                }
                mask_bit = t & 0x8000;
                semi &= mask_bit != 0;
                let tc = [(t & 31) as i32, ((t >> 5) & 31) as i32, ((t >> 10) & 31) as i32];
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
                let bc = [(dst & 31) as i32, ((dst >> 5) & 31) as i32, ((dst >> 10) & 31) as i32];
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
            surface.set(x, y, (cr as u16) | ((cg as u16) << 5) | ((cb as u16) << 10) | mask_bit | p.set);
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

fn sext11(v: i32) -> i32 {
    (v << 21) >> 21
}

/// A triangle after Mednafen's setup (`gpu_polygon.cpp`): attributes r, g,
/// b, u, v are planes in 8.24 fixed point (`value >> 24`), anchored at a
/// "core" vertex and stepped per pixel in x and per row in y; edges are 32.32.
pub(crate) struct Tri {
    /// Per-pixel and per-row change of r, g, b, u, v.
    pub(crate) dx: [u32; 5],
    dy: [u32; 5],
    /// The attributes at (0, 0), with the rounding bias of half a step.
    base: [u32; 5],
    /// Upper and lower parts, in drawing order.
    parts: [Part; 2],
}

#[derive(Clone, Copy, Default)]
struct Part {
    y: i32,
    y_bound: i32,
    /// Left and right edge X, and their change per row.
    x: [i64; 2],
    step: [i64; 2],
    /// Walk upwards from `y` to `y_bound`.
    up: bool,
}

/// An edge's starting X: just under the next integer, so `>> 32` rounds
/// (x + 1 - 2^-21) down and pixel centres follow the top-left rule.
fn xfp(x: i32) -> i64 {
    ((x as i64) << 32) + (1i64 << 32) - (1 << 11)
}

/// X change per row, 32.32, rounded away from zero.
fn xfp_step(dx: i32, dy: i32) -> i64 {
    let mut e = (dx as i64) << 32;
    if e < 0 {
        e -= (dy - 1) as i64;
    }
    if e > 0 {
        e += (dy - 1) as i64;
    }
    e / dy as i64
}

impl Tri {
    /// Setup, or `None` when nothing is drawn: zero height or area, or wider
    /// than 1023 or taller than 511 pixels. Only colours (`gouraud`) and
    /// texture coordinates (`textured`) that vary get gradients.
    pub(crate) fn new(v: [Vtx; 3], gouraud: bool, textured: bool) -> Option<Tri> {
        let mut v = v;
        // The core vertex, chosen on the unsorted vertices, and followed
        // through the sort by y.
        let mut cv: u32 = if v[1].x <= v[0].x {
            if v[2].x <= v[1].x { 4 } else { 2 }
        } else if v[2].x < v[0].x {
            4
        } else {
            1
        };
        let swap12 = |cv: u32| ((cv >> 1) & 2) | ((cv << 1) & 4) | (cv & 1);
        let swap01 = |cv: u32| ((cv >> 1) & 1) | ((cv << 1) & 2) | (cv & 4);
        if v[2].y < v[1].y {
            v.swap(2, 1);
            cv = swap12(cv);
        }
        if v[1].y < v[0].y {
            v.swap(1, 0);
            cv = swap01(cv);
        }
        if v[2].y < v[1].y {
            v.swap(2, 1);
            cv = swap12(cv);
        }
        let core = (cv >> 1) as usize;
        let [a, b, c] = v;
        if a.y == c.y || c.y - a.y >= 512 {
            return None;
        }
        if (c.x - a.x).abs() >= 1024 || (c.x - b.x).abs() >= 1024 || (b.x - a.x).abs() >= 1024 {
            return None;
        }
        let at = |v: &Vtx, k: usize| [v.r, v.g, v.b, v.u, v.v][k];
        let denom = (b.x - a.x).wrapping_mul(c.y - b.y).wrapping_sub((c.x - b.x).wrapping_mul(b.y - a.y));
        if denom == 0 {
            return None;
        }
        let mut t = Tri { dx: [0; 5], dy: [0; 5], base: [0; 5], parts: [Part::default(); 2] };
        let grad = |n: i32| (n.wrapping_mul(1 << 12).wrapping_div(denom) as u32) << 12;
        for k in 0..5 {
            if (k < 3 && gouraud) || (k >= 3 && textured) {
                let (pa, pb, pc) = (at(&a, k), at(&b, k), at(&c, k));
                t.dx[k] = grad((pb - pa).wrapping_mul(c.y - b.y).wrapping_sub((pc - pb).wrapping_mul(b.y - a.y)));
                t.dy[k] = grad((b.x - a.x).wrapping_mul(pc - pb).wrapping_sub((c.x - b.x).wrapping_mul(pb - pa)));
            }
            let cvx = &v[core];
            t.base[k] = (((at(cvx, k) as u32) << 12) + (1 << 11)) << 12;
            t.base[k] = t.base[k]
                .wrapping_add(t.dx[k].wrapping_mul(cvx.x.wrapping_neg() as u32))
                .wrapping_add(t.dy[k].wrapping_mul(cvx.y.wrapping_neg() as u32));
        }
        let base_x = xfp(a.x);
        let base_step = xfp_step(c.x - a.x, c.y - a.y);
        let (upper_step, right) = if b.y == a.y {
            (0, b.x > a.x)
        } else {
            let s = xfp_step(b.x - a.x, b.y - a.y);
            (s, s > base_step)
        };
        let lower_step = if c.y == b.y { 0 } else { xfp_step(c.x - b.x, c.y - b.y) };
        // The core vertex picks the order: a part that starts at it is walked
        // away from it (upwards when it is below).
        let vo = (core != 0) as usize;
        let vp = if core == 2 { 3 } else { 0 };
        let r = right as usize;
        let long_at = |y: i32| base_x.wrapping_add(((y - a.y) as i64).wrapping_mul(base_step));
        let mut p = Part { y: v[vo].y, y_bound: v[1 ^ vo].y, up: vo != 0, ..Default::default() };
        p.x[r] = xfp(v[vo].x);
        p.step[r] = upper_step;
        p.x[1 - r] = long_at(v[vo].y);
        p.step[1 - r] = base_step;
        t.parts[vo] = p;
        let mut p = Part { y: v[1 ^ vp].y, y_bound: v[2 ^ vp].y, up: vp != 0, ..Default::default() };
        p.x[r] = xfp(v[1 ^ vp].x);
        p.step[r] = lower_step;
        p.x[1 - r] = long_at(v[1 ^ vp].y);
        p.step[1 - r] = base_step;
        t.parts[vo ^ 1] = p;
        Some(t)
    }

    /// Calls `f(y, x0, x1, attrs)` for each row's pixels `x0..=x1` inside the
    /// clip rectangle, with the attributes at `x0`, in Mednafen's order.
    pub(crate) fn rows(&self, ras: &Raster, mut f: impl FnMut(i32, i32, i32, [u32; 5])) {
        let (cx0, cy0, cx1, cy1) = ras.clip;
        let mut span = |yi: i32, l: i64, r: i64| {
            if ras.skip_parity == Some(yi & 1) {
                return;
            }
            let (x_start, x_bound) = ((l >> 32) as i32, (r >> 32) as i32);
            let (mut adjust, mut w, mut x) = (x_start, x_bound - x_start, sext11(x_start));
            if x < cx0 {
                let d = cx0 - x;
                adjust += d;
                x += d;
                w -= d;
            }
            if x + w > cx1 + 1 {
                w = cx1 + 1 - x;
            }
            if w <= 0 {
                return;
            }
            let s = std::array::from_fn(|k| {
                self.base[k]
                    .wrapping_add(self.dx[k].wrapping_mul(adjust as u32))
                    .wrapping_add(self.dy[k].wrapping_mul(yi as u32))
            });
            f(sext11(yi), x, x + w - 1, s);
        };
        for p in &self.parts {
            let (mut yi, mut l, mut r) = (p.y, p.x[0], p.x[1]);
            if p.up {
                while yi > p.y_bound {
                    yi -= 1;
                    l = l.wrapping_sub(p.step[0]);
                    r = r.wrapping_sub(p.step[1]);
                    let y = sext11(yi);
                    if y < cy0 {
                        break;
                    }
                    if y <= cy1 {
                        span(yi, l, r);
                    }
                }
            } else {
                while yi < p.y_bound {
                    let y = sext11(yi);
                    if y > cy1 {
                        break;
                    }
                    if y >= cy0 {
                        span(yi, l, r);
                    }
                    yi += 1;
                    l = l.wrapping_add(p.step[0]);
                    r = r.wrapping_add(p.step[1]);
                }
            }
        }
    }
}

/// Triangle scan conversion as Mednafen does it (`gpu_polygon.cpp`), which
/// psx takes as the reference: see [`Tri`]. Rows run between the edges'
/// rounded X (left included, right excluded), from the top vertex down,
/// except that a triangle whose core vertex is the middle or bottom one draws
/// some rows bottom-up; that only matters when it reads texels it writes.
/// Vertices include the drawing offset and are not wrapped: each row's X and
/// Y wrap to 11 bits signed, so a triangle that crosses x=1023 runs off the
/// drawing area instead of splitting.
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
    let Some(tri) = Tri::new(v, sh.gouraud, sh.textured) else { return };
    let p = Pipe {
        grad: tri.dx,
        flat: [v[0].r, v[0].g, v[0].b],
        blend: sh.blend,
        set: if ras.set_mask { 0x8000 } else { 0 },
    };
    let mask = ras.check_mask;
    if !sh.textured {
        consts!(rows [S, NoTexture] [false, false,] [sh.semi, mask, sh.gouraud, sh.dither] [] (surface, &NoTexture, ras, &p, &tri));
    } else if sh.raw {
        // Raw texels ignore the colour, so neither shading nor dither matter.
        consts!(rows [S, T] [true, true,] [sh.semi, mask] [false, false] (surface, tex, ras, &p, &tri));
    } else {
        consts!(rows [S, T] [true, false,] [sh.semi, mask, sh.gouraud, sh.dither] [] (surface, tex, ras, &p, &tri));
    }
}

/// The rows of [`triangle`] for one primitive kind.
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
    tri: &Tri,
) {
    tri.rows(ras, |y, x0, x1, s| span::<S, T, TEX, RAW, SEMI, MASK, GOURAUD, DITHERED>(surface, tex, p, y, x0, x1, s));
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
    let sh = Shade { textured: false, ..*sh };
    let (a, b) = if b.x < a.x { (b, a) } else { (a, b) };
    let (dx, dy) = (b.x - a.x, (b.y - a.y).abs());
    let sy = if b.y < a.y { -1 } else { 1 };
    let steps = dx.max(dy);
    let colour = |i: i32| {
        if !sh.gouraud || steps == 0 {
            return [a.r, a.g, a.b];
        }
        let at =
            |p: i32, q: i32| ((((p as i64) << 16) + (((q - p) as i64) << 16) * i as i64 / steps as i64) >> 16) as i32;
        [at(a.r, b.r), at(a.g, b.g), at(a.b, b.b)]
    };
    let (mut x, mut y) = (a.x, a.y);
    // Error term and its increments for a straight and a diagonal step.
    let x_major = dx >= dy;
    let (mut err, straight, diag) =
        if x_major { (3 * dy - dx, 2 * dy, 2 * (dy - dx)) } else { (2 * dx - dy, 2 * dx, 2 * (dx - dy)) };
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
    // U as 8.24 so the triangle pixel loop can walk it; V is fixed per row.
    let du: u32 = if flip.0 { 0xFF00_0000 } else { 1 << 24 };
    let p = Pipe {
        grad: [0, 0, 0, du, 0],
        flat: [origin.r, origin.g, origin.b],
        blend: sh.blend,
        set: if r.set_mask { 0x8000 } else { 0 },
    };
    let area = (x0, y0, x1, y1);
    let mask = r.check_mask;
    if !sh.textured {
        consts!(rect_rows [S, NoTexture] [false, false,] [sh.semi, mask] [] (surface, &NoTexture, &p, origin, area, flip, r.skip_parity));
    } else {
        consts!(rect_rows [S, T] [true,] [sh.raw, sh.semi, mask] [] (surface, tex, &p, origin, area, flip, r.skip_parity));
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
    skip_parity: Option<i32>,
) {
    let col = x0 - origin.x;
    let u = if flip.0 { origin.u - col } else { origin.u + col };
    for y in (y0..=y1).filter(|&y| skip_parity != Some(y & 1)) {
        let row = y - origin.y;
        let v = if flip.1 { origin.v - row } else { origin.v + row };
        span::<S, T, TEX, RAW, SEMI, MASK, false, false>(
            surface,
            tex,
            p,
            y,
            x0,
            x1,
            [0, 0, 0, (u as u32) << 24, (v as u32) << 24],
        );
    }
}

/// The per-pixel [`plot`] versions of [`triangle`] and [`rect`] that the
/// specialised loops replaced; tests check the two give identical VRAM.
#[cfg(test)]
pub(crate) mod reference {
    use super::*;

    pub(crate) fn triangle<S: Surface + ?Sized, T: TexelSource<S> + ?Sized>(
        surface: &mut S,
        tex: &T,
        ras: &Raster,
        sh: &Shade,
        v: [Vtx; 3],
    ) {
        let Some(tri) = Tri::new(v, sh.gouraud, sh.textured) else { return };
        tri.rows(ras, |y, x0, x1, mut s| {
            for x in x0..=x1 {
                let rgb =
                    if sh.gouraud { [s[0], s[1], s[2]].map(|c| (c >> 24) as i32) } else { [v[0].r, v[0].g, v[0].b] };
                let uv = sh.textured.then(|| ((s[3] >> 24) as i32, (s[4] >> 24) as i32));
                plot(surface, tex, ras, sh, x, y, rgb, uv);
                for (c, d) in s.iter_mut().zip(tri.dx) {
                    *c = c.wrapping_add(d);
                }
            }
        });
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
        let sh = Shade { dither: false, gouraud: false, ..*sh };
        let x0 = origin.x.max(r.clip.0);
        let x1 = (origin.x + w - 1).min(r.clip.2);
        let y0 = origin.y.max(r.clip.1);
        let y1 = (origin.y + h - 1).min(r.clip.3);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (col, row) = (x - origin.x, y - origin.y);
                let uv = sh.textured.then(|| {
                    (
                        if flip.0 { origin.u - col } else { origin.u + col },
                        if flip.1 { origin.v - row } else { origin.v + row },
                    )
                });
                plot(surface, tex, r, &sh, x, y, [origin.r, origin.g, origin.b], uv);
            }
        }
    }
}
