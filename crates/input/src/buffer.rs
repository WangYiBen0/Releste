//! Raw input buffer: the window layer writes at high frequency, the simulation tick consumes.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::state::VirtualButton;

/// Number of buttons.
const BUTTONS: usize = 12;

/// Thread-safe raw input buffer.
///
/// The window / gamepad thread calls [`InputBuffer::press`] /
/// [`InputBuffer::release`] at high frequency (up to 1kHz), and the simulation thread
/// consumes and clears via [`InputBuffer::take`] on each tick.
///
/// - Held state uses one atomic bool per button.
/// - Press counts accumulate in atomics so short presses between ticks are not lost.
/// - Lock-free.
#[derive(Debug)]
pub struct InputBuffer {
    held: [AtomicBool; BUTTONS],
    presses: [AtomicU8; BUTTONS],
}

impl Default for InputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl InputBuffer {
    /// Creates an empty buffer.
    pub fn new() -> Self {
        InputBuffer {
            held: [const { AtomicBool::new(false) }; BUTTONS],
            presses: [const { AtomicU8::new(0) }; BUTTONS],
        }
    }

    /// Marks a press (callable from any thread).
    pub fn press(&self, button: VirtualButton) {
        let idx = button.index();
        self.held[idx].store(true, Ordering::Release);
        // Saturating accumulation of the press count
        let slot = &self.presses[idx];
        let mut cur = slot.load(Ordering::Relaxed);
        loop {
            let next = cur.saturating_add(1);
            match slot.compare_exchange_weak(cur, next, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => break,
                Err(actual) => cur = actual,
            }
        }
    }

    /// Marks a release (callable from any thread).
    pub fn release(&self, button: VirtualButton) {
        self.held[button.index()].store(false, Ordering::Release);
    }

    /// Consumes the current state and clears the press counts.
    ///
    /// Returns `(held, press_counts)`.
    pub fn take(&self) -> ([bool; BUTTONS], [u8; BUTTONS]) {
        let mut held = [false; BUTTONS];
        let mut counts = [0u8; BUTTONS];
        for button in VirtualButton::ALL {
            let idx = button.index();
            held[idx] = self.held[idx].load(Ordering::Acquire);
            counts[idx] = self.presses[idx].swap(0, Ordering::AcqRel);
        }
        (held, counts)
    }

    /// Queries whether a single button is held (without consuming).
    pub fn is_held(&self, button: VirtualButton) -> bool {
        self.held[button.index()].load(Ordering::Acquire)
    }

    /// Whether any button is held.
    pub fn any_held(&self) -> bool {
        VirtualButton::ALL
            .iter()
            .any(|b| self.held[b.index()].load(Ordering::Acquire))
    }

    /// Clears everything.
    pub fn clear(&self) {
        for button in VirtualButton::ALL {
            let idx = button.index();
            self.held[idx].store(false, Ordering::Release);
            self.presses[idx].store(0, Ordering::Release);
        }
    }
}

/// Result of a consume.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BufferedInput {
    pub held: [bool; BUTTONS],
    pub press_counts: [u8; BUTTONS],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_and_take() {
        let buf = InputBuffer::new();
        buf.press(VirtualButton::Jump);
        buf.press(VirtualButton::Jump);
        buf.release(VirtualButton::Jump);

        let (held, counts) = buf.take();
        assert!(!held[VirtualButton::Jump.index()]);
        assert_eq!(counts[VirtualButton::Jump.index()], 2);

        let (_, counts2) = buf.take();
        assert_eq!(counts2[VirtualButton::Jump.index()], 0);
    }

    #[test]
    fn counts_are_per_button() {
        let buf = InputBuffer::new();
        buf.press(VirtualButton::Jump);
        buf.press(VirtualButton::Dash);
        buf.press(VirtualButton::Dash);

        let (_, counts) = buf.take();
        assert_eq!(counts[VirtualButton::Jump.index()], 1);
        assert_eq!(counts[VirtualButton::Dash.index()], 2);
    }

    #[test]
    fn held_state_persists() {
        let buf = InputBuffer::new();
        buf.press(VirtualButton::Left);
        let (held, _) = buf.take();
        assert!(held[VirtualButton::Left.index()]);
        let (held, _) = buf.take();
        assert!(held[VirtualButton::Left.index()]);
    }
}
