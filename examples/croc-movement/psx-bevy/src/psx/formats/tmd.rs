//! TMD: the PlayStation's standard 3D model file (PsyQ "File Formats", TMD).
//!
//! ```text
//! u32 id = 0x41
//! u32 flags         bit 0 FIXP: object table pointers are absolute addresses
//! u32 nobj
//! nobj x { u32 vert_top, n_vert, normal_top, n_normal, primitive_top, n_primitive; i32 scale }
//! ```
//!
//! Pointers are byte offsets from the object table (byte 12) unless FIXP is
//! set, which is what `GsMapModelingData` leaves in RAM after relocating a
//! model; such models can be read from a RAM dump given its base address.
//! Vertices and normals are `{i16 x, y, z, pad}` (normals 1.3.12 fixed point).
//! Each primitive is a header word `olen, ilen, flag, mode` (bytes, low first)
//! followed by `ilen` words. For polygons (mode bits 7-5 = 001) the packet
//! holds, in order: UV words when textured (`u, v, CBA` then `u, v, TSB`,
//! then `u, v, pad`), colour words (one, or one per vertex for Gouraud
//! colours), and the normal/vertex indices as u16s. The layout follows from
//! the mode (IIP, QUAD, TME) and flag (LGT, GRD) bits; it is checked against
//! `ilen`, and packets that do not match are counted as skipped.
//!
//! This file is dependency-free and shared verbatim with the Bevy template
//! (`templates/psx-bevy/src/psx/formats/tmd.rs`); a test keeps the copies equal.

/// First word of every TMD.
pub const TMD_ID: u32 = 0x41;

/// Mode bits (primitive header byte 3) for polygons.
pub const MODE_TGE: u8 = 0x01; // texture brightness (modulation) off
pub const MODE_ABE: u8 = 0x02; // semi-transparency on
pub const MODE_TME: u8 = 0x04; // textured
pub const MODE_QUAD: u8 = 0x08;
pub const MODE_IIP: u8 = 0x10; // Gouraud
/// Flag bits (primitive header byte 2).
pub const FLAG_LGT: u8 = 0x01; // light source calculation off (colours used as-is)
pub const FLAG_FCE: u8 = 0x02; // double-sided
pub const FLAG_GRD: u8 = 0x04; // per-vertex colours on a lit untextured polygon

/// One polygon: 3 or 4 corners, each with a vertex index and the attributes
/// the packet supplies (per corner, or the first entry shared).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TmdPolygon {
    pub mode: u8,
    pub flag: u8,
    /// Indices into the object's vertex table.
    pub vertices: Vec<u16>,
    /// Indices into the object's normal table: empty (unlit), one (flat) or per corner.
    pub normals: Vec<u16>,
    /// RGB: none (lit textured), one (flat) or per corner.
    pub colors: Vec<[u8; 3]>,
    /// Texel coordinates within the texture page, per corner (textured only).
    pub uvs: Vec<[u8; 2]>,
    /// CLUT (CBA) and texture page (TSB) fields, textured only.
    pub cba: u16,
    pub tsb: u16,
}

impl TmdPolygon {
    pub fn textured(&self) -> bool {
        self.mode & MODE_TME != 0
    }
    pub fn semi_transparent(&self) -> bool {
        self.mode & MODE_ABE != 0
    }
    pub fn lit(&self) -> bool {
        self.flag & FLAG_LGT == 0
    }
    pub fn double_sided(&self) -> bool {
        self.flag & FLAG_FCE != 0
    }
    /// Corner triples to draw: (0,1,2) and, for quads, (1,3,2) as the GPU splits them.
    pub fn triangles(&self) -> &'static [[usize; 3]] {
        if self.vertices.len() == 4 { &[[0, 1, 2], [1, 3, 2]] } else { &[[0, 1, 2]] }
    }
    /// Colour of corner `i` (white when the packet has none).
    pub fn color(&self, i: usize) -> [u8; 3] {
        self.colors.get(i).or(self.colors.first()).copied().unwrap_or([128, 128, 128])
    }
    /// Normal index of corner `i`, if the packet has normals.
    pub fn normal(&self, i: usize) -> Option<u16> {
        self.normals.get(i).or(self.normals.first()).copied()
    }
}

#[derive(Clone, Debug, Default)]
pub struct TmdObject {
    pub vertices: Vec<[i16; 3]>,
    pub normals: Vec<[i16; 3]>,
    pub polygons: Vec<TmdPolygon>,
    /// Scale exponent field (unused by libgs; kept for reference).
    pub scale: i32,
    /// Lines, sprites and packets whose layout did not match `ilen`.
    pub skipped: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Tmd {
    pub fixp: bool,
    pub objects: Vec<TmdObject>,
    /// Bytes from the header to the end of the furthest table (0 when FIXP).
    pub size: usize,
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(o)?, *b.get(o + 1)?, *b.get(o + 2)?, *b.get(o + 3)?]))
}

fn svectors(b: &[u8], at: usize, n: usize, what: &str) -> Result<Vec<[i16; 3]>, String> {
    let end = at.checked_add(n * 8).filter(|&e| e <= b.len()).ok_or_else(|| format!("{what} table out of range"))?;
    Ok(b[at..end]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| [i16::from_le_bytes([c[0], c[1]]), i16::from_le_bytes([c[2], c[3]]), i16::from_le_bytes([c[4], c[5]])])
        .collect())
}

/// Byte range containing just vertex/normal u16 references within a polygon's
/// payload (after its four-byte header), excluding any final padding halfword.
/// This layout is shared by decoding and explicit preprocessed-TMD adapters.
pub fn polygon_reference_range(mode: u8, flag: u8, ilen: usize) -> Option<std::ops::Range<usize>> {
    if mode >> 5 != 1 {
        return None;
    }
    let n: usize = if mode & MODE_QUAD != 0 { 4 } else { 3 };
    let textured = mode & MODE_TME != 0;
    let lit = flag & FLAG_LGT == 0;
    let gouraud = mode & MODE_IIP != 0;
    let n_colors = match (lit, textured) {
        (true, true) => 0,
        (true, false) if flag & FLAG_GRD != 0 => n,
        (true, false) => 1,
        (false, _) if gouraud || flag & FLAG_GRD != 0 => n,
        (false, _) => 1,
    };
    let n_uv = if textured { n } else { 0 };
    let n_index = match (lit, gouraud) {
        (true, true) => 2 * n,
        (true, false) => 1 + n,
        (false, _) => n,
    };
    if n_uv + n_colors + n_index.div_ceil(2) != ilen {
        return None;
    }
    let start = (n_uv + n_colors) * 4;
    Some(start..start + n_index * 2)
}

/// Decode one polygon packet (the words after the header). `None` when the
/// layout implied by mode/flag does not match `ilen`.
fn polygon(mode: u8, flag: u8, words: &[u32]) -> Option<TmdPolygon> {
    let references = polygon_reference_range(mode, flag, words.len())?;
    let n: usize = if mode & MODE_QUAD != 0 { 4 } else { 3 };
    let textured = mode & MODE_TME != 0;
    let lit = flag & FLAG_LGT == 0;
    let gouraud = mode & MODE_IIP != 0;
    let n_uv = if textured { n } else { 0 };
    let n_colors = references.start / 4 - n_uv;
    let mut p = TmdPolygon { mode, flag, ..Default::default() };
    for (i, &w) in words[..n_uv].iter().enumerate() {
        p.uvs.push([w as u8, (w >> 8) as u8]);
        match i {
            0 => p.cba = (w >> 16) as u16,
            1 => p.tsb = (w >> 16) as u16,
            _ => {}
        }
    }
    for &w in &words[n_uv..n_uv + n_colors] {
        p.colors.push([w as u8, (w >> 8) as u8, (w >> 16) as u8]);
    }
    let idx: Vec<u16> = words[n_uv + n_colors..].iter().flat_map(|&w| [w as u16, (w >> 16) as u16]).collect();
    match (lit, gouraud) {
        (true, true) => {
            for c in idx[..2 * n].as_chunks::<2>().0 {
                p.normals.push(c[0]);
                p.vertices.push(c[1]);
            }
        }
        (true, false) => {
            p.normals.push(idx[0]);
            p.vertices.extend_from_slice(&idx[1..1 + n]);
        }
        (false, _) => p.vertices.extend_from_slice(&idx[..n]),
    }
    Some(p)
}

impl Tmd {
    /// Parse a TMD at the start of `bytes` (relative pointers only).
    pub fn parse(bytes: &[u8]) -> Result<Tmd, String> {
        Tmd::parse_at(bytes, 0, None)
    }

    /// Parse a TMD at `offset` in `buf`. `ram_base` is the address of
    /// `buf[0]` when `buf` is a RAM dump, which lets FIXP (relocated) models
    /// resolve their absolute pointers.
    pub fn parse_at(buf: &[u8], offset: usize, ram_base: Option<u32>) -> Result<Tmd, String> {
        if u32_at(buf, offset) != Some(TMD_ID) {
            return Err("not a TMD (id != 0x41)".into());
        }
        let flags = u32_at(buf, offset + 4).ok_or("TMD header truncated")?;
        if flags > 1 {
            return Err(format!("TMD flags 0x{flags:X} not 0 or 1"));
        }
        let fixp = flags == 1;
        let nobj = u32_at(buf, offset + 8).ok_or("TMD header truncated")? as usize;
        if nobj == 0 || nobj > 4096 {
            return Err(format!("implausible object count {nobj}"));
        }
        let table = offset + 12;
        if table + nobj * 28 > buf.len() {
            return Err("object table truncated".into());
        }
        // KSEG0/KSEG1/KUSEG mirrors fold onto the dump's base.
        let resolve = |p: u32| -> Result<usize, String> {
            if !fixp {
                return Ok(table + p as usize);
            }
            let base = ram_base.ok_or("FIXP (relocated) TMD needs a RAM base address")?;
            (p & 0x1FFF_FFFF)
                .checked_sub(base & 0x1FFF_FFFF)
                .map(|o| o as usize)
                .ok_or_else(|| format!("pointer 0x{p:08X} below the RAM base"))
        };
        let mut tmd = Tmd { fixp, objects: Vec::with_capacity(nobj), size: table - offset + nobj * 28 };
        for k in 0..nobj {
            let o = table + k * 28;
            let f = |i: usize| u32_at(buf, o + 4 * i).unwrap_or(0);
            let (vert_top, n_vert, normal_top, n_normal, prim_top, n_prim) =
                (f(0), f(1) as usize, f(2), f(3) as usize, f(4), f(5) as usize);
            if n_vert > 0x10000 || n_normal > 0x10000 || n_prim > 0x10000 {
                return Err(format!("object {k}: implausible counts {n_vert}/{n_normal}/{n_prim}"));
            }
            if !fixp && (vert_top % 4 != 0 || normal_top % 4 != 0 || prim_top % 4 != 0) {
                return Err(format!("object {k}: unaligned table pointer"));
            }
            let (va, na, pa) = (resolve(vert_top)?, resolve(normal_top)?, resolve(prim_top)?);
            let mut obj = TmdObject {
                vertices: svectors(buf, va, n_vert, "vertex")?,
                normals: svectors(buf, na, n_normal, "normal")?,
                scale: f(6) as i32,
                ..Default::default()
            };
            let mut at = pa;
            for i in 0..n_prim {
                let head = u32_at(buf, at).ok_or_else(|| format!("object {k}: primitive {i} out of range"))?;
                let (ilen, flag, mode) = (((head >> 8) & 0xFF) as usize, (head >> 16) as u8, (head >> 24) as u8);
                let words: Vec<u32> = (0..ilen)
                    .map(|w| u32_at(buf, at + 4 + 4 * w))
                    .collect::<Option<_>>()
                    .ok_or_else(|| format!("object {k}: primitive {i} truncated"))?;
                at += 4 + 4 * ilen;
                if mode >> 5 != 1 {
                    obj.skipped += 1;
                    continue;
                }
                match polygon(mode, flag, &words) {
                    Some(p) => {
                        if p.vertices.iter().any(|&v| v as usize >= n_vert) {
                            return Err(format!("object {k}: primitive {i} vertex index out of range"));
                        }
                        if p.normals.iter().any(|&v| v as usize >= n_normal) {
                            return Err(format!("object {k}: primitive {i} normal index out of range"));
                        }
                        obj.polygons.push(p);
                    }
                    None => obj.skipped += 1,
                }
            }
            if !fixp {
                tmd.size = tmd.size.max(va + n_vert * 8 - offset).max(na + n_normal * 8 - offset).max(at - offset);
            }
            tmd.objects.push(obj);
        }
        if fixp {
            tmd.size = 0;
        }
        Ok(tmd)
    }

    pub fn polygon_count(&self) -> usize {
        self.objects.iter().map(|o| o.polygons.len()).sum()
    }

    pub fn vertex_count(&self) -> usize {
        self.objects.iter().map(|o| o.vertices.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(lo: u16, hi: u16) -> u32 {
        lo as u32 | (hi as u32) << 16
    }

    fn uv(u: u8, v: u8, extra: u16) -> u32 {
        u as u32 | (v as u32) << 8 | (extra as u32) << 16
    }

    fn rgb(r: u8, g: u8, b: u8, mode: u8) -> u32 {
        u32::from_le_bytes([r, g, b, mode])
    }

    fn prim(flag: u8, mode: u8, words: &[u32]) -> Vec<u32> {
        let mut v = vec![u32::from_le_bytes([0, words.len() as u8, flag, mode])];
        v.extend_from_slice(words);
        v
    }

    /// One object: 4 vertices, 1 normal, the given primitives.
    fn build(prims: &[Vec<u32>]) -> Vec<u8> {
        let mut b = Vec::new();
        let mut put = |w: u32| b.extend_from_slice(&w.to_le_bytes());
        put(TMD_ID);
        put(0);
        put(1);
        // Object table entry at 12; tables follow it (offsets from 12).
        let vert_top = 28u32;
        let normal_top = vert_top + 4 * 8;
        let prim_top = normal_top + 8;
        let n_prim = prims.len() as u32;
        for w in [vert_top, 4, normal_top, 1, prim_top, n_prim, 0] {
            put(w);
        }
        for v in [[0i16, 0, 0], [100, 0, 0], [0, -100, 0], [100, -100, 0]] {
            put(word(v[0] as u16, v[1] as u16));
            put(word(v[2] as u16, 0));
        }
        put(word(0, (-4096i16) as u16));
        put(word(0, 0));
        for p in prims {
            for &w in p {
                put(w);
            }
        }
        b
    }

    #[test]
    fn parses_common_polygon_layouts() {
        let prims = vec![
            // 0x20 lit flat tri: colour, norm0 vert0, vert1 vert2
            prim(0, 0x20, &[rgb(10, 20, 30, 0x20), word(0, 0), word(1, 2)]),
            // 0x30 lit gouraud tri: colour, (norm, vert) x3
            prim(0, 0x30, &[rgb(1, 2, 3, 0x30), word(0, 0), word(0, 1), word(0, 2)]),
            // 0x2C lit flat textured quad: 4 uv, norm0 vert0, vert1 vert2, vert3 pad
            prim(
                0,
                0x2C,
                &[uv(0, 0, 0x7FC0), uv(64, 0, 0x0085), uv(0, 64, 0), uv(64, 64, 0), word(0, 0), word(1, 2), word(3, 0)],
            ),
            // 0x35 unlit gouraud textured tri: 3 uv, 3 colours, v0 v1, v2 pad
            prim(
                1,
                0x35,
                &[
                    uv(1, 2, 5),
                    uv(3, 4, 6),
                    uv(5, 6, 0),
                    rgb(255, 0, 0, 0x35),
                    rgb(0, 255, 0, 0),
                    rgb(0, 0, 255, 0),
                    word(0, 1),
                    word(2, 0),
                ],
            ),
            // 0x29 unlit flat quad: colour, v0 v1, v2 v3
            prim(1, 0x29, &[rgb(9, 9, 9, 0x29), word(0, 1), word(2, 3)]),
            // 0x38 lit gouraud quad with gradation colours: 4 colours, (norm, vert) x4 -> layout mismatch? GRD lit untextured gouraud = 4 colours + 4 words
            prim(
                FLAG_GRD,
                0x38,
                &[
                    rgb(1, 1, 1, 0x38),
                    rgb(2, 2, 2, 0),
                    rgb(3, 3, 3, 0),
                    rgb(4, 4, 4, 0),
                    word(0, 0),
                    word(0, 1),
                    word(0, 2),
                    word(0, 3),
                ],
            ),
            // a line (code 010): skipped
            prim(1, 0x40, &[rgb(0, 0, 0, 0x40), word(0, 1)]),
            // mismatched ilen: skipped
            prim(0, 0x20, &[rgb(0, 0, 0, 0x20), word(0, 0)]),
        ];
        let b = build(&prims);
        let tmd = Tmd::parse(&b).unwrap();
        assert_eq!(tmd.size, b.len());
        let o = &tmd.objects[0];
        assert_eq!(o.vertices[3], [100, -100, 0]);
        assert_eq!(o.normals[0], [0, -4096, 0]);
        assert_eq!(o.polygons.len(), 6);
        assert_eq!(o.skipped, 2);
        let p = &o.polygons[0];
        assert_eq!(
            (p.vertices.as_slice(), p.normals.as_slice(), p.color(2)),
            (&[0u16, 1, 2][..], &[0u16][..], [10, 20, 30])
        );
        assert_eq!(o.polygons[1].normals.len(), 3);
        let q = &o.polygons[2];
        assert!(q.textured() && q.lit());
        assert_eq!((q.cba, q.tsb, q.uvs[3]), (0x7FC0, 0x0085, [64, 64]));
        assert_eq!(q.vertices, vec![0, 1, 2, 3]);
        assert_eq!(q.triangles(), &[[0, 1, 2], [1, 3, 2]]);
        let t = &o.polygons[3];
        assert!(!t.lit() && t.normals.is_empty());
        assert_eq!((t.color(1), t.vertices.as_slice(), t.uvs[2]), ([0, 255, 0], &[0u16, 1, 2][..], [5, 6]));
        assert_eq!(o.polygons[4].vertices, vec![0, 1, 2, 3]);
        assert_eq!(o.polygons[5].colors.len(), 4);
    }

    #[test]
    fn rejects_out_of_range_indices_and_bad_headers() {
        let b = build(&[prim(1, 0x21, &[rgb(0, 0, 0, 0x21), word(0, 1), word(9, 0)])]);
        assert!(Tmd::parse(&b).unwrap_err().contains("vertex index"));
        let mut b = build(&[]);
        b[4] = 2;
        assert!(Tmd::parse(&b).is_err());
        let b = build(&[prim(1, 0x21, &[rgb(0, 0, 0, 0x21), word(0, 1), word(2, 0)])]);
        assert!(Tmd::parse(&b[..b.len() - 2]).is_err());
    }

    #[test]
    fn fixp_models_resolve_against_a_ram_base() {
        let rel = build(&[prim(1, 0x21, &[rgb(0, 0, 0, 0x21), word(0, 1), word(2, 0)])]);
        // Place it at RAM offset 0x100 of a dump based at 0x80000000 and relocate.
        let mut ram = vec![0u8; 0x100];
        ram.extend_from_slice(&rel);
        let base = 0x8000_0000u32;
        ram[0x104] = 1;
        for field in [0usize, 2, 4] {
            let at = 0x100 + 12 + field * 4;
            let p = u32::from_le_bytes(ram[at..at + 4].try_into().unwrap());
            ram[at..at + 4].copy_from_slice(&(base + 0x100 + 12 + p).to_le_bytes());
        }
        assert!(Tmd::parse_at(&ram, 0x100, None).is_err());
        let tmd = Tmd::parse_at(&ram, 0x100, Some(base)).unwrap();
        assert!(tmd.fixp);
        assert_eq!(tmd.objects[0].polygons.len(), 1);
    }
}
