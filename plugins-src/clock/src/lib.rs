//! `clock` — 样例 `.so` 插件。
//!
//! 每过一秒游戏时间（60 个固定 tick）就通过 host 的 `log` 回调报一声，
//! 用来演示 Appendix C 的插件约定：一个 `.so` 只导出 `releste_plugin_v1`，
//! 它返回一个静态 `PluginV1`；host 需要的一切都从那张表里拿。
//!
//! 这个 crate **不依赖主 workspace**（AGENTS.md §15.1），也不依赖任何
//! 第三方 crate —— `HostApi` / `PluginV1` 是手写的 `#[repr(C)]` 镜像。
//! host 和插件可以由不同的编译器编译，Rust 类型绝不跨界。
//!
//! 构建（见 `README.md`）：
//!
//! ```text
//! cargo build --release --manifest-path plugins-src/clock/Cargo.toml
//! cp plugins-src/clock/target/release/libclock.so plugins/clock/clock.so
//! ```

#![allow(dead_code)] // 镜像 host 的完整 vtable；插件只用其中几个字段

use std::ffi::CString;
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicPtr, Ordering};

/// 日志级别，镜像 `reles_plugin_host::abi::LogLevel` 的数值。
mod level {
    pub const TRACE: u32 = 0;
    pub const DEBUG: u32 = 1;
    pub const INFO: u32 = 2;
    pub const WARN: u32 = 3;
    pub const ERROR: u32 = 4;
}

/// 本插件说的 ABI 版本；host 不认识就拒载，绝不会调用任何回调。
const ABI: u32 = 1;

/// 每秒报一次：60 个固定 tick。
const TICKS_PER_SECOND: u64 = 60;

/// host 递给插件的函数表（`#[repr(C)]` 镜像）。
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

/// 插件导出的 vtable（`#[repr(C)]` 镜像 `PluginV1`）。
#[repr(C)]
pub struct PluginV1 {
    abi: u32,
    reserved: u32,
    name: *const c_char,
    load: Option<unsafe extern "C" fn(api: *const HostApi) -> i32>,
    tick: Option<unsafe extern "C" fn(frame: u64) -> i32>,
    unload: Option<unsafe extern "C" fn(api: *const HostApi) -> i32>,
}

/// `#[repr(C)]` 静态里有裸指针，手动声明 `Sync`。
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

/// `load()` 递进来的 api 指针。`tick` 收不到它，只能存下来；
/// host 保证它在载入期间一直有效，卸载前 `unload()` 会先跑。
static API: AtomicPtr<HostApi> = AtomicPtr::new(std::ptr::null_mut());

/// host 找的唯一符号（`reles_plugin_host::abi::ENTRY_SYMBOL`）。
#[no_mangle]
pub extern "C" fn releste_plugin_v1() -> *const PluginV1 {
    &VTABLE.0
}

/// 载入边界：存下 api，报一声。返回 `0` 表示成功。
unsafe extern "C" fn load(api: *const HostApi) -> i32 {
    if api.is_null() {
        return 1;
    }
    API.store(api.cast_mut(), Ordering::Release);
    say(level::INFO, "clock: loaded");
    0
}

/// 每个 tick 一次；每秒报一声。
unsafe extern "C" fn tick(frame: u64) -> i32 {
    if frame % TICKS_PER_SECOND == 0 {
        say(
            level::INFO,
            &format!("clock: {}s of game time", frame / TICKS_PER_SECOND),
        );
    }
    0
}

/// 卸载边界：报一声，然后忘掉 api。
unsafe extern "C" fn unload(_api: *const HostApi) -> i32 {
    say(level::INFO, "clock: unloaded");
    API.store(std::ptr::null_mut(), Ordering::Release);
    0
}

/// 通过 host 的 `log` 回调发一行日志。
///
/// 消息里不允许有内部 NUL；有的话这条就不发了（安静地丢一行日志
/// 比 panic 跨 FFI 边界好，AGENTS.md §15.8）。
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
