//! `reles-kernel`: 引擎内核。
//!
//! 只认识 `Service` trait、`EventBus`、`FrameId`。
//! **不能**依赖任何业务 crate（`world`、`render`、`audio`、`script`）。

#![deny(warnings)]

mod config;
mod event;
mod frame;
mod kernel;
mod service;

pub use config::KernelConfig;
pub use event::{EventBus, SubscribeError};
pub use frame::FrameId;
pub use kernel::Kernel;
pub use service::{Context, Service, ServiceId, ServiceRegistry};
