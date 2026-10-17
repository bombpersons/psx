//! Pixel values captured on real PlayStations.
//!
//! The expected values come from the MIT-licensed PCSX-Redux hardware test
//! suites (`src/mips/tests/gpu-raster-phase*` at the pinned commit), whose
//! headers mark them as silicon truth read back from SCPH-1000/5501/7001
//! consoles. Only the measured numbers and the primitive setups are reused.

use psx_gpu::Gpu;

/// The suites' reset: full draw area, zero offset, no mask, texpage 0 with dither off.
fn gpu() -> Gpu {
    let mut g = Gpu::new();
    g.gp0_all(&[
        0xE300_0000,
        0xE400_0000 | 1023 | (511 << 10),
        0xE500_0000,
        0xE200_0000,
        0xE600_0000,
        0xE100_0400,
    ]);
    g
}

fn xy(x: i32, y: i32) -> u32 {
    (x as u32 & 0xFFFF) | ((y as u32 & 0xFFFF) << 16)
}

/// `rasterCmdColor`: 5-bit channels at 8-bit alignment.
fn c5(r: u32, g: u32, b: u32) -> u32 {
    (r << 3) | (g << 11) | (b << 19)
}

const RED: u32 = 0x0000F8;
const GREEN: u32 = 0x00F800;
const BLUE: u32 = 0xF80000;

fn gouraud_tri(g: &mut Gpu, v: [(u32, i32, i32); 3]) {
    g.gp0_all(&[
        0x3000_0000 | v[0].0,
        xy(v[0].1, v[0].2),
        v[1].0,
        xy(v[1].1, v[1].2),
        v[2].0,
        xy(v[2].1, v[2].2),
    ]);
}

fn check(g: &Gpu, name: &str, probes: &[(i32, i32, u16)]) {
    let bad: Vec<String> = probes
        .iter()
        .filter(|&&(x, y, want)| g.vram.get(x, y) != want)
        .map(|&(x, y, want)| format!("({x},{y}) got {:04x} want {want:04x}", g.vram.get(x, y)))
        .collect();
    assert!(
        bad.is_empty(),
        "{name}: {} of {} probes differ: {bad:?}",
        bad.len(),
        probes.len()
    );
}

/// phase-22 SG_AB: slanted gouraud, full 0xFF command colours.
#[test]
fn slanted_gouraud_sg_ab() {
    let mut g = gpu();
    gouraud_tri(
        &mut g,
        [(0x0000FF, 20, 20), (0x00FF00, 200, 60), (0xFF0000, 90, 230)],
    );
    #[rustfmt::skip]
    check(&g, "SG_AB", &[
        (26, 22, 0x003e), (24, 23, 0x001f), (26, 23, 0x003e), (27, 24, 0x003e), (30, 24, 0x003d),
        (32, 24, 0x005d), (37, 24, 0x007c), (27, 25, 0x003e), (32, 25, 0x005d), (22, 26, 0x001f),
        (27, 26, 0x003e), (35, 26, 0x005d), (25, 27, 0x041e), (28, 27, 0x003d), (33, 27, 0x005d),
        (38, 27, 0x007c), (41, 27, 0x007b), (43, 27, 0x009b), (49, 27, 0x00ba), (28, 28, 0x043d),
        (29, 28, 0x043d), (30, 28, 0x043d), (33, 28, 0x005d), (49, 28, 0x00ba),
    ]);
}

/// phase-22 SG1/SG2/SG_R/SG3: slanted gouraud with 5-bit-aligned colours.
#[test]
fn slanted_gouraud_sg1_to_sg3() {
    let mut g = gpu();
    gouraud_tri(&mut g, [(RED, 8, 4), (GREEN, 58, 22), (BLUE, 22, 52)]);
    check(
        &g,
        "SG1",
        &[
            (29, 26, 0x294a),
            (24, 24, 0x28ed),
            (36, 28, 0x29c6),
            (20, 20, 0x20b1),
            (30, 32, 0x3926),
            (40, 24, 0x1a46),
        ],
    );

    let mut g = gpu();
    gouraud_tri(&mut g, [(RED, 40, 6), (GREEN, 70, 48), (BLUE, 6, 40)]);
    check(
        &g,
        "SG2",
        &[
            (6, 40, 0x7c00),
            (39, 31, 0x294a),
            (30, 30, 0x38ca),
            (48, 32, 0x15ca),
            (40, 20, 0x14d3),
            (20, 34, 0x5466),
        ],
    );

    let mut g = gpu();
    gouraud_tri(&mut g, [(RED, 6, 6), (0, 54, 18), (0, 18, 50)]);
    check(
        &g,
        "SG_R",
        &[
            (8, 8, 0x001c),
            (26, 25, 0x000a),
            (16, 16, 0x0014),
            (22, 28, 0x000a),
            (31, 20, 0x000a),
            (13, 22, 0x0012),
        ],
    );

    let mut g = gpu();
    gouraud_tri(&mut g, [(RED, 30, 4), (GREEN, 40, 50), (BLUE, 20, 46)]);
    check(
        &g,
        "SG3",
        &[
            (20, 46, 0x7c00),
            (30, 33, 0x294a),
            (30, 22, 0x18d2),
            (31, 40, 0x2dc5),
            (28, 30, 0x30cc),
            (33, 38, 0x1e07),
        ],
    );
}

/// phase-10 LG: a red→blue gouraud line over 10 pixels loses one LSB of blue
/// at the midpoint.
#[test]
fn gouraud_line_midpoint() {
    let mut g = gpu();
    g.gp0_all(&[
        0x5000_0000 | c5(31, 0, 0),
        xy(0, 5),
        c5(0, 0, 31),
        xy(10, 5),
    ]);
    check(&g, "LG", &[(0, 5, 0x001f), (5, 5, 0x3c0f), (10, 5, 0x7c00)]);
}

fn covered_row(g: &Gpu, y: i32) -> (usize, i32, i32) {
    let xs: Vec<i32> = (0..1024).filter(|&x| g.vram.get(x, y) != 0).collect();
    (
        xs.len(),
        xs.first().copied().unwrap_or(-1),
        xs.last().copied().unwrap_or(-1),
    )
}

/// phase-23 Q2/Q3: a quad is dropped when its shared diagonal v1-v2 spans
/// 1024 pixels, but not when only v0-v3 (an edge of neither triangle) does.
#[test]
fn quad_size_limit_uses_triangle_edges() {
    let quad = |v: [(i32, i32); 4]| -> Vec<u32> {
        let mut w = vec![0x2800_0000 | GREEN];
        w.extend(v.iter().map(|&(x, y)| xy(x, y)));
        w
    };
    let mut g = gpu();
    g.gp0_all(&quad([(511, 0), (900, 0), (200, 40), (511, 40)]));
    assert_eq!(covered_row(&g, 8), (374, 449, 822), "Q1 control");

    let mut g = gpu();
    g.gp0_all(&quad([(511, 0), (1023, 0), (-1, 40), (511, 40)]));
    assert_eq!(covered_row(&g, 8).0, 0, "Q2 diagonal over the limit");

    let mut g = gpu();
    g.gp0_all(&quad([(-1, 0), (511, 0), (511, 40), (1023, 40)]));
    assert_eq!(
        covered_row(&g, 8),
        (512, 102, 613),
        "Q3 v0-v3 over the limit still draws"
    );
}

/// phase-23 O1-O3: the drawing offset is added before the 11-bit wrap.
#[test]
fn drawing_offset_wraps_after_addition() {
    let tri = |g: &mut Gpu, off: i32, x0: i32, x1: i32| {
        g.gp0_all(&[
            0xE500_0000 | (off as u32 & 0x7FF),
            0x2000_0000 | RED,
            xy(x0, 0),
            xy(x1, 0),
            xy(x0, 40),
        ]);
    };
    for (off, x0, x1) in [(0, 500, 600), (-400, 900, 1000), (-1000, 1500, 1600)] {
        let mut g = gpu();
        tri(&mut g, off, x0, x1);
        assert_eq!(
            covered_row(&g, 8),
            (80, 500, 579),
            "offset {off}, x {x0}..{x1}"
        );
    }
}

/// phase-17 signature texture: 32x32 15-bit at (704, 0), texel(u, v) = (u, v, (u+v)|1).
fn tex17(g: &mut Gpu) {
    let mut w = vec![0xA000_0000, xy(704, 0), xy(32, 32)];
    let t = |u: u32, v: u32| (u & 31) | ((v & 31) << 5) | ((((u + v) & 31) | 1) << 10);
    for v in 0..32 {
        for u in (0..32).step_by(2) {
            w.push(t(u, v) | (t(u + 1, v) << 16));
        }
    }
    g.gp0_all(&w);
}

const TEX17_PAGE: u32 = 11 | (2 << 7);

/// `(x, y, u, v)` per vertex, drawn as GP0(24h) (triangle) or GP0(2Ch) (quad) with neutral colour.
fn textured(verts: &[(i32, i32, u32, u32)]) -> Vec<u32> {
    let op = if verts.len() == 4 { 0x2C } else { 0x24 };
    let mut w = vec![(op << 24) | 0x80_8080];
    for (k, &(x, y, u, v)) in verts.iter().enumerate() {
        w.push(xy(x, y));
        w.push(u | (v << 8) | if k == 1 { TEX17_PAGE << 16 } else { 0 });
    }
    w
}

/// A named primitive (`(x, y, u, v)` per vertex) and its `(x, y, pixel)` probes.
type UvCase<'a> = (&'a str, &'a [(i32, i32, u32, u32)], &'a [(i32, i32, u16)]);

/// phase-17 affine UV on triangles (T1-T8) and quads (Q1-Q5). 0 = not drawn.
#[test]
fn affine_uv_phase17() {
    #[rustfmt::skip]
    let cases: &[UvCase] = &[
        ("T1", &[(5, 5, 0, 0), (25, 5, 20, 0), (5, 15, 0, 10)],
         &[(10, 8, 0x2465), (20, 6, 0x442F), (8, 12, 0x2CE3), (24, 5, 0x4C13), (25, 5, 0)]),
        ("T2", &[(5, 5, 0, 0), (35, 5, 6, 0), (5, 15, 0, 3)],
         &[(10, 7, 0x0C21), (20, 8, 0x1423), (15, 10, 0x0C22), (30, 5, 0x1405)]),
        ("T3", &[(5, 5, 0, 0), (15, 5, 30, 0), (5, 10, 0, 20)],
         &[(6, 6, 0x1C83), (10, 7, 0x5D0F), (8, 8, 0x5589), (5, 5, 0x0400)]),
        ("T4", &[(4, 4, 0, 0), (20, 4, 0, 16), (4, 20, 16, 0)],
         &[(10, 10, 0x34C6), (6, 10, 0x2446), (10, 6, 0x24C2), (4, 4, 0x0400)]),
        ("T5", &[(4, 4, 8, 0), (20, 4, 16, 8), (4, 20, 0, 8)],
         &[(12, 4, 0x448C), (4, 12, 0x2484), (8, 8, 0x3488), (12, 12, 0)]),
        ("T6", &[(5, 5, 3, 2), (25, 9, 20, 5), (8, 22, 8, 22)],
         &[(12, 10, 0x44E9), (18, 15, 0), (10, 18, 0x6E29), (5, 5, 0)]),
        ("T7", &[(10, 4, 0, 0), (14, 4, 20, 0), (12, 24, 10, 30)],
         &[(11, 8, 0x2CC5), (12, 14, 0x65EA), (11, 20, 0), (13, 6, 0x4C6F)]),
        ("T8", &[(4, 8, 0, 0), (34, 9, 30, 0), (4, 11, 0, 4)],
         &[(8, 9, 0x1424), (20, 9, 0x4430), (30, 9, 0x6C1A), (6, 10, 0x1462)]),
        ("Q1", &[(5, 5, 0, 0), (20, 5, 15, 0), (5, 15, 0, 10), (20, 15, 15, 10)],
         &[(8, 8, 0x1C63), (15, 10, 0x3CAA), (5, 14, 0x2520), (20, 5, 0)]),
        ("Q2", &[(5, 5, 0, 0), (20, 5, 0, 15), (5, 18, 13, 0), (20, 18, 13, 15)],
         &[(8, 10, 0x2465), (15, 13, 0x4D48), (12, 8, 0x2CE3), (12, 11, 0x34E6)]),
        ("Q3", &[(8, 5, 0, 0), (20, 5, 15, 0), (5, 18, 0, 13), (23, 18, 15, 13)],
         &[(12, 8, 0x2466), (14, 13, 0x4508), (8, 15, 0x3543), (20, 15, 0x5D4D)]),
        ("Q4", &[(5, 5, 0, 0), (20, 8, 12, 0), (8, 22, 0, 12), (25, 18, 12, 12)],
         &[(12, 10, 0x2465), (15, 14, 0x34A7), (10, 15, 0x2CE3), (18, 12, 0x3489)]),
        ("Q5", &[(5, 5, 0, 0), (30, 5, 8, 0), (5, 22, 0, 5), (30, 22, 8, 5)],
         &[(10, 8, 0x0C22), (20, 15, 0x2465), (15, 10, 0x1423), (29, 21, 0x34A8)]),
    ];
    for &(name, verts, probes) in cases {
        let mut g = gpu();
        tex17(&mut g);
        g.gp0_all(&textured(verts));
        check(&g, name, probes);
    }
}

/// phase-23 R1-R3: variable rectangle width is `w & 3FFh` (1024 draws nothing).
#[test]
fn rect_width_is_masked() {
    for (w, want) in [
        (16, (16, 200, 215)),
        (1023, (824, 200, 1023)),
        (1024, (0, -1, -1)),
    ] {
        let mut g = gpu();
        g.gp0_all(&[0x6000_0000 | c5(31, 31, 31), xy(200, 0), xy(w, 40)]);
        assert_eq!(covered_row(&g, 8), want, "width {w}");
    }
}
