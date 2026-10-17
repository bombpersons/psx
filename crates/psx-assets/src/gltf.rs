//! TMD to glTF 2.0 (`.gltf` JSON with an embedded buffer).
//!
//! One node and mesh per TMD object, one glTF primitive per material (texture
//! page + CLUT + semi-transparency). Corners are unshared, so flat colours
//! and per-polygon UVs survive. Axes: the PS1's y-down, z-forward space is
//! rotated 180 degrees about x (y and z negated), so models stand upright in
//! glTF's y-up space; units are the TMD's own. Materials are double-sided
//! (PS1 culling was a per-game choice). Vertex colours are the packet's RGB:
//! for untextured polygons the colour itself, for textured ones the
//! modulation (128 = unchanged), both clamped to 1.0 in glTF.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::tim::{clut_from_cba, TexPage};
use crate::tmd::{Tmd, TmdPolygon, MODE_TGE};

/// What a textured polygon samples: page, CLUT position (4/8-bit pages only).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureKey {
    pub page: TexPage,
    pub clut: Option<(u16, u16)>,
}

impl TextureKey {
    pub fn of(p: &TmdPolygon) -> Option<TextureKey> {
        if !p.textured() {
            return None;
        }
        let page = TexPage::from_tsb(p.tsb);
        let clut = (page.bits != 16).then(|| clut_from_cba(p.cba));
        // abr only matters for blending; keep one image per page/CLUT.
        Some(TextureKey { page: TexPage { abr: 0, ..page }, clut })
    }

    /// A file-name-friendly label.
    pub fn label(&self) -> String {
        match self.clut {
            Some((cx, cy)) => format!("page{}_{}_{}b_clut{}_{}", self.page.x, self.page.y, self.page.bits, cx, cy),
            None => format!("page{}_{}_{}b", self.page.x, self.page.y, self.page.bits),
        }
    }
}

/// Every texture a TMD samples.
pub fn texture_keys(tmd: &Tmd) -> Vec<TextureKey> {
    let mut keys: Vec<TextureKey> = tmd.objects.iter().flat_map(|o| &o.polygons).filter_map(TextureKey::of).collect();
    keys.sort();
    keys.dedup();
    keys
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                s.push(B64[(n >> (18 - 6 * k)) as usize & 63] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[derive(Default)]
struct Group {
    pos: Vec<[f32; 3]>,
    nrm: Vec<[f32; 3]>,
    uv: Vec<[f32; 2]>,
    col: Vec<[f32; 4]>,
}

/// Vertex data per corner, shared with the Bevy loader's conventions.
fn corner(obj: &crate::tmd::TmdObject, p: &TmdPolygon, i: usize) -> ([f32; 3], [f32; 3], [f32; 2], [f32; 4]) {
    let v = obj.vertices[p.vertices[i] as usize];
    let pos = [v[0] as f32, -(v[1] as f32), -(v[2] as f32)];
    let nrm = match p.normal(i).and_then(|n| obj.normals.get(n as usize)) {
        Some(n) => {
            let n = [n[0] as f32, -(n[1] as f32), -(n[2] as f32)];
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt().max(1e-6);
            [n[0] / l, n[1] / l, n[2] / l]
        }
        None => [0.0, 0.0, 0.0],
    };
    let uv = p.uvs.get(i).map(|t| [t[0] as f32 / 256.0, t[1] as f32 / 256.0]).unwrap_or([0.0, 0.0]);
    let c = p.color(i);
    let col = if p.textured() {
        if p.colors.is_empty() || p.mode & MODE_TGE != 0 {
            [1.0; 3]
        } else {
            c.map(|x| (x as f32 / 128.0).min(1.0))
        }
    } else {
        c.map(|x| x as f32 / 255.0)
    };
    let a = if p.semi_transparent() { 0.5 } else { 1.0 };
    (pos, nrm, uv, [srgb_to_linear(col[0]), srgb_to_linear(col[1]), srgb_to_linear(col[2]), a])
}

/// Build the glTF document. `image_uri` names the PNG for a texture key, or
/// `None` to leave those polygons untextured.
pub fn tmd_to_gltf(tmd: &Tmd, name: &str, image_uri: &dyn Fn(&TextureKey) -> Option<String>) -> Value {
    let mut buffer: Vec<u8> = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut meshes = Vec::new();
    let mut nodes = Vec::new();
    let mut materials: Vec<Value> = Vec::new();
    let mut material_of: BTreeMap<(Option<TextureKey>, bool), usize> = BTreeMap::new();
    let mut images = Vec::new();
    let mut textures = Vec::new();
    let mut texture_of: BTreeMap<TextureKey, Option<usize>> = BTreeMap::new();

    let mut push = |data: Vec<f32>, comps: usize, kind: &str, minmax: bool, accessors: &mut Vec<Value>| -> usize {
        while !buffer.len().is_multiple_of(4) {
            buffer.push(0);
        }
        let offset = buffer.len();
        for f in &data {
            buffer.extend_from_slice(&f.to_le_bytes());
        }
        views.push(json!({"buffer": 0, "byteOffset": offset, "byteLength": data.len() * 4, "target": 34962}));
        let mut acc = json!({"bufferView": views.len() - 1, "componentType": 5126, "count": data.len() / comps, "type": kind});
        if minmax {
            let mut lo = vec![f32::MAX; comps];
            let mut hi = vec![f32::MIN; comps];
            for c in data.chunks(comps) {
                for k in 0..comps {
                    lo[k] = lo[k].min(c[k]);
                    hi[k] = hi[k].max(c[k]);
                }
            }
            acc["min"] = json!(lo);
            acc["max"] = json!(hi);
        }
        accessors.push(acc);
        accessors.len() - 1
    };

    for (k, obj) in tmd.objects.iter().enumerate() {
        let mut groups: BTreeMap<(Option<TextureKey>, bool), Group> = BTreeMap::new();
        for p in &obj.polygons {
            let key = TextureKey::of(p);
            let tex = key.filter(|k| {
                texture_of
                    .entry(*k)
                    .or_insert_with(|| {
                        image_uri(k).map(|u| {
                            images.push(json!({"uri": u}));
                            textures.push(json!({"source": images.len() - 1, "sampler": 0}));
                            textures.len() - 1
                        })
                    })
                    .is_some()
            });
            let g = groups.entry((tex, p.semi_transparent())).or_default();
            for tri in p.triangles() {
                for &i in tri {
                    let (pos, nrm, uv, col) = corner(obj, p, i);
                    g.pos.push(pos);
                    g.nrm.push(nrm);
                    g.uv.push(uv);
                    g.col.push(col);
                }
            }
        }
        let mut prims = Vec::new();
        for ((tex, semi), g) in groups {
            let material = *material_of.entry((tex, semi)).or_insert_with(|| {
                let mut m = json!({
                    "name": tex.map(|t| t.label()).unwrap_or_else(|| "untextured".into()) + if semi { "_semi" } else { "" },
                    "doubleSided": true,
                    "pbrMetallicRoughness": {"metallicFactor": 0.0, "roughnessFactor": 1.0},
                    "alphaMode": if semi { "BLEND" } else if tex.is_some() { "MASK" } else { "OPAQUE" },
                    "extensions": {"KHR_materials_unlit": {}},
                });
                if let Some(t) = tex.and_then(|t| texture_of[&t]) {
                    m["pbrMetallicRoughness"]["baseColorTexture"] = json!({"index": t});
                }
                materials.push(m);
                materials.len() - 1
            });
            let has_normals = g.nrm.iter().any(|n| n != &[0.0; 3]);
            let pos = push(g.pos.concat(), 3, "VEC3", true, &mut accessors);
            let mut attrs = json!({"POSITION": pos});
            if has_normals {
                // Unlit polygons have no normals; give them +y so the accessor stays valid.
                let n: Vec<f32> = g.nrm.iter().flat_map(|n| if n == &[0.0; 3] { [0.0, 1.0, 0.0] } else { *n }).collect();
                attrs["NORMAL"] = json!(push(n, 3, "VEC3", false, &mut accessors));
            }
            if tex.is_some() {
                attrs["TEXCOORD_0"] = json!(push(g.uv.concat(), 2, "VEC2", false, &mut accessors));
            }
            attrs["COLOR_0"] = json!(push(g.col.concat(), 4, "VEC4", false, &mut accessors));
            prims.push(json!({"attributes": attrs, "material": material, "mode": 4}));
        }
        meshes.push(json!({"name": format!("{name}_object{k}"), "primitives": prims}));
        nodes.push(json!({"name": format!("object{k}"), "mesh": meshes.len() - 1}));
    }
    let mut doc = json!({
        "asset": {"version": "2.0", "generator": "psx assets extract (TMD)"},
        "extensionsUsed": ["KHR_materials_unlit"],
        "scene": 0,
        "scenes": [{"name": name, "nodes": (0..nodes.len()).collect::<Vec<_>>()}],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials,
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{"byteLength": buffer.len(), "uri": format!("data:application/octet-stream;base64,{}", base64(&buffer))}],
    });
    if !images.is_empty() {
        doc["images"] = json!(images);
        doc["textures"] = json!(textures);
        // Nearest filtering, clamped: PS1 texels.
        doc["samplers"] = json!([{"magFilter": 9728, "minFilter": 9728, "wrapS": 33071, "wrapT": 33071}]);
    }
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmd::{TmdObject, TmdPolygon};

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn groups_by_material_and_flips_axes() {
        let obj = TmdObject {
            vertices: vec![[0, 0, 0], [10, 0, 0], [0, -10, 5], [10, -10, 0]],
            polygons: vec![
                TmdPolygon { mode: 0x2D, flag: 1, vertices: vec![0, 1, 2, 3], colors: vec![[128, 128, 128]], uvs: vec![[0, 0], [64, 0], [0, 64], [64, 64]], cba: 1, tsb: 5, ..Default::default() },
                TmdPolygon { mode: 0x21, flag: 1, vertices: vec![0, 1, 2], colors: vec![[255, 0, 0]], ..Default::default() },
            ],
            ..Default::default()
        };
        let tmd = Tmd { fixp: false, objects: vec![obj], size: 0 };
        assert_eq!(texture_keys(&tmd).len(), 1);
        let doc = tmd_to_gltf(&tmd, "m", &|k| Some(format!("{}.png", k.label())));
        assert_eq!(doc["meshes"][0]["primitives"].as_array().unwrap().len(), 2);
        assert_eq!(doc["images"][0]["uri"], "page320_0_4b_clut16_0.png");
        // Quad = 2 triangles = 6 corners; position max y = +10 after the flip.
        let quad_pos = doc["meshes"][0]["primitives"].as_array().unwrap().iter().find(|p| p["attributes"].get("TEXCOORD_0").is_some()).unwrap()["attributes"]["POSITION"].as_u64().unwrap();
        assert_eq!(doc["accessors"][quad_pos as usize]["count"], 6);
        assert_eq!(doc["accessors"][quad_pos as usize]["max"][1], 10.0);
        assert_eq!(doc["accessors"][quad_pos as usize]["min"][2], -5.0);
        let none = tmd_to_gltf(&tmd, "m", &|_| None);
        assert!(none.get("images").is_none());
    }
}
