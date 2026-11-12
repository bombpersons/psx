//! `psx assets textures`: decode VRAM texture pages through their CLUTs into PNGs,
//! either every page/CLUT pair a captured frame used (`gpu_frame.json`) or
//! one page given by hand.
//!
//! PNG alpha carries each texel's transparency class as the GPU sees it:
//! 0 for texel 0000h (never drawn), 128 for texels with the STP bit (bit 15)
//! set (blended when the primitive is semi-transparent), 255 for the rest
//! (always opaque). 4/8-bit pages are also written as raw indices
//! (`*_index.png`, 8-bit grayscale) with each CLUT as a 16/256×1 RGBA palette
//! (`clut_X_Y_N.png`, same alpha code), for palette-swapping materials.

use std::collections::BTreeMap;
use std::path::Path;

use crate::Error;
use crate::commands::{Ctx, Op, Outcome};
use psx_gpu::raster::TexelSource;
use psx_gpu::{Vram, VramTexture};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(clap::Args, Deserialize, JsonSchema)]
pub struct TexturesArgs {
    /// A run directory or a debugger stop's (vram.bin, and gpu_frame.json unless --page).
    pub run_dir: PathBuf,
    /// One page as X,Y,DEPTH (VRAM halfword x, line y, 4/8/15 bpp).
    #[arg(long)]
    pub page: Option<String>,
    /// CLUT position X,Y for 4/8-bit pages.
    #[arg(long)]
    pub clut: Option<String>,
    /// Output directory (default <run_dir>/textures).
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// VRAM dump to decode. Default: gpu_frame_vram.bin (the VRAM at the end of the
    /// captured frame) when present and no --page is given, else vram.bin (end of the run).
    #[arg(long)]
    pub vram: Option<PathBuf>,
}

impl Op for TexturesArgs {
    fn exec(self, _: &Ctx) -> Outcome {
        Outcome::result(run(
            &self.run_dir,
            self.page.as_deref(),
            self.clut.as_deref(),
            self.out.as_deref(),
            self.vram.as_deref(),
        ))
    }
}

type R = crate::Result<Value>;
/// (page x, page y, depth, clut x, clut y) -> (UV bounds u0 v0 u1 v1, uses).
type Pairs = BTreeMap<(i64, i64, i64, i64, i64), ([i64; 4], u64)>;

fn depth_bits(depth: u8) -> crate::Result<u16> {
    match depth {
        4 => Ok(0),
        8 => Ok(1),
        15 | 16 => Ok(2),
        d => Err(Error::invalid(format!("texture depth {d} (4, 8 or 15)"))),
    }
}

/// PNG alpha for a 15-bit texel or CLUT entry: 0 transparent (0000h),
/// 128 STP bit set (semi-transparent when the primitive is), 255 opaque.
fn stp_alpha(t: u16) -> u8 {
    match t {
        0 => 0,
        t if t & 0x8000 != 0 => 128,
        _ => 255,
    }
}

/// A 256×256 page as RGBA (alpha from [`stp_alpha`]), cropped to `(u0, v0, u1, v1)`.
fn decode_page(
    vram: &Vram,
    x: i32,
    y: i32,
    depth: u8,
    clut: Option<(i32, i32)>,
    crop: [i32; 4],
) -> crate::Result<(u32, u32, Vec<u8>)> {
    let texpage = ((x / 64) & 0xF) as u16 | (((y / 256) & 1) as u16) << 4 | depth_bits(depth)? << 7;
    let (cx, cy) = clut.unwrap_or((0, 0));
    let tex = VramTexture { texpage, clut_x: cx, clut_y: cy, window: (0, 0, 0, 0) };
    let [u0, v0, u1, v1] = crop;
    let (w, h) = ((u1 - u0 + 1) as u32, (v1 - v0 + 1) as u32);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for v in v0..=v1 {
        for u in u0..=u1 {
            let t = tex.texel(vram, u, v);
            let [r, g, b] = psx_gpu::vram::rgb15_to_rgb8(t);
            out.extend_from_slice(&[r, g, b, stp_alpha(t)]);
        }
    }
    Ok((w, h, out))
}

/// Raw CLUT indices of a 4/8-bit 256×256 page, one byte per texel.
fn page_indices(vram: &Vram, x: i32, y: i32, depth: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(256 * 256);
    for v in 0..256 {
        for u in 0..256 {
            out.push(if depth == 4 {
                (vram.get(x + u / 4, y + v) >> ((u % 4) * 4) & 0xF) as u8
            } else {
                (vram.get(x + u / 2, y + v) >> ((u % 2) * 8) & 0xFF) as u8
            });
        }
    }
    out
}

/// A CLUT as an N×1 RGBA palette (alpha from [`stp_alpha`]).
fn clut_rgba(vram: &Vram, x: i32, y: i32, entries: i32) -> Vec<u8> {
    (0..entries)
        .flat_map(|i| {
            let t = vram.get(x + i, y);
            let [r, g, b] = psx_gpu::vram::rgb15_to_rgb8(t);
            [r, g, b, stp_alpha(t)]
        })
        .collect()
}

fn load_vram(path: &Path) -> crate::Result<Vram> {
    let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
    if bytes.len() != 1024 * 512 * 2 {
        return Err(Error::malformed(format!("{}: {} bytes, VRAM is 1048576", path.display(), bytes.len())));
    }
    let mut v = Vram::default();
    v.load_bytes(&bytes);
    Ok(v)
}

fn parse_list(s: &str, n: usize) -> crate::Result<Vec<i32>> {
    let v: Vec<i32> = s
        .split(',')
        .map(|x| x.trim().parse::<i32>())
        .collect::<Result<_, _>>()
        .map_err(|_| Error::invalid(format!("bad list {s:?}")))?;
    if v.len() != n {
        return Err(Error::invalid(format!("{s:?}: expected {n} comma-separated numbers")));
    }
    Ok(v)
}

/// The VRAM to decode: `--vram` if given; for a captured frame the VRAM at
/// the end of that frame (gpu_frame_vram.bin) when the run wrote it, else the
/// run's final vram.bin.
fn pick_vram(run_dir: &Path, explicit: Option<&Path>, frame_mode: bool) -> std::path::PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    let at_frame = run_dir.join("gpu_frame_vram.bin");
    if frame_mode && at_frame.is_file() { at_frame } else { run_dir.join("vram.bin") }
}

pub fn run(run_dir: &Path, page: Option<&str>, clut: Option<&str>, out: Option<&Path>, vram_path: Option<&Path>) -> R {
    let vram_file = pick_vram(run_dir, vram_path, page.is_none());
    let vram = load_vram(&vram_file)?;
    let out_dir = out.map(Path::to_path_buf).unwrap_or_else(|| run_dir.join("textures"));
    std::fs::create_dir_all(&out_dir).map_err(|e| Error::io(&out_dir, e))?;
    let save = |name: String, (w, h, rgba): (u32, u32, Vec<u8>)| -> crate::Result<Value> {
        let p = out_dir.join(&name);
        crate::run::write_png(&p, w, h, &rgba, png::ColorType::Rgba).map_err(|e| Error::io(&p, e))?;
        Ok(json!({"path": p, "width": w, "height": h}))
    };
    if let Some(page) = page {
        let p = parse_list(page, 3)?;
        let c = clut.map(|c| parse_list(c, 2)).transpose()?.map(|c| (c[0], c[1]));
        if p[2] != 15 && c.is_none() {
            return Err(Error::invalid("4/8-bit pages need --clut X,Y"));
        }
        let name = format!(
            "page_{}_{}_{}bpp{}.png",
            p[0],
            p[1],
            p[2],
            c.map(|(x, y)| format!("_clut_{x}_{y}")).unwrap_or_default()
        );
        let file = save(name, decode_page(&vram, p[0], p[1], p[2] as u8, c, [0, 0, 255, 255])?)?;
        return Ok(json!({"vram": vram_file, "textures": [file]}));
    }
    // Every page/CLUT pair the captured frame sampled, with the UV bounds used.
    let frame_path = run_dir.join("gpu_frame.json");
    let text = std::fs::read_to_string(&frame_path).map_err(|e| Error::io(&frame_path, e))?;
    let frame: Value =
        serde_json::from_str(&text).map_err(|e| Error::malformed(format!("{}: {e}", frame_path.display())))?;
    let mut pairs = Pairs::new();
    for c in frame["commands"].as_array().into_iter().flatten() {
        if c["textured"] != json!(true) {
            continue;
        }
        let t = &c["texpage"];
        let (px, py, d) =
            (t["x"].as_i64().unwrap_or(0), t["y"].as_i64().unwrap_or(0), t["depth"].as_i64().unwrap_or(15));
        let (cx, cy) = (c["clut"][0].as_i64().unwrap_or(-1), c["clut"][1].as_i64().unwrap_or(-1));
        let e = pairs.entry((px, py, d, cx, cy)).or_insert(([255, 255, 0, 0], 0));
        e.1 += 1;
        let verts = c["vertices"].as_array().cloned().unwrap_or_default();
        // Rects sample uv..uv+size; polygons the hull of their vertex UVs.
        let mut uvs: Vec<(i64, i64)> =
            verts.iter().filter_map(|v| Some((v["uv"][0].as_i64()?, v["uv"][1].as_i64()?))).collect();
        if c["kind"] == "rect" {
            if let (Some(&(u, v)), Some(w), Some(h)) = (uvs.first(), c["area"][2].as_i64(), c["area"][3].as_i64()) {
                uvs.push(((u + w - 1).min(255), (v + h - 1).min(255)));
            }
        }
        for (u, v) in uvs {
            e.0 = [e.0[0].min(u), e.0[1].min(v), e.0[2].max(u), e.0[3].max(v)];
        }
    }
    let mut list = Vec::new();
    for ((px, py, d, cx, cy), (uv, uses)) in pairs {
        let clut = (cx >= 0).then_some((cx as i32, cy as i32));
        let base = format!("page_{px}_{py}_{d}bpp{}", clut.map(|(x, y)| format!("_clut_{x}_{y}")).unwrap_or_default());
        let full =
            save(format!("{base}.png"), decode_page(&vram, px as i32, py as i32, d as u8, clut, [0, 0, 255, 255])?)?;
        let crop = [uv[0] as i32, uv[1] as i32, uv[2].max(uv[0]) as i32, uv[3].max(uv[1]) as i32];
        let used = save(format!("{base}_used.png"), decode_page(&vram, px as i32, py as i32, d as u8, clut, crop)?)?;
        let mut entry = json!({"page": {"x": px, "y": py, "depth": d}, "clut": clut, "uses": uses, "uv_bounds": crop, "full": full, "used": used});
        if let (Some((cx, cy)), 4 | 8) = (clut, d) {
            // The same page as indices + palette, for palette-swap materials.
            let p = out_dir.join(format!("page_{px}_{py}_{d}bpp_index.png"));
            let indices = page_indices(&vram, px as i32, py as i32, d as u8);
            crate::run::write_png(&p, 256, 256, &indices, png::ColorType::Grayscale).map_err(|e| Error::io(&p, e))?;
            let n = if d == 4 { 16 } else { 256 };
            let palette = save(format!("clut_{cx}_{cy}_{n}.png"), (n as u32, 1, clut_rgba(&vram, cx, cy, n)))?;
            entry["index"] = json!(p);
            entry["palette"] = palette["path"].clone();
        }
        list.push(entry);
    }
    Ok(json!({
        "source": frame_path,
        "vram": vram_file,
        "note": "decoded from `vram`: gpu_frame_vram.bin is the VRAM at the end of the captured frame, vram.bin the run's final VRAM (older captures only have vram.bin); PNG alpha 0 = transparent (0000h), 128 = STP bit (blends on semi-transparent primitives), 255 = opaque",
        "textures": list,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_encodes_transparency_class() {
        assert_eq!(stp_alpha(0x0000), 0);
        assert_eq!(stp_alpha(0x8000), 128, "black with STP is drawn, and blends");
        assert_eq!(stp_alpha(0x801F), 128);
        assert_eq!(stp_alpha(0x001F), 255);
    }

    #[test]
    fn indices_and_clut_follow_vram_layout() {
        let mut vram = Vram::default();
        vram.set(64, 0, 0x4321); // 4-bit: texels 0..3 = 1, 2, 3, 4
        vram.set(128, 0, 0xBBAA); // 8-bit: texels 0, 1 = AAh, BBh
        vram.set(0, 480, 0x8000);
        vram.set(1, 480, 0x7FFF);
        assert_eq!(&page_indices(&vram, 64, 0, 4)[..4], &[1, 2, 3, 4]);
        assert_eq!(&page_indices(&vram, 128, 0, 8)[..2], &[0xAA, 0xBB]);
        assert_eq!(clut_rgba(&vram, 0, 480, 3), vec![0, 0, 0, 128, 255, 255, 255, 255, 0, 0, 0, 0]);
    }
}
