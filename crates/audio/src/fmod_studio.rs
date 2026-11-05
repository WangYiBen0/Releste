//! The real FMOD Studio bindings (the `fmod` feature).
//!
//! # Why a hand-written dlopen binding
//! AGENTS.md 4.7 requires that FMOD binaries are **not vendored** and that the user
//! supplies the shared library. So this uses `libloading` to resolve symbols at runtime
//! and has no link-time dependency.
//!
//! # Headerversion
//! `FMOD_Studio_System_Create` validates the `FMOD_STUDIO_VERSION` passed in and
//! returns an error on a mismatch. Different FMOD releases use different numbers, so
//! this **tries several known versions in order** and records the one actually
//! accepted, rather than hard-coding a single value.
//!
//! Measured: the vanilla Celeste `lib64/libfmodstudio.so.10` accepts `0x00011014`.

use std::ffi::{c_char, c_int, c_uint, c_void, CString};
use std::path::Path;

use libloading::Library;

use crate::AudioError;

/// Known FMOD Studio headerversion candidates, tried newest to oldest.
pub const KNOWN_HEADER_VERSIONS: &[u32] = &[
    0x0002_0300, // 2.03
    0x0002_0200, // 2.02
    0x0002_0100, // 2.01
    0x0002_0000, // 2.00
    0x0001_1014, // 1.10.14 (measured with the vanilla Celeste library)
    0x0001_1000,
];

/// Common values of FMOD_RESULT.
pub const FMOD_OK: c_int = 0;

// -- FMOD C API signatures ------------------------------------------
//
// All `extern "C"`; handles are always raw pointers and FMOD manages their lifetime.
type StudioSystemCreateFn = unsafe extern "C" fn(*mut *mut c_void, c_uint) -> c_int;
type StudioSystemInitializeFn =
    unsafe extern "C" fn(*mut c_void, c_int, c_uint, c_uint, *mut c_void) -> c_int;
type StudioSystemReleaseFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type StudioSystemUpdateFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type StudioSystemLoadBankFileFn =
    unsafe extern "C" fn(*mut c_void, *const c_char, c_uint, *mut *mut c_void) -> c_int;
type StudioSystemGetEventFn =
    unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void) -> c_int;
type BankGetEventCountFn = unsafe extern "C" fn(*mut c_void, *mut c_int) -> c_int;
type BankGetEventListFn =
    unsafe extern "C" fn(*mut c_void, *mut *mut c_void, c_int, *mut c_int) -> c_int;
type EventDescriptionGetPathFn =
    unsafe extern "C" fn(*mut c_void, *mut c_char, c_int, *mut c_int) -> c_int;
type EventDescriptionCreateInstanceFn =
    unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int;
type EventInstanceStartFn = unsafe extern "C" fn(*mut c_void) -> c_int;

/// Resolved symbol table.
struct StudioApi {
    create: StudioSystemCreateFn,
    initialize: StudioSystemInitializeFn,
    release: StudioSystemReleaseFn,
    update: StudioSystemUpdateFn,
    load_bank_file: StudioSystemLoadBankFileFn,
    get_event: StudioSystemGetEventFn,
    bank_event_count: BankGetEventCountFn,
    bank_event_list: BankGetEventListFn,
    event_path: EventDescriptionGetPathFn,
    event_create_instance: EventDescriptionCreateInstanceFn,
    event_start: EventInstanceStartFn,
}

impl StudioApi {
    /// Resolves all symbols from the dynamic library.
    ///
    /// # Safety
    /// The caller must ensure `lib` is a genuine FMOD Studio library and outlives this
    /// struct. The returned function pointers are called with FMOD's header-file signatures.
    unsafe fn resolve(lib: &Library) -> Result<Self, AudioError> {
        /// Take a symbol and copy out the function pointer (sidestepping the lifetime bound).
        macro_rules! sym {
            ($ty:ty, $name:literal) => {{
                let s: libloading::Symbol<$ty> =
                    lib.get(concat!($name, "\0").as_bytes()).map_err(|e| {
                        AudioError::FmodUnusable(format!("missing symbol {}: {e}", $name))
                    })?;
                *s
            }};
        }

        Ok(StudioApi {
            create: sym!(StudioSystemCreateFn, "FMOD_Studio_System_Create"),
            initialize: sym!(StudioSystemInitializeFn, "FMOD_Studio_System_Initialize"),
            release: sym!(StudioSystemReleaseFn, "FMOD_Studio_System_Release"),
            update: sym!(StudioSystemUpdateFn, "FMOD_Studio_System_Update"),
            load_bank_file: sym!(
                StudioSystemLoadBankFileFn,
                "FMOD_Studio_System_LoadBankFile"
            ),
            get_event: sym!(StudioSystemGetEventFn, "FMOD_Studio_System_GetEvent"),
            bank_event_count: sym!(BankGetEventCountFn, "FMOD_Studio_Bank_GetEventCount"),
            bank_event_list: sym!(BankGetEventListFn, "FMOD_Studio_Bank_GetEventList"),
            event_path: sym!(
                EventDescriptionGetPathFn,
                "FMOD_Studio_EventDescription_GetPath"
            ),
            event_create_instance: sym!(
                EventDescriptionCreateInstanceFn,
                "FMOD_Studio_EventDescription_CreateInstance"
            ),
            event_start: sym!(EventInstanceStartFn, "FMOD_Studio_EventInstance_Start"),
        })
    }
}

/// A loaded bank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BankHandle(pub *mut c_void);

// The raw-pointer handle is only used on the audio thread; FMOD itself is thread-safe.
unsafe impl Send for BankHandle {}

/// FMOD Studio system.
///
/// Holds the dynamic library handle, the resolved symbols and the FMOD system pointer.
pub struct FmodStudio {
    /// Must be kept alive: the function pointers in `api` all point into it.
    _library: Library,
    api: StudioApi,
    system: *mut c_void,
    /// The headerversion actually accepted.
    header_version: u32,
    /// Loaded banks (to prevent duplicate loading).
    banks: Vec<(String, BankHandle)>,
}

// The FMOD system is thread-safe; we only call it while holding &mut.
unsafe impl Send for FmodStudio {}

impl FmodStudio {
    /// Opens the library and creates / initializes the Studio system.
    ///
    /// # Errors
    /// - The library cannot be opened or lacks a required symbol -> [`AudioError::FmodUnusable`]
    /// - Every known headerversion was rejected -> [`AudioError::Init`]
    pub fn load(path: &Path) -> Result<Self, AudioError> {
        // SAFETY: loading an external dynamic library is outside Rust's guarantees. This
        // section only resolves symbols and calls them with FMOD's public header
        // signatures; FMOD manages the handles itself.
        let library = unsafe { Library::new(path) }.map_err(|e| {
            AudioError::FmodUnusable(format!("failed to load {}: {e}", path.display()))
        })?;

        let api = unsafe { StudioApi::resolve(&library)? };

        // Try each known headerversion in turn.
        let mut system: *mut c_void = std::ptr::null_mut();
        let mut accepted = None;
        for &version in KNOWN_HEADER_VERSIONS {
            let mut candidate: *mut c_void = std::ptr::null_mut();
            let result = unsafe { (api.create)(&mut candidate, version) };
            if result == FMOD_OK && !candidate.is_null() {
                system = candidate;
                accepted = Some(version);
                break;
            }
        }

        let Some(header_version) = accepted else {
            return Err(AudioError::Init(format!(
                "FMOD_Studio_System_Create rejected every known headerversion \
                 (tried {KNOWN_HEADER_VERSIONS:?}). \
                 This build is probably an unsupported FMOD release."
            )));
        };

        let studio = FmodStudio {
            _library: library,
            api,
            system,
            header_version,
            banks: Vec::new(),
        };
        Ok(studio)
    }

    /// Initializes the audio output.
    ///
    /// `max_channels` is the cap on simultaneously playing channels (the engine default is 32).
    pub fn initialize(&mut self, max_channels: c_int) -> Result<(), AudioError> {
        // FMOD_STUDIO_INIT_NORMAL = 0, FMOD_INIT_NORMAL = 0
        let result =
            unsafe { (self.api.initialize)(self.system, max_channels, 0, 0, std::ptr::null_mut()) };
        if result == FMOD_OK {
            tracing::info!(
                header_version = format_args!("0x{:08x}", self.header_version),
                max_channels,
                "FMOD Studio initialised"
            );
            Ok(())
        } else {
            Err(AudioError::Init(format!(
                "FMOD_Studio_System_Initialize failed: FMOD_RESULT={result}"
            )))
        }
    }

    /// The headerversion actually accepted.
    pub fn header_version(&self) -> u32 {
        self.header_version
    }

    /// Loads a bank file.
    ///
    /// Note: resolving events by path requires the **Master Bank** and its
    /// `Master Bank.strings.bank` to be loaded first.
    pub fn load_bank(&mut self, path: &Path) -> Result<BankHandle, AudioError> {
        let key = path.display().to_string();

        if let Some((_, handle)) = self.banks.iter().find(|(p, _)| *p == key) {
            return Ok(*handle);
        }

        let c_path = CString::new(key.clone()).map_err(|e| AudioError::BankLoad {
            path: key.clone(),
            message: format!("path contains NUL: {e}"),
        })?;

        let mut bank: *mut c_void = std::ptr::null_mut();
        let result =
            unsafe { (self.api.load_bank_file)(self.system, c_path.as_ptr(), 0, &mut bank) };
        if result != FMOD_OK || bank.is_null() {
            return Err(AudioError::BankLoad {
                path: key,
                message: format!("FMOD_RESULT={result}"),
            });
        }

        let handle = BankHandle(bank);
        self.banks.push((key, handle));
        Ok(handle)
    }

    /// Number of loaded banks.
    pub fn bank_count(&self) -> usize {
        self.banks.len()
    }

    /// Enumerates the event paths in a bank.
    pub fn event_paths(&self, bank: BankHandle) -> Result<Vec<String>, AudioError> {
        let mut count: c_int = 0;
        let result = unsafe { (self.api.bank_event_count)(bank.0, &mut count) };
        if result != FMOD_OK || count <= 0 {
            return Ok(Vec::new());
        }

        let mut descriptions: Vec<*mut c_void> = vec![std::ptr::null_mut(); count as usize];
        let mut got: c_int = 0;
        let result = unsafe {
            (self.api.bank_event_list)(bank.0, descriptions.as_mut_ptr(), count, &mut got)
        };
        if result != FMOD_OK {
            return Err(AudioError::Play {
                event: "<bank enumeration>".into(),
                message: format!("FMOD_Studio_Bank_GetEventList: FMOD_RESULT={result}"),
            });
        }
        descriptions.truncate(got.max(0) as usize);

        let mut paths = Vec::with_capacity(descriptions.len());
        for desc in descriptions {
            let mut buf = vec![0 as c_char; 512];
            let mut written: c_int = 0;
            let result = unsafe {
                (self.api.event_path)(desc, buf.as_mut_ptr(), buf.len() as c_int, &mut written)
            };
            if result == FMOD_OK {
                let bytes: Vec<u8> = buf
                    .iter()
                    .take_while(|c| **c != 0)
                    .map(|c| *c as u8)
                    .collect();
                paths.push(String::from_utf8_lossy(&bytes).into_owned());
            }
        }
        Ok(paths)
    }

    /// Plays an event by path (a one-shot instance).
    ///
    /// Requires the Master Bank + strings bank to be loaded.
    pub fn play_event(&mut self, event_path: &str) -> Result<(), AudioError> {
        let c_path = CString::new(event_path).map_err(|e| AudioError::Play {
            event: event_path.to_string(),
            message: format!("path contains NUL: {e}"),
        })?;

        let mut desc: *mut c_void = std::ptr::null_mut();
        let result = unsafe { (self.api.get_event)(self.system, c_path.as_ptr(), &mut desc) };
        if result != FMOD_OK || desc.is_null() {
            return Err(AudioError::Play {
                event: event_path.to_string(),
                message: format!(
                    "FMOD_Studio_System_GetEvent: FMOD_RESULT={result} \
                     (is the Master Bank + strings bank loaded?)"
                ),
            });
        }

        let mut instance: *mut c_void = std::ptr::null_mut();
        let result = unsafe { (self.api.event_create_instance)(desc, &mut instance) };
        if result != FMOD_OK || instance.is_null() {
            return Err(AudioError::Play {
                event: event_path.to_string(),
                message: format!(
                    "FMOD_Studio_EventDescription_CreateInstance: FMOD_RESULT={result}"
                ),
            });
        }

        let result = unsafe { (self.api.event_start)(instance) };
        if result != FMOD_OK {
            return Err(AudioError::Play {
                event: event_path.to_string(),
                message: format!("FMOD_Studio_EventInstance_Start: FMOD_RESULT={result}"),
            });
        }
        Ok(())
    }

    /// Called each frame: drives FMOD's internal virtual voice scheduling.
    pub fn update(&mut self) -> Result<(), AudioError> {
        let result = unsafe { (self.api.update)(self.system) };
        if result == FMOD_OK {
            Ok(())
        } else {
            Err(AudioError::Init(format!(
                "FMOD_Studio_System_Update: FMOD_RESULT={result}"
            )))
        }
    }
}

impl Drop for FmodStudio {
    fn drop(&mut self) {
        if !self.system.is_null() {
            // SAFETY: the system came from the create call above and is released only here, once.
            unsafe {
                (self.api.release)(self.system);
            }
            self.system = std::ptr::null_mut();
        }
    }
}

impl std::fmt::Debug for FmodStudio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FmodStudio")
            .field(
                "header_version",
                &format_args!("0x{:08x}", self.header_version),
            )
            .field("banks", &self.banks.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Measured value: the vanilla Celeste libfmodstudio.so.10 accepts this version.
    #[test]
    fn celeste_header_version_is_in_the_candidate_list() {
        assert!(
            KNOWN_HEADER_VERSIONS.contains(&0x0001_1014),
            "0x00011014 must stay in the list for the vanilla Celeste library"
        );
    }

    #[test]
    fn candidates_are_ordered_newest_first() {
        let mut sorted = KNOWN_HEADER_VERSIONS.to_vec();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(
            sorted, KNOWN_HEADER_VERSIONS,
            "candidates should be tried newest-first"
        );
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            KNOWN_HEADER_VERSIONS.len(),
            "candidates must be unique"
        );
    }

    /// If `RELESTE_FMOD_STUDIO_LIB` is set, really run the whole pipeline.
    ///
    /// ```text
    /// RELESTE_FMOD_STUDIO_LIB=/path/libfmodstudio.so \
    /// RELESTE_FMOD_BANKS=/path/to/assets/audio \
    ///   cargo test -p releste-audio --features fmod -- --nocapture
    /// ```
    #[test]
    fn full_studio_pipeline_when_library_provided() {
        let Ok(lib) = std::env::var("RELESTE_FMOD_STUDIO_LIB") else {
            eprintln!("skipping: RELESTE_FMOD_STUDIO_LIB not set");
            return;
        };

        let mut studio = FmodStudio::load(Path::new(&lib)).expect("load studio library");
        println!("header_version = 0x{:08x}", studio.header_version());
        studio.initialize(32).expect("initialize");
        studio.update().expect("update");

        let Ok(banks_dir) = std::env::var("RELESTE_FMOD_BANKS") else {
            eprintln!("skipping bank loading: RELESTE_FMOD_BANKS not set");
            return;
        };
        let banks_dir = Path::new(&banks_dir);

        // The Master Bank + strings are a prerequisite for path resolution.
        let master = banks_dir.join("Master Bank.bank");
        let strings = banks_dir.join("Master Bank.strings.bank");
        if master.is_file() {
            studio.load_bank(&master).expect("load master bank");
        }
        if strings.is_file() {
            studio.load_bank(&strings).expect("load strings bank");
        }

        let sfx = banks_dir.join("sfx.bank");
        if !sfx.is_file() {
            eprintln!("skipping event checks: sfx.bank not found");
            return;
        }
        let bank = studio.load_bank(&sfx).expect("load sfx bank");

        let events = studio.event_paths(bank).expect("enumerate events");
        println!("sfx.bank events = {}", events.len());
        for p in events.iter().take(3) {
            println!("  sample path: {p}");
        }
        assert!(!events.is_empty(), "sfx.bank should contain events");

        // The vast majority should carry the event:/ prefix. A few without a resolvable
        // path are allowed (FMOD returns an empty string for some embedded events), but
        // they must be reportable.
        let odd: Vec<&String> = events
            .iter()
            .filter(|p| !p.starts_with("event:/"))
            .collect();
        println!(
            "non event:/ paths = {} (samples: {:?})",
            odd.len(),
            odd.iter().take(3).collect::<Vec<_>>()
        );
        assert!(
            odd.len() * 10 < events.len(),
            "at least 90% of event paths should use the event:/ scheme; \
             got {} non-conforming out of {}",
            odd.len(),
            events.len()
        );

        // Actually play the first event.
        studio
            .play_event(&events[0])
            .unwrap_or_else(|e| panic!("play {}: {e}", events[0]));
        studio.update().expect("update after play");
        println!("played: {}", events[0]);
    }
}
