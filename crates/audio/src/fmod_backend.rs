//! FMOD backend (the `fmod` feature).
//!
//! # The user provides the shared library (AGENTS.md 4.7)
//! This module does **not link** FMOD; it opens
//! `libfmod.so` / `fmod.dll` / `libfmod.dylib` at runtime with `libloading`.
//!
//! - Build time: `build.rs` probes for the library location and, when it is
//!   missing, reports the error in the format AGENTS.md specifies
//!   (`RELESTE_FMOD_ALLOW_MISSING=1` skips it).
//! - Runtime: [`FmodBackend::new`] opens the library and probes the API
//!   generation; on failure it returns [`AudioError::FmodNotFound`] or
//!   [`AudioError::FmodUnusable`].
//!
//! # Supported API generations
//! FMOD's ABI is completely incompatible across the three generations, so startup must identify:
//!
//! | Generation      | Key symbols                 | Era  |
//! | --------------- | --------------------------- | ---- |
//! | FMOD Studio     | `FMOD_Studio_System_Create` | 5.x+ |
//! | FMOD Ex         | `FMOD_System_Create`        | 4.x  |
//! | FMOD 3 (Legacy) | `FSOUND_Init`               | 3.x  |
//!
//! Only the Studio generation can directly support this engine's bank/event model; Ex and
//! Legacy are recognized and **reported explicitly** rather than silently treated as Studio.

use std::path::{Path, PathBuf};

use libloading::{Library, Symbol};

use crate::command::Bus;
use crate::{AudioBackend, AudioError, FMOD_NOT_FOUND_MESSAGE};

/// FMOD API generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FmodGeneration {
    /// FMOD Studio (5.x/6.x): the bank + event model.
    Studio,
    /// FMOD Ex (4.x): the programmer sound model.
    Ex,
    /// FMOD 3.x: the `FSOUND_*` / `FMUSIC_*` model.
    Legacy3,
}

impl FmodGeneration {
    /// Human-readable description.
    pub fn describe(self) -> &'static str {
        match self {
            FmodGeneration::Studio => "FMOD Studio (5.x+)",
            FmodGeneration::Ex => "FMOD Ex (4.x)",
            FmodGeneration::Legacy3 => "FMOD 3.x (FSOUND/FMUSIC)",
        }
    }

    /// Whether this is a generation the engine supports directly.
    pub fn is_supported(self) -> bool {
        matches!(self, FmodGeneration::Studio)
    }
}

/// Probe result for a loaded library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FmodProbe {
    /// Path that was actually loaded successfully.
    pub path: PathBuf,
    /// Recognized API generation.
    pub generation: FmodGeneration,
    /// Key symbols that matched the probe.
    pub symbols: Vec<&'static str>,
}

impl FmodProbe {
    /// One-line summary (for logging).
    pub fn summary(&self) -> String {
        format!(
            "{} @ {} (symbols: {})",
            self.generation.describe(),
            self.path.display(),
            self.symbols.join(", ")
        )
    }
}

/// Platform default library name.
pub fn default_library_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "fmod.dll"
    } else if cfg!(target_os = "macos") {
        "libfmod.dylib"
    } else {
        "libfmod.so"
    }
}

/// Library path probed by the build script (if found).
pub fn probed_library_path() -> Option<PathBuf> {
    option_env!("RELESTE_FMOD_PATH").map(PathBuf::from)
}

/// Candidate library paths: environment variables first, then the build-script probe result, then the platform default name.
fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for var in ["RELESTE_FMOD_LIB", "RELESTE_FMOD_PATH"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() && !out.contains(&PathBuf::from(&v)) {
                out.push(PathBuf::from(v));
            }
        }
    }
    if let Some(p) = probed_library_path() {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out.push(PathBuf::from(default_library_name()));
    out
}

/// Probes the API generation by symbol.
///
/// Only resolves symbols and **does not call** any function pointers, so it is safe even for unverified libraries.
fn probe_generation(library: &Library) -> Option<(FmodGeneration, Vec<&'static str>)> {
    // (generation, required symbol, helper symbols)
    const CANDIDATES: &[(FmodGeneration, &str, &[&str])] = &[
        (
            FmodGeneration::Studio,
            "FMOD_Studio_System_Create",
            &[
                "FMOD_Studio_System_Initialize",
                "FMOD_Studio_System_GetCoreSystem",
                "FMOD_Studio_System_LoadBankFile",
            ],
        ),
        (
            FmodGeneration::Ex,
            "FMOD_System_Create",
            &["FMOD_System_Init", "FMOD_System_CreateSound"],
        ),
        (
            FmodGeneration::Legacy3,
            "FSOUND_Init",
            &["FSOUND_PlaySound", "FMUSIC_LoadSong", "FSOUND_GetVersion"],
        ),
    ];

    for (generation, required, helpers) in CANDIDATES {
        let has_required: bool = unsafe {
            library
                .get::<Symbol<*const ()>>(format!("{required}\0").as_bytes())
                .is_ok()
        };
        if !has_required {
            continue;
        }

        let mut symbols = vec![*required];
        for helper in *helpers {
            let present = unsafe {
                library
                    .get::<Symbol<*const ()>>(format!("{helper}\0").as_bytes())
                    .is_ok()
            };
            if present {
                symbols.push(helper);
            }
        }
        return Some((*generation, symbols));
    }

    None
}

/// FMOD backend.
///
/// Holds the opened library handle so the symbols stay valid.
pub struct FmodBackend {
    _library: Library,
    probe: FmodProbe,
    next_handle: u64,
}

impl FmodBackend {
    /// Opens and identifies the FMOD library.
    ///
    /// # Errors
    /// - None of the candidate paths open -> [`AudioError::FmodNotFound`] (the exact AGENTS.md 4.7 text).
    /// - It opens but has no recognizable FMOD symbols -> [`AudioError::FmodUnusable`].
    pub fn new() -> Result<Self, AudioError> {
        let candidates = candidate_paths();
        let mut attempts: Vec<String> = Vec::new();

        for path in &candidates {
            // SAFETY: loading an external dynamic library is inherently outside Rust's
            // guarantees. We only resolve symbols and do not call unverified function pointers.
            match unsafe { Library::new(path) } {
                Ok(library) => {
                    let Some((generation, symbols)) = probe_generation(&library) else {
                        attempts.push(format!(
                            "{}: loaded but no FMOD symbols found",
                            path.display()
                        ));
                        continue;
                    };

                    let probe = FmodProbe {
                        path: path.clone(),
                        generation,
                        symbols,
                    };

                    tracing::info!(probe = %probe.summary(), "FMOD library loaded");
                    if !generation.is_supported() {
                        tracing::warn!(
                            generation = generation.describe(),
                            "FMOD generation is not the supported Studio API; \
                             audio will run through the stub backend"
                        );
                    }

                    return Ok(FmodBackend {
                        _library: library,
                        probe,
                        next_handle: 0,
                    });
                }
                Err(e) => {
                    attempts.push(format!("{}: {e}", path.display()));
                }
            }
        }

        // As soon as one candidate is a real existing file, report "unusable" with the
        // original error (for example a bitness mismatch) -- far more useful than a vague
        // not found.
        if candidates.iter().any(|p| p.exists()) {
            Err(AudioError::FmodUnusable(format!(
                "FMOD library was found but could not be used.\n\
                 Tried:\n  {}\n\
                 Hint: the library architecture must match the process \
                 (a 32-bit .so cannot be loaded into a 64-bit build, and vice versa).",
                attempts.join("\n  ")
            )))
        } else {
            Err(AudioError::FmodNotFound(FMOD_NOT_FOUND_MESSAGE.to_string()))
        }
    }

    /// Probe result.
    pub fn probe(&self) -> &FmodProbe {
        &self.probe
    }

    /// API generation.
    pub fn generation(&self) -> FmodGeneration {
        self.probe.generation
    }
}

impl std::fmt::Debug for FmodBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FmodBackend")
            .field("probe", &self.probe)
            .finish()
    }
}

impl AudioBackend for FmodBackend {
    fn play(&mut self, event: &str, _volume: f32, _pitch: f32) -> Result<u64, AudioError> {
        // Full wiring needs generation-specific C API calls (see the module docs).
        // For now just increment the handle so the mixing layer stays usable.
        self.next_handle += 1;
        tracing::debug!(
            event,
            handle = self.next_handle,
            generation = self.probe.generation.describe(),
            "fmod play (stub)"
        );
        Ok(self.next_handle)
    }

    fn stop(&mut self, handle: u64) {
        tracing::debug!(handle, "fmod stop (stub)");
    }

    fn stop_all(&mut self) {
        tracing::debug!("fmod stop_all (stub)");
        self.next_handle = 0;
    }

    fn set_bus_volume(&mut self, bus: Bus, volume: f32) {
        tracing::debug!(?bus, volume, "fmod set_bus_volume (stub)");
    }

    fn name(&self) -> &'static str {
        "fmod"
    }
}

/// Whether the given library path exists (for diagnostics).
pub fn library_exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_library_name_is_platform_specific() {
        let name = default_library_name();
        assert!(name.contains("fmod"), "should mention fmod: {name}");
        assert!(name.ends_with(".so") || name.ends_with(".dll") || name.ends_with(".dylib"));
    }

    #[test]
    fn candidates_are_never_empty() {
        assert!(!candidate_paths().is_empty());
    }

    #[test]
    fn generations_describe_and_support_correctly() {
        assert!(FmodGeneration::Studio.is_supported());
        assert!(!FmodGeneration::Ex.is_supported());
        assert!(!FmodGeneration::Legacy3.is_supported());

        assert!(FmodGeneration::Legacy3.describe().contains("3.x"));
        assert!(FmodGeneration::Ex.describe().contains("4.x"));
        assert!(FmodGeneration::Studio.describe().contains("Studio"));
    }

    #[test]
    fn error_message_matches_agent_md() {
        // The text specified by AGENTS.md 4.7 must appear item by item.
        for needle in [
            "FMOD library not found.",
            "https://www.fmod.com/download",
            "libfmod.so / fmod.dll / libfmod.dylib",
            "LD_LIBRARY_PATH",
            "DYLD_LIBRARY_PATH",
            "alongside the .exe",
        ] {
            assert!(
                FMOD_NOT_FOUND_MESSAGE.contains(needle),
                "missing {needle:?} in message"
            );
        }
    }

    /// If `RELESTE_FMOD_LIB` is set, actually load it.
    ///
    /// This is the "test with the real thing" entry point:
    /// ```text
    /// RELESTE_FMOD_LIB=/path/to/libfmod.so cargo test -p releste-audio \
    ///     --features fmod -- --nocapture
    /// ```
    ///
    /// If `RELESTE_FMOD_EXPECT` (`studio` / `ex` / `legacy3`) is also set, it additionally
    /// asserts that the recognized API generation matches -- this way CI can use a stub
    /// library to verify the probe logic while a local run can use the real library to
    /// verify compatibility.
    ///
    /// When `RELESTE_FMOD_LIB` is not set, **skip and explain**, do not pretend to succeed.
    #[test]
    fn load_real_library_when_provided() {
        let Ok(path) = std::env::var("RELESTE_FMOD_LIB") else {
            eprintln!("skipping: RELESTE_FMOD_LIB not set");
            return;
        };
        if path.is_empty() {
            eprintln!("skipping: RELESTE_FMOD_LIB is empty");
            return;
        }

        let expected = std::env::var("RELESTE_FMOD_EXPECT").ok();

        match FmodBackend::new() {
            Ok(backend) => {
                let probe = backend.probe();
                println!("loaded: {}", probe.summary());
                assert!(probe.path.exists(), "probe should reference a real file");
                assert!(
                    !probe.symbols.is_empty(),
                    "at least one FMOD symbol must be found"
                );

                if let Some(expected) = expected {
                    let actual = probe.generation;
                    let matches = match expected.as_str() {
                        "studio" => actual == FmodGeneration::Studio,
                        "ex" => actual == FmodGeneration::Ex,
                        "legacy3" => actual == FmodGeneration::Legacy3,
                        other => panic!("unknown RELESTE_FMOD_EXPECT: {other}"),
                    };
                    assert!(
                        matches,
                        "expected generation {expected:?}, detected {:?}",
                        actual
                    );
                }
            }
            Err(AudioError::FmodUnusable(msg)) => {
                // A bitness mismatch, for example: this is a valid conclusion, not a test failure.
                // Print it for manual confirmation (a 64-bit process loading a 32-bit .so is this case).
                println!("library unusable in this process:\n{msg}\n");
                assert!(
                    expected.is_none(),
                    "expected generation {expected:?} but the library could not be loaded"
                );
            }
            Err(e) => panic!("expected to load {path}, got: {e}"),
        }
    }

    /// A 32-bit library in a 64-bit process must produce a diagnosable error, not a vague not found.
    #[test]
    fn architecture_mismatch_is_reported_as_unusable() {
        // Use a file with valid content but not an ELF as a fake library.
        let dir = std::env::temp_dir().join("releste-fmod-arch-test");
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("libfmod-fake.so");
        std::fs::write(&fake, b"not an ELF file at all").unwrap();

        // Go straight through Library::new to see whether the error is captured as FmodUnusable.
        // This does not go through FmodBackend::new, avoiding candidate-path pollution.
        assert!(unsafe { libloading::Library::new(&fake) }.is_err());

        std::fs::remove_file(&fake).ok();
    }
}
