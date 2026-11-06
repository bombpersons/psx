//! glTF models as psx-assets writes them (and as most tools do): triangle
//! primitives with POSITION, optional NORMAL (lit), TEXCOORD_0 and COLOR_0,
//! one base colour texture, alpha mode OPAQUE, MASK or BLEND.
use crate::{Alpha, Mat4, Mesh, Primitive, Renderer, TextureId, Vertex};
use std::collections::HashMap;
use std::path::Path;

/// glTF stores linear colour; the renderer works on encoded colour.
fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

fn rgba(image: &gltf::image::Data) -> Result<Vec<u8>, String> {
    use gltf::image::Format;
    let p = &image.pixels;
    Ok(match image.format {
        Format::R8G8B8A8 => p.clone(),
        Format::R8G8B8 => p.as_chunks::<3>().0.iter().flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        Format::R8G8 => p.as_chunks::<2>().0.iter().flat_map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        Format::R8 => p.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        other => return Err(format!("unsupported image format {other:?}")),
    })
}

pub(crate) fn load(renderer: &mut Renderer, path: &Path) -> Result<Mesh, String> {
    let (document, buffers, images) = gltf::import(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut textures: HashMap<usize, TextureId> = HashMap::new();
    let mut mesh = Mesh::default();
    let scene = document.default_scene().or_else(|| document.scenes().next()).ok_or("glTF has no scene")?;
    let mut stack: Vec<(gltf::Node, Mat4)> = scene.nodes().map(|n| (n, Mat4::IDENTITY)).collect();
    while let Some((node, parent)) = stack.pop() {
        let transform = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
        stack.extend(node.children().map(|c| (c, transform)));
        let Some(gltf_mesh) = node.mesh() else { continue };
        let normal_matrix = transform.inverse().transpose();
        for primitive in gltf_mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            let reader = primitive.reader(|b| Some(&buffers[b.index()]));
            let Some(positions) = reader.read_positions() else { continue };
            let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|n| n.collect());
            let uvs: Option<Vec<[f32; 2]>> = reader.read_tex_coords(0).map(|t| t.into_f32().collect());
            let colors: Option<Vec<[f32; 4]>> = reader.read_colors(0).map(|c| c.into_rgba_f32().collect());
            let material = primitive.material();
            let factor = material.pbr_metallic_roughness().base_color_factor();
            let vertices: Vec<Vertex> = positions
                .enumerate()
                .map(|(i, p)| {
                    let c = colors.as_ref().map_or([1.0; 4], |c| c[i]);
                    let n = normals.as_ref().map_or([0.0, 1.0, 0.0], |n| n[i]);
                    let n = normal_matrix.transform_vector3(n.into()).normalize_or_zero();
                    Vertex {
                        position: transform.transform_point3(p.into()).into(),
                        normal: n.into(),
                        uv: uvs.as_ref().map_or([0.0; 2], |t| t[i]),
                        color: [
                            linear_to_srgb(c[0] * factor[0]),
                            linear_to_srgb(c[1] * factor[1]),
                            linear_to_srgb(c[2] * factor[2]),
                            c[3] * factor[3],
                        ],
                    }
                })
                .collect();
            let indices = reader.read_indices().map(|i| i.into_u32().collect()).unwrap_or_default();
            let texture = match material.pbr_metallic_roughness().base_color_texture() {
                Some(info) => {
                    let source = info.texture().source().index();
                    match textures.get(&source) {
                        Some(&t) => Some(t),
                        None => {
                            let image = &images[source];
                            let t = renderer.texture(image.width, image.height, &rgba(image)?);
                            textures.insert(source, t);
                            Some(t)
                        }
                    }
                }
                None => None,
            };
            let alpha = match material.alpha_mode() {
                gltf::material::AlphaMode::Opaque => Alpha::Opaque,
                gltf::material::AlphaMode::Mask => Alpha::Mask,
                gltf::material::AlphaMode::Blend => Alpha::Blend,
            };
            mesh.primitives.push(Primitive { vertices, indices, texture, alpha, lit: normals.is_some() });
        }
    }
    Ok(mesh)
}
