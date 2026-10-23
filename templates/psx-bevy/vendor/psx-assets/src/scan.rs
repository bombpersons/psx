//! Find TIM, TMD, VAB/VH and VAG data inside arbitrary bytes: loose files,
//! game archives (which mostly store assets unmodified at some offset) and RAM
//! dumps. Candidates are found by their id word and kept only if the whole
//! structure validates (block lengths, VRAM bounds, table pointers, vertex
//! indices, ...). Compressed or encrypted data is invisible to this.

use serde::Serialize;

use crate::tim::Tim;
use crate::tmd::Tmd;
use crate::vab::{Vab, Vag};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Tim,
    Tmd,
    Vab,
    Vag,
}

impl Kind {
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s.to_ascii_lowercase().as_str() {
            "tim" => Kind::Tim,
            "tmd" => Kind::Tmd,
            "vab" | "vh" => Kind::Vab,
            "vag" => Kind::Vag,
            _ => return None,
        })
    }

    pub fn ext(self) -> &'static str {
        match self {
            Kind::Tim => "tim",
            Kind::Tmd => "tmd",
            Kind::Vab => "vab",
            Kind::Vag => "vag",
        }
    }
}

/// Format-specific summary of a hit.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum Info {
    Tim {
        bpp: u32,
        width: u32,
        height: u32,
        palettes: usize,
        /// VRAM `[x, y, w, h]` (w in halfwords) the TIM uploads its pixels to.
        image_vram: [u16; 4],
        clut_vram: Option<[u16; 4]>,
    },
    Tmd {
        objects: usize,
        vertices: usize,
        polygons: usize,
        textured: usize,
        skipped: u32,
        /// Pointers already relocated to RAM addresses (found in RAM dumps).
        fixp: bool,
    },
    Vab {
        programs: usize,
        tones: usize,
        waveforms: usize,
        header_size: usize,
        body_size: usize,
        /// The VB follows the VH here (a `.VAB`, possibly after a u32 size);
        /// false for a lone `.VH`.
        body_included: bool,
    },
    Vag {
        rate: u32,
        name: String,
        data_size: usize,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    pub offset: usize,
    pub kind: Kind,
    /// Bytes the asset occupies (0 when unknown, e.g. a relocated TMD).
    pub size: usize,
    #[serde(flatten)]
    pub info: Info,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Address of `bytes[0]` when scanning a RAM dump (resolves relocated TMDs).
    pub ram_base: Option<u32>,
}

fn tim_info(t: &Tim) -> Info {
    let r = |r: crate::tim::VramRect| [r.x, r.y, r.w, r.h];
    Info::Tim {
        bpp: t.depth.bits(),
        width: t.width(),
        height: t.height(),
        palettes: t.palette_count(),
        image_vram: r(t.image_rect),
        clut_vram: t.clut_rect.map(r),
    }
}

/// Shared model summary for the standard scanner and explicit format adapters.
pub fn tmd_info(t: &Tmd) -> Info {
    Info::Tmd {
        objects: t.objects.len(),
        vertices: t.vertex_count(),
        polygons: t.polygon_count(),
        textured: t.objects.iter().flat_map(|o| &o.polygons).filter(|p| p.textured()).count(),
        skipped: t.objects.iter().map(|o| o.skipped).sum(),
        fixp: t.fixp,
    }
}

/// Try every format at `offset`.
pub fn probe(bytes: &[u8], offset: usize, opts: Options) -> Option<Hit> {
    let b = &bytes[offset..];
    if b.len() < 8 {
        return None;
    }
    let hit = |kind, size, info| Some(Hit { offset, kind, size, info });
    match &b[..4] {
        [0x10, 0, 0, 0] if b[5..8] == [0, 0, 0] => {
            let t = Tim::parse(b).ok().filter(Tim::plausible)?;
            hit(Kind::Tim, t.size, tim_info(&t))
        }
        [0x41, 0, 0, 0] if b[5..8] == [0, 0, 0] && b[4] <= 1 => {
            let t = Tmd::parse_at(bytes, offset, opts.ram_base).ok()?;
            let polygons = t.polygon_count();
            if polygons == 0 {
                return None;
            }
            let skipped: u32 = t.objects.iter().map(|o| o.skipped).sum();
            // Mostly-unparsed packets mean this is not really a TMD.
            if skipped as usize > polygons * 4 {
                return None;
            }
            hit(Kind::Tmd, t.size, tmd_info(&t))
        }
        b"pBAV" => {
            let v = Vab::parse(b).ok()?;
            let body = v.body_offset(b);
            let body_included = body.is_some();
            let size = body.map_or(v.header_size, |at| at + v.body_size);
            let info = Info::Vab {
                programs: v.programs.len(),
                tones: v.programs.iter().map(|p| p.tones.len()).sum(),
                waveforms: v.vag_sizes.len(),
                header_size: v.header_size,
                body_size: v.body_size,
                body_included,
            };
            hit(Kind::Vab, size, info)
        }
        b"VAGp" => {
            let v = Vag::parse(b).ok()?;
            hit(Kind::Vag, v.size, Info::Vag { rate: v.rate, name: v.name.clone(), data_size: v.data_size })
        }
        _ => None,
    }
}

/// All hits in `bytes`, in offset order. After a hit the scan resumes past
/// its end, so pixel data or sample data never yields nested false hits.
pub fn scan(bytes: &[u8], opts: Options) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut i = 0;
    while i + 8 <= bytes.len() {
        let c = bytes[i];
        if matches!(c, 0x10 | 0x41 | b'p' | b'V') {
            if let Some(h) = probe(bytes, i, opts) {
                i += h.size.max(4);
                hits.push(h);
                continue;
            }
        }
        i += 1;
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vab::tests::{vab_bytes, vag_bytes};

    fn tim16(w: u16, h: u16) -> Vec<u8> {
        let mut b = Vec::new();
        for v in [0x10u32, 2, 12 + 2 * (w as u32 * h as u32)] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for v in [0u16, 0, w, h] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        for i in 0..w as u32 * h as u32 {
            b.extend_from_slice(&(0x10u16 + i as u16).to_le_bytes());
        }
        b
    }

    fn tmd() -> Vec<u8> {
        let mut b = Vec::new();
        let words: [u32; 3 + 7 + 6 + 4] = [
            0x41,
            0,
            1, // header
            28,
            3,
            52,
            0,
            52,
            1,
            0, // object: 3 vertices at 28, 0 normals, 1 primitive at 52
            0,
            0,
            100,
            0,
            100 << 16,
            0,                                   // vertices (x,y),(z,pad)
            u32::from_le_bytes([0, 3, 1, 0x21]), // unlit flat tri
            0x21FF_FFFF,
            1 << 16,
            2,
        ];
        for w in words {
            b.extend_from_slice(&w.to_le_bytes());
        }
        b
    }

    #[test]
    fn finds_assets_at_odd_offsets_in_noise() {
        let mut data = vec![0x41u8, 0, 0, 0, 0, 0, 0, 0, 0, 0x10, 0, 0, 0]; // decoys
        data.extend_from_slice(&[0xAB; 7]);
        let tim_at = data.len();
        data.extend_from_slice(&tim16(4, 2));
        data.extend_from_slice(&[1, 2, 3]);
        let tmd_at = data.len();
        data.extend_from_slice(&tmd());
        let vag_at = data.len();
        data.extend_from_slice(&vag_bytes(11025));
        let vab_at = data.len();
        data.extend_from_slice(&vab_bytes());
        data.extend_from_slice(&[0x10, 0, 0, 0, 2, 0, 0, 0, 0xFF]); // truncated TIM
        let hits = scan(&data, Options::default());
        let got: Vec<(usize, Kind)> = hits.iter().map(|h| (h.offset, h.kind)).collect();
        assert_eq!(got, vec![(tim_at, Kind::Tim), (tmd_at, Kind::Tmd), (vag_at, Kind::Vag), (vab_at, Kind::Vab)]);
        assert!(matches!(hits[0].info, Info::Tim { width: 4, height: 2, .. }));
        assert!(matches!(hits[1].info, Info::Tmd { polygons: 1, .. }));
        assert!(matches!(hits[3].info, Info::Vab { body_included: true, .. }));
        assert_eq!(hits[0].size, tim16(4, 2).len());
    }

    #[test]
    fn lone_vh_is_reported_without_body() {
        let b = vab_bytes();
        let v = Vab::parse(&b).unwrap();
        let hits = scan(&b[..v.header_size], Options::default());
        assert!(matches!(hits[0].info, Info::Vab { body_included: false, .. }));
    }
}
