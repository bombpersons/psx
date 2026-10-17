//! TIM: the PlayStation's standard texture file (PsyQ "File Formats", TIM;
//! psx-spx "CDROM File Formats").
//!
//! ```text
//! u32 id = 0x10
//! u32 flags          bits 0-2 pixel mode (0: 4-bit, 1: 8-bit, 2: 16-bit, 3: 24-bit), bit 3 CLUT present
//! [CLUT block]       u32 length (incl. 12-byte header), u16 x, y, w, h (VRAM), w*h u16 colours
//! image block        u32 length, u16 x, y, w (VRAM halfwords), h, w*h u16 of pixel data
//! ```
//!
//! Colours are 15-bit BGR with bit 15 as STP. On textured polygons the GPU
//! treats 0x0000 as fully transparent; any other colour is drawn, and STP
//! marks the texel semi-transparent when the polygon enables blending.
//!
//! This file is dependency-free and shared verbatim with the Bevy template
//! (`templates/psx-bevy/src/psx/formats/tim.rs`); a test keeps the copies equal.

/// First word of every TIM.
pub const TIM_ID: u32 = 0x10;

/// VRAM size in halfwords.
pub const VRAM_WIDTH: usize = 1024;
pub const VRAM_HEIGHT: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimDepth {
    Bpp4,
    Bpp8,
    Bpp16,
    Bpp24,
}

impl TimDepth {
    pub fn bits(self) -> u32 {
        match self {
            TimDepth::Bpp4 => 4,
            TimDepth::Bpp8 => 8,
            TimDepth::Bpp16 => 16,
            TimDepth::Bpp24 => 24,
        }
    }

    /// Colours per palette for indexed modes.
    pub fn palette_len(self) -> Option<usize> {
        match self {
            TimDepth::Bpp4 => Some(16),
            TimDepth::Bpp8 => Some(256),
            _ => None,
        }
    }
}

/// A VRAM rectangle; `w` is in 16-bit halfwords, as the GPU addresses VRAM.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VramRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// How 16-bit colours become alpha.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimAlpha {
    /// 0x0000 is transparent, every other colour opaque: what the GPU does for
    /// textures on polygons without semi-transparency (and for sprites).
    #[default]
    Cutout,
    /// Like `Cutout`, but colours with STP set get alpha 128: what such texels
    /// look like on a semi-transparent (B/2 + F/2) polygon.
    SemiStp,
    /// Every texel opaque (24-bit images, or inspecting raw colour).
    Opaque,
}

/// A parsed TIM. `pixels` is the raw image block (halfwords, little-endian).
#[derive(Clone, Debug)]
pub struct Tim {
    pub depth: TimDepth,
    /// CLUT VRAM position and its colours, row-major (`clut_rect.w * clut_rect.h`).
    pub clut_rect: Option<VramRect>,
    pub clut: Vec<u16>,
    pub image_rect: VramRect,
    pub pixels: Vec<u16>,
    /// Bytes the TIM occupies from its first byte.
    pub size: usize,
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(o)?, *b.get(o + 1)?]))
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(o)?, *b.get(o + 1)?, *b.get(o + 2)?, *b.get(o + 3)?]))
}

fn rect_at(b: &[u8], o: usize) -> Option<VramRect> {
    Some(VramRect { x: u16_at(b, o)?, y: u16_at(b, o + 2)?, w: u16_at(b, o + 4)?, h: u16_at(b, o + 6)? })
}

fn halfwords(b: &[u8], o: usize, n: usize) -> Vec<u16> {
    b[o..o + 2 * n].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect()
}

/// A block (CLUT or image) must fit VRAM and its length must match its size.
fn check_block(b: &[u8], o: usize, what: &str) -> Result<(VramRect, usize), String> {
    let len = u32_at(b, o).ok_or_else(|| format!("{what} block header truncated"))? as usize;
    let r = rect_at(b, o + 4).ok_or_else(|| format!("{what} block header truncated"))?;
    if r.w == 0 || r.h == 0 || r.x as usize >= VRAM_WIDTH || r.y as usize >= VRAM_HEIGHT {
        return Err(format!("{what} rect {r:?} outside VRAM or empty"));
    }
    if r.x as usize + r.w as usize > VRAM_WIDTH || r.y as usize + r.h as usize > VRAM_HEIGHT {
        return Err(format!("{what} rect {r:?} exceeds VRAM"));
    }
    let n = r.w as usize * r.h as usize;
    if len != 12 + 2 * n {
        return Err(format!("{what} block length {len} != 12 + 2*{}x{}", r.w, r.h));
    }
    if o + len > b.len() {
        return Err(format!("{what} block truncated ({} of {len} bytes)", b.len().saturating_sub(o)));
    }
    Ok((r, n))
}

impl Tim {
    /// Parse a TIM at the start of `bytes`. Trailing bytes are ignored.
    pub fn parse(bytes: &[u8]) -> Result<Tim, String> {
        if u32_at(bytes, 0) != Some(TIM_ID) {
            return Err("not a TIM (id != 0x10)".into());
        }
        let flags = u32_at(bytes, 4).ok_or("TIM header truncated")?;
        if flags & !0xF != 0 {
            return Err(format!("TIM flags 0x{flags:X} have reserved bits set"));
        }
        let depth = match flags & 7 {
            0 => TimDepth::Bpp4,
            1 => TimDepth::Bpp8,
            2 => TimDepth::Bpp16,
            3 => TimDepth::Bpp24,
            m => return Err(format!("unsupported TIM pixel mode {m}")),
        };
        let has_clut = flags & 8 != 0;
        let mut o = 8;
        let (mut clut_rect, mut clut) = (None, Vec::new());
        if has_clut {
            let (r, n) = check_block(bytes, o, "CLUT")?;
            clut = halfwords(bytes, o + 12, n);
            clut_rect = Some(r);
            o += 12 + 2 * n;
        }
        let (image_rect, n) = check_block(bytes, o, "image")?;
        if depth == TimDepth::Bpp24 && !(image_rect.w as usize * 2).is_multiple_of(3) {
            return Err(format!("24-bit TIM width {} halfwords is not a whole number of pixels", image_rect.w));
        }
        let pixels = halfwords(bytes, o + 12, n);
        Ok(Tim { depth, clut_rect, clut, image_rect, pixels, size: o + 12 + 2 * n })
    }

    /// Whether a TIM found inside other data is likely real: indexed images
    /// without a CLUT are legal but rare, and short ones match by chance.
    pub fn plausible(&self) -> bool {
        self.depth.palette_len().is_none() || !self.clut.is_empty() || self.pixels.len() >= 64
    }

    /// Every TIM in `bytes` (an archive, a RAM dump, ...), by id word, full
    /// validation and [`Tim::plausible`], skipping past each one found.
    pub fn find_all(bytes: &[u8]) -> Vec<(usize, Tim)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i + 8 <= bytes.len() {
            // (No let-chains: this file also builds as edition 2021.)
            let id = bytes[i] == 0x10 && bytes[i + 1..i + 4] == [0, 0, 0] && bytes[i + 5..i + 8] == [0, 0, 0];
            if let Some(t) = id.then(|| Tim::parse(&bytes[i..]).ok()).flatten().filter(Tim::plausible) {
                i += t.size;
                out.push((i - t.size, t));
                continue;
            }
            i += 1;
        }
        out
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        let w = self.image_rect.w as u32;
        match self.depth {
            TimDepth::Bpp4 => w * 4,
            TimDepth::Bpp8 => w * 2,
            TimDepth::Bpp16 => w,
            TimDepth::Bpp24 => w * 2 / 3,
        }
    }

    pub fn height(&self) -> u32 {
        self.image_rect.h as u32
    }

    /// Number of palettes in the CLUT block (0 without one). A CLUT with fewer
    /// entries than the depth needs counts as one palette.
    pub fn palette_count(&self) -> usize {
        match self.depth.palette_len() {
            Some(n) if !self.clut.is_empty() => self.clut.len().div_ceil(n),
            _ => 0,
        }
    }

    /// Palette index `index` read from the image: the 4/8-bit value of pixel (x, y).
    fn index_at(&self, x: usize, y: usize) -> usize {
        let row = &self.pixels[y * self.image_rect.w as usize..];
        match self.depth {
            TimDepth::Bpp4 => ((row[x / 4] >> ((x % 4) * 4)) & 0xF) as usize,
            TimDepth::Bpp8 => ((row[x / 2] >> ((x % 2) * 8)) & 0xFF) as usize,
            _ => 0,
        }
    }

    /// Decode to RGBA8 (row-major, `width * height * 4`) through palette
    /// `palette` (ignored for 16/24-bit). Indexed images without a CLUT
    /// decode as greyscale indices.
    pub fn to_rgba(&self, palette: usize, alpha: TimAlpha) -> Vec<u8> {
        let (w, h) = (self.width() as usize, self.height() as usize);
        let mut out = Vec::with_capacity(w * h * 4);
        match self.depth {
            TimDepth::Bpp4 | TimDepth::Bpp8 => {
                let n = self.depth.palette_len().unwrap_or(16);
                let base = palette.min(self.palette_count().saturating_sub(1)) * n;
                for y in 0..h {
                    for x in 0..w {
                        let i = self.index_at(x, y);
                        if self.clut.is_empty() {
                            let g = (i * 255 / (n - 1)) as u8;
                            out.extend_from_slice(&[g, g, g, 255]);
                        } else {
                            let c = self.clut.get(base + i).copied().unwrap_or(0);
                            out.extend_from_slice(&color16(c, alpha));
                        }
                    }
                }
            }
            TimDepth::Bpp16 => {
                for &c in &self.pixels[..w * h] {
                    out.extend_from_slice(&color16(c, alpha));
                }
            }
            TimDepth::Bpp24 => {
                let stride = self.image_rect.w as usize * 2;
                for y in 0..h {
                    for x in 0..w {
                        let at = |k: usize| {
                            let b = y * stride + x * 3 + k;
                            (self.pixels[b / 2] >> ((b % 2) * 8)) as u8
                        };
                        out.extend_from_slice(&[at(0), at(1), at(2), 255]);
                    }
                }
            }
        }
        out
    }
}

/// 5-bit channel to 8-bit, replicating the top bits (31 -> 255).
pub fn expand5(c: u16) -> u8 {
    let c = (c & 31) as u8;
    (c << 3) | (c >> 2)
}

/// A 15-bit BGR + STP colour as RGBA8.
pub fn color16(c: u16, alpha: TimAlpha) -> [u8; 4] {
    let a = match alpha {
        TimAlpha::Opaque => 255,
        _ if c == 0 => 0,
        TimAlpha::SemiStp if c & 0x8000 != 0 => 128,
        _ => 255,
    };
    [expand5(c), expand5(c >> 5), expand5(c >> 10), a]
}

/// Texture page attributes as the GPU (and TMD `TSB`/`CBA` fields) encode them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TexPage {
    /// VRAM x of the page (multiple of 64 halfwords).
    pub x: u16,
    /// VRAM y of the page (0 or 256).
    pub y: u16,
    /// 4, 8 or 16 bits per texel.
    pub bits: u8,
    /// Semi-transparency equation 0-3 (B/2+F/2, B+F, B-F, B+F/4).
    pub abr: u8,
}

impl TexPage {
    /// Decode a TSB (texture page) halfword.
    pub fn from_tsb(tsb: u16) -> TexPage {
        TexPage {
            x: (tsb & 0xF) * 64,
            y: ((tsb >> 4) & 1) * 256,
            bits: match (tsb >> 7) & 3 {
                0 => 4,
                1 => 8,
                _ => 16,
            },
            abr: ((tsb >> 5) & 3) as u8,
        }
    }
}

/// CLUT position from a CBA halfword: x in units of 16 halfwords, y in lines.
pub fn clut_from_cba(cba: u16) -> (u16, u16) {
    ((cba & 0x3F) * 16, (cba >> 6) & 0x1FF)
}

/// A VRAM image built by uploading TIMs, for decoding texture pages the way
/// models (TMD) reference them.
#[derive(Clone)]
pub struct Vram {
    pub data: Vec<u16>,
    /// Which halfwords some upload covered.
    pub written: Vec<bool>,
}

impl Default for Vram {
    fn default() -> Self {
        Vram { data: vec![0; VRAM_WIDTH * VRAM_HEIGHT], written: vec![false; VRAM_WIDTH * VRAM_HEIGHT] }
    }
}

impl Vram {
    pub fn upload(&mut self, r: VramRect, words: &[u16]) {
        for row in 0..r.h as usize {
            for col in 0..r.w as usize {
                let (x, y) = ((r.x as usize + col) % VRAM_WIDTH, (r.y as usize + row) % VRAM_HEIGHT);
                if let Some(&v) = words.get(row * r.w as usize + col) {
                    self.data[y * VRAM_WIDTH + x] = v;
                    self.written[y * VRAM_WIDTH + x] = true;
                }
            }
        }
    }

    /// Upload a TIM's CLUT and image where its header places them.
    pub fn load_tim(&mut self, tim: &Tim) {
        if let Some(r) = tim.clut_rect {
            self.upload(r, &tim.clut);
        }
        self.upload(tim.image_rect, &tim.pixels);
    }

    fn at(&self, x: usize, y: usize) -> (u16, bool) {
        let i = (y % VRAM_HEIGHT) * VRAM_WIDTH + (x % VRAM_WIDTH);
        (self.data[i], self.written[i])
    }

    /// Decode a 256x256 texel window of a texture page through a CLUT (for
    /// 4/8-bit pages), as RGBA8. Returns the image and the fraction of its
    /// texels whose VRAM (page and CLUT entry) some upload covered.
    pub fn decode_page(&self, page: TexPage, clut: (u16, u16), alpha: TimAlpha) -> (Vec<u8>, f32) {
        let mut out = Vec::with_capacity(256 * 256 * 4);
        let mut covered = 0usize;
        let (px, py) = (page.x as usize, page.y as usize);
        for v in 0..256 {
            for u in 0..256 {
                let (c, ok) = match page.bits {
                    4 => {
                        let (hw, ok) = self.at(px + u / 4, py + v);
                        let (c, ok2) =
                            self.at(clut.0 as usize + ((hw >> ((u % 4) * 4)) & 0xF) as usize, clut.1 as usize);
                        (c, ok && ok2)
                    }
                    8 => {
                        let (hw, ok) = self.at(px + u / 2, py + v);
                        let (c, ok2) =
                            self.at(clut.0 as usize + ((hw >> ((u % 2) * 8)) & 0xFF) as usize, clut.1 as usize);
                        (c, ok && ok2)
                    }
                    _ => self.at(px + u, py + v),
                };
                covered += ok as usize;
                out.extend_from_slice(&color16(c, alpha));
            }
        }
        (out, covered as f32 / (256.0 * 256.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a TIM from parts (test helper).
    pub fn build(flags: u32, clut: Option<(VramRect, &[u16])>, image: VramRect, pixels: &[u16]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&TIM_ID.to_le_bytes());
        b.extend_from_slice(&flags.to_le_bytes());
        let mut block = |r: VramRect, words: &[u16]| {
            b.extend_from_slice(&(12 + 2 * words.len() as u32).to_le_bytes());
            for v in [r.x, r.y, r.w, r.h] {
                b.extend_from_slice(&v.to_le_bytes());
            }
            for w in words {
                b.extend_from_slice(&w.to_le_bytes());
            }
        };
        if let Some((r, c)) = clut {
            block(r, c);
        }
        block(image, pixels);
        b
    }

    const RED: u16 = 0x001F;
    const GREEN: u16 = 0x03E0;
    const STP_BLUE: u16 = 0x8000 | 0x7C00;

    #[test]
    fn four_bit_with_two_cluts() {
        let mut clut = vec![0u16; 32];
        clut[1] = RED;
        clut[2] = STP_BLUE;
        clut[16 + 1] = GREEN;
        // 8x1 pixels = 2 halfwords: indices 0,1,2,3 | 1,1,1,1
        let pixels = [0x3210, 0x1111];
        let b = build(
            8,
            Some((VramRect { x: 0, y: 480, w: 16, h: 2 }, &clut)),
            VramRect { x: 640, y: 0, w: 2, h: 1 },
            &pixels,
        );
        let t = Tim::parse(&b).unwrap();
        assert_eq!((t.width(), t.height(), t.palette_count(), t.size), (8, 1, 2, b.len()));
        let p0 = t.to_rgba(0, TimAlpha::Cutout);
        assert_eq!(&p0[0..4], &[0, 0, 0, 0], "0x0000 is transparent");
        assert_eq!(&p0[4..8], &[255, 0, 0, 255]);
        assert_eq!(&p0[8..12], &[0, 0, 255, 255], "STP colour opaque in cutout mode");
        assert_eq!(&p0[12..16], &[0, 0, 0, 0], "index 3 is 0x0000");
        let semi = t.to_rgba(0, TimAlpha::SemiStp);
        assert_eq!(semi[11], 128);
        let p1 = t.to_rgba(1, TimAlpha::Cutout);
        assert_eq!(&p1[4..8], &[0, 255, 0, 255]);
    }

    #[test]
    fn eight_bit_and_black_stp_is_opaque() {
        let mut clut = vec![0u16; 256];
        clut[200] = 0x8000; // opaque black
        let b = build(
            9,
            Some((VramRect { x: 0, y: 500, w: 256, h: 1 }, &clut)),
            VramRect { x: 0, y: 0, w: 1, h: 1 },
            &[0x00C8],
        );
        let t = Tim::parse(&b).unwrap();
        assert_eq!((t.width(), t.palette_count()), (2, 1));
        assert_eq!(t.to_rgba(0, TimAlpha::Cutout), vec![0, 0, 0, 255, 0, 0, 0, 0]);
    }

    #[test]
    fn sixteen_and_twenty_four_bit() {
        let b = build(2, None, VramRect { x: 0, y: 0, w: 2, h: 1 }, &[0x7FFF, 0]);
        let t = Tim::parse(&b).unwrap();
        assert_eq!(t.to_rgba(0, TimAlpha::Cutout), vec![255, 255, 255, 255, 0, 0, 0, 0]);
        // 24-bit: 2 pixels = 6 bytes = 3 halfwords. RGB (1,2,3), (4,5,6).
        let b = build(3, None, VramRect { x: 0, y: 0, w: 3, h: 1 }, &[0x0201, 0x0403, 0x0605]);
        let t = Tim::parse(&b).unwrap();
        assert_eq!(t.width(), 2);
        assert_eq!(t.to_rgba(0, TimAlpha::Cutout), vec![1, 2, 3, 255, 4, 5, 6, 255]);
    }

    #[test]
    fn rejects_bad_lengths_and_flags() {
        let mut b = build(2, None, VramRect { x: 0, y: 0, w: 2, h: 1 }, &[1, 2]);
        assert!(Tim::parse(&b[..b.len() - 1]).is_err());
        b[8] += 2; // block length
        assert!(Tim::parse(&b).is_err());
        let b = build(0x12, None, VramRect { x: 0, y: 0, w: 2, h: 1 }, &[1, 2]);
        assert!(Tim::parse(&b).is_err());
        let b = build(2, None, VramRect { x: 1020, y: 0, w: 8, h: 1 }, &[0; 8]);
        assert!(Tim::parse(&b).is_err());
    }

    #[test]
    fn finds_every_tim_in_an_archive() {
        let a = build(2, None, VramRect { x: 0, y: 0, w: 2, h: 1 }, &[1, 2]);
        let mut archive = vec![0x10, 0, 0, 0, 0x55];
        archive.extend_from_slice(&a);
        archive.push(7);
        archive.extend_from_slice(&a);
        let found: Vec<usize> = Tim::find_all(&archive).into_iter().map(|(o, _)| o).collect();
        assert_eq!(found, vec![5, 6 + a.len()]);
    }

    #[test]
    fn vram_pages_decode_through_cluts() {
        let mut clut = vec![0u16; 16];
        clut[5] = RED;
        let b = build(
            8,
            Some((VramRect { x: 32, y: 500, w: 16, h: 1 }, &clut)),
            VramRect { x: 128, y: 256, w: 1, h: 1 },
            &[0x0055],
        );
        let mut vram = Vram::default();
        vram.load_tim(&Tim::parse(&b).unwrap());
        // TSB: page 2 (x=128), y=256 (bit 4), 4-bit. CBA: x=32/16=2, y=500.
        let page = TexPage::from_tsb(2 | 0x10);
        assert_eq!((page.x, page.y, page.bits), (128, 256, 4));
        let clut_xy = clut_from_cba(2 | (500 << 6));
        assert_eq!(clut_xy, (32, 500));
        let (img, cover) = vram.decode_page(page, clut_xy, TimAlpha::Cutout);
        assert_eq!(&img[0..4], &[255, 0, 0, 255]);
        assert_eq!(&img[4 * 4..4 * 4 + 4], &[0, 0, 0, 0], "texel 4 is outside the upload");
        assert!(cover > 0.0 && cover < 0.001);
    }
}
