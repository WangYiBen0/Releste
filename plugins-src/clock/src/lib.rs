//! `clock` - a sample `.so` plugin.
//!
//! Once per second of game time (60 fixed ticks) it calls the host's `log`
//! callback, which demonstrates the Appendix C plugin contract: a `.so` exports
//! exactly one symbol, `releste_plugin_v1`, which returns a static `PluginV1`;
//! everything the host needs comes from that table.
//!
//! This crate has **no dependency on the main workspace** (AGENTS.md 15.1) and
//! none on third-party crates - `HostApi` / `PluginV1` are hand-written
//! `#[repr(C)]` mirrors. Host and plugin may be built by different compilers;
//! Rust types never cross the boundary.
//!
//! Build (see `README.md`):
//!
//! ```text
//! cargo build --release --manifest-path plugins-src/clock/Cargo.toml
//! cp plugins-src/clock/target/release/libclock.so plugins/clock/clock.so
//! ```

#![allow(dead_code)] // mirrors the host's full vtable; the plugin uses a few fields

use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

/// Log levels mirroring the numeric values of `releste_plugin_host::abi::LogLevel`.
mod level {
    pub const TRACE: u32 = 0;
    pub const DEBUG: u32 = 1;
    pub const INFO: u32 = 2;
    pub const WARN: u32 = 3;
    pub const ERROR: u32 = 4;
}

/// The ABI version this plugin speaks; a host that does not know it refuses to
/// load and never calls any callback.
const ABI: u32 = 1;

/// Report once per second: 60 fixed ticks.
const TICKS_PER_SECOND: u64 = 60;

/// The function table the host hands to the plugin (`#[repr(C)]` mirror).
#[repr(C)]
pub struct HostApi {
    abi: u32,
    reserved: u32,
    user_data: *mut c_void,
    log: unsafe extern "C" fn(user_data: *mut c_void, level: u32, message: *const c_char),
    provide_service: unsafe extern "C" fn(user_data: *mut c_void, service: *const c_char) -> u64,
    revoke_service: unsafe extern "C" fn(user_data: *mut c_void, service: *const c_char),
    register_entity_kind: unsafe extern "C" fn(
        user_data: *mut c_void,
        name: *const c_char,
        schema: *const c_char,
    ) -> u32,
    entity_kind_id: unsafe extern "C" fn(user_data: *mut c_void, name: *const c_char) -> u32,
}

/// The vtable the plugin exports (`#[repr(C)]` mirror of `PluginV1`).
#[repr(C)]
pub struct PluginV1 {
    abi: u32,
    reserved: u32,
    name: *const c_char,
    load: Option<unsafe extern "C" fn(api: *const HostApi) -> i32>,
    tick: Option<unsafe extern "C" fn(frame: u64) -> i32>,
    unload: Option<unsafe extern "C" fn(api: *const HostApi) -> i32>,
}

/// A `#[repr(C)]` static holding raw pointers: `Sync` is declared by hand.
struct VTable(PluginV1);
unsafe impl Sync for VTable {}

static PLUGIN_NAME: &[u8] = b"clock\0";
static VTABLE: VTable = VTable(PluginV1 {
    abi: ABI,
    reserved: 0,
    name: PLUGIN_NAME.as_ptr().cast(),
    load: Some(load),
    tick: Some(tick),
    unload: Some(unload),
});

/// The api pointer handed in by `load()`. `tick` does not receive it, so it is
/// stored here; the host guarantees it stays valid while loaded, and `unload()`
/// runs before it goes away.
static API: AtomicPtr<HostApi> = AtomicPtr::new(std::ptr::null_mut());

/// The single symbol the host looks for (`releste_plugin_host::abi::ENTRY_SYMBOL`).
#[no_mangle]
pub extern "C" fn releste_plugin_v1() -> *const PluginV1 {
    &VTABLE.0
}

/// Load boundary: store the api and say hello. Returning `0` means success.
unsafe extern "C" fn load(api: *const HostApi) -> i32 {
    if api.is_null() {
        return 1;
    }
    API.store(api.cast_mut(), Ordering::Release);
    say(level::INFO, "clock: loaded");
    0
}

/// Once per tick; reports once per second.
unsafe extern "C" fn tick(frame: u64) -> i32 {
    if frame % TICKS_PER_SECOND == 0 {
        say(
            level::INFO,
            &format!("clock: {}s of game time", frame / TICKS_PER_SECOND),
        );
    }
    0
}

/// Unload boundary: say goodbye, then forget the api.
unsafe extern "C" fn unload(_api: *const HostApi) -> i32 {
    say(level::INFO, "clock: unloaded");
    API.store(std::ptr::null_mut(), Ordering::Release);
    0
}

/// Emit one log line through the host's `log` callback.
///
/// The message may not contain an interior NUL; if it does, the line is
/// dropped (losing one log line beats panicking across an FFI boundary,
/// AGENTS.md 15.8).
unsafe fn say(level: u32, message: &str) {
    let api = API.load(Ordering::Acquire);
    if api.is_null() {
        return;
    }
    let api = &*api;
    let Ok(message) = CString::new(message) else {
        return;
    };
    (api.log)(api.user_data, level, message.as_ptr());
}
