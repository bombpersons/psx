//! PS1 pixel rules, generic over where pixels go and where texels come from.
//!
//! The faithful [`crate::Gpu`] instantiates these with VRAM as both surface and
//! texture memory. A native game renderer can use its own framebuffer and
//! free-standing textures of any size while keeping the same look:
//! top-left fill convention, affine (non perspective-correct) UVs, texture
//! modulation `texel * color / 128`, 4×4 ordered dither, 15-bit output, the
//! four semi-transparency equations and mask bits (psx-spx "GPU").
//!
//! Interpolation rounding is a close approximation, not yet verified bit-exact
//! against hardware captures.

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

/// Target-wide state: clip rectangle (inclusive) and mask bit handling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Raster {
    pub clip: (i32, i32, i32, i32),
    pub check_mask: bool,
    pub set_mask: bool,
}

impl Raster {
    pub fn full(width: u32, height: u32) -> Self {
        Raster { clip: (0, 0, width as i32 - 1, height as i32 - 1), check_mask: false, set_mask: false }
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
#[inline(always)]
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

#[inline]
fn orient(a: &Vtx, b: &Vtx, px: i32, py: i32) -> i64 {
    (b.x - a.x) as i64 * (py - a.y) as i64 - (b.y - a.y) as i64 * (px - a.x) as i64
}

#[inline]
fn top_left(a: &Vtx, b: &Vtx) -> bool {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    dy < 0 || (dy == 0 && dx > 0)
}

pub fn triangle<S: Surface + ?Sized, T: TexelSource<S> + ?Sized>(
    surface: &mut S,
    tex: &T,
    ras: &Raster,
    sh: &Shade,
    v: [Vtx; 3],
) {
    // One specialised rasteriser per (textured, gouraud) pair, so the per-pixel
    // loop only steps the attributes it uses and plot() folds the constant flags.
    match (sh.textured, sh.gouraud) {
        (false, false) => triangle_with::<S, T, false, false>(surface, tex, ras, sh, v),
        (false, true) => triangle_with::<S, T, false, true>(surface, tex, ras, sh, v),
        (true, false) => triangle_with::<S, T, true, false>(surface, tex, ras, sh, v),
        (true, true) => triangle_with::<S, T, true, true>(surface, tex, ras, sh, v),
    }
}

#[inline(always)]
fn triangle_with<S: Surface + ?Sized, T: TexelSource<S> + ?Sized, const TEX: bool, const GOURAUD: bool>(
    surface: &mut S,
    tex: &T,
    ras: &Raster,
    sh: &Shade,
    v: [Vtx; 3],
) {
    let sh = &Shade { textured: TEX, gouraud: GOURAUD, ..*sh };
    let [v0, mut v1, mut v2] = v;
    let mut area = orient(&v0, &v1, v2.x, v2.y);
    if area == 0 {
        return;
    }
    if area < 0 {
        std::mem::swap(&mut v1, &mut v2);
        area = -area;
    }
    let minx = v0.x.min(v1.x).min(v2.x).max(ras.clip.0);
    let maxx = v0.x.max(v1.x).max(v2.x).min(ras.clip.2);
    let miny = v0.y.min(v1.y).min(v2.y).max(ras.clip.1);
    let maxy = v0.y.max(v1.y).max(v2.y).min(ras.clip.3);
    let (tl0, tl1, tl2) = (top_left(&v1, &v2), top_left(&v2, &v0), top_left(&v0, &v1));
    if minx > maxx || miny > maxy {
        return;
    }
    // Edge functions and attribute numerators step linearly in x, so each row
    // is walked with additions only. Attributes keep exact floor division by the
    // triangle area via a quotient/remainder stepper (identical results to
    // computing `numerator.div_euclid(area)` per pixel).
    let (e0dx, e1dx, e2dx) = (-((v2.y - v1.y) as i64), -((v0.y - v2.y) as i64), -((v1.y - v0.y) as i64));
    let attrs: [fn(&Vtx) -> i32; 5] = [|v| v.r, |v| v.g, |v| v.b, |v| v.u, |v| v.v];
    // Attributes stepped: colour (0..3) when Gouraud shaded, UV (3..5) when textured.
    let (a_lo, a_hi) = (if GOURAUD { 0 } else { 3 }, if TEX { 5 } else { 3 });
    // d(numerator)/dx for each attribute.
    let mut ndx = [0i64; 5];
    for (k, f) in attrs.iter().enumerate().take(a_hi).skip(a_lo) {
        ndx[k] = e0dx * f(&v0) as i64 + e1dx * f(&v1) as i64 + e2dx * f(&v2) as i64;
    }
    // A pixel is inside an edge when w > 0, or w == 0 on a top-left edge.
    let thresh = |tl: bool| if tl { 0i64 } else { 1 };
    let (t0, t1, t2) = (thresh(tl0), thresh(tl1), thresh(tl2));
    // Columns `[lo, hi]` (relative to minx) where `w + k*e >= t`.
    let span = |w: i64, e: i64, t: i64, lo: &mut i64, hi: &mut i64| {
        if e > 0 {
            *lo = (*lo).max((t - w).div_euclid(e) + ((t - w).rem_euclid(e) != 0) as i64);
        } else if e < 0 {
            *hi = (*hi).min((w - t).div_euclid(-e));
        } else if w < t {
            *hi = -1;
        }
    };
    // Per-pixel attribute steps as quotient and remainder of the x derivative.
    let mut step_q = [0i64; 5];
    let mut step_r = [0i64; 5];
    for k in a_lo..a_hi {
        step_q[k] = ndx[k].div_euclid(area);
        step_r[k] = ndx[k].rem_euclid(area);
    }
    let width = (maxx - minx) as i64;
    for y in miny..=maxy {
        let w0 = orient(&v1, &v2, minx, y);
        let w1 = orient(&v2, &v0, minx, y);
        let w2 = orient(&v0, &v1, minx, y);
        // Only the covered span of the row is walked.
        let (mut lo, mut hi) = (0i64, width);
        span(w0, e0dx, t0, &mut lo, &mut hi);
        span(w1, e1dx, t1, &mut lo, &mut hi);
        span(w2, e2dx, t2, &mut lo, &mut hi);
        if lo > hi {
            continue;
        }
        // Quotient and remainder of each attribute at the span start.
        let mut q = [0i64; 5];
        let mut r = [0i64; 5];
        for (k, f) in attrs.iter().enumerate().take(a_hi).skip(a_lo) {
            let n = (w0 + lo * e0dx) * f(&v0) as i64 + (w1 + lo * e1dx) * f(&v1) as i64 + (w2 + lo * e2dx) * f(&v2) as i64;
            q[k] = n.div_euclid(area);
            r[k] = n.rem_euclid(area);
        }
        for x in minx + lo as i32..=minx + hi as i32 {
            let rgb = if sh.gouraud { [q[0] as i32, q[1] as i32, q[2] as i32] } else { [v0.r, v0.g, v0.b] };
            let uv = sh.textured.then(|| (q[3] as i32, q[4] as i32));
            plot(surface, tex, ras, sh, x, y, rgb, uv);
            for k in a_lo..a_hi {
                q[k] += step_q[k];
                r[k] += step_r[k];
                if r[k] >= area {
                    r[k] -= area;
                    q[k] += 1;
                }
            }
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

pub fn line<S: Surface + ?Sized>(surface: &mut S, r: &Raster, sh: &Shade, a: Vtx, b: Vtx) {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let steps = dx.abs().max(dy.abs());
    // Round-half-away-from-zero DDA; both endpoints are drawn.
    let at = |p: i32, q: i32, i: i32| {
        if steps == 0 {
            return p;
        }
        let n = (q - p) * i;
        p + (2 * n + n.signum() * steps) / (2 * steps)
    };
    let sh = Shade { textured: false, ..*sh };
    for i in 0..=steps {
        let rgb = if sh.gouraud { [at(a.r, b.r, i), at(a.g, b.g, i), at(a.b, b.b, i)] } else { [a.r, a.g, a.b] };
        plot(surface, &NoTexture, r, &sh, at(a.x, b.x, i), at(a.y, b.y, i), rgb, None);
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
