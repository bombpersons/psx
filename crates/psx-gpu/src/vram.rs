//! 1 MiB of VRAM: 1024×512 halfwords, 1555 BGR with bit 15 as the mask/semi bit.

pub const WIDTH: usize = 1024;
pub const HEIGHT: usize = 512;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Vram {
    data: Vec<u16>,
}

impl Default for Vram {
    fn default() -> Self {
        Vram {
            data: vec![0; WIDTH * HEIGHT],
        }
    }
}

impl std::fmt::Debug for Vram {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Vram(1024x512)")
    }
}

impl Vram {
    /// Coordinates wrap like the hardware's VRAM addressing.
    #[inline]
    pub fn get(&self, x: i32, y: i32) -> u16 {
        self.data[(y as usize & (HEIGHT - 1)) * WIDTH + (x as usize & (WIDTH - 1))]
    }

    #[inline]
    pub fn set(&mut self, x: i32, y: i32, value: u16) {
        self.data[(y as usize & (HEIGHT - 1)) * WIDTH + (x as usize & (WIDTH - 1))] = value;
    }

    pub fn as_slice(&self) -> &[u16] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [u16] {
        &mut self.data
    }

    /// Replace the whole contents (e.g. from an emulator VRAM dump, little-endian).
    pub fn load_bytes(&mut self, bytes: &[u8]) {
        for (dst, src) in self.data.iter_mut().zip(bytes.as_chunks::<2>().0) {
            *dst = u16::from_le_bytes(*src);
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.data.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    /// Rectangle as RGBA8, interpreting each halfword as a 15-bit pixel.
    /// Alpha is 255 except fully black 0x0000 texels, which get 0.
    pub fn rgba15(&self, x: i32, y: i32, w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for row in 0..h as i32 {
            for col in 0..w as i32 {
                let p = self.get(x + col, y + row);
                let [r, g, b] = rgb15_to_rgb8(p);
                out.extend_from_slice(&[r, g, b, if p == 0 { 0 } else { 255 }]);
            }
        }
        out
    }

    /// Rectangle as RGB8, interpreting VRAM as packed 24-bit pixels
    /// (display 24-bit mode; `x` is in halfword units, `w` in pixels).
    pub fn rgb24(&self, x: i32, y: i32, w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 3) as usize);
        for row in 0..h as i32 {
            for px in 0..w as i32 {
                let byte = px * 3;
                let mut rgb = [0u8; 3];
                for (i, c) in rgb.iter_mut().enumerate() {
                    let b = byte + i as i32;
                    let half = self.get(x + b / 2, y + row);
                    *c = if b % 2 == 0 {
                        half as u8
                    } else {
                        (half >> 8) as u8
                    };
                }
                out.extend_from_slice(&rgb);
            }
        }
        out
    }
}

/// 5-bit channel expansion that maps 31 to 255.
#[inline]
pub fn rgb15_to_rgb8(p: u16) -> [u8; 3] {
    let c = |v: u16| {
        let v = (v & 31) as u8;
        (v << 3) | (v >> 2)
    };
    [c(p), c(p >> 5), c(p >> 10)]
}

#[inline]
pub fn rgb8_to_rgb15(r: u8, g: u8, b: u8) -> u16 {
    (r as u16 >> 3) | ((g as u16 >> 3) << 5) | ((b as u16 >> 3) << 10)
}
