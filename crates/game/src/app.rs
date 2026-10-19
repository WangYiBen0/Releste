//! 窗口宿主：winit 事件循环 + `FIXED_DT` 累加器 + 渲染。
//!
//! 宿主只做「喂输入、按固定步长 tick、把 service 的快照画出来」，
//! 所有游戏状态都在 [`GameService`] 里（ROADMAP Phase 2.2）。

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

/// 最多连续补 8 个 tick，之后丢弃欠账（避免 spiral of death）。
const MAX_CATCHUP: u32 = 8;
/// 允许采样到的最大墙钟间隔。
const MAX_WALL_GAP: Duration = Duration::from_millis(250);
/// 标题栏刷新间隔。
const TITLE_INTERVAL: Duration = Duration::from_secs(1);

/// 窗口化运行的应用。
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
    /// 用户按键绑定（`<root>/settings.toml`，缺省 = 原版默认）。
    bindings: crate::settings::KeyBindings,
    accumulator: Duration,
    last: Instant,
    alpha: f32,
    fps_frames: u32,
    fps_last: Instant,
    fps: u32,
    ticks: u64,
    /// `--screenshot`：第 `screenshot_after` 个渲染帧存成 PNG 后退出。
    screenshot: Option<PathBuf>,
    screenshot_after: u64,
    screenshot_saved: bool,
    rendered: u64,
    /// 截图等异步阶段攒下的致命错误；事件循环结束后由 `run` 上抛。
    fatal: Option<anyhow::Error>,
}

impl GameApp {
    /// 建应用。此时只准备好数据，GPU 与窗口要等 `resumed`。
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

    /// 进入事件循环；返回时已完成服务收尾。
    pub fn run(mut self) -> anyhow::Result<()> {
        let event_loop = new_event_loop()?;
        event_loop.set_control_flow(ControlFlow::Poll);
        event_loop
            .run_app(&mut self)
            .map_err(|e| anyhow::anyhow!("event loop: {e}"))?;

        if let Some(mut kernel) = self.kernel.take() {
            kernel.shutdown();
        }
        // 销毁顺序：wgpu 的 Surface 持有 Window 的引用，必须**先于**
        // Window 释放。结构体字段序是 window 在前 gpu 在后，默认 drop
        // 会先放掉 Window —— 部分后端上退出即段错误（关窗 SegFault）。
        self.gpu = None;
        self.window = None;
        if let Some(e) = self.fatal.take() {
            return Err(e);
        }
        info!(ticks = self.ticks, "game exited");
        Ok(())
    }

    /// 建窗口 + GPU + kernel。只在 `resumed` 里跑一次。
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

    /// 创建 wgpu 设备。不引入 `pollster`（AGENTS.md §15.1）。
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
        // OS 键盘重复会以 `Pressed` 反复投递同一个键。移动/跳跃键的赋值是
        // 幂等的，但 Esc 的 toggle 不是 —— 按住 Esc 会把暂停快速来回切。
        // 所以重复事件一律忽略（松开事件没有 repeat，不受影响）。
        if event.repeat {
            return;
        }
        let PhysicalKey::Code(code) = event.physical_key else {
            return;
        };
        let down = event.state == ElementState::Pressed;
        // 键位 = `settings.toml`（缺省即原版默认：方向键移动、C 跳 ——
        // `SetDefaultKeyboardControls`，pureCeleste.cs:101602-101651；
        // `TranslateKeys` 101708 的物理键语义 = winit `PhysicalKey`）。
        // Enter = 原版 Pause（101642）；ESC = 原版菜单键（`Input.ESC`，
        // 110396），引擎沿用为暂停开关。冲刺/抓攀等 Phase 4.1 后加入。
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

    /// 固定步长推进。
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
        // 输入在 tick 之前写进 service，保证本帧的 tick 用到的是最新按键。
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

    /// 画一帧；`--screenshot` 到点时抓帧落盘并退出。
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

    /// 渲染当前快照并写 PNG（真实管线输出，含截屏那次独立渲染）。
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

/// 把紧凑 RGBA8 像素写成 PNG；父目录不存在时顺手创建。
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
                // 松开按键，避免切窗后角色一直跑
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

/// 事件循环：Wayland 库加载不了就强制 X11。
///
/// winit 0.30 只在非空 `WAYLAND_DISPLAY` / `WAYLAND_SOCKET` 下试 Wayland，
/// libwayland 加载不了就直接报错，**没有 X11 回落**（`platform_impl/linux/
/// mod.rs` 的 backend 选择）。
///
/// 为什么不在 wayland 失败后再建一个 x11 loop：`EventLoopBuilder::build()`
/// 在创建之前就把 `EVENT_LOOP_CREATED` 置位，同一进程的第二次 `build()`
/// 直接返回 `RecreationAttempt`。所以只能在建 loop **之前**选好后端 ——
/// 先用 `libloading` 探一下 `libwayland-client` 能不能 dlopen。
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
        // 别让 winit 的报错只提 wayland —— 把两边的库都点名，
        // 用户才知道该装什么（ROADMAP Appendix B：错误要点修复方法）。
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

/// 一个 SONAME 能不能被 dlopen。
///
/// dlopen 是引用计数的，`Library` 在表达式结束时就 dlclose；失败不打日志
/// —— 调用方会把**决定**作为一条 `warn` 报出来。
#[cfg(target_os = "linux")]
fn library_loadable(soname: &str) -> bool {
    unsafe { libloading::Library::new(soname) }.is_ok()
}

/// 非 Linux 平台没有 Wayland/X11 之分，winit 自己挑。
#[cfg(not(target_os = "linux"))]
fn new_event_loop() -> anyhow::Result<EventLoop<()>> {
    EventLoop::new().map_err(|e| anyhow::anyhow!("event loop: {e}"))
}

/// 手写的 future 阻塞器。
///
/// wgpu 的 `request_adapter` / `request_device` 返回 future，而窗口启动是
/// 同步的。这里阻塞等待，不引入 `pollster` 依赖（AGENTS.md §15.1）。
///
/// # Why `clippy::manual_noop_waker` is allowed
/// 标准库的 `Waker::noop()` 需要 Rust 1.85，而本工作区 `rust-version` 锁在
/// 1.80 —— AGENTS.md §15.2 禁止改。所以手写 no-op waker，理由写在这里
/// 满足 AGENTS.md §5.1。
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
