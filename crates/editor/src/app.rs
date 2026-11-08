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

use crate::l10n::{Locale, Localizer};
use crate::model::Editor;
use crate::ui::{EditorUi, PendingIo};

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
    /// Root of the converted assets (`assets/`), used to load tile art.
    assets_root: std::path::PathBuf,
}

impl EditorApp {
    /// Creates a new application in the `en_US` source locale.
    pub fn new(editor: Editor) -> Self {
        Self::new_with_locale(
            editor,
            Locale::default(),
            std::path::PathBuf::from("locales"),
        )
    }

    /// Creates a new application with an explicit locale and catalog root.
    ///
    /// The root is threaded through rather than rediscovered so the binary
    /// resolves `locales/` exactly once (see `l10n::find_catalog_root`).
    pub fn new_with_locale(
        editor: Editor,
        locale: Locale,
        catalog_root: std::path::PathBuf,
    ) -> Self {
        EditorApp {
            editor,
            ui: EditorUi::with_locale_at(locale, catalog_root),
            window: None,
            surface: None,
            device: None,
            queue: None,
            config: None,
            egui_state: None,
            egui_renderer: None,
            assets_root: std::path::PathBuf::from("assets"),
        }
    }

    /// Sets the asset root before the first frame and loads the initial tile art.
    pub fn set_assets_root(&mut self, root: std::path::PathBuf) {
        self.assets_root = root;
        self.reload_tile_art();
    }

    /// The localizer driving the interface language.
    pub fn localizer(&self) -> &Localizer {
        self.ui.localizer()
    }

    /// Registers a Schema for a loaded entity kind so the inspector can auto-generate a panel.
    pub fn register_schema(&mut self, kind: &str, schema: &releste_world::Schema) {
        self.ui.register_schema(kind, schema);
    }

    /// Runs the event loop.
    ///
    /// # Shutdown order
    ///
    /// `window` and `surface` hold handles into the platform display
    /// connection, which the `EventLoop` owns. Releasing the window after the
    /// loop has been dropped leaves those handles dangling and segfaults during
    /// window teardown - the "close the window, get a SIGSEGV" bug.
    ///
    /// `run_app` *consumes* the event loop, so it is destroyed inside this call
    /// and there is no point after it returns where the loop is still alive. The
    /// release therefore has to happen from `ApplicationHandler::exiting`, which
    /// winit dispatches for `Event::LoopExiting` while the platform connection
    /// is still valid. See `teardown`.
    pub fn run(mut self) -> anyhow::Result<()> {
        let event_loop = EventLoop::new()?;
        event_loop.set_control_flow(ControlFlow::Wait);
        event_loop.run_app(&mut self)?;
        Ok(())
    }

    /// Releases GPU and window resources in dependency-safe order.
    ///
    /// Called from `ApplicationHandler::exiting`, before the event loop goes
    /// away. Each step is idempotent, so calling it twice is harmless.
    fn teardown(&mut self) {
        // The egui renderer records GPU work against the device, so it goes first.
        self.egui_renderer = None;
        self.egui_state = None;
        // The surface borrows the window handle and was configured on the
        // device; release it before either.
        self.surface = None;
        self.device = None;
        self.queue = None;
        self.config = None;
        // The window itself is last: the surface held a clone of it.
        self.window = None;
    }

    /// Applies a file operation requested by the Open / Save As dialog.
    ///
    /// `Open` replaces the whole document (it is a new file, so the undo stack
    /// of the old one is meaningless) and reloads tile art for the new atlas.
    /// `SaveAs` writes and adopts the new path.
    fn apply_pending_io(&mut self) {
        let Some(op) = self.ui.take_pending_io() else {
            return;
        };
        match op {
            PendingIo::Open(path) => match Editor::open(&path) {
                Ok(editor) => {
                    self.editor = editor;
                    self.ui.set_map_dir(
                        path.parent()
                            .map(std::path::Path::to_path_buf)
                            .unwrap_or_else(|| std::path::PathBuf::from("maps")),
                    );
                    self.reload_tile_art();
                    self.ui.set_status(
                        self.ui
                            .tf("status.opened", &[("path", path.display().to_string())]),
                    );
                    info!(path = %path.display(), "map opened via dialog");
                }
                Err(e) => {
                    let msg = self
                        .ui
                        .tf("status.open_failed", &[("error", e.to_string())]);
                    self.ui.set_status(msg);
                    warn!(error = %e, "failed to open map via dialog");
                }
            },
            PendingIo::SaveAs(path) => match self.editor.save_as(&path) {
                Ok(()) => {
                    self.ui.set_map_dir(
                        path.parent()
                            .map(std::path::Path::to_path_buf)
                            .unwrap_or_else(|| std::path::PathBuf::from("maps")),
                    );
                    self.ui.set_status(
                        self.ui
                            .tf("status.saved_as", &[("path", path.display().to_string())]),
                    );
                    info!(path = %path.display(), "map saved via dialog");
                }
                Err(e) => {
                    let msg = self
                        .ui
                        .tf("status.save_failed", &[("error", e.to_string())]);
                    self.ui.set_status(msg);
                    warn!(error = %e, "failed to save map via dialog");
                }
            },
        }
    }

    /// Loads tile art for the open map's atlas.
    fn reload_tile_art(&mut self) {
        let atlas = self
            .editor
            .room()
            .map(|r| r.tiles.atlas.clone())
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| "Gameplay".to_string());
        self.ui.load_tile_art(self.assets_root.clone(), &atlas);
    }

    /// Initialises GPU resources.
    fn init_gpu(&mut self, window: Arc<Window>) -> anyhow::Result<()> {
        let size = window.inner_size();
        // Primary backends only: probing GLES on Wayland spams a spurious
        // "Re-initializing Gles context" warning, and every real GPU (and
        // llvmpipe) speaks Vulkan anyway. Validation layers are debug-only
        // noise here; they log a warning when the layer is not installed and
        // relay every Vulkan loader probe as a warning.
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            flags: wgpu::InstanceFlags::empty(),
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
                label: Some("releste-editor-device"),
                required_features: wgpu::Features::empty(),
                // Request the adapter's full limits: downlevel_defaults caps
                // 2D textures at 2048, which forced every 4096x4096 atlas
                // page through a blurry downscale before display.
                required_limits: adapter.limits(),
                memory_hints: wgpu::MemoryHints::Performance,
            },
            None,
        ))?;

        // egui blends in sRGB itself and warns about srgb render targets, so
        // prefer the linear (non-srgb) format for the egui pipeline.
        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
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

        // Register a CJK fallback before the first frame, or Chinese labels
        // render as tofu boxes (see crate::fonts). The settings file selects
        // the font; the editor UI can change it at run time.
        let egui_ctx = egui::Context::default();
        let settings = crate::settings::Settings::load();
        if crate::fonts::install_cjk_fallback_with(&egui_ctx, &settings.font_config()).is_none() {
            crate::fonts::warn_missing_cjk_font();
        }

        let egui_state = egui_winit::State::new(
            egui_ctx,
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

    /// Applies a file operation queued by the dialog, then reloads tile art.
    ///
    /// Called from `window_event` after `render` returns, so no borrow of
    /// `self` is live: `render` holds `&mut self.egui_state` for its whole body.
    fn flush_pending_io(&mut self) {
        self.apply_pending_io();
    }
}

impl ApplicationHandler for EditorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let title = self.ui.t("app.window_title").to_string();
        let attrs = Window::default_attributes()
            .with_title(title)
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
                if self.editor.is_dirty() && !self.ui.exit_approved {
                    // Never drop unsaved work silently: keep the window
                    // open and ask what to do with the changes.
                    self.ui.close_prompt = true;
                    window.request_redraw();
                    info!("close requested with unsaved changes; asking the user");
                } else {
                    event_loop.exit();
                }
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
                self.flush_pending_io();
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.ui.exit_approved {
            event_loop.exit();
            return;
        }
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Called by winit on shutdown, before `run_app` returns.
    ///
    /// Tearing down here means the window and surface are released while the
    /// platform connection is still valid.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.teardown();
    }
}

/// A minimal future blocker.
///
/// wgpu's `request_adapter` / `request_device` return futures;
/// editor startup is synchronous, so block and wait here.
/// Does not pull in the `pollster` dependency (AGENTS.md 15 constraint 1).
///
/// # Why `clippy::manual_noop_waker` is allowed
/// The standard library's `Waker::noop()` requires Rust 1.85, but this
/// workspace's `rust-version` is pinned to 1.80 -- AGENTS.md 15 constraint 2
/// forbids changing it. So the no-op waker is written by hand, with the
/// reason stated here as AGENTS.md 5.1 requires.
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
