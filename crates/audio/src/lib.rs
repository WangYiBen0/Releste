//! `releste-audio`: FMOD wrapper and audio mixing.
//!
//! # The FMOD shared library is provided by the user (AGENTS.md 4.7)
//! FMOD binaries are not vendored. With the `fmod` feature off, the mixing layer
//! stays usable (headless / tests / environments without an audio device) and
//! reports a clear error.
//!
//! # Layering
//! - [`command`]: audio requests (play / stop / volume).
//! - [`mixer`]: bus volumes, same-frame deduplication, priority preemption (pure logic, tested).
//! - `fmod_backend`: real FMOD calls (the `fmod` feature, off by default).
//! - [`AudioManager`]: the Service wrapper.

#![deny(warnings)]

pub mod command;
pub mod mixer;

#[cfg(feature = "fmod")]
pub mod fmod_backend;

#[cfg(feature = "fmod")]
pub mod fmod_studio;

#[cfg(feature = "fmod")]
pub use fmod_backend::{FmodBackend, FmodGeneration, FmodProbe};
#[cfg(feature = "fmod")]
pub use fmod_studio::FmodStudio;

pub use command::{AudioRequest, Bus, SoundHandle, SoundPriority};
pub use mixer::{AudioBackend, Mixer, MixerStats, NullBackend};

/// The ServiceId for audio.
pub const SERVICE_ID: &str = "audio";

/// The message when the FMOD library is missing (the format specified by AGENTS.md 4.7).
pub const FMOD_NOT_FOUND_MESSAGE: &str = "\
error: FMOD library not found.
Download from https://www.fmod.com/download
Place libfmod.so / fmod.dll / libfmod.dylib in:
  - Linux: /usr/local/lib/ or $LD_LIBRARY_PATH
  - Windows: alongside the .exe
  - macOS: /usr/local/lib/ or DYLD_LIBRARY_PATH";

/// Audio error.
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("{0}")]
    FmodNotFound(String),
    /// The library was found but cannot be used (bitness mismatch, no recognizable symbols, and so on).
    #[error("{0}")]
    FmodUnusable(String),
    #[error("failed to initialise FMOD: {0}")]
    Init(String),
    #[error("failed to load bank {path}: {message}")]
    BankLoad { path: String, message: String },
    #[error("failed to play event {event}: {message}")]
    Play { event: String, message: String },
    #[error("audio backend is not available (built without the `fmod` feature)")]
    BackendUnavailable,
}

/// Audio service.
///
/// Every game-side audio request goes through [`AudioManager::request`], and the
/// [`Mixer`] handles deduplication / priority before handing it to the backend.
pub struct AudioManager {
    mixer: Mixer,
}

impl AudioManager {
    /// Creates with the given backend.
    pub fn new(backend: Box<dyn AudioBackend>) -> Self {
        AudioManager {
            mixer: Mixer::new(backend),
        }
    }

    /// Creates without a backend (silent).
    ///
    /// For headless tests and environments without FMOD.
    pub fn silent() -> Self {
        Self::new(Box::new(NullBackend::new()))
    }

    /// Submits one audio request.
    pub fn request(&mut self, req: AudioRequest) -> Result<SoundHandle, AudioError> {
        self.mixer.submit(req)
    }

    /// Advances one frame (handles deferred release and dedup-window reset).
    pub fn tick(&mut self) {
        self.mixer.tick();
    }

    /// Mixing statistics.
    pub fn stats(&self) -> &MixerStats {
        self.mixer.stats()
    }

    /// The backend (mutable).
    pub fn backend_mut(&mut self) -> &mut dyn AudioBackend {
        self.mixer.backend_mut()
    }
}

impl Default for AudioManager {
    fn default() -> Self {
        Self::silent()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmod_message_mentions_all_platforms() {
        for needle in [
            "libfmod.so",
            "fmod.dll",
            "libfmod.dylib",
            "LD_LIBRARY_PATH",
            "DYLD_LIBRARY_PATH",
        ] {
            assert!(
                FMOD_NOT_FOUND_MESSAGE.contains(needle),
                "message should mention {needle}"
            );
        }
    }

    #[test]
    fn silent_manager_works() {
        let mut mgr = AudioManager::silent();
        mgr.request(AudioRequest::play_sfx(
            "event:/sfx/jump",
            1.0,
            SoundPriority::Normal,
        ))
        .unwrap();
        mgr.tick();
        assert_eq!(mgr.stats().played, 1);
    }
}
