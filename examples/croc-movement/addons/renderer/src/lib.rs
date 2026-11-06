//! A wgpu renderer with two looks the player switches between at run time:
//!
//! - [`Look::Ps1`] draws at the game's resolution into an offscreen canvas
//!   and scales it to the window: vertices snapped to whole pixels, affine
//!   texture mapping, per-vertex lighting, ordered dithering to 15-bit
//!   colour, the PS1's semi-transparency modes, depth cue, nearest texels.
//! - [`Look::Modern`] draws the same scene at the window's resolution:
//!   perspective-correct, filtered textures, full colour, 4x antialiasing.
//!
//! The interface is the same for both: textures, models, a camera, lights
//! and sprites, gathered in a [`Scene`] and drawn with [`Renderer::render`]
//! (to the window) or [`Renderer::capture`] (to a [`Frame`], for `--replay`
//! and tests). With [`Aspect::Window`] the screen keeps the game's height and
//! its width follows the window's shape, so any aspect ratio is filled and
//! the camera's horizontal field of view widens with it.
//!
//! Colours are as the PS1 stored them: gamma-encoded, 0..1 per channel. A
//! vertex colour or tint of 1.0 leaves a texture unchanged, up to 2.0
//! brightens it. Texture alpha follows `psx textures`: 0 transparent, about
//! 0.5 a texel with the STP bit, 1 opaque.
//!
//! Models come from glTF ([`Renderer::load_gltf`], as psx-assets writes
//! them) or from code ([`Mesh`]); textures from PNG ([`Renderer::load_png`])
//! or RGBA bytes. The original's formats are the importers' business.
//!
//! Headless ([`Renderer::headless`]) uses any adapter, falling back to a
//! software one (lavapipe, SwiftShader) where there is no GPU.
mod font;
mod gltf_load;
mod glyphs;

pub use font::Font;
pub use glam::{self, Mat4, Quat, Vec3};
pub use psx_scenario::Frame;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// Which renderer the player sees.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Look {
    #[default]
    Ps1,
    Modern,
}

/// How the screen's shape relates to the window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Aspect {
    /// The game's shape, letterboxed in the window.
    #[default]
    Fixed,
    /// The game's height; the width follows the window, so any shape is
    /// filled and more of the scene shows at the sides.
    Window,
}

/// The PS1 look's effects, each of which the player may turn off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ps1Effects {
    /// Vertices snap to whole screen pixels (polygon wobble).
    pub vertex_snap: bool,
    /// The GPU's 4x4 ordered dither on shaded surfaces.
    pub dither: bool,
    /// 15-bit colour.
    pub quantize: bool,
}

impl Default for Ps1Effects {
    fn default() -> Self {
        Self { vertex_snap: true, dither: true, quantize: true }
    }
}

/// The player's display settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Display {
    pub look: Look,
    pub aspect: Aspect,
    pub ps1: Ps1Effects,
}

/// The PS1's semi-transparency equations (B what is drawn, F this surface),
/// on encoded colour, saturating per channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Blend {
    #[default]
    Opaque,
    /// B/2 + F/2.
    Half,
    /// B + F.
    Add,
    /// B - F.
    Subtract,
    /// B + F/4.
    AddQuarter,
}

impl Blend {
    fn code(self) -> u32 {
        match self {
            Blend::Opaque => 0,
            Blend::Half => 1,
            Blend::Add => 2,
            Blend::Subtract => 3,
            Blend::AddQuarter => 4,
        }
    }

    /// Shaders output a premultiplied colour and, in alpha, how much of the
    /// background to keep; subtract has its own operation.
    fn state(self) -> Option<wgpu::BlendState> {
        use wgpu::{BlendComponent, BlendFactor, BlendOperation};
        let color = match self {
            Blend::Opaque => return None,
            Blend::Half | Blend::Add | Blend::AddQuarter => BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::OneMinusSrcAlpha,
                operation: BlendOperation::Add,
            },
            Blend::Subtract => BlendComponent {
                src_factor: BlendFactor::One,
                dst_factor: BlendFactor::One,
                operation: BlendOperation::ReverseSubtract,
            },
        };
        let alpha = BlendComponent {
            src_factor: BlendFactor::Zero,
            dst_factor: BlendFactor::One,
            operation: BlendOperation::Add,
        };
        Some(wgpu::BlendState { color, alpha })
    }
}

/// A model vertex. `color` is encoded RGB and alpha.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

/// How a primitive's texels cover what is below.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Alpha {
    #[default]
    Opaque,
    /// Transparent texels (alpha below 0.25) are not drawn.
    Mask,
    /// Semi-transparent: drawn with the instance's [`Blend`], and on a
    /// textured primitive only the STP texels blend, as on the PS1.
    Blend,
}

/// Triangles with one texture.
#[derive(Clone, Debug, Default)]
pub struct Primitive {
    pub vertices: Vec<Vertex>,
    /// Triangle list; empty draws the vertices in order.
    pub indices: Vec<u32>,
    pub texture: Option<TextureId>,
    pub alpha: Alpha,
    /// Lit by the scene's lights through the normals.
    pub lit: bool,
}

/// A model built in code.
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub primitives: Vec<Primitive>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TextureId(usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModelId(usize);

/// The camera: where it is and the game's field of view.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    /// World to view: right-handed, looking down -Z.
    pub view: Mat4,
    /// Vertical field of view in radians; the horizontal one follows the
    /// screen's shape.
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { view: Mat4::IDENTITY, fov_y: 60f32.to_radians(), near: 1.0, far: 10_000.0 }
    }
}

/// A directional light.
#[derive(Clone, Copy, Debug)]
pub struct Light {
    /// World-space direction towards the light.
    pub towards: Vec3,
    pub color: [f32; 3],
}

/// Depth cue: vertex colours move towards `color` between `near` and `far`
/// (view depth).
#[derive(Clone, Copy, Debug)]
pub struct Fog {
    pub color: [f32; 3],
    pub near: f32,
    pub far: f32,
}

/// A model placed in the scene.
#[derive(Clone, Copy, Debug)]
pub struct Instance {
    pub model: ModelId,
    pub transform: Mat4,
    /// Multiplies vertex colours (1.0 unchanged).
    pub tint: [f32; 4],
    /// The equation for the model's semi-transparent primitives.
    pub blend: Blend,
}

impl Instance {
    pub fn new(model: ModelId, transform: Mat4) -> Self {
        Self { model, transform, tint: [1.0; 4], blend: Blend::Half }
    }
}

/// A textured rectangle on the screen, in layout pixels
/// ([`Renderer::layout_size`]) from the top left.
#[derive(Clone, Copy, Debug)]
pub struct Sprite {
    pub texture: TextureId,
    /// x, y, width, height in texels.
    pub src: [f32; 4],
    /// x, y, width, height in layout pixels.
    pub dst: [f32; 4],
    /// Multiplies the texels (1.0 unchanged); alpha 0 hides it.
    pub color: [f32; 4],
    pub blend: Blend,
}

/// What one frame draws: models (depth-tested; opaque, then
/// semi-transparent in order), then sprites in order on top.
#[derive(Clone, Debug)]
pub struct Scene {
    pub clear: [f32; 3],
    pub camera: Camera,
    /// Light every lit vertex gets (1.0 with no lights leaves colours as they are).
    pub ambient: [f32; 3],
    pub lights: Vec<Light>,
    pub fog: Option<Fog>,
    pub models: Vec<Instance>,
    pub sprites: Vec<Sprite>,
}

impl Default for Scene {
    fn default() -> Self {
        Self {
            clear: [0.0; 3],
            camera: Camera::default(),
            ambient: [1.0; 3],
            lights: Vec::new(),
            fog: None,
            models: Vec::new(),
            sprites: Vec::new(),
        }
    }
}

impl Scene {
    /// A rectangle of one colour.
    pub fn rect(&mut self, white: TextureId, dst: [f32; 4], color: [f32; 4]) {
        self.sprites.push(Sprite { texture: white, src: [0.0, 0.0, 1.0, 1.0], dst, color, blend: Blend::Opaque });
    }
}

/// The width that gives a screen `height` pixels high the window's shape:
/// even, 2 to 4096.
pub fn width_for_aspect(height: u32, window: [u32; 2]) -> u32 {
    let width = (height as f32 * window[0] as f32 / window[1].max(1) as f32 / 2.0).round() as u32 * 2;
    width.clamp(2, 4096)
}

/// The screen sprites are laid out on: the game's size, or with
/// [`Aspect::Window`] its height at the window's shape.
pub fn layout_size(display: &Display, game: [u32; 2], window: [u32; 2]) -> [u32; 2] {
    match display.aspect {
        Aspect::Fixed => game,
        Aspect::Window => [width_for_aspect(game[1], window), game[1]],
    }
}

/// The canvas the scene is drawn into: the layout size in the PS1 look; in
/// the modern look as many window pixels as that shape takes.
pub fn canvas_size(display: &Display, game: [u32; 2], window: [u32; 2]) -> [u32; 2] {
    let layout = layout_size(display, game, window);
    match display.look {
        Look::Ps1 => layout,
        Look::Modern => {
            let [x, y, w, h] = fit(window, layout);
            let _ = (x, y);
            [(w.round() as u32).max(1), (h.round() as u32).max(1)]
        }
    }
}

/// The largest rectangle of `inner`'s shape centred in `outer`: x, y,
/// width, height.
pub fn fit(outer: [u32; 2], inner: [u32; 2]) -> [f32; 4] {
    let (ow, oh) = (outer[0].max(1) as f32, outer[1].max(1) as f32);
    let scale = (ow / inner[0].max(1) as f32).min(oh / inner[1].max(1) as f32);
    let (w, h) = (inner[0] as f32 * scale, inner[1] as f32 * scale);
    [((ow - w) / 2.0).floor(), ((oh - h) / 2.0).floor(), w, h]
}

/// What the renderer needs at the start.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// The original's display size in pixels (320x240 for most games).
    pub game: [u32; 2],
    pub display: Display,
    /// Prefer a software adapter.
    pub software: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { game: [320, 240], display: Display::default(), software: false }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    clip_from_world: [[f32; 4]; 4],
    view_from_world: [[f32; 4]; 4],
    ambient: [f32; 4],
    light_dirs: [[f32; 4]; 4],
    light_colors: [[f32; 4]; 4],
    fog_color: [f32; 4],
    fog: [f32; 4],
    sizes: [f32; 4],
    look: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawUniform {
    world_from_local: [[f32; 4]; 4],
    normal_from_local: [[f32; 4]; 4],
    tint: [f32; 4],
    flags: [u32; 4],
}

const LOOK_SNAP: u32 = 1;
const LOOK_AFFINE: u32 = 2;
const LOOK_DITHER: u32 = 4;
const LOOK_QUANTIZE: u32 = 8;
const TEXTURED: u32 = 1;
const LIT: u32 = 2;
const CUTOUT: u32 = 4;
const STP_TEXEL: u32 = 8;
const MAX_LIGHTS: usize = 4;
const CANVAS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

struct Texture {
    size: [u32; 2],
    nearest: wgpu::BindGroup,
    linear: wgpu::BindGroup,
}

struct GpuPrimitive {
    vertices: wgpu::Buffer,
    indices: Option<wgpu::Buffer>,
    count: u32,
    texture: Option<TextureId>,
    alpha: Alpha,
    lit: bool,
}

struct Canvas {
    size: [u32; 2],
    samples: u32,
    color: wgpu::Texture,
    view: wgpu::TextureView,
    msaa: Option<wgpu::TextureView>,
    depth: wgpu::TextureView,
    nearest: wgpu::BindGroup,
    linear: wgpu::BindGroup,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PipelineKey {
    sprite: bool,
    blend: Blend,
    samples: u32,
}

struct Surface {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    blit: wgpu::RenderPipeline,
}

/// One draw call of a frame.
struct Call {
    key: PipelineKey,
    uniform: DrawUniform,
    texture: TextureId,
    source: Source,
}

enum Source {
    Model(ModelId, usize),
    Sprites(std::ops::Range<u32>),
}

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: Option<Surface>,
    window: [u32; 2],
    game: [u32; 2],
    display: Display,
    shader: wgpu::ShaderModule,
    blit_shader: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    blit_layout: wgpu::BindGroupLayout,
    texture_layout: wgpu::BindGroupLayout,
    pipelines: HashMap<PipelineKey, wgpu::RenderPipeline>,
    globals: wgpu::Buffer,
    draws: wgpu::Buffer,
    draw_stride: u64,
    draw_capacity: u64,
    group0_layout: wgpu::BindGroupLayout,
    group0: wgpu::BindGroup,
    nearest: wgpu::Sampler,
    linear: wgpu::Sampler,
    present: wgpu::Buffer,
    sprite_vertices: Option<(wgpu::Buffer, u64)>,
    textures: Vec<Texture>,
    models: Vec<Vec<GpuPrimitive>>,
    bounds: Vec<Option<(Vec3, Vec3)>>,
    canvas: Option<Canvas>,
    white: TextureId,
}

impl Renderer {
    /// A renderer drawing into `window`, `size` pixels inside.
    pub fn new<W>(window: Arc<W>, size: [u32; 2], options: Options) -> Result<Self, String>
    where
        W: wgpu::rwh::HasWindowHandle + wgpu::rwh::HasDisplayHandle + std::fmt::Debug + Send + Sync + 'static,
    {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(window.clone())));
        let surface = instance.create_surface(window).map_err(|e| format!("window surface: {e}"))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: options.software,
            compatible_surface: Some(&surface),
            apply_limit_buckets: false,
        }))
        .map_err(|e| format!("no graphics adapter for the window: {e}"))?;
        let mut renderer = Self::with_adapter(&adapter, size, options)?;
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or("the window's surface offers no formats")?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size[0].max(1),
            height: size[1].max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&renderer.device, &config);
        renderer.queue.write_buffer(
            &renderer.present,
            0,
            bytemuck::cast_slice(&[u32::from(format.is_srgb()), 0, 0, 0]),
        );
        let blit = renderer.blit_pipeline(format);
        renderer.surface = Some(Surface { surface, config, blit });
        Ok(renderer)
    }

    /// A renderer without a window, drawing only for [`Self::capture`], as
    /// if into a window of the game's size.
    pub fn headless(options: Options) -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let request = |fallback| {
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                force_fallback_adapter: fallback,
                compatible_surface: None,
                apply_limit_buckets: false,
            }))
        };
        let adapter = request(options.software).or_else(|_| request(!options.software)).map_err(|e| {
            format!("no graphics adapter: {e} (install Mesa's lavapipe or SwiftShader for headless drawing)")
        })?;
        Self::with_adapter(&adapter, options.game, options)
    }

    fn with_adapter(adapter: &wgpu::Adapter, window: [u32; 2], options: Options) -> Result<Self, String> {
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("renderer"),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| format!("graphics device: {e}"))?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("renderer"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blit.wgsl").into()),
        });
        let uniform = |binding, dynamic, size: usize| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: wgpu::BufferSize::new(size as u64),
            },
            count: None,
        };
        let texture_entries = |first| {
            [
                wgpu::BindGroupLayoutEntry {
                    binding: first,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: first + 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ]
        };
        let group0_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("frame"),
            entries: &[uniform(0, false, size_of::<Globals>()), uniform(1, true, size_of::<DrawUniform>())],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture"),
            entries: &texture_entries(0),
        });
        let [canvas_texture, canvas_sampler] = texture_entries(0);
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("blit"),
            entries: &[canvas_texture, canvas_sampler, uniform(2, false, 16)],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("renderer"),
            bind_group_layouts: &[Some(&group0_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let align = u64::from(device.limits().min_uniform_buffer_offset_alignment);
        let draw_stride = (size_of::<DrawUniform>() as u64).next_multiple_of(align);
        let draw_capacity = 64;
        let draws = Self::draw_buffer(&device, draw_stride * draw_capacity);
        let group0 = Self::frame_group(&device, &group0_layout, &globals, &draws);
        let sampler = |filter| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                mag_filter: filter,
                min_filter: filter,
                address_mode_u: wgpu::AddressMode::Repeat,
                address_mode_v: wgpu::AddressMode::Repeat,
                ..Default::default()
            })
        };
        let (nearest, linear) = (sampler(wgpu::FilterMode::Nearest), sampler(wgpu::FilterMode::Linear));
        let present = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("present"),
            contents: bytemuck::cast_slice(&[0u32; 4]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let mut renderer = Self {
            device,
            queue,
            surface: None,
            window,
            game: options.game,
            display: options.display,
            shader,
            blit_shader,
            layout,
            blit_layout,
            texture_layout,
            pipelines: HashMap::new(),
            globals,
            draws,
            draw_stride,
            draw_capacity,
            group0_layout,
            group0,
            nearest,
            linear,
            present,
            sprite_vertices: None,
            textures: Vec::new(),
            models: Vec::new(),
            bounds: Vec::new(),
            canvas: None,
            white: TextureId(0),
        };
        renderer.white = renderer.texture(1, 1, &[255; 4]);
        Ok(renderer)
    }

    fn draw_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("draws"),
            size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn frame_group(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        globals: &wgpu::Buffer,
        draws: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: draws,
                        offset: 0,
                        size: wgpu::BufferSize::new(size_of::<DrawUniform>() as u64),
                    }),
                },
            ],
        })
    }

    /// The window's new size in pixels.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.window = [width.max(1), height.max(1)];
        if let Some(s) = &mut self.surface {
            s.config.width = self.window[0];
            s.config.height = self.window[1];
            s.surface.configure(&self.device, &s.config);
        }
    }

    pub fn display(&self) -> Display {
        self.display
    }

    /// Switches look, aspect or effects; the next frame draws with them.
    pub fn set_display(&mut self, display: Display) {
        self.display = display;
    }

    /// The game's display size, as given in [`Options`].
    pub fn game_size(&self) -> [u32; 2] {
        self.game
    }

    /// The screen sprites are placed on, in pixels: see [`layout_size`].
    pub fn layout_size(&self) -> [u32; 2] {
        layout_size(&self.display, self.game, self.window)
    }

    /// A 1x1 white texture, for plain rectangles.
    pub fn white(&self) -> TextureId {
        self.white
    }

    /// A texture from RGBA bytes, rows top to bottom.
    pub fn texture(&mut self, width: u32, height: u32, rgba: &[u8]) -> TextureId {
        assert_eq!(rgba.len(), (width * height * 4) as usize, "texture needs width x height RGBA");
        let size = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
        let texture = self.device.create_texture_with_data(
            &self.queue,
            &wgpu::TextureDescriptor {
                label: Some("texture"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            rgba,
        );
        let view = texture.create_view(&Default::default());
        let group = |sampler| self.texture_group(&self.texture_layout, &view, sampler);
        let (nearest, linear) = (group(&self.nearest), group(&self.linear));
        self.textures.push(Texture { size: [width, height], nearest, linear });
        TextureId(self.textures.len() - 1)
    }

    fn texture_group(
        &self,
        layout: &wgpu::BindGroupLayout,
        view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("texture"),
            layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
            ],
        })
    }

    /// A texture's size in texels.
    pub fn texture_size(&self, texture: TextureId) -> [u32; 2] {
        self.textures[texture.0].size
    }

    /// A PNG as a texture.
    pub fn load_png(&mut self, path: &Path) -> Result<TextureId, String> {
        let (width, height, rgba) = read_png(path)?;
        Ok(self.texture(width, height, &rgba))
    }

    /// A model built in code.
    pub fn model(&mut self, mesh: &Mesh) -> ModelId {
        let points = mesh.primitives.iter().flat_map(|p| &p.vertices).map(|v| Vec3::from(v.position));
        self.bounds
            .push(points.fold(None, |b, p| Some(b.map_or((p, p), |(lo, hi): (Vec3, Vec3)| (lo.min(p), hi.max(p))))));
        let primitives = mesh
            .primitives
            .iter()
            .filter(|p| !p.vertices.is_empty())
            .map(|p| {
                let vertices = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("vertices"),
                    contents: bytemuck::cast_slice(&p.vertices),
                    usage: wgpu::BufferUsages::VERTEX,
                });
                let indices = (!p.indices.is_empty()).then(|| {
                    self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("indices"),
                        contents: bytemuck::cast_slice(&p.indices),
                        usage: wgpu::BufferUsages::INDEX,
                    })
                });
                let count = if p.indices.is_empty() { p.vertices.len() } else { p.indices.len() } as u32;
                GpuPrimitive { vertices, indices, count, texture: p.texture, alpha: p.alpha, lit: p.lit }
            })
            .collect();
        self.models.push(primitives);
        ModelId(self.models.len() - 1)
    }

    /// The smallest and largest corner of a model's box, in its own units;
    /// None when it has no vertices.
    pub fn bounds(&self, model: ModelId) -> Option<(Vec3, Vec3)> {
        self.bounds[model.0]
    }

    /// Every mesh of a glTF file's default scene as one model, in the
    /// file's units, with its textures.
    pub fn load_gltf(&mut self, path: &Path) -> Result<ModelId, String> {
        let mesh = gltf_load::load(self, path)?;
        Ok(self.model(&mesh))
    }

    /// The projection for the canvas's shape: the camera's vertical field
    /// of view at any width.
    fn clip_from_world(&self, camera: &Camera, canvas: [u32; 2]) -> Mat4 {
        let aspect = canvas[0] as f32 / canvas[1].max(1) as f32;
        glam::camera::rh::proj::directx::perspective(camera.fov_y, aspect, camera.near, camera.far) * camera.view
    }

    fn ensure_canvas(&mut self) {
        let size = canvas_size(&self.display, self.game, self.window);
        let samples = if self.display.look == Look::Modern { 4 } else { 1 };
        if self.canvas.as_ref().is_some_and(|c| c.size == size && c.samples == samples) {
            return;
        }
        let texture = |format, samples, usage| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("canvas"),
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: samples,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        let attachment = wgpu::TextureUsages::RENDER_ATTACHMENT;
        let color = texture(
            CANVAS_FORMAT,
            1,
            attachment | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
        );
        let view = color.create_view(&Default::default());
        let msaa = (samples > 1).then(|| texture(CANVAS_FORMAT, samples, attachment).create_view(&Default::default()));
        let depth = texture(DEPTH_FORMAT, samples, attachment).create_view(&Default::default());
        let blit_group = |sampler: &wgpu::Sampler| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("blit"),
                layout: &self.blit_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
                    wgpu::BindGroupEntry { binding: 2, resource: self.present.as_entire_binding() },
                ],
            })
        };
        let (nearest, linear) = (blit_group(&self.nearest), blit_group(&self.linear));
        self.canvas = Some(Canvas { size, samples, color, view, msaa, depth, nearest, linear });
    }

    fn pipeline(&mut self, key: PipelineKey) {
        if self.pipelines.contains_key(&key) {
            return;
        }
        let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x4];
        let opaque = key.blend == Blend::Opaque;
        let pipeline = self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(if key.sprite { "sprites" } else { "models" }),
            layout: Some(&self.layout),
            vertex: wgpu::VertexState {
                module: &self.shader,
                entry_point: Some(if key.sprite { "vs_sprite" } else { "vs_model" }),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<Vertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes,
                })],
            },
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(!key.sprite && opaque),
                depth_compare: Some(if key.sprite {
                    wgpu::CompareFunction::Always
                } else {
                    wgpu::CompareFunction::Less
                }),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState { count: key.samples, ..Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &self.shader,
                entry_point: Some(if key.sprite { "fs_sprite" } else { "fs_model" }),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: CANVAS_FORMAT,
                    blend: key.blend.state(),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        self.pipelines.insert(key, pipeline);
    }

    fn blit_pipeline(&self, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        let layout = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("blit"),
            bind_group_layouts: &[Some(&self.blit_layout)],
            immediate_size: 0,
        });
        self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("blit"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &self.blit_shader,
                entry_point: Some("vs_blit"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.blit_shader,
                entry_point: Some("fs_blit"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    /// Draws the scene into the canvas.
    fn draw_canvas(&mut self, scene: &Scene, encoder: &mut wgpu::CommandEncoder) {
        self.ensure_canvas();
        let (size, samples) = {
            let c = self.canvas.as_ref().expect("canvas");
            (c.size, c.samples)
        };
        let layout = self.layout_size();
        let ps1 = self.display.look == Look::Ps1;
        let effects = self.display.ps1;
        let mut look = 0;
        if ps1 {
            look |= LOOK_AFFINE;
            look |= if effects.vertex_snap { LOOK_SNAP } else { 0 };
            look |= if effects.dither { LOOK_DITHER } else { 0 };
            look |= if effects.quantize { LOOK_QUANTIZE } else { 0 };
        }
        let mut globals = Globals {
            clip_from_world: self.clip_from_world(&scene.camera, size).to_cols_array_2d(),
            view_from_world: scene.camera.view.to_cols_array_2d(),
            ambient: [scene.ambient[0], scene.ambient[1], scene.ambient[2], 0.0],
            light_dirs: [[0.0; 4]; 4],
            light_colors: [[0.0; 4]; 4],
            fog_color: [0.0; 4],
            fog: [0.0; 4],
            sizes: [size[0] as f32, size[1] as f32, layout[0] as f32, layout[1] as f32],
            look: [look, scene.lights.len().min(MAX_LIGHTS) as u32, 0, 0],
        };
        for (i, light) in scene.lights.iter().take(MAX_LIGHTS).enumerate() {
            let d = light.towards.normalize_or_zero();
            globals.light_dirs[i] = [d.x, d.y, d.z, 0.0];
            globals.light_colors[i] = [light.color[0], light.color[1], light.color[2], 0.0];
        }
        if let Some(fog) = scene.fog {
            globals.fog_color = [fog.color[0], fog.color[1], fog.color[2], 1.0];
            globals.fog = [fog.near, fog.far, 1.0, 0.0];
        }
        self.queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));

        // Opaque primitives first, then the semi-transparent ones in order.
        let mut calls = Vec::new();
        for pass in [false, true] {
            for instance in &scene.models {
                let Some(primitives) = self.models.get(instance.model.0) else { continue };
                let normal = instance.transform.inverse().transpose();
                for (i, p) in primitives.iter().enumerate() {
                    let semi = p.alpha == Alpha::Blend;
                    if semi != pass {
                        continue;
                    }
                    let blend = if semi { instance.blend } else { Blend::Opaque };
                    let mut flags = 0;
                    flags |= if p.texture.is_some() { TEXTURED } else { 0 };
                    flags |= if p.lit && !scene.lights.is_empty() { LIT } else { 0 };
                    flags |= if p.alpha != Alpha::Opaque { CUTOUT } else { 0 };
                    flags |= if semi && p.texture.is_some() { STP_TEXEL } else { 0 };
                    calls.push(Call {
                        key: PipelineKey { sprite: false, blend, samples },
                        uniform: DrawUniform {
                            world_from_local: instance.transform.to_cols_array_2d(),
                            normal_from_local: normal.to_cols_array_2d(),
                            tint: instance.tint,
                            flags: [flags, blend.code(), 0, 0],
                        },
                        texture: p.texture.unwrap_or(self.white),
                        source: Source::Model(instance.model, i),
                    });
                }
            }
        }
        // Sprites: six vertices each, one call per run of the same texture and blend.
        let mut vertices: Vec<Vertex> = Vec::with_capacity(scene.sprites.len() * 6);
        for (n, s) in scene.sprites.iter().enumerate() {
            let tex = self.textures.get(s.texture.0).map_or([1, 1], |t| t.size);
            let (u0, v0) = (s.src[0] / tex[0] as f32, s.src[1] / tex[1] as f32);
            let (u1, v1) = ((s.src[0] + s.src[2]) / tex[0] as f32, (s.src[1] + s.src[3]) / tex[1] as f32);
            let (x0, y0, x1, y1) = (s.dst[0], s.dst[1], s.dst[0] + s.dst[2], s.dst[1] + s.dst[3]);
            let corner = |x, y, u, v| Vertex { position: [x, y, 0.0], normal: [0.0; 3], uv: [u, v], color: s.color };
            let a = corner(x0, y0, u0, v0);
            let b = corner(x1, y0, u1, v0);
            let c = corner(x1, y1, u1, v1);
            let d = corner(x0, y1, u0, v1);
            vertices.extend([a, b, c, a, c, d]);
            let start = n as u32 * 6;
            let same = matches!(calls.last(), Some(Call { source: Source::Sprites(r), texture, key, .. })
                if r.end == start && *texture == s.texture && key.blend == s.blend);
            if same {
                if let Some(Call { source: Source::Sprites(r), .. }) = calls.last_mut() {
                    r.end = start + 6;
                }
            } else {
                calls.push(Call {
                    key: PipelineKey { sprite: true, blend: s.blend, samples },
                    uniform: DrawUniform {
                        world_from_local: Mat4::IDENTITY.to_cols_array_2d(),
                        normal_from_local: Mat4::IDENTITY.to_cols_array_2d(),
                        tint: [1.0; 4],
                        flags: [TEXTURED | if s.blend == Blend::Opaque { CUTOUT } else { 0 }, s.blend.code(), 0, 0],
                    },
                    texture: s.texture,
                    source: Source::Sprites(start..start + 6),
                });
            }
        }
        if !vertices.is_empty() {
            let bytes: &[u8] = bytemuck::cast_slice(&vertices);
            let fits = self.sprite_vertices.as_ref().is_some_and(|(_, size)| *size >= bytes.len() as u64);
            if !fits {
                let size = (bytes.len() as u64).next_power_of_two();
                let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("sprites"),
                    size,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                self.sprite_vertices = Some((buffer, size));
            }
            let (buffer, _) = self.sprite_vertices.as_ref().expect("sprite buffer");
            self.queue.write_buffer(buffer, 0, bytes);
        }
        if calls.len() as u64 > self.draw_capacity {
            self.draw_capacity = (calls.len() as u64).next_power_of_two();
            self.draws = Self::draw_buffer(&self.device, self.draw_stride * self.draw_capacity);
            self.group0 = Self::frame_group(&self.device, &self.group0_layout, &self.globals, &self.draws);
        }
        let mut uniforms = vec![0u8; (self.draw_stride as usize) * calls.len()];
        for (i, call) in calls.iter().enumerate() {
            let at = i * self.draw_stride as usize;
            uniforms[at..at + size_of::<DrawUniform>()].copy_from_slice(bytemuck::bytes_of(&call.uniform));
        }
        if !uniforms.is_empty() {
            self.queue.write_buffer(&self.draws, 0, &uniforms);
        }
        for call in &calls {
            self.pipeline(call.key);
        }

        let canvas = self.canvas.as_ref().expect("canvas");
        let [r, g, b] = scene.clear;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("canvas"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: canvas.msaa.as_ref().unwrap_or(&canvas.view),
                depth_slice: None,
                resolve_target: canvas.msaa.as_ref().map(|_| &canvas.view),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r: r.into(), g: g.into(), b: b.into(), a: 1.0 }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: &canvas.depth,
                depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        for (i, call) in calls.iter().enumerate() {
            pass.set_pipeline(&self.pipelines[&call.key]);
            pass.set_bind_group(0, &self.group0, &[(i as u64 * self.draw_stride) as u32]);
            let texture = &self.textures[call.texture.0];
            // Sprites keep their texels sharp in either look.
            let filtered = !ps1 && matches!(call.source, Source::Model(..));
            pass.set_bind_group(1, if filtered { &texture.linear } else { &texture.nearest }, &[]);
            match &call.source {
                Source::Model(model, p) => {
                    let p = &self.models[model.0][*p];
                    pass.set_vertex_buffer(0, p.vertices.slice(..));
                    match &p.indices {
                        Some(indices) => {
                            pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                            pass.draw_indexed(0..p.count, 0, 0..1);
                        }
                        None => pass.draw(0..p.count, 0..1),
                    }
                }
                Source::Sprites(range) => {
                    let (buffer, _) = self.sprite_vertices.as_ref().expect("sprite buffer");
                    pass.set_vertex_buffer(0, buffer.slice(..));
                    pass.draw(range.clone(), 0..1);
                }
            }
        }
    }

    /// Draws the scene and shows it in the window, letterboxed. Does nothing
    /// without a window, or while the window cannot be drawn (minimised).
    pub fn render(&mut self, scene: &Scene) -> Result<(), String> {
        let Some(surface) = &self.surface else { return Ok(()) };
        let frame = match surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                surface.surface.configure(&self.device, &surface.config);
                return Ok(());
            }
            _ => return Ok(()),
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.draw_canvas(scene, &mut encoder);
        let canvas = self.canvas.as_ref().expect("canvas");
        let surface = self.surface.as_ref().expect("surface");
        let target = frame.texture.create_view(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            let [x, y, w, h] = fit(self.window, canvas.size);
            pass.set_viewport(x, y, w.max(1.0), h.max(1.0), 0.0, 1.0);
            pass.set_pipeline(&surface.blit);
            // The PS1 canvas scales up with sharp pixels; the modern one is
            // already at the window's resolution.
            let group = if self.display.look == Look::Ps1 { &canvas.nearest } else { &canvas.linear };
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        Ok(())
    }

    /// Draws the scene and reads the canvas back: the game's resolution in
    /// the PS1 look, the window's in the modern one.
    pub fn capture(&mut self, scene: &Scene) -> Result<Frame, String> {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.draw_canvas(scene, &mut encoder);
        let canvas = self.canvas.as_ref().expect("canvas");
        let [width, height] = canvas.size;
        let row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("capture"),
            size: u64::from(row * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            canvas.color.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);
        let (sender, receiver) = std::sync::mpsc::channel();
        buffer.map_async(wgpu::MapMode::Read, .., move |r| drop(sender.send(r)));
        self.device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
            .map_err(|e| format!("capture: {e}"))?;
        receiver.recv().map_err(|e| e.to_string())?.map_err(|e| format!("capture: {e}"))?;
        let mapped = buffer.get_mapped_range(..).map_err(|e| format!("capture: {e}"))?;
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            let at = (y * row) as usize;
            rgba.extend_from_slice(&mapped[at..at + (width * 4) as usize]);
        }
        drop(mapped);
        buffer.unmap();
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        Ok(Frame { width, height, rgba })
    }
}

/// A PNG's size and RGBA bytes.
pub fn read_png(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
    let pixels = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Rgb => pixels.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => {
            pixels.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect()
        }
        png::ColorType::Grayscale => pixels.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        png::ColorType::Indexed => return Err(format!("{}: palette not expanded", path.display())),
    };
    Ok((info.width, info.height, rgba))
}

/// A cube of side `size` centred on the origin, one colour per face, with
/// normals: for examples and tests.
pub fn cube(size: f32, colors: [[f32; 3]; 6]) -> Mesh {
    let h = size / 2.0;
    let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
        ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]),
        ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]),
        ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
        ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
        ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
        ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    ];
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for ((n, u, v), c) in faces.iter().zip(colors) {
        let (n, u, v) = (Vec3::from(*n), Vec3::from(*u), Vec3::from(*v));
        let base = vertices.len() as u32;
        for (su, sv, uv) in
            [(-1.0, -1.0, [0.0, 1.0]), (1.0, -1.0, [1.0, 1.0]), (1.0, 1.0, [1.0, 0.0]), (-1.0, 1.0, [0.0, 0.0])]
        {
            let p = (n + u * su + v * sv) * h;
            vertices.push(Vertex { position: p.into(), normal: n.into(), uv, color: [c[0], c[1], c[2], 1.0] });
        }
        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Mesh { primitives: vec![Primitive { vertices, indices, texture: None, alpha: Alpha::Opaque, lit: true }] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_keeps_the_game_height_and_follows_the_window() {
        let mut d = Display::default();
        assert_eq!(layout_size(&d, [320, 240], [1920, 1080]), [320, 240]);
        d.aspect = Aspect::Window;
        assert_eq!(layout_size(&d, [320, 240], [1920, 1080]), [426, 240]);
        assert_eq!(layout_size(&d, [320, 240], [1000, 1000]), [240, 240]);
        assert_eq!(width_for_aspect(240, [4000, 10]), 4096);
    }

    #[test]
    fn the_modern_canvas_is_the_window_shape_at_window_resolution() {
        let mut d = Display { look: Look::Modern, ..Default::default() };
        assert_eq!(canvas_size(&d, [320, 240], [1920, 1080]), [1440, 1080]);
        d.aspect = Aspect::Window;
        assert_eq!(canvas_size(&d, [320, 240], [1920, 1080]), [1917, 1080]);
        d.look = Look::Ps1;
        assert_eq!(canvas_size(&d, [320, 240], [1920, 1080]), [426, 240]);
    }

    #[test]
    fn fitting_never_stretches() {
        for window in [[1600, 900], [300, 900], [160, 120]] {
            let [_, _, w, h] = fit(window, [320, 240]);
            assert!(w <= window[0] as f32 + 0.001 && h <= window[1] as f32 + 0.001);
            assert!((w / h - 320.0 / 240.0).abs() < 1e-4);
        }
    }

    #[test]
    fn display_settings_read_with_defaults() {
        let d: Display = serde_json::from_str(r#"{"look": "modern"}"#).unwrap();
        assert_eq!(d, Display { look: Look::Modern, ..Default::default() });
    }
}
