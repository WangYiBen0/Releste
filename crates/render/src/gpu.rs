//! wgpu render backend.
//!
//! # Manual checklist (AGENTS.md 9: pure rendering code has no unit tests, but needs a checklist)
//! - [ ] After a window resize the image is neither stretched nor distorted (letterbox works)
//! - [ ] With camera zoom 1.0, sprites within 320x180 align 1:1 with the art pixels
//! - [ ] With many sprites (>5000) the draw call count equals the texture group count, and the frame time stays < 4ms
//! - [ ] Texture sampling uses nearest, with no blurring (preserving the pixel look)
//! - [ ] Alpha blending is premultiplied, so semi-transparent edges do not darken
//! - [ ] After switching out of and back into the window, the surface can still be reconfigured
//!
//! # `.data` payloads
//! The vanilla `.data` is not Crunch but a per-command **pixel RLE**. `tools/content-pipeline`
//! decodes it to PNG offline (`kind = "png"`); payloads the pipeline has not yet converted
//! (per-sprite atlases) are skipped with a `warn` instead of rendering wrong content
//! (AGENTS.md 13).

use std::collections::HashMap;
use std::path::Path;

use bytemuck::{Pod, Zeroable};
use releste_math::Vec2;
use thiserror::Error;
use tracing::{info, warn};

use crate::atlas::{SpriteAtlas, TextureKind};
use crate::batch::{SpriteBatch, SpriteVertex};
use crate::camera::Camera;

/// Render error.
#[derive(Debug, Error)]
pub enum RenderError {
    #[error("no suitable GPU adapter found: {0}")]
    NoAdapter(String),
    #[error("failed to create GPU device: {0}")]
    Device(String),
    #[error("surface error: {0}")]
    Surface(String),
    #[error("texture decode failed for {path}: {source}")]
    TextureDecode {
        path: String,
        source: image::ImageError,
    },
    #[error("texture I/O failed for {path}: {source}")]
    TextureIo {
        path: String,
        source: std::io::Error,
    },
    #[error(
        "texture `{path}` uses {kind:?}, which is Crunch-compressed. \
         Pixel decoding is not implemented yet (AGENTS.md 13). \
         Re-export the atlas with PNG payloads in the content pipeline."
    )]
    UndecodableTexture { path: String, kind: TextureKind },
    #[error("atlas error: {0}")]
    Atlas(#[from] crate::atlas::AtlasError),
    #[error("vertex data is not Pod-aligned")]
    BufferCast,
}

/// Renderer configuration.
#[derive(Debug, Clone)]
pub struct GpuRendererConfig {
    /// Initial window width (physical pixels).
    pub width: u32,
    /// Initial window height (physical pixels).
    pub height: u32,
    /// Vertical sync.
    pub vsync: bool,
    /// MSAA sample count (1 = off).
    pub msaa_samples: u32,
}

impl Default for GpuRendererConfig {
    fn default() -> Self {
        GpuRendererConfig {
            width: 1280,
            height: 720,
            vsync: true,
            // Pixel look: MSAA blurs the art, so it is off by default.
            msaa_samples: 1,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    /// Reserved: time / global tint.
    time: f32,
    _pad: [f32; 3],
}

/// An uploaded texture.
///
/// The `bind_group` holds a reference to the texture view internally, so the view does not need to be stored separately.
struct GpuTexture {
    bind_group: wgpu::BindGroup,
    /// Texture pixel size (needed by `uv_for_sprite` to normalize UVs).
    size: (u32, u32),
}

/// wgpu renderer.
pub struct GpuRenderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    uniform_buffer: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    vertex_buffer: wgpu::Buffer,
    vertex_capacity: usize,
    textures: HashMap<u32, GpuTexture>,
    sampler: wgpu::Sampler,
    /// Actual viewport size.
    viewport: (u32, u32),
    /// Clear color (the vanilla `Level`'s `<Style color=...>` -> `BackgroundColor`;
    /// see `pureCeleste.cs:112430-112436` for the `MapData`/`Style` parsing).
    /// Defaults to pure black, matching the vanilla `Level.BackgroundColor` default.
    clear_color: wgpu::Color,
}

impl GpuRenderer {
    /// Creates the renderer.
    ///
    /// `window` must be passed as an `Arc` (wgpu requires the surface to be `'static`).
    pub async fn new(
        window: std::sync::Arc<winit::window::Window>,
        config: GpuRendererConfig,
    ) -> Result<Self, RenderError> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance
            .create_surface(window)
            .map_err(|e| RenderError::Surface(e.to_string()))?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| {
                RenderError::NoAdapter("no adapter compatible with the target surface".to_string())
            })?;

        let mut limits = wgpu::Limits::downlevel_defaults();
        // Vanilla atlases are at most 4096x2048; leave some headroom.
        limits.max_texture_dimension_2d = 8192;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("releste-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: limits,
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .map_err(|e| RenderError::Device(e.to_string()))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);

        let present_mode = if config.vsync {
            wgpu::PresentMode::AutoVsync
        } else {
            wgpu::PresentMode::AutoNoVsync
        };

        let surface_config = wgpu::SurfaceConfiguration {
            // COPY_SRC: `--screenshot` copies the render target back to the CPU to read pixels.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            format,
            width: config.width.max(1),
            height: config.height.max(1),
            present_mode,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("sprite-shader"),
            source: wgpu::ShaderSource::Wgsl(SPRITE_WGSL.into()),
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("uniform-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("uniform-bind-group"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("texture-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sprite-pipeline-layout"),
            bind_group_layouts: &[&uniform_layout, &texture_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("sprite-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<SpriteVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, // position
                        1 => Float32x2, // uv
                        2 => Float32x4  // color
                    ],
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState {
                count: config.msaa_samples.clamp(1, 4),
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });

        // Pixel look: nearest sampling.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("nearest-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let vertex_capacity = crate::batch::DEFAULT_CAPACITY * 6;
        let vertex_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sprite-vertices"),
            size: (vertex_capacity * std::mem::size_of::<SpriteVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        info!(
            adapter = adapter.get_info().name,
            backend = ?adapter.get_info().backend,
            format = ?format,
            "wgpu renderer initialised"
        );

        Ok(GpuRenderer {
            device,
            queue,
            surface,
            config: surface_config,
            pipeline,
            uniform_buffer,
            uniform_bind_group,
            vertex_buffer,
            vertex_capacity,
            textures: HashMap::new(),
            sampler,
            viewport: (config.width.max(1), config.height.max(1)),
            clear_color: wgpu::Color::BLACK,
        })
    }

    /// Sets the clear color (used by each frame's `LoadOp::Clear`).
    ///
    /// Driven by the map style's `background_color` (`MapStyle::background_color`);
    /// see `pureCeleste.cs:112434` (`Calc.HexToColor` -> `BackgroundColor`).
    pub fn set_clear_color(&mut self, rgb: [u8; 3]) {
        self.clear_color = wgpu::Color {
            r: f64::from(rgb[0]) / 255.0,
            g: f64::from(rgb[1]) / 255.0,
            b: f64::from(rgb[2]) / 255.0,
            a: 1.0,
        };
    }

    /// Current viewport size.
    pub fn viewport(&self) -> (u32, u32) {
        self.viewport
    }

    /// Surface pixel format.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Reconfigures the surface after a window size change.
    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if (width, height) == self.viewport {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.viewport = (width, height);
        self.surface.configure(&self.device, &self.config);
    }

    /// Uploads a PNG texture to the GPU and binds it to `atlas_id`.
    pub fn upload_png(&mut self, atlas_id: u32, path: &Path) -> Result<(), RenderError> {
        let img = image::open(path).map_err(|source| RenderError::TextureDecode {
            path: path.display().to_string(),
            source,
        })?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        self.upload_rgba(atlas_id, w, h, rgba.as_raw())
    }

    /// Uploads RGBA8 pixels directly.
    pub fn upload_rgba(
        &mut self,
        atlas_id: u32,
        width: u32,
        height: u32,
        pixels: &[u8],
    ) -> Result<(), RenderError> {
        let size = wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        };

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("atlas-texture"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4 * size.width),
                rows_per_image: Some(size.height),
            },
            size,
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas-bind-group"),
            layout: &self.pipeline.get_bind_group_layout(1),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        self.textures.insert(
            atlas_id,
            GpuTexture {
                bind_group,
                size: (size.width, size.height),
            },
        );
        Ok(())
    }

    /// Loads all decodable (PNG) textures from a [`SpriteAtlas`].
    ///
    /// `.data` payloads the pipeline has not converted yet are skipped with a `warn` (see the module docs).
    pub fn load_atlas(&mut self, atlas: &SpriteAtlas) -> Result<usize, RenderError> {
        let mut loaded = 0usize;
        for (id, group) in atlas.descriptor().textures.iter().enumerate() {
            let id = id as u32;
            if !group.kind.is_decodable() {
                warn!(
                    group = %group.name,
                    kind = ?group.kind,
                    "skipping payload that is not a PNG (content-pipeline has not converted it)"
                );
                continue;
            }
            self.upload_png(id, &atlas.group_payload_path(group))?;
            loaded += 1;
        }
        Ok(loaded)
    }

    /// Uploads one frame of vertices and draws it.
    ///
    /// Call `batch.sort()` first.
    pub fn render(&mut self, batch: &SpriteBatch, camera: &Camera) -> Result<(), RenderError> {
        self.render_inner(batch, camera, None)
    }

    /// Renders one frame and reads the pixels back to the CPU: compact RGBA8 (BGRA surface formats are swapped back to RGBA).
    /// Returns `(width, height, pixels)` -- the height may be shorter than the viewport (see below).
    ///
    /// "Is the drawing correct?" should not rely only on staring at the window -- `--screenshot` uses this
    /// to write the real pipeline output to a PNG, so CI / remote environments can check the render result too.
    ///
    /// `copy_texture_to_buffer` requires the row pitch to be 256-aligned, so the pitch is >= `width*4`.
    /// Some drivers (llvmpipe) shrink the mapped range back to a padding-free size, so with the pitch
    /// only the **first several whole rows** can be read -- the cropped part is the bottom (usually the
    /// letterbox black bars in a window) and the game content is still there; take the actual row count,
    /// prefer missing content over crashing.
    pub fn capture_pixels(
        &mut self,
        batch: &SpriteBatch,
        camera: &Camera,
    ) -> Result<(u32, u32, Vec<u8>), RenderError> {
        let (width, height) = self.viewport;
        let bytes_per_row = (width * 4).div_ceil(256) * 256;
        let mut capture = FrameCapture::new(&self.device, width, height, bytes_per_row);
        self.render_inner(batch, camera, Some(&mut capture))?;
        let padded = capture.read(&self.device);
        let rows = (padded.len() / bytes_per_row as usize) as u32;
        if rows != height {
            warn!(rows, height, "frame capture got fewer rows than requested");
        }
        let swap_rb = matches!(
            self.config.format,
            wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
        );
        let pixels = unpack_rows(&padded, width, rows, bytes_per_row, swap_rb);
        Ok((width, rows, pixels))
    }

    /// Renders one frame; when `capture` is given, additionally copies the render target into a CPU-readable buffer.
    fn render_inner(
        &mut self,
        batch: &SpriteBatch,
        camera: &Camera,
        capture: Option<&mut FrameCapture>,
    ) -> Result<(), RenderError> {
        let vertices = batch.vertices(camera, self.viewport);
        if vertices.len() > self.vertex_capacity {
            return Err(RenderError::BufferCast);
        }

        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                self.surface
                    .get_current_texture()
                    .map_err(|e| RenderError::Surface(e.to_string()))?
            }
            Err(e) => return Err(RenderError::Surface(e.to_string())),
        };

        // Clear even when no sprite is visible -- otherwise the first frame leaves uninitialized surface contents.
        self.queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&Uniforms {
                time: 0.0,
                _pad: [0.0; 3],
            }),
        );
        if !vertices.is_empty() {
            self.queue
                .write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&vertices));
        }

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("sprite-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(self.clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));

            // One run = one draw call.
            for range in batch.ranges() {
                let Some(tex) = self.textures.get(&range.atlas) else {
                    // A texture that was not uploaded (for example `.data` not yet converted): skip instead of rendering wrong content.
                    continue;
                };
                pass.set_bind_group(1, &tex.bind_group, &[]);
                let start = (range.start * 6) as u32;
                let count = (range.len * 6) as u32;
                pass.draw(start..start + count, 0..1);
            }
        }

        // Screenshot: copy the whole render target into the staging buffer before submit
        // (on the GPU timeline this is independent of present; present still happens as
        // usual and the swapchain cadence is unchanged).
        if let Some(cap) = capture {
            encoder.copy_texture_to_buffer(
                frame.texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer: &cap.buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(cap.bytes_per_row),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width: cap.width,
                    height: cap.height,
                    depth_or_array_layers: 1,
                },
            );
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }

    /// Number of uploaded textures.
    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }

    /// Vertex capacity.
    pub fn vertex_capacity(&self) -> usize {
        self.vertex_capacity
    }

    /// Whether a given texture group has been uploaded.
    pub fn has_texture(&self, atlas_id: u32) -> bool {
        self.textures.contains_key(&atlas_id)
    }

    /// Pixel size of an uploaded texture group; returns `None` when it is not uploaded
    /// (for example `.data` not yet converted).
    ///
    /// The `.atlas` descriptor does not record texture sizes, so normalized UVs can only be
    /// asked of the GPU side.
    pub fn texture_size(&self, atlas_id: u32) -> Option<(u32, u32)> {
        self.textures.get(&atlas_id).map(|t| t.size)
    }

    /// Convenience: computes a sprite's UVs from world coordinates.
    pub fn uv_for_sprite(
        sprite: &crate::atlas::Sprite,
        texture_size: Vec2,
    ) -> ((f32, f32), (f32, f32)) {
        let tw = texture_size.x.to_f32().max(1.0);
        let th = texture_size.y.to_f32().max(1.0);
        let x0 = f32::from(sprite.x) / tw;
        let y0 = f32::from(sprite.y) / th;
        let x1 = f32::from(sprite.x + sprite.w) / tw;
        let y1 = f32::from(sprite.y + sprite.h) / th;
        ((x0, y0), (x1, y1))
    }
}

/// Sprite shader.
///
/// Vertex coordinates are already transformed to NDC on the CPU side
/// (see [`crate::batch::SpriteBatch::vertices`]), so this only passes them through.
const SPRITE_WGSL: &str = r#"
struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = vec4<f32>(in.position, 0.0, 1.0);
    out.uv = in.uv;
    out.color = in.color;
    return out;
}

@group(1) @binding(0) var t_diffuse: texture_2d<f32>;
@group(1) @binding(1) var s_diffuse: sampler;

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return textureSample(t_diffuse, s_diffuse, in.uv) * in.color;
}
"#;

/// CPU readback staging for one frame.
///
/// `copy_texture_to_buffer` requires each row to be 256-byte aligned, so the row pitch may be
/// larger than `width*4`; [`FrameCapture::read`] returns the **raw rows with padding**, which
/// [`unpack_rows`] trims and swaps R/B as needed.
struct FrameCapture {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    bytes_per_row: u32,
}

impl FrameCapture {
    /// Creates a mapped buffer for a `width x height` frame with row pitch `bytes_per_row` (256-aligned).
    fn new(device: &wgpu::Device, width: u32, height: u32, bytes_per_row: u32) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame-capture"),
            size: bytes_per_row as u64 * height.max(1) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Self {
            buffer,
            width,
            height,
            bytes_per_row,
        }
    }

    /// Reads the whole frame back after submission completes (blocking until the mapping is done).
    ///
    /// Returns the bytes **as-is** within the mapped range; for the row pitch see [`FrameCapture::bytes_per_row`].
    fn read(self, device: &wgpu::Device) -> Vec<u8> {
        let (tx, rx) = std::sync::mpsc::channel();
        let slice = self.buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result.is_ok());
        });
        device.poll(wgpu::Maintain::Wait);
        let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
        let data = slice.get_mapped_range();
        let out = data.to_vec();
        drop(data);
        self.buffer.unmap();
        out
    }
}

/// Converts padded frame-buffer rows into compact RGBA8; `swap_rb` handles BGRA surface formats.
fn unpack_rows(
    padded: &[u8],
    width: u32,
    height: u32,
    bytes_per_row: u32,
    swap_rb: bool,
) -> Vec<u8> {
    let stride = (width * 4) as usize;
    let row = bytes_per_row as usize;
    let mut out = Vec::with_capacity(stride * height as usize);
    for r in 0..height as usize {
        let src = &padded[r * row..r * row + stride];
        if swap_rb {
            // `as_chunks` turns each RGBA pixel into a `&[u8; 4]` (clippy
            // chunks_exact_to_as_chunks: write it this way when the chunk size is constant).
            for px in src.as_chunks::<4>().0 {
                out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        } else {
            out.extend_from_slice(src);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::Sprite;

    /// Frame rows strip padding + swap R/B as needed: a 2x2 frame with pitch 16 (a minimal
    /// stand-in instead of 256-alignment); BGRA pixels must be restored to compact RGBA order.
    #[test]
    fn unpack_rows_strips_padding_and_swaps_rb() {
        let padded: Vec<u8> = vec![
            1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0, //
            9, 10, 11, 12, 13, 14, 15, 16, 0, 0, 0, 0, 0, 0, 0, 0, //
        ];
        assert_eq!(
            unpack_rows(&padded, 2, 2, 16, true),
            vec![3, 2, 1, 4, 7, 6, 5, 8, 11, 10, 9, 12, 15, 14, 13, 16]
        );
        // No swap: compact as-is (take the first 8 bytes of each row, dropping the padding).
        assert_eq!(
            unpack_rows(&padded, 2, 2, 16, false),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
    }

    #[test]
    fn uv_is_computed_from_sprite_rect() {
        let sprite = Sprite {
            name: "x".into(),
            x: 0,
            y: 0,
            w: 16,
            h: 8,
            origin_x: 0,
            origin_y: 0,
            frame_w: 16,
            frame_h: 8,
            source: None,
        };
        let ((u0, v0), (u1, v1)) = GpuRenderer::uv_for_sprite(&sprite, Vec2::from_f32s(64.0, 32.0));
        assert!((u0 - 0.0).abs() < 1e-6);
        assert!((v0 - 0.0).abs() < 1e-6);
        assert!((u1 - 0.25).abs() < 1e-6);
        assert!((v1 - 0.25).abs() < 1e-6);
    }

    #[test]
    fn vertex_layout_is_pod() {
        // The bytemuck derive already guarantees this; here we confirm the stride matches the fields.
        assert_eq!(
            std::mem::size_of::<SpriteVertex>(),
            std::mem::size_of::<[f32; 8]>()
        );
    }

    #[test]
    fn uniform_is_pod() {
        let u = Uniforms {
            time: 1.0,
            _pad: [0.0; 3],
        };
        let bytes = bytemuck::bytes_of(&u);
        assert_eq!(bytes.len(), std::mem::size_of::<Uniforms>());
    }
}
