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
        list.push(json!({"page": {"x": px, "y": py, "depth": d}, "clut": clut, "uses": uses, "uv_bounds": crop, "full": full, "used": used}));
    }
    let scene = scene(&frame, &images, &image_of);
    let scene_path = out_dir.join("scene.psxscene");
    let bytes = serde_json::to_vec_pretty(&scene).map_err(|e| bad(e.to_string()))?;
    std::fs::write(&scene_path, bytes).map_err(|e| MediaError::io(&scene_path, e))?;
    Ok(json!({
        "source": frame_path, "note": "decoded from the run's final VRAM", "textures": list,
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
                elements.push(json!({"kind": "mesh3d", "image": image, "vertices": vertices, "triangles": tris, "semi": semi, "blend": blend, "command": c["index"]}));
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
                        elements.push(json!({"kind": "sprite", "image": image, "rect": [x0, y0, x1 - x0, y1 - y0], "uv": [u0, v0, u1 - u0, v1 - v0],
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
                elements.push(json!({"kind": "mesh", "image": image, "vertices": vertices, "triangles": tris, "semi": semi, "blend": blend, "command": c["index"]}));
            }
            "line" | "polyline" => *skipped.entry(kind.to_string()).or_default() += 1,
            _ => {}
        }
    }
    let d = &frame["summary"]["display"];
    let g = &frame["summary"]["gte"];
    json!({
        "format": 1,
        "screen": [d["width"].as_i64().unwrap_or(320), d["height"].as_i64().unwrap_or(240)],
        // GTE projection for mesh3d elements: sx = ofx + h·x/z, sy = ofy + h·y/z.
        "projection": if g["h"].is_number() { json!({"h": g["h"], "ofx": g["ofx"], "ofy": g["ofy"]}) } else { Value::Null },
        "images": images,
        "elements": elements,
        "skipped_primitives": skipped,
    })
}
