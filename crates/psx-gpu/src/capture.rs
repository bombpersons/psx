//! Decode a captured GP0 command stream (see [`crate::Gpu::log`]) into
//! primitives for analysis: what a frame drew, where, from which texture page
//! and CLUT, with what blending. Draw-environment commands (E1h-E6h) are
//! tracked so every vertex is reported in absolute VRAM coordinates.

use crate::DrawState;

fn sext11(v: u32) -> i32 {
    ((v as i32) << 21) >> 21
}

fn rgb(w: u32) -> [u8; 3] {
    [w as u8, (w >> 8) as u8, (w >> 16) as u8]
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Vertex {
    /// Absolute VRAM position (drawing offset applied).
    pub x: i32,
    pub y: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uv: Option<[u8; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
    /// View-space position from the GTE transform that produced this screen
    /// position, when a capture matched one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<[i32; 3]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct TexturePage {
    /// VRAM position of the page (64-halfword columns, 256-line rows).
    pub x: i32,
    pub y: i32,
    /// 4, 8 or 15 (bits per texel).
    pub depth: u8,
    /// Semi-transparency equation 0-3 (B/2+F/2, B+F, B-F, B+F/4).
    pub blend: u8,
}

impl TexturePage {
    pub fn from_bits(page: u16) -> Self {
        TexturePage {
            x: (page & 0xF) as i32 * 64,
            y: ((page >> 4) & 1) as i32 * 256,
            depth: [4, 8, 15, 15][((page >> 7) & 3) as usize],
            blend: ((page >> 5) & 3) as u8,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Command {
    /// Position in the frame's command stream.
    pub index: usize,
    /// GP0 opcode (top byte of the first word).
    pub op: u8,
    /// poly3, poly4, line, polyline, rect, fill, copy, upload, download, texpage,
    /// texwindow, area_tl, area_br, offset, mask, nop.
    pub kind: &'static str,
    pub words: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub gouraud: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub textured: bool,
    /// Texture without colour modulation.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub raw: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub semi: bool,
    /// Polygons and lines: whether the GPU dithers this primitive (E1h bit 9
    /// and, for polygons, gouraud shading or texture modulation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dither: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub vertices: Vec<Vertex>,
    /// Rect/fill/copy/upload area `(x, y, w, h)` in VRAM.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub area: Option<[i32; 4]>,
    /// Copy source `(x, y)`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<[i32; 2]>,
    /// Texture page in effect (textured primitives) or set (E1h).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texpage: Option<TexturePage>,
    /// CLUT position in VRAM (4/8-bit textures).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clut: Option<[i32; 2]>,
    /// E2h-E6h values: window, area corner, offset, mask bits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<[i32; 4]>,
    /// Drawing offset in effect (E5h): subtract it from vertices for
    /// positions relative to the buffer being drawn (the screen origin).
    pub offset: [i32; 2],
}

/// Decode one frame's commands, starting from the draw state in effect.
pub fn decode(start: &DrawState, commands: &[Vec<u32>]) -> Vec<Command> {
    let mut st = *start;
    commands
        .iter()
        .enumerate()
        .map(|(index, p)| decode_one(&mut st, index, p))
        .collect()
}

fn decode_one(st: &mut DrawState, index: usize, p: &[u32]) -> Command {
    let op = (p[0] >> 24) as u8;
    let mut c = Command {
        index,
        op,
        words: p.len(),
        kind: "nop",
        offset: [st.offset.0, st.offset.1],
        ..Default::default()
    };
    let word = |i: usize| p.get(i).copied().unwrap_or(0);
    let xy = |w: u32, st: &DrawState| (sext11(w) + st.offset.0, sext11(w >> 16) + st.offset.1);
    let size = |w: u32| {
        (
            (((w & 0xFFFF).wrapping_sub(1) & 0x3FF) + 1) as i32,
            ((((w >> 16) & 0xFFFF).wrapping_sub(1) & 0x1FF) + 1) as i32,
        )
    };
    let pos = |w: u32| ((w & 0x3FF) as i32, ((w >> 16) & 0x1FF) as i32);
    match op {
        0x02 => {
            c.kind = "fill";
            let (x, y) = ((word(1) & 0x3F0) as i32, ((word(1) >> 16) & 0x1FF) as i32);
            c.area = Some([
                x,
                y,
                (((word(2) & 0x3FF) + 0xF) & !0xF) as i32,
                ((word(2) >> 16) & 0x1FF) as i32,
            ]);
            c.vertices = vec![Vertex {
                x,
                y,
                uv: None,
                color: Some(rgb(p[0])),
                view: None,
            }];
        }
        0x20..=0x3F => {
            c.gouraud = op & 0x10 != 0;
            c.textured = op & 0x04 != 0;
            c.raw = c.textured && op & 0x01 != 0;
            c.semi = op & 0x02 != 0;
            let n = if op & 0x08 != 0 { 4 } else { 3 };
            c.kind = if n == 4 { "poly4" } else { "poly3" };
            let mut i = 1;
            let mut page = None;
            for k in 0..n {
                let col = if k > 0 && c.gouraud {
                    i += 1;
                    rgb(word(i - 1))
                } else {
                    rgb(p[0])
                };
                let (x, y) = xy(word(i), st);
                i += 1;
                let mut uv = None;
                if c.textured {
                    let w = word(i);
                    uv = Some([w as u8, (w >> 8) as u8]);
                    match k {
                        0 => {
                            c.clut =
                                Some([((w >> 16) & 0x3F) as i32 * 16, ((w >> 22) & 0x1FF) as i32])
                        }
                        1 => page = Some((w >> 16) as u16),
                        _ => {}
                    }
                    i += 1;
                }
                c.vertices.push(Vertex {
                    x,
                    y,
                    uv,
                    color: Some(col),
                    view: None,
                });
            }
            if let Some(pg) = page {
                st.texpage = (st.texpage & !0x9FF) | (pg & 0x9FF);
            }
            if c.textured || c.semi {
                c.texpage = Some(TexturePage::from_bits(st.texpage));
            }
            if c.textured && c.texpage.is_some_and(|t| t.depth == 15) {
                c.clut = None;
            }
            // As the rasteriser decides (lib.rs): shaded or modulated polygons.
            c.dither = Some(st.texpage & 0x200 != 0 && (c.gouraud || (c.textured && !c.raw)));
        }
        0x40..=0x5F => {
            c.gouraud = op & 0x10 != 0;
            c.semi = op & 0x02 != 0;
            c.kind = if op & 0x08 != 0 { "polyline" } else { "line" };
            let mut i = 1;
            let mut col = rgb(p[0]);
            while i < p.len() {
                if p[i] & 0xF000_F000 == 0x5000_5000 && c.vertices.len() >= 2 {
                    break;
                }
                if c.gouraud && !c.vertices.is_empty() {
                    col = rgb(p[i]);
                    i += 1;
                }
                let Some(&w) = p.get(i) else { break };
                let (x, y) = xy(w, st);
                c.vertices.push(Vertex {
                    x,
                    y,
                    uv: None,
                    color: Some(col),
                    view: None,
                });
                i += 1;
            }
            if c.semi {
                c.texpage = Some(TexturePage::from_bits(st.texpage));
            }
            c.dither = Some(st.texpage & 0x200 != 0);
        }
        0x60..=0x7F => {
            c.kind = "rect";
            c.textured = op & 0x04 != 0;
            c.raw = c.textured && op & 0x01 != 0;
            c.semi = op & 0x02 != 0;
            let (x, y) = xy(word(1), st);
            let mut i = 2;
            let mut uv = None;
            if c.textured {
                let w = word(i);
                uv = Some([w as u8, (w >> 8) as u8]);
                c.clut = Some([((w >> 16) & 0x3F) as i32 * 16, ((w >> 22) & 0x1FF) as i32]);
                i += 1;
            }
            let (w, h) = match (op >> 3) & 3 {
                0 => ((word(i) & 0x3FF) as i32, ((word(i) >> 16) & 0x1FF) as i32),
                1 => (1, 1),
                2 => (8, 8),
                _ => (16, 16),
            };
            c.area = Some([x, y, w, h]);
            c.vertices = vec![Vertex {
                x,
                y,
                uv,
                color: Some(rgb(p[0])),
                view: None,
            }];
            if c.textured || c.semi {
                c.texpage = Some(TexturePage::from_bits(st.texpage));
            }
            if c.texpage.is_some_and(|t| t.depth == 15) {
                c.clut = None;
            }
        }
        0x80..=0x9F => {
            c.kind = "copy";
            let ((sx, sy), (dx, dy), (w, h)) = (pos(word(1)), pos(word(2)), size(word(3)));
            c.source = Some([sx, sy]);
            c.area = Some([dx, dy, w, h]);
        }
        0xA0..=0xBF => {
            c.kind = "upload";
            let ((x, y), (w, h)) = (pos(word(1)), size(word(2)));
            c.area = Some([x, y, w, h]);
        }
        0xC0..=0xDF => {
            c.kind = "download";
            let ((x, y), (w, h)) = (pos(word(1)), size(word(2)));
            c.area = Some([x, y, w, h]);
        }
        0xE1 => {
            c.kind = "texpage";
            st.texpage = (p[0] & 0x3FFF) as u16;
            st.rect_flip = (p[0] & 0x1000 != 0, p[0] & 0x2000 != 0);
            c.texpage = Some(TexturePage::from_bits(st.texpage));
        }
        0xE2 => {
            c.kind = "texwindow";
            let w = p[0];
            st.tex_window = (
                (w & 31) as u8,
                ((w >> 5) & 31) as u8,
                ((w >> 10) & 31) as u8,
                ((w >> 15) & 31) as u8,
            );
            c.value = Some([
                (w & 31) as i32,
                ((w >> 5) & 31) as i32,
                ((w >> 10) & 31) as i32,
                ((w >> 15) & 31) as i32,
            ]);
        }
        0xE3 => {
            c.kind = "area_tl";
            st.area.0 = (p[0] & 0x3FF) as i32;
            st.area.1 = ((p[0] >> 10) & 0x1FF) as i32;
            c.value = Some([st.area.0, st.area.1, 0, 0]);
        }
        0xE4 => {
            c.kind = "area_br";
            st.area.2 = (p[0] & 0x3FF) as i32;
            st.area.3 = ((p[0] >> 10) & 0x1FF) as i32;
            c.value = Some([st.area.2, st.area.3, 0, 0]);
        }
        0xE5 => {
            c.kind = "offset";
            st.offset = (sext11(p[0]), sext11(p[0] >> 11));
            c.value = Some([st.offset.0, st.offset.1, 0, 0]);
        }
        0xE6 => {
            c.kind = "mask";
            st.set_mask = p[0] & 1 != 0;
            st.check_mask = p[0] & 2 != 0;
            c.value = Some([st.set_mask as i32, st.check_mask as i32, 0, 0]);
        }
        _ => {}
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_textured_gouraud_quad_with_offset() {
        let st = DrawState {
            offset: (100, 50),
            texpage: 0,
            ..Default::default()
        };
        // E5 offset is already in `st`; GT4 (3Ch): colour+xy+uv(clut) per vertex.
        let words = vec![
            0x3C10_2030,
            0x0000_0000,
            0x7FC0_0201, // v0: rgb 30/20/10, (0,0), uv 01/02, clut (0, 511)
            0x0040_5060,
            0x0000_0010,
            0x0095_0403, // v1: (16,0), page 0x95
            0x0070_8090,
            0x0010_0000,
            0x0000_0605, // v2
            0x00A0_B0C0,
            0x0010_0010,
            0x0000_0807, // v3
        ];
        let c = &decode(&st, &[words])[0];
        assert_eq!((c.kind, c.textured, c.gouraud), ("poly4", true, true));
        assert_eq!((c.vertices[0].x, c.vertices[0].y), (100, 50));
        assert_eq!(c.vertices[1].x, 116);
        assert_eq!(c.vertices[0].uv, Some([1, 2]));
        assert_eq!(c.vertices[1].color, Some([0x60, 0x50, 0x40]));
        assert_eq!(c.clut, Some([0, 511]));
        // Page 95h: x = 5*64, y = 256, depth bits 1 -> 8-bit.
        assert_eq!(
            c.texpage,
            Some(TexturePage {
                x: 320,
                y: 256,
                depth: 8,
                blend: 0
            })
        );
        assert_eq!(c.dither, Some(false), "E1h dither bit clear");
    }

    #[test]
    fn dithers_shaded_polygons_when_enabled() {
        let st = DrawState {
            texpage: 0x200,
            ..Default::default()
        };
        let gouraud = vec![0x3010_2030, 0, 0x0040_5060, 0x10, 0x0070_8090, 0x0010_0000];
        let flat = vec![0x2010_2030, 0, 0x10, 0x0010_0000];
        let c = decode(&st, &[gouraud, flat]);
        assert_eq!((c[0].dither, c[1].dither), (Some(true), Some(false)));
    }

    #[test]
    fn tracks_environment() {
        let cmds = vec![
            vec![0xE500_0000 | (20 << 11) | 10],
            vec![0x6000_00FF, 0x0005_0005, 0x0010_0020],
        ];
        let d = decode(&DrawState::default(), &cmds);
        assert_eq!(d[0].value, Some([10, 20, 0, 0]));
        assert_eq!((d[1].kind, d[1].area), ("rect", Some([15, 25, 32, 16])));
    }
}
