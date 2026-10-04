//! The frozen `#[repr(C)]` ABI shared with `.so` plugins.
//!
//! Everything crossing this boundary is a plain integer, a `CStr`, or JSON.
//! **No Rust type crosses it** — the host and the plugin may have been compiled
//! with different compilers, and a `String` is not an ABI.
//!
//! # How a plugin exports itself
//!
//! A `.so` exports exactly one symbol, [`ENTRY_SYMBOL`] (`releste_plugin_v1`),
//! which returns a pointer to a static [`PluginV1`]:
//!
//! ```ignore
//! #[no_mangle]
//! pub extern "C" fn releste_plugin_v1() -> *const relest::PluginV1 {
//!     &VTAB
//! }
//! ```
//!
//! The host reads `abi` first. A mismatch is rejected with a warning, before
//! any callback is invoked (ROADMAP Appendix C rule 8).

use std::os::raw::{c_char, c_void};

/// The one symbol every `.so` plugin exports, NUL-terminated for `dlsym`.
pub const ENTRY_SYMBOL: &[u8] = b"releste_plugin_v1\0";

/// ABI version this host speaks. Bump it only for a breaking vtable change.
pub const ABI_VERSION: u32 = 1;

/// Severity passed to [`HostApi::log`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum LogLevel {
    /// Very fine-grained tracing.
    Trace = 0,
    /// Developer detail, off by default.
    Debug = 1,
    /// Normal operation.
    Info = 2,
    /// Something recoverable; the host kept going.
    Warn = 3,
    /// Something failed; that one thing did not.
    Error = 4,
}

impl LogLevel {
    /// Convert a numeric level, mapping unknown values to `Info`.
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::Trace,
            1 => Self::Debug,
            2 => Self::Info,
            3 => Self::Warn,
            4 => Self::Error,
            _ => Self::Info,
        }
    }
}

/// Services the host offers a plugin, as plain function pointers.
///
/// A plugin never links against a host symbol; the host hands it this table.
#[repr(C)]
pub struct HostApi {
    /// Must equal [`ABI_VERSION`].
    pub abi: u32,
    /// Reserved for future expansion; must be zero so the struct can grow
    /// without shifting offsets.
    pub reserved: u32,
    /// Opaque pointer back to the host's own state. Only ever passed through.
    pub user_data: *mut c_void,
    /// Emit a log line from the plugin. `message` is a NUL-terminated UTF-8
    /// string that is only borrowed for the duration of the call.
    pub log: unsafe extern "C" fn(user_data: *mut c_void, level: u32, message: *const c_char),
    /// Publish a service. Returns the uid, or `0` if the call was rejected.
    pub provide_service:
        unsafe extern "C" fn(user_data: *mut c_void, service: *const c_char) -> u64,
    /// Withdraw a previously published service.
    pub revoke_service: unsafe extern "C" fn(user_data: *mut c_void, service: *const c_char),
    /// Register an entity kind. `schema` is a JSON document describing its
    /// fields. Returns the host-computed `EntityKindId`, or `0` on rejection.
    pub register_entity_kind: unsafe extern "C" fn(
        user_data: *mut c_void,
        name: *const c_char,
        schema: *const c_char,
    ) -> u32,
    /// Look up the entity kind id for a name the plugin did not itself define.
    /// Returns `0` when unknown — the caller must warn and skip, never panic.
    pub entity_kind_id: unsafe extern "C" fn(user_data: *mut c_void, name: *const c_char) -> u32,
}

/// The table a plugin exports through `releste_plugin_v1()`.
#[repr(C)]
pub struct PluginV1 {
    /// Must equal [`ABI_VERSION`]. Checked before anything else.
    pub abi: u32,
    /// Reserved; must be zero.
    pub reserved: u32,
    /// Owning plugin name, NUL-terminated. May be null; the manifest wins.
    pub name: *const c_char,
    /// Called once at the load boundary. Return `0` for success; any other
    /// value marks the fiber `Failed` with the reason the plugin logged.
    pub load: Option<unsafe extern "C" fn(api: *const HostApi) -> i32>,
    /// Called every frame while active. May be null.
    pub tick: Option<unsafe extern "C" fn(frame: u64) -> i32>,
    /// Called at the unload boundary, before the host drops capabilities.
    pub unload: Option<unsafe extern "C" fn(api: *const HostApi) -> i32>,
}
