//! Version 1 external-model interchange. Parsers emit JSON geometry; the toolkit
//! validates it and reuses its normal model exporters. No GPU packet encoding is
//! required from the parser. Coordinates are signed PS1 units (Y down, Z forward).

use crate::tim::{TexPage, clut_from_cba};
use crate::tmd::{FLAG_FCE, FLAG_LGT, MODE_ABE, MODE_IIP, MODE_QUAD, MODE_TGE, MODE_TME, Tmd, TmdObject, TmdPolygon};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDocument {
    pub version: u32,
    pub objects: Vec<Object>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Object {
    pub vertices: Vec<[i16; 3]>,
    #[serde(default)]
    pub normals: Vec<[i16; 3]>,
    pub polygons: Vec<Polygon>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Polygon {
    pub vertices: Vec<u16>,
    #[serde(default)]
    pub normals: Vec<u16>,
    #[serde(default)]
    pub colors: Vec<[u8; 3]>,
    #[serde(default)]
    pub uvs: Vec<[u8; 2]>,
    #[serde(default)]
    pub texture: Option<Texture>,
    #[serde(default)]
    pub semi_transparent: bool,
    #[serde(default)]
    pub double_sided: bool,
    #[serde(default)]
    pub unmodulated: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Texture {
    pub page: [u16; 2],
    pub depth: u8,
    #[serde(default)]
    pub clut: Option<[u16; 2]>,
}

impl ModelDocument {
    pub fn into_model(self) -> Result<Tmd, String> {
        if self.version != 1 {
            return Err(format!("unsupported model document version {}", self.version));
        }
        if self.objects.is_empty() || self.objects.len() > 4096 {
            return Err("model must contain 1..=4096 objects".into());
        }
        let mut model = Tmd::default();
        for (oi, source) in self.objects.into_iter().enumerate() {
            if source.vertices.len() > 65536 || source.normals.len() > 65536 || source.polygons.len() > 65536 {
                return Err(format!("object {oi}: more than 65536 vertices, normals or polygons"));
            }
            let mut object = TmdObject { vertices: source.vertices, normals: source.normals, ..Default::default() };
            for (pi, p) in source.polygons.into_iter().enumerate() {
                let n = p.vertices.len();
                let fail = |message: &str| format!("object {oi}, polygon {pi}: {message}");
                if !matches!(n, 3 | 4) {
                    return Err(fail("expected a triangle or quad"));
                }
                if p.vertices.iter().any(|&v| v as usize >= object.vertices.len()) {
                    return Err(fail("vertex index out of range"));
                }
                if ![0, 1, n].contains(&p.normals.len())
                    || p.normals.iter().any(|&v| v as usize >= object.normals.len())
                {
                    return Err(fail("normal count or index out of range"));
                }
                if ![0, 1, n].contains(&p.colors.len()) {
                    return Err(fail("expected zero, one or per-corner colours"));
                }
                let (cba, tsb) = if let Some(texture) = &p.texture {
                    if p.uvs.len() != n {
                        return Err(fail("textured polygon needs a UV pair per corner"));
                    }
                    let [x, y] = texture.page;
                    if x > 960 || !x.is_multiple_of(64) || !matches!(y, 0 | 256) {
                        return Err(fail("invalid texture page"));
                    }
                    let depth = match texture.depth {
                        4 => 0,
                        8 => 1,
                        16 => 2,
                        _ => return Err(fail("texture depth must be 4, 8 or 16")),
                    };
                    let cba = match (texture.depth, texture.clut) {
                        (16, None) => 0,
                        (4 | 8, Some([cx, cy])) if cx <= 1008 && cx.is_multiple_of(16) && cy < 512 => {
                            (cy << 6) | (cx / 16)
                        }
                        _ => return Err(fail("indexed textures require an aligned CLUT; direct colour has no CLUT")),
                    };
                    (cba, (x / 64) | ((y / 256) << 4) | (depth << 7))
                } else {
                    if !p.uvs.is_empty() || p.unmodulated {
                        return Err(fail("UVs/unmodulated require a texture"));
                    }
                    (0, 0)
                };
                let mut mode = 0x20;
                if n == 4 {
                    mode |= MODE_QUAD;
                }
                if p.texture.is_some() {
                    mode |= MODE_TME;
                }
                if p.colors.len() == n || p.normals.len() == n {
                    mode |= MODE_IIP;
                }
                if p.semi_transparent {
                    mode |= MODE_ABE;
                }
                if p.unmodulated {
                    mode |= MODE_TGE;
                }
                let flag = if p.normals.is_empty() { FLAG_LGT } else { 0 } | if p.double_sided { FLAG_FCE } else { 0 };
                let colors = if p.colors.is_empty() && p.texture.is_none() { vec![[255; 3]] } else { p.colors };
                object.polygons.push(TmdPolygon {
                    mode,
                    flag,
                    vertices: p.vertices,
                    normals: p.normals,
                    colors,
                    uvs: p.uvs,
                    cba,
                    tsb,
                });
            }
            model.objects.push(object);
        }
        if model.polygon_count() == 0 {
            return Err("model contains no polygons".into());
        }
        Ok(model)
    }
}

impl From<&Tmd> for ModelDocument {
    fn from(model: &Tmd) -> Self {
        Self {
            version: 1,
            objects: model
                .objects
                .iter()
                .map(|object| Object {
                    vertices: object.vertices.clone(),
                    normals: object.normals.clone(),
                    polygons: object
                        .polygons
                        .iter()
                        .map(|p| Polygon {
                            vertices: p.vertices.clone(),
                            normals: p.normals.clone(),
                            colors: p.colors.clone(),
                            uvs: p.uvs.clone(),
                            texture: p.textured().then(|| {
                                let page = TexPage::from_tsb(p.tsb);
                                let clut = clut_from_cba(p.cba);
                                Texture {
                                    page: [page.x, page.y],
                                    depth: page.bits,
                                    clut: (page.bits != 16).then_some([clut.0, clut.1]),
                                }
                            }),
                            semi_transparent: p.semi_transparent(),
                            double_sided: p.double_sided(),
                            unmodulated: p.textured() && p.mode & MODE_TGE != 0,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn triangle() -> serde_json::Value {
        json!({"version":1,"objects":[{"vertices":[[0,0,0],[10,0,0],[0,10,0]],"polygons":[{"vertices":[0,1,2]}]}]})
    }

    #[test]
    fn validates_geometry_before_export_and_defaults_to_white() {
        let model = serde_json::from_value::<ModelDocument>(triangle()).unwrap().into_model().unwrap();
        assert_eq!(model.polygon_count(), 1);
        assert_eq!(model.objects[0].polygons[0].colors, [[255; 3]]);
        let again = ModelDocument::from(&model).into_model().unwrap();
        assert_eq!(model.objects[0].polygons, again.objects[0].polygons);
        for replacement in [json!([0, 1]), json!([0, 1, 9])] {
            let mut doc = triangle();
            doc["objects"][0]["polygons"][0]["vertices"] = replacement;
            assert!(serde_json::from_value::<ModelDocument>(doc).unwrap().into_model().is_err());
        }
        let mut doc = triangle();
        doc["version"] = json!(2);
        assert!(serde_json::from_value::<ModelDocument>(doc).unwrap().into_model().is_err());
        let mut doc = triangle();
        doc["typo"] = json!(1);
        assert!(serde_json::from_value::<ModelDocument>(doc).is_err());
    }

    #[test]
    fn texture_pages_cluts_uvs_and_normals_round_trip_and_validate() {
        let mut doc = triangle();
        doc["objects"][0]["normals"] = json!([[0, 0, 4096]]);
        let polygon = &mut doc["objects"][0]["polygons"][0];
        polygon["normals"] = json!([0]);
        polygon["uvs"] = json!([[0, 0], [10, 0], [0, 10]]);
        polygon["texture"] = json!({"page":[960,256],"depth":8,"clut":[1008,511]});
        polygon["semi_transparent"] = json!(true);
        polygon["double_sided"] = json!(true);
        polygon["unmodulated"] = json!(true);
        let model = serde_json::from_value::<ModelDocument>(doc.clone()).unwrap().into_model().unwrap();
        let again = ModelDocument::from(&model).into_model().unwrap();
        assert_eq!(model.objects[0].polygons, again.objects[0].polygons);
        for (field, value) in [
            ("texture", json!({"page":[1,0],"depth":8,"clut":[0,0]})),
            ("texture", json!({"page":[0,0],"depth":8})),
            ("texture", json!({"page":[0,0],"depth":16,"clut":[0,0]})),
            ("texture", json!({"page":[0,0],"depth":24})),
            ("normals", json!([1])),
            ("normals", json!([0, 0])),
            ("colors", json!([[0, 0, 0], [0, 0, 0]])),
            ("uvs", json!([])),
        ] {
            let mut invalid = doc.clone();
            invalid["objects"][0]["polygons"][0][field] = value;
            assert!(serde_json::from_value::<ModelDocument>(invalid).unwrap().into_model().is_err());
        }
    }
}
