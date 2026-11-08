//! `releste-kernel`: the engine kernel.
//!
//! It only knows the `Service` trait, `EventBus`, and `FrameId`.
//! It **must not** depend on any business crate (`world`, `render`, `audio`,
//! `script`).

#![deny(warnings)]

mod config;
mod event;
mod frame;
mod kernel;
pub mod log;
mod service;

pub use config::KernelConfig;
pub use event::{EventBus, SubscribeError};
pub use frame::FrameId;
pub use kernel::Kernel;
pub use log::AppendixA;
pub use service::{Context, Service, ServiceId, ServiceRegistry};
