//! Scanning, loading, and frame-boundary unload.
//!
//! The scan is driven entirely by `plugin.toml` — nothing is `dlopen`ed until a
//! plugin has been chosen for loading. One broken plugin must never hide the
//! other N: every failure is a `[WARN]` plus a skipped entry, and the host
//! keeps running (ROADMAP Appendix C rule 8, AGENTS.md §15.8/§15.9).

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::os::raw::{c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use libloading::Library;
use tracing::{info, warn};

use crate::abi::{HostApi, LogLevel, PluginV1, ABI_VERSION, ENTRY_SYMBOL};
use crate::fiber::{Fiber, FiberState};
use crate::manifest::Manifest;
use crate::registry::{kind_id_for, EntityKind, Registry};

/// A manifest found by [`PluginHost::scan`], before anything is loaded.
#[derive(Debug, Clone)]
pub struct ScanEntry {
    /// Plugin name from the manifest.
    pub name: String,
    /// Directory containing `plugin.toml`.
    pub dir: PathBuf,
    /// Parsed manifest.
    pub manifest: Manifest,
}

/// Anything that stopped a plugin from being used.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// `plugins/` could not be read at all.
    #[error("cannot read plugin directory {0}: {1}")]
    Scan(String, #[source] std::io::Error),

    /// The artifact named by `entry` is not there.
    #[error("plugin `{0}`: artifact not found at {1}")]
    MissingArtifact(String, String),

    /// `dlopen` failed (bad ELF, missing symbol, unloadable dependency).
    #[error("plugin `{0}`: {1}")]
    Load(String, String),

    /// The plugin declared an ABI this host does not speak.
    #[error("plugin `{name}`: ABI {found} is unsupported (host speaks {expected})")]
    AbiMismatch {
        /// Plugin name.
        name: String,
        /// ABI the artifact declared.
        found: u32,
        /// ABI the host speaks.
        expected: u32,
    },

    /// The artifact exported no entry symbol.
    #[error("plugin `{0}`: missing entry symbol `releste_plugin_v1`")]
    MissingEntry(String),

    /// `load()` returned a non-zero status.
    #[error("plugin `{0}`: load failed: {1}")]
    LoadRejected(String, String),
}

/// State reachable from an `extern "C"` callback.
///
/// Kept on the heap behind a raw pointer handed to the plugin, so its address
/// is stable for as long as the plugin is loaded.
struct HostState {
    /// Name of the plugin being called back into.
    plugin: String,
    /// Shared capability registry. Shared by `Arc` because a callback fires
    /// while the host itself is borrowed.
    registry: Arc<Mutex<Registry>>,
}

/// A loaded `.so`.
struct Loaded {
    /// Heap copy of the table handed to the plugin. `user_data` inside it
    /// points at `_state`, so `_api` is declared first and dropped first.
    _api: Box<HostApi>,
    /// Kept alive purely for its address: the plugin dereferences it through
    /// `user_data` from every callback, for as long as it is loaded.
    _state: Box<HostState>,
    /// Entry pointer returned by `releste_plugin_v1()`; points into
    /// `_library`'s mapping, which this struct keeps alive.
    vtable: *const PluginV1,
    /// Dropping this `dlclose`s the artifact.
    _library: Library,
}

impl Loaded {
    fn vtable(&self) -> &PluginV1 {
        // Safety: produced by `releste_plugin_v1()` from `_library`, whose
        // mapping is still alive, and validated before insertion.
        unsafe { &*self.vtable }
    }
}

/// The host: a scanner, a loader, and a registry of live fibers.
///
/// All mutation happens between frames — [`PluginHost::tick`] is the boundary
/// at which loads and unloads are actually allowed to take effect.
pub struct PluginHost {
    root: PathBuf,
    registry: Arc<Mutex<Registry>>,
    /// Manifests seen on the last [`PluginHost::scan`].
    scanned: BTreeMap<String, ScanEntry>,
    /// Lifecycle state per plugin.
    fibers: BTreeMap<String, Fiber>,
    /// Teardown scopes per plugin, so a `--inspect-plugins` dump can print the
    /// effect tree of anything still live.
    scopes: BTreeMap<String, Arc<crate::scope::Scope>>,
    /// Artifacts currently mapped.
    loaded: BTreeMap<String, Loaded>,
    /// Unloads requested during a frame, executed at the next boundary.
    pending_unloads: Vec<String>,
}

impl PluginHost {
    /// Create a host rooted at `root` (normally `plugins/` next to the exe).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            registry: Arc::new(Mutex::new(Registry::new())),
            scanned: BTreeMap::new(),
            fibers: BTreeMap::new(),
            scopes: BTreeMap::new(),
            loaded: BTreeMap::new(),
            pending_unloads: Vec::new(),
        }
    }

    /// Directory this host scans.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Shared capability registry. Entity kind lookups go through this.
    pub fn registry(&self) -> Arc<Mutex<Registry>> {
        Arc::clone(&self.registry)
    }

    /// Scan `root` for `<dir>/plugin.toml`.
    ///
    /// A missing directory is a warning, not an error: `plugins/` may be empty
    /// and the game still boots (Appendix C rule 3).
    ///
    /// Malformed manifests and ABI mismatches are logged and skipped; the rest
    /// of the scan continues.
    pub fn scan(&mut self) -> Result<usize, HostError> {
        self.scanned.clear();
        if !self.root.is_dir() {
            warn!(
                "plugin directory {} does not exist; running without plugins",
                self.root.display()
            );
            return Ok(0);
        }

        let mut found = 0usize;
        let read = std::fs::read_dir(&self.root)
            .map_err(|e| HostError::Scan(self.root.display().to_string(), e))?;
        for entry in read.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let manifest_path = dir.join("plugin.toml");
            if !manifest_path.is_file() {
                continue;
            }
            let manifest = match Manifest::from_file(&manifest_path) {
                Ok(m) => m,
                Err(e) => {
                    warn!("skipping {}: {e}", manifest_path.display());
                    continue;
                }
            };
            if manifest.abi != ABI_VERSION {
                warn!(
                    "skipping plugin `{}`: ABI {} is unsupported (host speaks {})",
                    manifest.name, manifest.abi, ABI_VERSION
                );
                continue;
            }
            found += 1;
            self.fibers
                .entry(manifest.name.clone())
                .or_insert_with(|| Fiber::new(manifest.name.clone()));
            self.scanned.insert(
                manifest.name.clone(),
                ScanEntry {
                    name: manifest.name.clone(),
                    dir,
                    manifest,
                },
            );
        }
        info!(
            "scanned {} plugin directory: {} manifest(s)",
            self.root.display(),
            found
        );
        Ok(found)
    }

    /// Names returned by the last scan, in stable order.
    pub fn scanned(&self) -> impl Iterator<Item = &str> {
        self.scanned.keys().map(String::as_str)
    }

    /// Fiber for `name`, if the host knows about it.
    pub fn fiber(&self, name: &str) -> Option<&Fiber> {
        self.fibers.get(name)
    }

    /// Teardown tree for `name`, if it owns a scope.
    pub fn effects(&self, name: &str) -> Vec<crate::scope::EffectMeta> {
        self.scopes
            .get(name)
            .map(|s| s.effects())
            .unwrap_or_default()
    }

    /// Names of plugins currently mapped into the process.
    pub fn loaded(&self) -> impl Iterator<Item = &str> {
        self.loaded.keys().map(String::as_str)
    }

    /// Whether `name` is mapped and `Active`.
    pub fn is_active(&self, name: &str) -> bool {
        self.loaded.contains_key(name)
            && self
                .fibers
                .get(name)
                .is_some_and(|f| f.state() == FiberState::Active)
    }

    /// Load `name` from a previously successful scan.
    ///
    /// On any failure the fiber is marked [`FiberState::Failed`] with a reason
    /// and `Err` is returned; the host itself stays usable.
    pub fn load(&mut self, name: &str) -> Result<(), HostError> {
        if self.loaded.contains_key(name) {
            return Ok(());
        }
        let entry =
            self.scanned.get(name).cloned().ok_or_else(|| {
                HostError::MissingArtifact(name.to_string(), "not scanned".into())
            })?;

        let path = entry.manifest.entry_path(&entry.dir);
        if !path.is_file() {
            let msg = HostError::MissingArtifact(name.to_string(), path.display().to_string());
            self.fail(name, msg.to_string());
            return Err(msg);
        }

        match self.load_artifact(&entry, &path) {
            Ok(()) => {
                let fiber = self
                    .fibers
                    .entry(name.to_string())
                    .or_insert_with(|| Fiber::new(name));
                let _ = fiber.advance(FiberState::Loading);
                let _ = fiber.advance(FiberState::Active);
                info!(target: "plugins", "loaded `{name}`");
                Ok(())
            }
            Err(e) => {
                self.fail(name, e.to_string());
                warn!("failed to load plugin `{name}`: {e}; skipping and continuing");
                Err(e)
            }
        }
    }

    fn load_artifact(&mut self, entry: &ScanEntry, path: &Path) -> Result<(), HostError> {
        let name = entry.name.clone();
        // Safety: `Library::new` maps the file; every symbol we take out of it
        // is validated before the plugin is allowed to run.
        let library = unsafe { Library::new(path) }.map_err(|e| {
            HostError::Load(name.clone(), format!("dlopen {}: {e}", path.display()))
        })?;

        let sym = unsafe { library.get::<unsafe extern "C" fn() -> *const PluginV1>(ENTRY_SYMBOL) }
            .map_err(|_| HostError::MissingEntry(name.clone()))?;
        let vtable = unsafe { sym() };
        if vtable.is_null() {
            return Err(HostError::Load(
                name.clone(),
                "entry symbol returned null".into(),
            ));
        }

        // Safety: the symbol just returned a pointer we can read.
        let vt: &PluginV1 = unsafe { &*vtable };
        if vt.abi != ABI_VERSION {
            return Err(HostError::AbiMismatch {
                name,
                found: vt.abi,
                expected: ABI_VERSION,
            });
        }

        let mut state = Box::new(HostState {
            plugin: name.clone(),
            registry: Arc::clone(&self.registry),
        });
        let api = Box::new(HostApi {
            abi: ABI_VERSION,
            reserved: 0,
            user_data: (&mut *state as *mut HostState).cast::<std::os::raw::c_void>(),
            log: host_log,
            provide_service: host_provide_service,
            revoke_service: host_revoke_service,
            register_entity_kind: host_register_entity_kind,
            entity_kind_id: host_entity_kind_id,
        });

        let status = match vt.load {
            Some(load_fn) => unsafe { load_fn(&*api) },
            None => 0,
        };
        if status != 0 {
            return Err(HostError::LoadRejected(
                name,
                format!("load() returned {status}"),
            ));
        }

        let scope = crate::scope::Scope::new();
        self.scopes.insert(name.clone(), Arc::new(scope));
        self.loaded.insert(
            name.clone(),
            Loaded {
                _api: api,
                _state: state,
                vtable,
                _library: library,
            },
        );
        Ok(())
    }

    /// Queue `name` for unload; it actually unloads at the next frame boundary.
    pub fn request_unload(&mut self, name: &str) {
        if self.loaded.contains_key(name) && !self.pending_unloads.iter().any(|n| n == name) {
            self.pending_unloads.push(name.to_string());
        }
    }

    /// Drain queued unloads. Call this at a frame boundary.
    ///
    /// The plugin's `unload()` runs first, then its disposers in LIFO order,
    /// then its capabilities are revoked — so a dependent fiber observing the
    /// registry never sees a half-torn plugin.
    pub fn drain_unloads(&mut self) {
        let pending: Vec<String> = std::mem::take(&mut self.pending_unloads);
        for name in pending {
            self.unload_now(&name);
        }
    }

    fn unload_now(&mut self, name: &str) {
        let Some(loaded) = self.loaded.remove(name) else {
            return;
        };

        if let Some(fiber) = self.fibers.get_mut(name) {
            let _ = fiber.advance(FiberState::Unloading);
        }

        let vt = loaded.vtable();
        if let Some(unload_fn) = vt.unload {
            let status = unsafe { unload_fn(&*loaded._api) };
            if status != 0 {
                warn!(
                    target: "plugins",
                    "plugin `{name}` unload() returned {status}; continuing teardown"
                );
            }
        }

        // Drop the mapping before revoking capabilities, so a late callback
        // cannot land on unmapped memory.
        drop(loaded);

        if let Some(scope) = self.scopes.remove(name) {
            scope.close();
            scope.dispose_all();
        }

        if let Ok(mut registry) = self.registry.lock() {
            registry.revoke_provider(name);
        } else {
            warn!(target: "plugins", "capability registry poisoned while unloading `{name}`");
        }

        if let Some(fiber) = self.fibers.get_mut(name) {
            let _ = fiber.advance(FiberState::Disposed);
        }
        info!(target: "plugins", "unloaded `{name}`");
    }

    /// Tick every `Active` plugin. Loads and unloads queued during the frame
    /// take effect here, at the boundary.
    pub fn tick(&mut self, frame: u64) {
        self.drain_unloads();

        let active: Vec<String> = self
            .fibers
            .iter()
            .filter(|(_, f)| f.state() == FiberState::Active)
            .map(|(n, _)| n.clone())
            .collect();

        for name in active {
            let Some(loaded) = self.loaded.get(&name) else {
                continue;
            };
            let Some(tick_fn) = loaded.vtable().tick else {
                continue;
            };
            let status = unsafe { tick_fn(frame) };
            if status != 0 {
                warn!(
                    target: "plugins",
                    "plugin `{name}` tick() returned {status}; marking failed"
                );
                self.fail(&name, format!("tick() returned {status}"));
                self.request_unload(&name);
            }
        }
    }

    /// Unload everything, in reverse load order.
    pub fn shutdown(&mut self) {
        let names: Vec<String> = self.loaded.keys().cloned().rev().collect();
        for n in names {
            self.unload_now(&n);
        }
        self.pending_unloads.clear();
    }

    fn fail(&mut self, name: &str, reason: String) {
        let fiber = self
            .fibers
            .entry(name.to_string())
            .or_insert_with(|| Fiber::new(name));
        fiber.fail(reason);
    }
}

// ---------------------------------------------------------------------------
// extern "C" callbacks
//
// Each recovers its `HostState` from `user_data`. All of them are best-effort:
// a callback that cannot make sense of its arguments logs and returns a
// neutral value rather than unwinding across an FFI boundary.
// ---------------------------------------------------------------------------

/// # Safety
/// `user_data` must be the pointer the host handed to this plugin.
unsafe fn state<'a>(user_data: *mut c_void) -> Option<&'a HostState> {
    if user_data.is_null() {
        return None;
    }
    Some(&*(user_data as *const HostState))
}

unsafe extern "C" fn host_log(user_data: *mut c_void, level: u32, message: *const c_char) {
    let Some(st) = state(user_data) else {
        return;
    };
    let msg = if message.is_null() {
        "<null message>".to_string()
    } else {
        CStr::from_ptr(message).to_string_lossy().into_owned()
    };
    let plugin = &st.plugin;
    match LogLevel::from_raw(level) {
        LogLevel::Trace => tracing::trace!(target: "plugins", %plugin, "{msg}"),
        LogLevel::Debug => tracing::debug!(target: "plugins", %plugin, "{msg}"),
        LogLevel::Info => tracing::info!(target: "plugins", %plugin, "{msg}"),
        LogLevel::Warn => tracing::warn!(target: "plugins", %plugin, "{msg}"),
        LogLevel::Error => tracing::error!(target: "plugins", %plugin, "{msg}"),
    }
}

unsafe extern "C" fn host_provide_service(user_data: *mut c_void, service: *const c_char) -> u64 {
    let Some(st) = state(user_data) else {
        return 0;
    };
    if service.is_null() {
        return 0;
    }
    let service = CStr::from_ptr(service).to_string_lossy();
    match st.registry.lock() {
        Ok(mut r) => r.provide(service.as_ref(), st.plugin.clone()),
        Err(_) => 0,
    }
}

unsafe extern "C" fn host_revoke_service(user_data: *mut c_void, service: *const c_char) {
    let Some(st) = state(user_data) else {
        return;
    };
    if service.is_null() {
        return;
    }
    let service = CStr::from_ptr(service).to_string_lossy();
    if let Ok(mut r) = st.registry.lock() {
        r.revoke(service.as_ref());
    }
}

unsafe extern "C" fn host_register_entity_kind(
    user_data: *mut c_void,
    name: *const c_char,
    schema: *const c_char,
) -> u32 {
    let Some(st) = state(user_data) else {
        return 0;
    };
    if name.is_null() {
        return 0;
    }
    let kind_name = CStr::from_ptr(name).to_string_lossy().into_owned();
    if kind_name.is_empty() {
        return 0;
    }
    let schema = if schema.is_null() {
        "{}".to_string()
    } else {
        CStr::from_ptr(schema).to_string_lossy().into_owned()
    };
    let kind_id = kind_id_for(&kind_name);
    match st.registry.lock() {
        Ok(mut r) => {
            r.register_entity_kind(EntityKind {
                kind_id,
                schema,
                provider: st.plugin.clone(),
                name: kind_name,
            });
            kind_id
        }
        Err(_) => 0,
    }
}

unsafe extern "C" fn host_entity_kind_id(user_data: *mut c_void, name: *const c_char) -> u32 {
    let Some(st) = state(user_data) else {
        return 0;
    };
    if name.is_null() {
        return 0;
    }
    let kind_name = CStr::from_ptr(name).to_string_lossy();
    match st.registry.lock() {
        Ok(r) => r
            .entity_kind(kind_id_for(&kind_name))
            .map(|k| k.kind_id)
            .unwrap_or(0),
        Err(_) => 0,
    }
}
