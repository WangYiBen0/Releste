//! `reles-math`: 基础数学类型。
//!
//! 提供定点数 [`Fx`]、二维向量 [`Vec2`] 与轴对齐矩形 [`Rect`]。
//! 仿真层所有实数运算必须使用 [`Fx`]，禁止 `f32`。

#![deny(warnings)]

mod fixed;
mod rect;
mod vec2;

pub use fixed::Fx;
pub use rect::Rect;
pub use vec2::Vec2;

/// 世界坐标空间使用的分量类型。
///
/// Releste 的仿真全部使用定点数，帧时间戳 `dt` 固定为 `Fx::from(1) / 60`。
pub type WorldScalar = Fx;

/// 一帧的固定时间步长（1/60 秒）。
pub const FIXED_DT: Fx = Fx::from_bits(Fx::DT_ONE_SIXTIETH);
