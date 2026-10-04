//! `reles-plugin-host`: the plugin loader and lifecycle.
//!
//! The engine core ships no gameplay. Everything below the host boundary is a
//! plugin dropped into `plugins/`, which makes hot-plug a runtime concern
//! rather than a recompile.
//!
//! # What lives here
//!
//! - [`manifest`]: `plugin.toml`, read **without** instantiating anything.
//! - [`fiber`]: the [`fiber::Fiber`] state machine, adopted verbatim from
//!   Cordis (`PENDING → LOADING → ACTIVE → UNLOADING → DISPOSED`, plus `FAILED`).
//! - [`scope`]: [`scope::Scope::effect`] — LIFO disposers with mandatory labels,
//!   so a teardown tree can be printed before anything is torn down.
//! - [`registry`]: capability registration and the `inject` epoch.
//! - [`abi`]: the frozen `#[repr(C)]` vtable shared with `.so` plugins.
//! - [`host`]: scanning, loading, and frame-boundary unload.
//!
//! # Safety contract
//!
//! A `.so` plugin runs in-process, so the boundary is data-only: no Rust types
//! cross it, only integers, `CStr`s and JSON. `#[repr(C)]` structs and
//! `extern "C"` functions are the whole ABI (see [`abi`]).

#![deny(warnings)]

pub mod abi;
pub mod fiber;
pub mod host;
pub mod manifest;
pub mod registry;
pub mod scope;

pub use abi::{HostApi, LogLevel, PluginV1, ABI_VERSION, ENTRY_SYMBOL};
pub use fiber::{Fiber, FiberState};
pub use host::{HostError, PluginHost, ScanEntry};
pub use manifest::{LoadMode, Manifest, ManifestError};
pub use registry::{EntityKind, Registry, ServiceEntry};
pub use scope::{EffectMeta, EffectToken, Scope};
