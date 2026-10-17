//! GP0 rules from psx-spx (and the GPU accuracy audit in research/native-core.md),
//! one behaviour per test, driven with raw GP0 words.

use psx_gpu::Gpu;

fn gpu() -> Gpu {
    let mut g = Gpu::new();
    g.gp0_all(&[
        0xE300_0000,
        0xE400_0000 | 1023 | (511 << 10),
        0xE500_0000,
        0xE600_0000,
        0xE100_0000,
    ]);
    g
}

fn xy(x: i32, y: i32) -> u32 {
    (x as u32 & 0xFFFF) | ((y as u32 & 0xFFFF) << 16)
}

fn lit(g: &Gpu, x: i32, y: i32) -> bool {
    g.vram.get(x, y) != 0
}

#[test]
fn x_major_line_uses_centre_biased_bresenham() {
    let mut g = gpu();
    g.gp0_all(&[0x40FF_FFFF, xy(0, 0), xy(10, 3)]);
    let ys: Vec<i32> = (0..=10)
        .map(|x| (0..4).find(|&y| lit(&g, x, y)).unwrap())
        .collect();
    assert_eq!(ys, [0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3]);
    assert_eq!(
        g.vram.as_slice().iter().filter(|&&p| p != 0).count(),
        11,
        "one pixel per column"
    );
}

#[test]
fn reversed_line_draws_the_same_pixels() {
    let (mut a, mut b) = (gpu(), gpu());
    a.gp0_all(&[0x40FF_FFFF, xy(3, 2), xy(40, 17)]);
    b.gp0_all(&[0x40FF_FFFF, xy(40, 17), xy(3, 2)]);
    assert_eq!(a.vram.as_slice(), b.vram.as_slice());
}

#[test]
fn gouraud_line_keeps_colours_on_their_vertices() {
    let mut g = gpu();
    // Drawn right to left: red at x=20, blue at x=0.
    g.gp0_all(&[0x5000_00F8, xy(20, 0), 0x00F8_0000, xy(0, 0)]);
    assert_eq!(g.vram.get(20, 0), 0x001F);
    assert_eq!(g.vram.get(0, 0), 0x7C00);
}

#[test]
fn mono_lines_are_dithered_when_enabled() {
    // A flat mid-grey line on each row of a 4x4 tile: dithering (E1 bit 9)
    // must produce more than one 5-bit value, and none without it.
    let tile = |e1: u32| {
        let mut g = gpu();
        g.gp0_all(&[0xE100_0000 | e1]);
        for y in 0..4 {
            g.gp0_all(&[0x4042_4242, xy(0, y), xy(3, y)]);
        }
        (0..16)
            .map(|i| g.vram.get(i % 4, i / 4))
            .collect::<std::collections::BTreeSet<u16>>()
    };
    assert!(tile(0x200).len() > 1);
    assert_eq!(tile(0).len(), 1);
}

#[test]
fn rect_x_flip_walks_texels_backwards() {
    let mut g = gpu();
    // 15-bit texels 1..=8 at (640, 0), page tx=10.
    g.gp0_all(&[
        0xA000_0000,
        xy(640, 0),
        xy(8, 1),
        1 | (2 << 16),
        3 | (4 << 16),
        5 | (6 << 16),
        7 | (8 << 16),
    ]);
    g.gp0_all(&[0xE100_0000 | 10 | (2 << 7) | 0x1000]);
    g.gp0_all(&[0x6500_0000, xy(0, 10), 7, xy(8, 1)]);
    let row: Vec<u16> = (0..8).map(|x| g.vram.get(x, 10)).collect();
    assert_eq!(row, [8, 7, 6, 5, 4, 3, 2, 1]);
}

#[test]
fn rect_texture_window_repeats_texels() {
    let mut g = gpu();
    let texels: Vec<u32> = (0..16).map(|i| (2 * i + 1) | ((2 * i + 2) << 16)).collect();
    let mut w = vec![0xA000_0000, xy(640, 0), xy(32, 1)];
    w.extend(texels);
    g.gp0_all(&w);
    // Window mask 1, offset 1 (8-texel units): u' = (u & !8) | 8.
    g.gp0_all(&[0xE100_0000 | 10 | (2 << 7), 0xE200_0000 | 1 | (1 << 10)]);
    g.gp0_all(&[0x6500_0000, xy(0, 20), 0, xy(16, 1)]);
    let row: Vec<u16> = (0..16).map(|x| g.vram.get(x, 20)).collect();
    // Both halves of the rect sample texels 8..15 (values 9..16).
    let want: Vec<u16> = (0..16).map(|x| ((x & 7) | 8) as u16 + 1).collect();
    assert_eq!(row, want);
}

#[test]
fn check_mask_protects_pixels_from_lines_and_rects() {
    let mut g = gpu();
    g.gp0_all(&[0xE600_0001, 0x6000_00F8, xy(0, 0), xy(4, 4)]);
    assert_eq!(g.vram.get(1, 1), 0x801F, "set-mask ORs bit 15");
    g.gp0_all(&[
        0xE600_0002,
        0x6000_F800,
        xy(0, 0),
        xy(8, 1),
        0x40F8_0000,
        xy(0, 2),
        xy(8, 2),
    ]);
    assert_eq!(g.vram.get(2, 0), 0x801F, "rect skipped masked pixel");
    assert_eq!(g.vram.get(6, 0), 0x03E0, "rect drew unmasked pixel");
    assert_eq!(g.vram.get(2, 2), 0x801F, "line skipped masked pixel");
    assert_eq!(g.vram.get(6, 2), 0x7C00, "line drew unmasked pixel");
}

#[test]
fn fill_ignores_mask_and_draw_area_and_wraps() {
    let mut g = gpu();
    g.gp0_all(&[
        0xE600_0003,
        0xE300_0000 | 100,
        0xE400_0000 | 200 | (200 << 10),
    ]);
    g.vram.set(1020, 5, 0x8000);
    // x=1008 rounds to 1008, width 32 → columns 1008..1023 and 0..15 (wrapped).
    g.gp0_all(&[0x0200_00F8, xy(1008, 0), xy(32, 8)]);
    assert_eq!(
        g.vram.get(1020, 5),
        0x001F,
        "masked pixel overwritten, bit 15 cleared"
    );
    assert_eq!(g.vram.get(5, 5), 0x001F, "wrapped to the left edge");
    assert_eq!(g.vram.get(16, 5), 0);
}

#[test]
fn overlapping_vram_copy_reads_each_row_before_writing() {
    let mut g = gpu();
    for x in 0..8 {
        g.vram.set(x, 0, x as u16 + 1);
    }
    g.gp0_all(&[0x8000_0000, xy(0, 0), xy(3, 0), xy(8, 1)]);
    let row: Vec<u16> = (0..11).map(|x| g.vram.get(x, 0)).collect();
    assert_eq!(row, [1, 2, 3, 1, 2, 3, 4, 5, 6, 7, 8]);
}
