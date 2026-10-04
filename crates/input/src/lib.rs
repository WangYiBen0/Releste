//! `reles-input`: input sampling, virtual buttons, key bindings.
//!
//! # Design
//! - The window layer pushes raw key events into [`InputBuffer`].
//! - [`InputManager`] consumes the buffer each simulation tick, computes
//!   pressed/held/released edges, and produces [`InputFrame`].
//! - The simulation only reads [`InputFrame`], never touching window / gamepad APIs.
//!
//! Sample rate: the window layer may write into the buffer at up to 1kHz,
//! while the simulation consumes at 60Hz, so short presses between ticks are not lost.

#![deny(warnings)]

mod binding;
mod buffer;
mod manager;
mod state;

pub use binding::{InputBinding, InputMap, KeyCode};
pub use buffer::{BufferedInput, InputBuffer};
pub use manager::{InputFrameEvent, InputManager};
pub use state::{ButtonState, InputFrame, VirtualButton};
