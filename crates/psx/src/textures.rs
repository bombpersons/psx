//! `psx textures`: decode VRAM texture pages through their CLUTs into PNGs,
//! either every page/CLUT pair a captured frame used (`gpu_frame.json`) or
//! one page given by hand.

use std::collections::BTreeMap;
use std::path::Path;

use psx_gpu::raster::TexelSource;
use psx_gpu::{Vram, VramTexture};
use psx_media::MediaError;
use serde_json::{json, Value};

type R = psx_media::Result<Value>;

fn bad(m: impl std::fmt::Display) -> MediaError {
    MediaError::Malformed(m.to_string())
}

fn depth_bits(depth: u8) -> psx_media::Result<u16> {
    match depth {
        4 => Ok(0),
        8 => Ok(1),
        15 | 16 => Ok(2),
        d => Err(bad(format!("texture depth {d} (4, 8 or 15)"))),
    }
}

/// A 256×256 page as RGBA (texel 0000h transparent), cropped to `(u0, v0, u1, v1)`.
fn decode_page(vram: &Vram, x: i32, y: i32, depth: u8, clut: Option<(i32, i32)>, crop: [i32; 4]) -> psx_media::Result<(u32, u32, Vec<u8>)> {
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
            out.extend_from_slice(&[r, g, b, if t == 0 { 0 } else { 255 }]);
        }
    }
    Ok((w, h, out))
}

fn load_vram(path: &Path) -> psx_media::Result<Vram> {
    let bytes = std::fs::read(path).map_err(|e| MediaError::io(path, e))?;
    if bytes.len() != 1024 * 512 * 2 {
        return Err(bad(format!("{}: {} bytes, VRAM is 1048576", path.display(), bytes.len())));
    }
    let mut v = Vram::default();
    v.load_bytes(&bytes);
    Ok(v)
}

fn parse_list(s: &str, n: usize) -> psx_media::Result<Vec<i32>> {
    let v: Vec<i32> = s.split(',').map(|x| x.trim().parse::<i32>()).collect::<Result<_, _>>().map_err(|_| bad(format!("bad list {s:?}")))?;
    if v.len() != n {
        return Err(bad(format!("{s:?}: expected {n} comma-separated numbers")));
    }
    Ok(v)
}

pub fn run(run_dir: &Path, page: Option<&str>, clut: Option<&str>, out: Option<&Path>) -> R {
    let vram = load_vram(&run_dir.join("vram.bin"))?;
    let out_dir = out.map(Path::to_path_buf).unwrap_or_else(|| run_dir.join("textures"));
    std::fs::create_dir_all(&out_dir).map_err(|e| MediaError::io(&out_dir, e))?;
    let save = |name: String, (w, h, rgba): (u32, u32, Vec<u8>)| -> psx_media::Result<Value> {
        let p = out_dir.join(&name);
        crate::run::write_png(&p, w, h, &rgba, png::ColorType::Rgba).map_err(|e| MediaError::io(&p, e))?;
        Ok(json!({"path": p, "width": w, "height": h}))
    };
    if let Some(page) = page {
        let p = parse_list(page, 3)?;
        let c = clut.map(|c| parse_list(c, 2)).transpose()?.map(|c| (c[0], c[1]));
        if p[2] != 15 && c.is_none() {
            return Err(bad("4/8-bit pages need --clut X,Y"));
        }
        let name = format!("page_{}_{}_{}bpp{}.png", p[0], p[1], p[2], c.map(|(x, y)| format!("_clut_{x}_{y}")).unwrap_or_default());
        let file = save(name, decode_page(&vram, p[0], p[1], p[2] as u8, c, [0, 0, 255, 255])?)?;
        return Ok(json!({"textures": [file]}));
    }
    // Every page/CLUT pair the captured frame sampled, with the UV bounds used.
    let frame_path = run_dir.join("gpu_frame.json");
    let text = std::fs::read_to_string(&frame_path).map_err(|e| MediaError::io(&frame_path, e))?;
    let frame: Value = serde_json::from_str(&text).map_err(|e| bad(format!("{}: {e}", frame_path.display())))?;
    let mut pairs: BTreeMap<(i64, i64, i64, i64, i64), ([i64; 4], u64)> = BTreeMap::new();
    for c in frame["commands"].as_array().into_iter().flatten() {
        if c["textured"] != json!(true) {
            continue;
        }
        let t = &c["texpage"];
        let (px, py, d) = (t["x"].as_i64().unwrap_or(0), t["y"].as_i64().unwrap_or(0), t["depth"].as_i64().unwrap_or(15));
        let (cx, cy) = (c["clut"][0].as_i64().unwrap_or(-1), c["clut"][1].as_i64().unwrap_or(-1));
        let e = pairs.entry((px, py, d, cx, cy)).or_insert(([255, 255, 0, 0], 0));
        e.1 += 1;
        let verts = c["vertices"].as_array().cloned().unwrap_or_default();
        // Rects sample uv..uv+size; polygons the hull of their vertex UVs.
        let mut uvs: Vec<(i64, i64)> = verts.iter().filter_map(|v| Some((v["uv"][0].as_i64()?, v["uv"][1].as_i64()?))).collect();
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
        let full = save(format!("{base}.png"), decode_page(&vram, px as i32, py as i32, d as u8, clut, [0, 0, 255, 255])?)?;
        let crop = [uv[0] as i32, uv[1] as i32, uv[2].max(uv[0]) as i32, uv[3].max(uv[1]) as i32];
        let used = save(format!("{base}_used.png"), decode_page(&vram, px as i32, py as i32, d as u8, clut, crop)?)?;
        list.push(json!({"page": {"x": px, "y": py, "depth": d}, "clut": clut, "uses": uses, "uv_bounds": crop, "full": full, "used": used}));
    }
    Ok(json!({"source": frame_path, "note": "decoded from the run's final VRAM", "textures": list}))
}
