//! winit + egui-wgpu host.
//!
//! See the crate documentation for the manual checklist.

use std::sync::Arc;

use egui_wgpu::ScreenDescriptor;
use tracing::{error, info, warn};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::model::Editor;
use crate::ui::EditorUi;

/// The editor application.
pub struct EditorApp {
    editor: Editor,
    ui: EditorUi,
    /// Created lazily: winit 0.30 requires the window to be built in `resumed`.
    window: Option<Arc<Window>>,
    surface: Option<wgpu::Surface<'static>>,
    device: Option<wgpu::Device>,
    queue: Option<wgpu::Queue>,
    config: Option<wgpu::SurfaceConfiguration>,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
}

impl EditorApp {
    /// Creates a new application.
    pub fn new(editor: Editor) -> Self {
        EditorApp {
            editor,
            ui: EditorUi::new(),
            window: None,
            surface: None,
            device: None,
            queue: None,
            config: None,
            egui_state: None,
            egui_renderer: None,
        }
    }

    /// Registers a Schema for a loaded entity kind so the inspector can auto-generate a panel.
    pub fn register_schema(&mut self, kind: &str, schema: &reles_world::Schema) {
        self.ui.register_schema(kind, schema);
    }

    /// Runs the event loop.
    pub fn run(mut self) -> anyhow::Result<()> {
        let event_loop = EventLoop::new()?;
        event_loop.set_control_flow(ControlFlow::Wait);
        event_loop.run_app(&mut self)?;
        Ok(())
    }

    /// Initialises GPU resources.
    fn init_gpu(&mut self, window: Arc<Window>) -> anyhow::Result<()> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance
            .create_surface(Arc::clone(&window))
            .map_err(|e| anyhow::anyhow!("create_surface: {e}"))?;

        let adapter = pollster_block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .ok_or_else(|| anyhow::anyhow!("no GPU adapter available"))?;

        let (device, queue) = pollster_block_on(adapter.request_device(
            &wgpu::DeviceDescriptor {
                label: Some("reles-editor-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))?;

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let egui_state = egui_winit::State::new(
            egui::Context::default(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(device.limits().max_texture_dimension_2d as usize),
        );

        let egui_renderer = egui_wgpu::Renderer::new(&device, format, None, 1, false);

        info!(
            adapter = adapter.get_info().name,
            ?format,
            "editor GPU initialised"
        );

        self.window = Some(window);
        self.surface = Some(surface);
        self.device = Some(device);
        self.queue = Some(queue);
        self.config = Some(config);
        self.egui_state = Some(egui_state);
        self.egui_renderer = Some(egui_renderer);
        Ok(())
    }

    /// Renders one frame.
    fn render(&mut self) -> anyhow::Result<()> {
        let (Some(window), Some(surface), Some(device), Some(queue), Some(config)) = (
            self.window.as_ref(),
            self.surface.as_ref(),
            self.device.as_ref(),
            self.queue.as_ref(),
            self.config.as_ref(),
        ) else {
            return Ok(());
        };

        let Some(egui_state) = self.egui_state.as_mut() else {
            return Ok(());
        };
        let Some(egui_renderer) = self.egui_renderer.as_mut() else {
            return Ok(());
        };

        let raw_input = egui_state.take_egui_input(window);
        let ctx = egui_state.egui_ctx().clone();

        // Assemble the UI
        let full_output = ctx.run(raw_input, |ctx| {
            self.ui.show(ctx, &mut self.editor);
        });

        egui_state.handle_platform_output(window, full_output.platform_output.clone());

        let pixels_per_point = ctx.pixels_per_point();
        let paint_jobs = ctx.tessellate(full_output.shapes, pixels_per_point);

        let frame = match surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                surface.configure(device, config);
                surface
                    .get_current_texture()
                    .map_err(|e| anyhow::anyhow!("surface: {e}"))?
            }
            Err(e) => return Err(anyhow::anyhow!("surface: {e}")),
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let screen_descriptor = ScreenDescriptor {
            size_in_pixels: [config.width, config.height],
            pixels_per_point,
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("editor-encoder"),
        });

        for (id, delta) in &full_output.textures_delta.set {
            egui_renderer.update_texture(device, queue, *id, delta);
        }

        egui_renderer.update_buffers(device, queue, &mut encoder, &paint_jobs, &screen_descriptor);

        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("editor-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.06,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            let mut pass = pass.forget_lifetime();
            egui_renderer.render(&mut pass, &paint_jobs, &screen_descriptor);
        }

        for id in &full_output.textures_delta.free {
            egui_renderer.free_texture(id);
        }

        queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}

impl ApplicationHandler for EditorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Releste Editor")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0));
        match event_loop.create_window(attrs) {
            Ok(window) => {
                let window = Arc::new(window);
                if let Err(e) = self.init_gpu(Arc::clone(&window)) {
                    error!(error = %e, "failed to initialise GPU; exiting");
                    event_loop.exit();
                }
            }
            Err(e) => {
                error!(error = %e, "failed to create window");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(egui_state) = self.egui_state.as_mut() else {
            return;
        };
        let Some(window) = self.window.as_ref() else {
            return;
        };

        let response = egui_state.on_window_event(window, &event);
        if response.repaint {
            window.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => {
                if self.editor.is_dirty() {
                    warn!("closing with unsaved changes");
                }
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let (Some(surface), Some(device), Some(config)) = (
                    self.surface.as_ref(),
                    self.device.as_ref(),
                    self.config.as_mut(),
                ) {
                    config.width = size.width.max(1);
                    config.height = size.height.max(1);
                    surface.configure(device, config);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.render() {
                    error!(error = %e, "render failed");
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

/// A minimal future blocker.
///
/// wgpu's `request_adapter` / `request_device` return futures;
/// editor startup is synchronous, so block and wait here.
/// Does not pull in the `pollster` dependency (AGENTS.md §15 constraint 1).
///
/// # Why `clippy::manual_noop_waker` is allowed
/// The standard library's `Waker::noop()` requires Rust 1.85, but this
/// workspace's `rust-version` is pinned to 1.80 — AGENTS.md §15 constraint 2
/// forbids changing it. So the no-op waker is written by hand, with the
/// reason stated here as AGENTS.md §5.1 requires.
#[allow(clippy::manual_noop_waker)]
fn pollster_block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::pin::pin;
    use std::task::{Context, Poll, Wake, Waker};

    struct NoopWaker;
    impl Wake for NoopWaker {
        fn wake(self: Arc<Self>) {}
    }

    let waker = Waker::from(Arc::new(NoopWaker));
    let mut cx = Context::from_waker(&waker);
    let mut fut = pin!(fut);

    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
