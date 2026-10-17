//! `psx textures`: decode VRAM texture pages through their CLUTs into PNGs,
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

use psx_gpu::raster::TexelSource;
use psx_gpu::{Vram, VramTexture};
use psx_media::MediaError;
use serde_json::{json, Value};

type R = psx_media::Result<Value>;
/// (page x, page y, depth, clut x, clut y) -> (UV bounds u0 v0 u1 v1, uses).
type Pairs = BTreeMap<(i64, i64, i64, i64, i64), ([i64; 4], u64)>;

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

/// The VRAM to decode: `--vram` if given; for a captured frame the VRAM at
/// the end of that frame (gpu_frame_vram.bin) when the run wrote it, else the
/// run's final vram.bin.
fn pick_vram(run_dir: &Path, explicit: Option<&Path>, frame_mode: bool) -> std::path::PathBuf {
    if let Some(p) = explicit {
        return p.to_path_buf();
    }
    let at_frame = run_dir.join("gpu_frame_vram.bin");
    if frame_mode && at_frame.is_file() {
        at_frame
    } else {
        run_dir.join("vram.bin")
    }
}

pub fn run(run_dir: &Path, page: Option<&str>, clut: Option<&str>, out: Option<&Path>, vram_path: Option<&Path>) -> R {
    let vram_file = pick_vram(run_dir, vram_path, page.is_none());
    let vram = load_vram(&vram_file)?;
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
        return Ok(json!({"vram": vram_file, "textures": [file]}));
    }
    // Every page/CLUT pair the captured frame sampled, with the UV bounds used.
    let frame_path = run_dir.join("gpu_frame.json");
    let text = std::fs::read_to_string(&frame_path).map_err(|e| MediaError::io(&frame_path, e))?;
    let frame: Value = serde_json::from_str(&text).map_err(|e| bad(format!("{}: {e}", frame_path.display())))?;
    let mut pairs = Pairs::new();
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
    let mut images: Vec<String> = Vec::new();
    let mut image_of: BTreeMap<(i64, i64, i64, i64, i64), usize> = BTreeMap::new();
    for ((px, py, d, cx, cy), (uv, uses)) in pairs {
        let clut = (cx >= 0).then_some((cx as i32, cy as i32));
        let base = format!("page_{px}_{py}_{d}bpp{}", clut.map(|(x, y)| format!("_clut_{x}_{y}")).unwrap_or_default());
        let full = save(format!("{base}.png"), decode_page(&vram, px as i32, py as i32, d as u8, clut, [0, 0, 255, 255])?)?;
        image_of.insert((px, py, d, cx, cy), images.len());
        images.push(format!("{base}.png"));
        let crop = [uv[0] as i32, uv[1] as i32, uv[2].max(uv[0]) as i32, uv[3].max(uv[1]) as i32];
        let used = save(format!("{base}_used.png"), decode_page(&vram, px as i32, py as i32, d as u8, clut, crop)?)?;
        let mut entry = json!({"page": {"x": px, "y": py, "depth": d}, "clut": clut, "uses": uses, "uv_bounds": crop, "full": full, "used": used});
        if let (Some((cx, cy)), 4 | 8) = (clut, d) {
            // The same page as indices + palette, for palette-swap materials.
            let p = out_dir.join(format!("page_{px}_{py}_{d}bpp_index.png"));
            let indices = page_indices(&vram, px as i32, py as i32, d as u8);
            crate::run::write_png(&p, 256, 256, &indices, png::ColorType::Grayscale).map_err(|e| MediaError::io(&p, e))?;
            let n = if d == 4 { 16 } else { 256 };
            let palette = save(format!("clut_{cx}_{cy}_{n}.png"), (n as u32, 1, clut_rgba(&vram, cx, cy, n)))?;
            entry["index"] = json!(p);
            entry["palette"] = palette["path"].clone();
        }
        list.push(entry);
    }
    let scene = scene(&frame, &images, &image_of);
    let scene_path = out_dir.join("scene.psxscene");
    let bytes = serde_json::to_vec_pretty(&scene).map_err(|e| bad(e.to_string()))?;
    std::fs::write(&scene_path, bytes).map_err(|e| MediaError::io(&scene_path, e))?;
    Ok(json!({
        "source": frame_path,
        "vram": vram_file,
        "note": "decoded from `vram`: gpu_frame_vram.bin is the VRAM at the end of the captured frame, vram.bin the run's final VRAM (older captures only have vram.bin); PNG alpha 0 = transparent (0000h), 128 = STP bit (blends on semi-transparent primitives), 255 = opaque",
        "textures": list,
        "scene": {"path": scene_path, "elements": scene["elements"].as_array().map_or(0, |a| a.len()), "skipped_primitives": scene["skipped_primitives"]},
    }))
}

/// The frame's 2D layout for the Bevy template (`psx::scene`), in draw order:
/// - `sprite`: a textured rect or axis-aligned textured quad, i.e. an image
///   sub-rectangle at a screen rectangle (with flips), or an untextured fill;
/// - `mesh3d`: a polygon whose vertices the capture lifted back to GTE view
///   space (x right, y down, z forward), with UV and colour;
/// - `mesh`: any other polygon as triangles of screen position, UV and colour
///   (quads split as the hardware draws them: 0-1-2, 1-2-3).
///
/// Positions are relative to the drawing offset (the buffer's screen origin).
/// Lines are counted as skipped.
///
/// Format 2: image alpha 128 marks STP texels, so a `semi` element blends
/// only those (format 1 blended every texel); `dither` says whether the GPU
/// dithers the polygon (shaded or texture-modulated).
fn scene(frame: &Value, images: &[String], image_of: &BTreeMap<(i64, i64, i64, i64, i64), usize>) -> Value {
    let mut elements = Vec::new();
    let mut skipped: BTreeMap<String, u64> = BTreeMap::new();
    let num = |v: &Value| v.as_i64().unwrap_or(0);
    for c in frame["commands"].as_array().into_iter().flatten() {
        let kind = c["kind"].as_str().unwrap_or("");
        let (ox, oy) = (num(&c["offset"][0]), num(&c["offset"][1]));
        let textured = c["textured"] == json!(true);
        let t = &c["texpage"];
        let image = if textured {
            let key = (num(&t["x"]), num(&t["y"]), t["depth"].as_i64().unwrap_or(15), c["clut"][0].as_i64().unwrap_or(-1), c["clut"][1].as_i64().unwrap_or(-1));
            match image_of.get(&key) {
                Some(&i) => Some(i),
                None => continue,
            }
        } else {
            None
        };
        let semi = c["semi"] == json!(true);
        let blend = if semi { t["blend"].clone() } else { Value::Null };
        // Texture modulation is colour/128 (128 = unchanged); raw textures ignore colour.
        let raw = c["raw"] == json!(true);
        // Captures record whether the GPU dithered the polygon (E1h bit 9 and
        // shading/modulation); older ones did not: assume E1h enabled it.
        let dither = c["dither"].as_bool().unwrap_or(c["gouraud"] == json!(true) || (textured && !raw));
        let colour = |v: &Value| -> [f64; 3] {
            [0, 1, 2].map(|k| {
                let x = v["color"][k].as_f64().unwrap_or(128.0);
                if textured { if raw { 1.0 } else { x / 128.0 } } else { x / 255.0 }
            })
        };
        let verts: Vec<&Value> = c["vertices"].as_array().into_iter().flatten().collect();
        let pos = |v: &Value| (num(&v["x"]) - ox, num(&v["y"]) - oy);
        let uv = |v: &Value| (num(&v["uv"][0]), num(&v["uv"][1]));
        match kind {
            "fill" => {
                let a = &c["area"];
                // Fills ignore the drawing offset: absolute VRAM, made relative here.
                elements.push(json!({"kind": "sprite", "image": null, "rect": [num(&a[0]) - ox, num(&a[1]) - oy, num(&a[2]), num(&a[3])],
                    "color": colour(verts[0]), "command": c["index"]}));
            }
            "rect" if !verts.is_empty() => {
                let (x, y) = pos(verts[0]);
                let (w, h) = (num(&c["area"][2]), num(&c["area"][3]));
                let (u, v) = if textured { uv(verts[0]) } else { (0, 0) };
                elements.push(json!({"kind": "sprite", "image": image, "rect": [x, y, w, h], "uv": textured.then_some([u, v, w, h]),
                    "flip_x": false, "flip_y": false, "color": colour(verts[0]), "semi": semi, "blend": blend, "command": c["index"]}));
            }
            "poly3" | "poly4" if verts.iter().all(|v| v["view"].is_array()) => {
                // Lifted to 3D by the capture (GTE view-space positions).
                let tris: &[[usize; 3]] = if verts.len() == 4 { &[[0, 1, 2], [1, 2, 3]] } else { &[[0, 1, 2]] };
                let vertices: Vec<Value> = verts.iter().map(|v| {
                    let (u, t) = uv(v);
                    json!({"x": v["view"][0], "y": v["view"][1], "z": v["view"][2], "u": u, "v": t, "color": colour(v)})
                }).collect();
                elements.push(json!({"kind": "mesh3d", "image": image, "vertices": vertices, "triangles": tris, "semi": semi, "blend": blend, "dither": dither, "command": c["index"]}));
            }
            "poly3" | "poly4" => {
                let p: Vec<(i64, i64)> = verts.iter().map(|v| pos(v)).collect();
                let q: Vec<(i64, i64)> = verts.iter().map(|v| uv(v)).collect();
                let aligned = textured && p.len() == 4
                    && p[0].1 == p[1].1 && p[2].1 == p[3].1 && p[0].0 == p[2].0 && p[1].0 == p[3].0
                    && q[0].1 == q[1].1 && q[2].1 == q[3].1 && q[0].0 == q[2].0 && q[1].0 == q[3].0
                    && c["gouraud"] != json!(true);
                if aligned {
                    let (x0, x1, y0, y1) = (p[0].0.min(p[1].0), p[0].0.max(p[1].0), p[0].1.min(p[2].1), p[0].1.max(p[2].1));
                    let (u0, u1, v0, v1) = (q[0].0.min(q[1].0), q[0].0.max(q[1].0), q[0].1.min(q[2].1), q[0].1.max(q[2].1));
                    // A flip is the UV running against the screen direction.
                    let flip_x = (p[1].0 - p[0].0).signum() * (q[1].0 - q[0].0).signum() < 0;
                    let flip_y = (p[2].1 - p[0].1).signum() * (q[2].1 - q[0].1).signum() < 0;
                    if x1 > x0 && y1 > y0 && u1 > u0 && v1 > v0 {
                        // The GPU interpolates UVs from the pixel's left/top edge, so a
                        // mirrored quad spanning u1→u0 shows texels u1..u0+1 (not u1-1..u0):
                        // the sprite's texture rect moves one texel up on a flipped axis.
                        let uv = [u0 + flip_x as i64, v0 + flip_y as i64, u1 - u0, v1 - v0];
                        elements.push(json!({"kind": "sprite", "image": image, "rect": [x0, y0, x1 - x0, y1 - y0], "uv": uv,
                            "flip_x": flip_x, "flip_y": flip_y, "color": colour(verts[0]), "semi": semi, "blend": blend, "command": c["index"]}));
                        continue;
                    }
                }
                let tris: &[[usize; 3]] = if p.len() == 4 { &[[0, 1, 2], [1, 2, 3]] } else { &[[0, 1, 2]] };
                let vertices: Vec<Value> = verts.iter().map(|v| {
                    let (x, y) = pos(v);
                    let (u, t) = uv(v);
                    json!({"x": x, "y": y, "u": u, "v": t, "color": colour(v)})
                }).collect();
                elements.push(json!({"kind": "mesh", "image": image, "vertices": vertices, "triangles": tris, "semi": semi, "blend": blend, "dither": dither, "command": c["index"]}));
            }
            "line" | "polyline" => *skipped.entry(kind.to_string()).or_default() += 1,
            _ => {}
        }
    }
    let d = &frame["summary"]["display"];
    let g = &frame["summary"]["gte"];
    json!({
        "format": 2,
        "screen": [d["width"].as_i64().unwrap_or(320), d["height"].as_i64().unwrap_or(240)],
        // GTE projection for mesh3d elements: sx = ofx + h·x/z, sy = ofy + h·y/z.
        "projection": if g["h"].is_number() { json!({"h": g["h"], "ofx": g["ofx"], "ofy": g["ofy"]}) } else { Value::Null },
        "images": images,
        "elements": elements,
        "skipped_primitives": skipped,
    })
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

    /// Mirrored quads: the exported sprite, sampled the way a GPU samples a
    /// flipped sprite (pixel centres, nearest), must show exactly the texels the
    /// PS1 rasteriser (psx-gpu) draws, for every flip combination.
    #[test]
    fn mirrored_quads_sample_the_same_texels_as_the_gpu() {
        let mut gpu = psx_gpu::Gpu::new();
        // A 15-bit texture page at (0, 0): every texel distinct.
        for v in 0..32 {
            for u in 0..32 {
                gpu.vram.set(u, v, (1 + u as u16) | (v as u16) << 5 | 0x4000);
            }
        }
        gpu.gp0_all(&[0xE300_0000]);
        gpu.gp0_all(&[0xE400_0000 | 511 << 10 | 1023]);
        gpu.gp0_all(&[0xE500_0000]);
        let texpage = 2u32 << 7; // 15 bpp, page (0, 0)
        let (u0, u1, v0, v1) = (0, 15, 3, 19); // 15 texels wide, 16 tall, like Mega Man's pieces
        let mut commands = Vec::new();
        for (k, (fx, fy)) in [(false, false), (true, false), (false, true), (true, true)].into_iter().enumerate() {
            let (x0, y0) = (100 + 40 * k as i64, 300);
            let (xa, xb) = if fx { (x0 + 15, x0) } else { (x0, x0 + 15) };
            let (ya, yb) = if fy { (y0 + 16, y0) } else { (y0, y0 + 16) };
            let verts = [(xa, ya, u0, v0), (xb, ya, u1, v0), (xa, yb, u0, v1), (xb, yb, u1, v1)];
            let xy = |x: i64, y: i64| (y as u32) << 16 | (x as u32 & 0xFFFF);
            let uv = |u: i64, v: i64| (v as u32) << 8 | u as u32;
            gpu.gp0_all(&[
                0x2D80_8080,
                xy(verts[0].0, verts[0].1),
                uv(verts[0].2, verts[0].3),
                xy(verts[1].0, verts[1].1),
                uv(verts[1].2, verts[1].3) | texpage << 16,
                xy(verts[2].0, verts[2].1),
                uv(verts[2].2, verts[2].3),
                xy(verts[3].0, verts[3].1),
                uv(verts[3].2, verts[3].3),
            ]);
            commands.push(json!({"index": k, "kind": "poly4", "offset": [0, 0], "textured": true, "raw": true,
                "texpage": {"x": 0, "y": 0, "depth": 15}, "clut": null,
                "vertices": verts.iter().map(|&(x, y, u, v)| json!({"x": x, "y": y, "uv": [u, v], "color": [128, 128, 128]})).collect::<Vec<_>>()}));
        }
        let frame = json!({"commands": commands, "summary": {"display": {"width": 320, "height": 240}, "gte": {}}});
        let mut image_of = BTreeMap::new();
        image_of.insert((0, 0, 15, -1, -1), 0);
        let s = scene(&frame, &["page.png".into()], &image_of);
        let elements = s["elements"].as_array().unwrap();
        assert_eq!(elements.len(), 4);
        for e in elements {
            assert_eq!(e["kind"], "sprite", "{e}");
            let n = |v: &Value, i: usize| v[i].as_i64().unwrap();
            let (rx, ry, rw, rh) = (n(&e["rect"], 0), n(&e["rect"], 1), n(&e["rect"], 2), n(&e["rect"], 3));
            let (tu, tv, tw, th) = (n(&e["uv"], 0), n(&e["uv"], 1), n(&e["uv"], 2), n(&e["uv"], 3));
            let (fx, fy) = (e["flip_x"] == json!(true), e["flip_y"] == json!(true));
            for py in 0..rh {
                for px in 0..rw {
                    // Normalised pixel centre, mirrored on a flipped axis, then nearest texel.
                    let mut s = (px as f64 + 0.5) / rw as f64;
                    let mut t = (py as f64 + 0.5) / rh as f64;
                    if fx {
                        s = 1.0 - s;
                    }
                    if fy {
                        t = 1.0 - t;
                    }
                    let (u, v) = ((tu as f64 + s * tw as f64).floor() as i32, (tv as f64 + t * th as f64).floor() as i32);
                    assert_eq!(
                        gpu.vram.get((rx + px) as i32, (ry + py) as i32),
                        gpu.vram.get(u, v),
                        "element {} pixel ({px}, {py}) samples texel ({u}, {v})",
                        e["command"]
                    );
                }
            }
        }
    }

    #[test]
    fn scene_is_format_two_with_dither() {
        let tri = |c: i64| json!([{"x": 0, "y": 0, "color": [c, 0, 0]}, {"x": 9, "y": 0, "color": [0, c, 0]}, {"x": 0, "y": 9, "color": [0, 0, c]}]);
        let frame = json!({"commands": [
            {"index": 0, "kind": "poly3", "offset": [0, 0], "gouraud": true, "semi": true, "texpage": {"blend": 2}, "vertices": tri(255)},
            {"index": 1, "kind": "poly3", "offset": [0, 0], "vertices": tri(9)},
        ], "summary": {"display": {"width": 320, "height": 240}, "gte": {}}});
        let s = scene(&frame, &[], &BTreeMap::new());
        assert_eq!(s["format"], 2);
        assert_eq!(s["elements"][0]["dither"], true);
        assert_eq!(s["elements"][0]["blend"], 2);
        assert_eq!(s["elements"][1]["dither"], false, "flat untextured polygons are not dithered");
    }
}
