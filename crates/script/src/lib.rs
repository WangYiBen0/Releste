//! `releste-script`: the cutscene scripting layer and [`CutsceneBridge`].
//!
//! # Core invariant (AGENTS.md 4.5)
//! **Lua never directly modifies physics state.** All side effects go through
//! [`SimCommand`] queued and consumed by the sim on the next frame.
//!
//! # Feature
//! - Default: only [`CutsceneBridge`] / [`SimCommand`] (pure logic, no external
//!   dependencies).
//! - `lua`: enables the mlua integration (`ScriptEngine`, off by default).

#![deny(warnings)]

mod bridge;
mod command;

pub use bridge::CutsceneBridge;
pub use command::{ActiveCommand, ActorKind, DurationFrames, PlayerState, SimCommand};

#[cfg(feature = "lua")]
mod engine;
#[cfg(feature = "lua")]
pub use engine::{ScriptEngine, ScriptError};
