//! A small bitmap font for menus and debug text: 6x9 pixel cells, printable
//! ASCII, drawn as sprites at whole layout pixels.
use crate::glyphs::GLYPHS;
use crate::{Blend, Renderer, Scene, Sprite, TextureId};

const COLUMNS: u32 = 16;
const FIRST: u8 = b' ';

pub struct Font {
    texture: TextureId,
    /// Cell size in texels.
    pub cell: [u32; 2],
}

impl Font {
    /// The built-in 6x9 font.
    pub fn builtin(renderer: &mut Renderer) -> Self {
        let cell = [6u32, 9u32];
        let rows = (GLYPHS.len() as u32).div_ceil(COLUMNS);
        let (width, height) = (COLUMNS * cell[0], rows * cell[1]);
        let mut rgba = vec![0u8; (width * height * 4) as usize];
        for (i, glyph) in GLYPHS.iter().enumerate() {
            let (cx, cy) = (i as u32 % COLUMNS * cell[0], i as u32 / COLUMNS * cell[1]);
            for (y, bits) in glyph.iter().enumerate() {
                for x in 0..cell[0] {
                    if bits >> (cell[0] - 1 - x) & 1 != 0 {
                        let at = (((cy + y as u32) * width + cx + x) * 4) as usize;
                        rgba[at..at + 4].copy_from_slice(&[255; 4]);
                    }
                }
            }
        }
        Self { texture: renderer.texture(width, height, &rgba), cell }
    }

    /// How wide `text` is at `scale`, in layout pixels.
    pub fn width(&self, text: &str, scale: f32) -> f32 {
        text.chars().count() as f32 * self.cell[0] as f32 * scale
    }

    /// How tall a line is at `scale`, in layout pixels.
    pub fn height(&self, scale: f32) -> f32 {
        self.cell[1] as f32 * scale
    }

    /// Adds `text` to the scene with its top left at `at`. Characters
    /// outside printable ASCII show as `?`.
    pub fn text(&self, scene: &mut Scene, text: &str, at: [f32; 2], scale: f32, color: [f32; 4]) {
        let [w, h] = [self.cell[0] as f32, self.cell[1] as f32];
        let (x, y) = (at[0].round(), at[1].round());
        for (n, c) in text.chars().enumerate() {
            if c == ' ' {
                continue;
            }
            let code = if c.is_ascii_graphic() { c as u8 } else { b'?' };
            let i = u32::from(code - FIRST);
            let src = [(i % COLUMNS) as f32 * w, (i / COLUMNS) as f32 * h, w, h];
            let dst = [x + n as f32 * w * scale, y, w * scale, h * scale];
            scene.sprites.push(Sprite { texture: self.texture, src, dst, color, blend: Blend::Opaque });
        }
    }
}
