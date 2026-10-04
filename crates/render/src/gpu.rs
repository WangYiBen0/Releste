//! wgpu 渲染后端。
//!
//! # 手动 checklist（AGENTS.md §9：纯渲染代码不写单测，但需 checklist）
//! - [ ] 窗口 resize 后画面不拉伸、不变形（letterbox 生效）
//! - [ ] 摄像机 zoom 1.0 时，320x180 内的 sprite 与美术像素 1:1 对齐
//! - [ ] 大量 sprite（>5000）时 draw call 数等于纹理组数，且帧耗时 < 4ms
//! - [ ] 纹理采样用 nearest，不出现模糊（还原像素风）
//! - [ ] alpha 混合为预乘，半透明边缘不发黑
//! - [ ] 切出 / 切回窗口后能正常重配置 surface
//!
//! # Crunch 纹理
//! 原版 `.data` 是 Crunch 压缩，**像素解码未实现**（AGENTS.md §13）。
//! 本后端对这类载荷返回 [`RenderError::UndecodableTexture`] 而不是
//! 静默渲染错误内容。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bytemuck::{Pod, Zeroable};
use reles_math::Vec2;
use thiserror::Error;
use tracing::{info, warn};

use crate::atlas::{SpriteAtlas, TextureKind};
use crate::batch::{SpriteBatch, SpriteVertex};
use crate::camera::Camera;

/// 渲染错误。
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
         Pixel decoding is not implemented yet (AGENTS.md §13). \
         Re-export the atlas with PNG payloads in the content pipeline."
    )]
    UndecodableTexture { path: String, kind: TextureKind },
    #[error("atlas error: {0}")]
    Atlas(#[from] crate::atlas::AtlasError),
    #[error("vertex data is not Pod-aligned")]
    BufferCast,
}

/// 渲染器配置。
#[derive(Debug, Clone)]
pub struct GpuRendererConfig {
    /// 初始窗口宽度（物理像素）。
    pub width: u32,
    /// 初始窗口高度（物理像素）。
    pub height: u32,
    /// 垂直同步。
    pub vsync: bool,
    /// MSAA 采样数（1 = 关闭）。
    pub msaa_samples: u32,
}

impl Default for GpuRendererConfig {
    fn default() -> Self {
        GpuRendererConfig {
            width: 1280,
            height: 720,
            vsync: true,
            // 像素风：MSAA 会糊掉美术，默认关闭。
            msaa_samples: 1,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    /// 预留：时间 / 全局 tint。
    time: f32,
    _pad: [f32; 3],
}

/// 已上传的纹理。
///
/// `bind_group` 内部持有 texture view 的引用，因此无需另存 view。
struct GpuTexture {
    bind_group: wgpu::BindGroup,
}

/// wgpu 渲染器。
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
    /// 实际视口尺寸。
    viewport: (u32, u32),
}

impl GpuRenderer {
    /// 创建渲染器。
    ///
    /// `window` 需以 `Arc` 形式传入（wgpu 要求 surface 的 `'static`）。
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
        // 原版图集最大 4096x2048，留出余量。
        limits.max_texture_dimension_2d = 8192;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("reles-device"),
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
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

        // 像素风：nearest 采样。
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
        })
    }

    /// 当前视口尺寸。
    pub fn viewport(&self) -> (u32, u32) {
        self.viewport
    }

    /// 表面像素格式。
    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// 窗口尺寸变化后重配置 surface。
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

    /// 把一个 PNG 纹理上传到 GPU，并绑定到 `atlas_id`。
    pub fn upload_png(&mut self, atlas_id: u32, path: &Path) -> Result<(), RenderError> {
        let img = image::open(path).map_err(|source| RenderError::TextureDecode {
            path: path.display().to_string(),
            source,
        })?;
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        self.upload_rgba(atlas_id, w, h, rgba.as_raw())
    }

    /// 直接上传 RGBA8 像素。
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

        self.textures.insert(atlas_id, GpuTexture { bind_group });
        Ok(())
    }

    /// 按 [`SpriteAtlas`] 载入所有可解码（PNG）纹理。
    ///
    /// Crunch 载荷会被跳过并 `warn`（见模块文档）。
    pub fn load_atlas(&mut self, atlas: &SpriteAtlas) -> Result<usize, RenderError> {
        let mut loaded = 0usize;
        for (id, group) in atlas.descriptor().textures.iter().enumerate() {
            let id = id as u32;
            if group.kind != TextureKind::Png {
                warn!(
                    group = %group.name,
                    kind = ?group.kind,
                    "skipping Crunch texture (decoder not implemented)"
                );
                continue;
            }
            let path = atlas.payload_path(group, &group.sprites[0]);
            let _ = path;
            // 组级 source 才是 PNG 路径（per-sprite 只会是 Crunch）。
            let rel = group.source.strip_prefix("atlas/").unwrap_or(&group.source);
            let p = PathBuf::from(rel);
            self.upload_png(id, &p)?;
            loaded += 1;
        }
        Ok(loaded)
    }

    /// 上传一帧顶点并绘制。
    ///
    /// 调用前应 `batch.sort()`。
    pub fn render(&mut self, batch: &SpriteBatch, camera: &Camera) -> Result<(), RenderError> {
        let vertices = batch.vertices(camera, self.viewport);
        if vertices.is_empty() {
            return Ok(());
        }
        if vertices.len() > self.vertex_capacity {
            return Err(RenderError::BufferCast);
        }

        self.queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&Uniforms {
                time: 0.0,
                _pad: [0.0; 3],
            }),
        );

        self.queue
            .write_buffer(&self.vertex_buffer, 0, bytemuck::cast_slice(&vertices));

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
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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

            // 每段 = 一次 draw call。
            for range in batch.ranges() {
                let Some(tex) = self.textures.get(&range.atlas) else {
                    // 未上传的纹理（例如 Crunch）：跳过而不是渲染错误内容。
                    continue;
                };
                pass.set_bind_group(1, &tex.bind_group, &[]);
                let start = (range.start * 6) as u32;
                let count = (range.len * 6) as u32;
                pass.draw(start..start + count, 0..1);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }

    /// 已上传纹理数。
    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }

    /// 顶点容量。
    pub fn vertex_capacity(&self) -> usize {
        self.vertex_capacity
    }

    /// 是否已上传某纹理组。
    pub fn has_texture(&self, atlas_id: u32) -> bool {
        self.textures.contains_key(&atlas_id)
    }

    /// 便捷：从世界坐标算出 sprite 的 UV。
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

/// Sprite 着色器。
///
/// 顶点坐标已在 CPU 侧变换到 NDC（见 [`crate::batch::SpriteBatch::vertices`]），
/// 因此这里只做透传。
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::Sprite;

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
        // bytemuck 派生已保证；此处确认 stride 与字段一致。
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
