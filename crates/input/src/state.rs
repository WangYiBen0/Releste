//! Virtual buttons and per-frame input state.

use serde::{Deserialize, Serialize};

/// Logical button.
///
/// Decoupled from physical keys / gamepads, bound via [`crate::InputMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VirtualButton {
    /// Left.
    Left,
    /// Right.
    Right,
    /// Up.
    Up,
    /// Down.
    Down,
    /// Jump.
    Jump,
    /// Dash.
    Dash,
    /// Grab.
    Grab,
    /// Pause.
    Pause,
    /// Confirm.
    Confirm,
    /// Cancel / back.
    Cancel,
    /// Journal.
    Journal,
    /// Quick restart.
    Restart,
}

impl VirtualButton {
    /// All buttons (for iteration).
    pub const ALL: [VirtualButton; 12] = [
        VirtualButton::Left,
        VirtualButton::Right,
        VirtualButton::Up,
        VirtualButton::Down,
        VirtualButton::Jump,
        VirtualButton::Dash,
        VirtualButton::Grab,
        VirtualButton::Pause,
        VirtualButton::Confirm,
        VirtualButton::Cancel,
        VirtualButton::Journal,
        VirtualButton::Restart,
    ];

    /// Index (for compact arrays).
    pub const fn index(self) -> usize {
        match self {
            VirtualButton::Left => 0,
            VirtualButton::Right => 1,
            VirtualButton::Up => 2,
            VirtualButton::Down => 3,
            VirtualButton::Jump => 4,
            VirtualButton::Dash => 5,
            VirtualButton::Grab => 6,
            VirtualButton::Pause => 7,
            VirtualButton::Confirm => 8,
            VirtualButton::Cancel => 9,
            VirtualButton::Journal => 10,
            VirtualButton::Restart => 11,
        }
    }
}

/// State of a single button within one frame.
///
/// - `pressed`: just pressed this frame (edge).
/// - `held`: held continuously this frame.
/// - `released`: just released this frame (edge).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ButtonState {
    pub pressed: bool,
    pub held: bool,
    pub released: bool,
}

impl ButtonState {
    /// Whether it changed this frame.
    pub fn changed(self) -> bool {
        self.pressed || self.released
    }

    /// Computes the edges from "held last frame" and "held this frame".
    pub fn from_transition(was_held: bool, is_held: bool) -> Self {
        ButtonState {
            pressed: !was_held && is_held,
            held: is_held,
            released: was_held && !is_held,
        }
    }
}

/// Input snapshot for one simulation tick.
///
/// The simulation layer only reads this struct and never touches any window / gamepad API.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputFrame {
    states: [ButtonState; 12],
    /// Press counts accumulated this frame (used to buffer jumps etc.).
    press_counts: [u8; 12],
}

impl InputFrame {
    /// Empty input.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queries button state.
    pub fn get(&self, button: VirtualButton) -> ButtonState {
        self.states[button.index()]
    }

    /// Queries the press count accumulated this frame.
    ///
    /// Used for "input buffering" (e.g. a jump within 6 frames before landing still fires).
    pub fn press_count(&self, button: VirtualButton) -> u8 {
        self.press_counts[button.index()]
    }

    /// Sets button state (internal use).
    pub(crate) fn set(&mut self, button: VirtualButton, state: ButtonState) {
        self.states[button.index()] = state;
    }

    /// Increments the press count (internal use).
    pub(crate) fn inc_press(&mut self, button: VirtualButton) {
        let slot = &mut self.press_counts[button.index()];
        *slot = slot.saturating_add(1);
    }

    /// Clears per-frame edges (keeps held).
    pub(crate) fn clear_edges(&mut self) {
        for s in &mut self.states {
            s.pressed = false;
            s.released = false;
        }
        self.press_counts = [0; 12];
    }

    /// Horizontal axis (-1 / 0 / 1).
    pub fn axis_x(&self) -> i8 {
        let l = self.get(VirtualButton::Left).held;
        let r = self.get(VirtualButton::Right).held;
        match (l, r) {
            (true, false) => -1,
            (false, true) => 1,
            // When both are held, the buffer layer handles last-press priority; zero out here.
            _ => 0,
        }
    }

    /// Vertical axis (-1 / 0 / 1, positive downward).
    pub fn axis_y(&self) -> i8 {
        let u = self.get(VirtualButton::Up).held;
        let d = self.get(VirtualButton::Down).held;
        match (u, d) {
            (true, false) => -1,
            (false, true) => 1,
            _ => 0,
        }
    }

    /// Whether any direction button is held.
    pub fn has_direction(&self) -> bool {
        self.axis_x() != 0 || self.axis_y() != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_detect_edges() {
        let s = ButtonState::from_transition(false, true);
        assert!(s.pressed && s.held && !s.released);

        let s = ButtonState::from_transition(true, true);
        assert!(!s.pressed && s.held && !s.released);

        let s = ButtonState::from_transition(true, false);
        assert!(!s.pressed && !s.held && s.released);
    }

    #[test]
    fn axis_from_buttons() {
        let mut f = InputFrame::new();
        f.set(
            VirtualButton::Left,
            ButtonState {
                held: true,
                ..Default::default()
            },
        );
        assert_eq!(f.axis_x(), -1);
        assert_eq!(f.axis_y(), 0);
    }
}
