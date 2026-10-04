//! `reles-reload`: FileWatcher, Patch, and conflict resolution.
//!
//! The core of hot reloading: diff two maps into a `Vec<Patch>` and
//! apply them to the live World without restarting or dropping the session.
//!
//! # Conflict handling strategies (in order)
//! 1. Player overlaps a newly added entity → push apart (`World::resolve_overlap`)
//! 2. A referenced ID disappears → log a warning, skip that patch
//! 3. Room bounds shrink and the player is outside → log an error, roll back the whole patch

#![deny(warnings)]

mod apply;
mod watcher;

pub use apply::{ApplyError, ApplyReport, Reload, ReloadError};
pub use watcher::{FileWatcher, WatchEvent, WatcherError};

/// Re-export the patch types so consumers only depend on `reles-reload`.
pub use reles_map::Patch;
