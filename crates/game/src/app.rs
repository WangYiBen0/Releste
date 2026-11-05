//! Window host: winit event loop + `FIXED_DT` accumulator + rendering.
//!
//! The host only does "feed input, tick at a fixed step, draw the service's snapshot",
//! and all game state lives in [`GameService`] (ROADMAP Phase 2.2).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use releste_kernel::Kernel;
use releste_math::FIXED_DT;
use releste_render::{GpuRenderer, GpuRendererConfig, SpriteBatch};
use tracing::{error, info, warn};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
#[cfg(target_os = "linux")]
use winit::platform::x11::EventLoopBuilderExtX11 as _;
use winit::window::{Window, WindowId};

use crate::paths::Paths;
use crate::player::Input;
use crate::service::{boot_plugins, watch_maps, GameService};
use crate::session::Session;

/// At most 8 catch-up ticks in a row, after which the backlog is dropped (avoiding spiral of death).
const MAX_CATCHUP: u32 = 8;
/// The maximum wall-clock gap allowed to be sampled.
const MAX_WALL_GAP: Duration = Duration::from_millis(250);
/// Title bar refresh interval.
const TITLE_INTERVAL: Duration = Duration::from_secs(1);

/// The windowed application.
pub struct GameApp {
    paths: Paths,
    session: Option<Session>,
    watcher: Option<releste_reload::FileWatcher>,
    load_plugins: bool,
    window: Option<Arc<Window>>,
    gpu: Option<GpuRenderer>,
    kernel: Option<Kernel>,
    batch: SpriteBatch,
    keys: Input,
    /// User key bindings (`<root>/settings.toml`, absent = vanilla defaults).
    bindings: crate::settings::KeyBindings,
    accumulator: Duration,
    last: Instant,
    alpha: f32,
    fps_frames: u32,
    fps_last: Instant,
    fps: u32,
    ticks: u64,
    /// `--screenshot`: save the `screenshot_after`-th rendered frame as a PNG and exit.
    screenshot: Option<PathBuf>,
    screenshot_after: u64,
    screenshot_saved: bool,
    rendered: u64,
    /// A fatal error accumulated during asynchronous phases such as screenshots; propagated by `run` after the event loop ends.
    fatal: Option<anyhow::Error>,
}

impl GameApp {
    /// Build the application. Only data is prepared here; the GPU and window wait for `resumed`.
    pub fn new(
        paths: Paths,
        session: Session,
        load_plugins: bool,
        screenshot: Option<PathBuf>,
        screenshot_after: u64,
    ) -> Self {
        let bindings = crate::settings::KeyBindings::load(paths.root());
        let watcher = watch_maps(&paths);
        let now = Instant::now();
        GameApp {
            paths,
            session: Some(session),
            watcher,
            load_plugins,
            window: None,
            gpu: None,
            kernel: None,
            batch: SpriteBatch::default(),
            keys: Input::default(),
            bindings,
            accumulator: Duration::ZERO,
            last: now,
            alpha: 0.0,
            fps_frames: 0,
            fps_last: now,
            fps: 0,
            ticks: 0,
            screenshot,
            screenshot_after,
            screenshot_saved: false,
            rendered: 0,
            fatal: None,
        }
    }

    /// Enter the event loop; on return the service has already been shut down.
    pub fn run(mut self) -> anyhow::Result<()> {
        let event_loop = new_event_loop()?;
        event_loop.set_control_flow(ControlFlow::Poll);
        event_loop
            .run_app(&mut self)
            .map_err(|e| anyhow::anyhow!("event loop: {e}"))?;

        if let Some(mut kernel) = self.kernel.take() {
            kernel.shutdown();
        }
        // Destruction order: wgpu's Surface holds a reference to the Window and must be released **before**
        // the Window. The struct field order is window before gpu, so the default drop
        // would release the Window first -- on some backends exiting then segfaults (close-window SegFault).
        self.gpu = None;
        self.window = None;
        if let Some(e) = self.fatal.take() {
            return Err(e);
        }
        info!(ticks = self.ticks, "game exited");
        Ok(())
    }

    /// Create the window + GPU + kernel. Runs only once, inside `resumed`.
    fn boot(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = Window::default_attributes()
            .with_title("releste")
            .with_inner_size(LogicalSize::new(960.0, 540.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                error!(error = %e, "failed to create the window");
                event_loop.exit();
                return;
            }
        };

        let mut gpu = match Self::init_gpu(Arc::clone(&window)) {
            Ok(gpu) => gpu,
            Err(e) => {
                error!(error = %format!("{e:#}"), "failed to initialise the GPU");
                event_loop.exit();
                return;
            }
        };

        let Some(session) = self.session.take() else {
            error!("map was never loaded");
            event_loop.exit();
            return;
        };

        match gpu.load_atlas(&session.atlas) {
            Ok(groups) => info!(groups, textures = gpu.texture_count(), "atlas uploaded"),
            Err(e) => warn!(error = %e, "atlas upload failed; some sprites will not draw"),
        }

        let host = if self.load_plugins {
            boot_plugins(&self.paths)
        } else {
            info!("--no-plugins: skipping plugin scan");
            None
        };

        let service = GameService::new(session, host, self.watcher.take(), Some(&gpu));
        self.kernel = Some(Kernel::new().with(service));
        self.gpu = Some(gpu);
        self.window = Some(window);

        let now = Instant::now();
        self.last = now;
        self.fps_last = now;
        info!("window ready");
    }

    /// Create the wgpu device. `pollster` is not pulled in (AGENTS.md 15.1).
    fn init_gpu(window: Arc<Window>) -> anyhow::Result<GpuRenderer> {
        let size = window.inner_size();
        let config = GpuRendererConfig {
            width: size.width.max(1),
            height: size.height.max(1),
            vsync: true,
            msaa_samples: 1,
        };
        pollster_block_on(GpuRenderer::new(window, config)).map_err(|e| anyhow::anyhow!("gpu: {e}"))
    }

    fn on_key(&mut self, event: &KeyEvent) {
        // OS key repeat delivers the same key repeatedly as `Pressed`. Assigning movement/jump keys is
        // idempotent, but the Esc toggle is not -- holding Esc flips pause back and forth rapidly.
        // So repeated events are ignored (release events have no repeat and are unaffected).
        if event.repeat {
            return;
        }
        let PhysicalKey::Code(code) = event.physical_key else {
            return;
        };
        let down = event.state == ElementState::Pressed;
        // Key bindings = `settings.toml` (absent means vanilla defaults: arrow keys move, C jumps --
        // `SetDefaultKeyboardControls`, pureCeleste.cs:101602-101651;
        // `TranslateKeys` 101708's physical-key semantics = winit `PhysicalKey`).
        // Enter = vanilla Pause (101642); ESC = the vanilla menu key (`Input.ESC`,
        // 110396), which the engine reuses as the pause toggle. Dash/grab etc. are added after Phase 4.1.
        if code == KeyCode::Escape || code == KeyCode::Enter {
            if down {
                self.toggle_pause();
            }
            return;
        }
        if self.bindings.left.contains(&code) {
            self.keys.left = down;
        }
        if self.bindings.right.contains(&code) {
            self.keys.right = down;
        }
        if self.bindings.up.contains(&code) {
            self.keys.up = down;
        }
        if self.bindings.down.contains(&code) {
            self.keys.down = down;
        }
        if self.bindings.jump.contains(&code) {
            self.keys.jump = down;
        }
        if self.bindings.dash.contains(&code) {
            self.keys.dash = down;
        }
        if self.bindings.grab.contains(&code) {
            self.keys.grab = down;
        }
    }

    fn toggle_pause(&mut self) {
        let Some(kernel) = self.kernel.as_mut() else {
            return;
        };
        let Some(service) = kernel.resolve_mut::<GameService>() else {
            return;
        };
        service.paused = !service.paused;
        if service.paused {
            info!("paused");
        } else {
            info!("resumed");
        }
    }

    /// Fixed-step advance.
    fn tick(&mut self) {
        let now = Instant::now();
        let mut wall = now.duration_since(self.last);
        self.last = now;
        if wall > MAX_WALL_GAP {
            warn!(
                gap_ms = wall.as_millis() as u64,
                "wall clock gap is large; clamping the simulation step"
            );
            wall = MAX_WALL_GAP;
        }
        // Input is written into the service before the tick, ensuring this frame's tick uses the latest keys.
        if let Some(kernel) = self.kernel.as_mut() {
            if let Some(service) = kernel.resolve_mut::<GameService>() {
                service.input = self.keys;
            }
        }

        self.accumulator += wall;

        let step = Duration::from_secs_f64(FIXED_DT.to_f64());
        let mut steps = 0u32;
        while self.accumulator >= step && steps < MAX_CATCHUP {
            self.accumulator -= step;
            steps += 1;
            self.ticks += 1;
            if let Some(kernel) = self.kernel.as_mut() {
                kernel.tick();
            }
        }
        if steps == MAX_CATCHUP && self.accumulator >= step {
            warn!("simulation fell behind; dropping the backlog");
            self.accumulator = Duration::ZERO;
        }
        self.alpha = (self.accumulator.as_secs_f64() / step.as_secs_f64()).clamp(0.0, 1.0) as f32;

        self.fps_frames += 1;
        if now.duration_since(self.fps_last) >= TITLE_INTERVAL {
            self.fps = self.fps_frames;
            self.fps_frames = 0;
            self.fps_last = now;
            self.update_title();
        }
    }

    fn update_title(&self) {
        let (Some(window), Some(kernel)) = (self.window.as_ref(), self.kernel.as_ref()) else {
            return;
        };
        let Some(service) = kernel.resolve::<GameService>() else {
            return;
        };
        let title = format!(
            "releste | {} | {} FPS | {} sprites | {} plugins",
            service.room_name(),
            self.fps,
            service.static_draw_count(),
            service.plugin_count(),
        );
        window.set_title(&title);
    }

    /// Draw one frame; with `--screenshot`, capture the frame to disk and exit when it is due.
    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let (Some(kernel), Some(gpu)) = (self.kernel.as_mut(), self.gpu.as_mut()) else {
            return;
        };
        let Some(service) = kernel.resolve_mut::<GameService>() else {
            return;
        };
        service.render(gpu, self.alpha, &mut self.batch);
        let camera = service.render_camera(self.alpha);
        if let Err(e) = gpu.render(&self.batch, &camera) {
            error!(error = %e, "render failed");
        }
        self.rendered += 1;

        if self.screenshot.is_some()
            && !self.screenshot_saved
            && self.rendered >= self.screenshot_after
        {
            if let Err(e) = self.save_screenshot() {
                self.fatal = Some(e);
            }
            event_loop.exit();
        }
    }

    /// Render the current snapshot and write a PNG (real pipeline output, including the separate render for the capture).
    fn save_screenshot(&mut self) -> anyhow::Result<()> {
        let Some(path) = self.screenshot.clone() else {
            return Ok(());
        };
        let (Some(kernel), Some(gpu)) = (self.kernel.as_mut(), self.gpu.as_mut()) else {
            anyhow::bail!("renderer is not ready");
        };
        let Some(service) = kernel.resolve_mut::<GameService>() else {
            anyhow::bail!("game service is gone");
        };
        service.render(gpu, self.alpha, &mut self.batch);
        let camera = service.render_camera(self.alpha);
        let (width, height, pixels) = gpu
            .capture_pixels(&self.batch, &camera)
            .map_err(|e| anyhow::anyhow!("frame capture: {e}"))?;

        save_png(&path, &pixels, width, height)?;
        info!(
            path = %path.display(),
            width,
            height,
            frame = self.rendered,
            "screenshot saved"
        );
        self.screenshot_saved = true;
        Ok(())
    }
}

/// Write compact RGBA8 pixels as a PNG; create the parent directory if it is missing.
fn save_png(path: &Path, rgba: &[u8], width: u32, height: u32) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| anyhow::anyhow!("pixel buffer does not match {width}x{height}x4"))?;
    img.save(path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

impl ApplicationHandler for GameApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.boot(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                info!("close requested");
                event_loop.exit();
            }
            WindowEvent::Focused(false) => {
                // Release the keys so the character does not keep running after an alt-tab
                self.keys = Input::default();
            }
            WindowEvent::Resized(size) => {
                if let Some(gpu) = self.gpu.as_mut() {
                    gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.on_key(&event),
            WindowEvent::RedrawRequested => self.draw(event_loop),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if self.kernel.is_some() {
            self.tick();
        }
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }
}

/// Event loop: force X11 when the Wayland library cannot be loaded.
///
/// winit 0.30 only tries Wayland with a non-empty `WAYLAND_DISPLAY` / `WAYLAND_SOCKET`;
/// if libwayland cannot be loaded it errors out directly, **with no X11 fallback** (the backend selection in
/// `platform_impl/linux/mod.rs`).
///
/// Why not build an x11 loop after wayland fails: `EventLoopBuilder::build()`
/// sets `EVENT_LOOP_CREATED` before creation, so a second `build()` in the same process
/// returns `RecreationAttempt` directly. So the backend must be chosen **before** building the loop --
/// first probe with `libloading` whether `libwayland-client` can be dlopened.
#[cfg(target_os = "linux")]
fn new_event_loop() -> anyhow::Result<EventLoop<()>> {
    let wayland_wanted = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
        .iter()
        .any(|var| std::env::var_os(var).is_some_and(|v| !v.is_empty()));

    let wayland_ok =
        library_loadable("libwayland-client.so.0") || library_loadable("libwayland-client.so");
    let x11_ok = ["libX11.so.6", "libxkbcommon.so.0", "libxkbcommon-x11.so.0"]
        .iter()
        .all(|soname| library_loadable(soname));

    if wayland_wanted && !wayland_ok && x11_ok {
        warn!("libwayland-client could not be loaded; forcing the X11 backend");
        return EventLoop::<()>::builder()
            .with_x11()
            .build()
            .map_err(|e| anyhow::anyhow!("event loop (x11): {e}"));
    }
    if wayland_wanted && !wayland_ok && !x11_ok {
        // Do not let winit's error mention only wayland -- name the libraries on both sides,
        // so the user knows what to install (ROADMAP Appendix B: errors must point to the fix).
        return Err(anyhow::anyhow!(
            "no display backend can be loaded:\n  \
             wayland: libwayland-client could not be dlopened\n  \
             x11: one of libX11.so.6 / libxkbcommon.so.0 / libxkbcommon-x11.so.0 \
             could not be dlopened\n  \
             fix: install the missing libraries, or point LD_LIBRARY_PATH at them \
             before running (e.g. the nix GUI env)"
        ));
    }

    EventLoop::new().map_err(|e| anyhow::anyhow!("event loop: {e}"))
}

/// Whether a SONAME can be dlopened.
///
/// dlopen is reference-counted and `Library` dlcloses at the end of the expression; failures are not logged
/// -- the caller reports the **decision** as a `warn`.
#[cfg(target_os = "linux")]
fn library_loadable(soname: &str) -> bool {
    unsafe { libloading::Library::new(soname) }.is_ok()
}

/// Non-Linux platforms have no Wayland/X11 distinction; winit chooses by itself.
#[cfg(not(target_os = "linux"))]
fn new_event_loop() -> anyhow::Result<EventLoop<()>> {
    EventLoop::new().map_err(|e| anyhow::anyhow!("event loop: {e}"))
}

/// A hand-written future blocker.
///
/// wgpu's `request_adapter` / `request_device` return futures while window startup is
/// synchronous. This blocks and waits without pulling in the `pollster` dependency (AGENTS.md 15.1).
///
/// # Why `clippy::manual_noop_waker` is allowed
/// The standard library's `Waker::noop()` requires Rust 1.85, while this workspace's `rust-version` is locked to
/// 1.80 -- AGENTS.md 15.2 forbids changing it. So a no-op waker is hand-written, with the reason stated here
/// to satisfy AGENTS.md 5.1.
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
