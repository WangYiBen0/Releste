//! Kernel: the main loop and Service orchestration.

use std::time::Instant;

use tracing::trace;

use crate::config::KernelConfig;
use crate::event::EventBus;
use crate::frame::FrameId;
use crate::service::{Service, ServiceRegistry};

/// Engine kernel.
///
/// Manages the lifecycle of all services and drives the main loop.
pub struct Kernel {
    config: KernelConfig,
    services: ServiceRegistry,
    bus: EventBus,
    frame: FrameId,
    running: bool,
}

impl Kernel {
    /// Create a new kernel.
    pub fn new() -> Self {
        Kernel {
            config: KernelConfig::default(),
            services: ServiceRegistry::new(),
            bus: EventBus::new(),
            frame: FrameId::ZERO,
            running: false,
        }
    }

    /// Add a service to the kernel (builder style).
    ///
    /// # Panics
    /// Panics if a service with the same name is already registered (a
    /// configuration error, i.e. a development-time bug).
    pub fn with<S: Service>(mut self, svc: S) -> Self {
        if let Err(existing) = self.services.register(svc) {
            panic!("service `{existing}` registered twice");
        }
        self
    }

    /// Shut down the kernel: release all services.
    pub fn shutdown(&mut self) {
        self.services.dispose_each();
        self.running = false;
    }

    /// Run `attach` on all services.
    fn boot(&mut self) {
        self.services.attach_each(&mut self.bus);
    }

    /// One frame.
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

    /// Run the main loop; never returns.
    ///
    /// Calls `tick` with a fixed time step and catches up when necessary.
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

    /// Set the kernel configuration.
    pub fn set_config(&mut self, config: KernelConfig) {
        self.config = config;
    }

    /// Get a service reference (for resolving dependencies during setup).
    pub fn resolve<S: Service>(&self) -> Option<&S> {
        self.services.get::<S>()
    }

    /// Get a **mutable** service reference.
    ///
    /// `tick` only advances the simulation at a fixed step; a window host also
    /// needs to read and write service state between ticks -- take a render
    /// snapshot out to feed wgpu, or inject keyboard events into the input
    /// service. This is a normal host-side outlet and does not change the
    /// contract that "`dt` is always [`FIXED_DT`](releste_math::FIXED_DT)".
    pub fn resolve_mut<S: Service>(&mut self) -> Option<&mut S> {
        self.services.get_mut::<S>()
    }
}

impl Default for Kernel {
    fn default() -> Self {
        Self::new()
    }
}
