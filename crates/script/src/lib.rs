//! `reles-script`: 过场脚本层与 [`CutsceneBridge`]。
//!
//! # 核心不变量（AGENTS.md §4.5）
//! **Lua 永远不直接改物理状态。** 所有副作用通过
//! [`SimCommand`] 排队，下一帧由 sim 消费。
//!
//! # Feature
//! - 默认：仅 [`CutsceneBridge`] / [`SimCommand`]（纯逻辑，无外部依赖）。
//! - `lua`：启用 mlua 集成（`ScriptEngine`，默认关闭）。

#![deny(warnings)]

mod bridge;
mod command;

pub use bridge::CutsceneBridge;
pub use command::{ActiveCommand, ActorKind, DurationFrames, PlayerState, SimCommand};

#[cfg(feature = "lua")]
mod engine;
#[cfg(feature = "lua")]
pub use engine::{ScriptEngine, ScriptError};
