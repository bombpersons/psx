use psx_gpu::ot::OrderingTable;
use psx_gpu::prim::*;
use psx_gpu::Gpu;

fn gpu() -> Gpu {
    let mut g = Gpu::new();
    let mut w = Vec::new();
    Env::DrawArea { x1: 0, y1: 0, x2: 1023, y2: 511 }.encode(&mut w);
    g.gp0_all(&w);
    g
}

fn draw(g: &mut Gpu, p: Prim) {
    let mut w = Vec::new();
    p.encode(&mut w);
    g.gp0_all(&w);
}

fn count(g: &Gpu, f: impl Fn(u16) -> bool) -> usize {
    g.vram.as_slice().iter().filter(|&&p| f(p)).count()
}

fn flat(verts: &[(i16, i16)], c: Rgb) -> Prim {
    Prim::Poly(Poly {
        verts: verts.iter().map(|&(x, y)| PolyVertex { xy: Xy(x, y), color: c, uv: Uv(0, 0) }).collect(),
        gouraud: false,
        texture: None,
        semi: false,
    })
}

#[test]
fn quad_covers_exact_rectangle_with_top_left_rule() {
    let mut g = gpu();
    draw(&mut g, flat(&[(10, 10), (20, 10), (10, 20), (20, 20)], Rgb(255, 255, 255)));
    // Right and bottom edges are excluded: 10x10 pixels.
    assert_eq!(count(&g, |p| p != 0), 100);
    assert_ne!(g.vram.get(10, 10), 0);
    assert_eq!(g.vram.get(20, 10), 0);
    assert_eq!(g.vram.get(10, 20), 0);
}

#[test]
fn quad_halves_do_not_overlap_under_additive_blend() {
    let mut g = gpu();
    let mut p = flat(&[(0, 0), (16, 0), (0, 16), (16, 16)], Rgb(64, 64, 64));
    if let Prim::Poly(ref mut poly) = p {
        poly.semi = true;
    }
    let mut w = Vec::new();
    Env::TexPage { page: TexPage::new(0, 0, TexDepth::Direct15, Blend::Add), dither: false, draw_to_display: false }.encode(&mut w);
    g.gp0_all(&w);
    draw(&mut g, p);
    // Every pixel blended exactly once: 0 + 8 (64>>3).
    assert_eq!(count(&g, |p| p != 0), 256);
    assert_eq!(count(&g, |p| p & 31 == 8), 256);
}

#[test]
fn clut4_texture_and_transparent_texel() {
    let mut g = gpu();
    // CLUT at (0, 480): index 1 = pure red, index 0 = 0x0000 (transparent).
    let mut clut = vec![0u16; 16];
    clut[1] = 0x001F;
    let mut w = Vec::new();
    Env::Upload { x: 0, y: 480, w: 16, h: 1, data: clut }.encode(&mut w);
    // 4bpp texture page at (640,0): one halfword = 4 texels "1,0,1,0".
    Env::Upload { x: 640, y: 0, w: 1, h: 1, data: vec![0x0101] }.encode(&mut w);
    g.gp0_all(&w);
    let tex = Texture { page: TexPage::at(640, 0, TexDepth::Clut4, Blend::Half), clut: Clut::at(0, 480), raw: true };
    let v = |x, y, u| PolyVertex { xy: Xy(x, y), color: Rgb::NEUTRAL, uv: Uv(u, 0) };
    draw(&mut g, Prim::Poly(Poly { verts: vec![v(100, 100, 0), v(104, 100, 4), v(100, 101, 0), v(104, 101, 4)], gouraud: false, texture: Some(tex), semi: false }));
    let row: Vec<u16> = (100..104).map(|x| g.vram.get(x, 100)).collect();
    assert_eq!(row, vec![0x001F, 0, 0x001F, 0]);
}

#[test]
fn modulation_neutral_colour_is_identity() {
    let mut g = gpu();
    let mut w = Vec::new();
    Env::Upload { x: 640, y: 0, w: 1, h: 1, data: vec![0x7C1F | 0x0200] }.encode(&mut w);
    g.gp0_all(&w);
    let tex = Texture { page: TexPage::at(640, 0, TexDepth::Direct15, Blend::Half), clut: Clut(0), raw: false };
    let v = |x, y| PolyVertex { xy: Xy(x, y), color: Rgb::NEUTRAL, uv: Uv(0, 0) };
    draw(&mut g, Prim::Poly(Poly { verts: vec![v(0, 0), v(2, 0), v(0, 2)], gouraud: false, texture: Some(tex), semi: false }));
    assert_eq!(g.vram.get(0, 0), 0x7C1F | 0x0200);
}

#[test]
fn sprite_fill_and_draw_area_clip() {
    let mut g = gpu();
    let mut w = Vec::new();
    Env::Fill { x: 0, y: 0, w: 32, h: 4, color: Rgb(0, 0, 255) }.encode(&mut w);
    Env::DrawArea { x1: 0, y1: 0, x2: 9, y2: 9 }.encode(&mut w);
    g.gp0_all(&w);
    assert_eq!(g.vram.get(31, 3), 0x7C00);
    draw(&mut g, Prim::Sprite(Sprite { xy: Xy(5, 5), size: (16, 16), color: Rgb(255, 0, 0), texture: None, semi: false }));
    // Clipped to x,y in 5..=9: 25 pixels of red.
    assert_eq!(count(&g, |p| p == 0x001F), 25);
}

#[test]
fn polyline_and_offset() {
    let mut g = gpu();
    let mut w = Vec::new();
    Env::DrawOffset { x: 100, y: 100 }.encode(&mut w);
    g.gp0_all(&w);
    draw(&mut g, Prim::Line(Line { points: vec![(Xy(0, 0), Rgb(255, 255, 255)), (Xy(4, 0), Rgb(255, 255, 255)), (Xy(4, 4), Rgb(255, 255, 255))], gouraud: false, semi: false }));
    assert_eq!(count(&g, |p| p != 0), 9);
    assert_ne!(g.vram.get(104, 104), 0);
}

#[test]
fn ordering_table_draws_far_first() {
    let mut g = gpu();
    let mut ot = OrderingTable::new(16);
    ot.add(2, &flat(&[(0, 0), (8, 0), (0, 8)], Rgb(255, 0, 0)));
    ot.add(10, &flat(&[(0, 0), (8, 0), (0, 8)], Rgb(0, 255, 0)));
    ot.draw(&mut g);
    assert_eq!(g.vram.get(1, 1), 0x001F, "near (z=2) red drawn last, on top");
}

#[test]
fn logging_and_gpuread_round_trip() {
    let mut g = gpu();
    g.log = Some(Vec::new());
    let mut w = Vec::new();
    Env::Upload { x: 3, y: 4, w: 2, h: 1, data: vec![0x1234, 0x5678] }.encode(&mut w);
    g.gp0_all(&w);
    g.gp0_all(&[0xC000_0000, 3 | 4 << 16, 2 | 1 << 16]);
    assert_eq!(g.gpuread(), Some(0x5678_1234));
    let log = g.log.as_ref().unwrap();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0], vec![0xA000_0000, 3 | 4 << 16, 2 | 1 << 16, 0x5678_1234]);
}
