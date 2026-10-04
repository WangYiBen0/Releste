//! 内核：主循环与 Service 编排。

use std::time::Instant;

use tracing::trace;

use crate::config::KernelConfig;
use crate::event::EventBus;
use crate::frame::FrameId;
use crate::service::{Service, ServiceRegistry};

/// 引擎内核。
///
/// 管理所有 service 的生命周期，驱动主循环。
pub struct Kernel {
    config: KernelConfig,
    services: ServiceRegistry,
    bus: EventBus,
    frame: FrameId,
    running: bool,
}

impl Kernel {
    /// 创建新内核。
    pub fn new() -> Self {
        Kernel {
            config: KernelConfig::default(),
            services: ServiceRegistry::new(),
            bus: EventBus::new(),
            frame: FrameId::ZERO,
            running: false,
        }
    }

    /// 添加 service 到内核（builder 模式）。
    ///
    /// # Panics
    /// 若同名 service 已注册则 panic（配置错误，属于开发期 bug）。
    pub fn with<S: Service>(mut self, svc: S) -> Self {
        if let Err(existing) = self.services.register(svc) {
            panic!("service `{existing}` registered twice");
        }
        self
    }

    /// 关闭内核：释放所有 service。
    pub fn shutdown(&mut self) {
        self.services.dispose_each();
        self.running = false;
    }

    /// 运行所有 service 的 `attach`。
    fn boot(&mut self) {
        self.services.attach_each(&mut self.bus);
    }

    /// 一帧。
    pub fn tick(&mut self) {
        if !self.running {
            self.boot();
            self.running = true;
        }

        let dt = self.config.fixed_dt;
        let frame = self.frame;
        self.frame = frame.next();

        trace!(%frame, ?dt, "tick");

        self.services.update_each(&mut self.bus, frame, dt);
    }

    /// 运行主循环，永不返回。
    ///
    /// 用固定时间步长调用 `tick`，必要时追赶。
    pub fn run(&mut self) -> ! {
        self.boot();
        self.running = true;

        let mut last_time = Instant::now();
        let dt_secs = self.config.fixed_dt.to_f64();

        loop {
            let now = Instant::now();
            let elapsed = now.duration_since(last_time);
            let elapsed_secs = elapsed.as_secs_f64();

            if elapsed_secs >= dt_secs {
                let frames_to_run =
                    ((elapsed_secs / dt_secs) as u32).min(self.config.max_catchup_frames);

                for _ in 0..frames_to_run.max(1) {
                    self.tick();
                }

                last_time = now;
            } else {
                std::hint::spin_loop();
            }
        }
    }

    /// 设置内核配置。
    pub fn set_config(&mut self, config: KernelConfig) {
        self.config = config;
    }

    /// 获取 service 引用（用于在 setup 阶段解析依赖）。
    pub fn resolve<S: Service>(&self) -> Option<&S> {
        self.services.get::<S>()
    }
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}
