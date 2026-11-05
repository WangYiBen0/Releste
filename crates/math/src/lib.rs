//! `releste-math`: basic math types.
//!
//! Provides the fixed-point number [`Fx`], the 2D vector [`Vec2`], and the
//! axis-aligned rectangle [`Rect`].
//! All real-number arithmetic in the simulation layer must use [`Fx`]; `f32` is
//! forbidden.

#![deny(warnings)]

mod fixed;
mod rect;
mod vec2;

pub use fixed::Fx;
pub use rect::Rect;
pub use vec2::Vec2;

/// The component type used in the world coordinate space.
///
/// Releste's simulation is entirely fixed-point; the frame time step `dt` is
/// fixed at `Fx::from(1) / 60`.
pub type WorldScalar = Fx;

/// The fixed time step of one frame (1/60 second).
pub const FIXED_DT: Fx = Fx::from_bits(Fx::DT_ONE_SIXTIETH);
