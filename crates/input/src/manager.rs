//! InputManager: converts the raw input buffer into a per-frame [`InputFrame`].

use std::sync::Arc;

use reles_kernel::{Context, FrameId, Service, ServiceId};
use reles_math::Fx;
use tracing::trace;

use crate::binding::InputMap;
use crate::buffer::InputBuffer;
use crate::state::{ButtonState, InputFrame, VirtualButton};

/// Input manager (service).
///
/// - The window layer gets an [`Arc<InputBuffer>`] via [`InputManager::buffer`] and
///   writes at high frequency from any thread.
/// - This service consumes the buffer each tick, computes edges, produces [`InputFrame`],
///   and broadcasts it on the event bus for the simulation layer to subscribe to.
#[derive(Debug)]
pub struct InputManager {
    buffer: Arc<InputBuffer>,
    map: InputMap,
    /// Held state from the previous tick (used for edge detection).
    prev_held: [bool; 12],
    /// Current frame.
    current: InputFrame,
}

impl InputManager {
    /// Creates.
    pub fn new(map: InputMap) -> Self {
        InputManager {
            buffer: Arc::new(InputBuffer::new()),
            map,
            prev_held: [false; 12],
            current: InputFrame::new(),
        }
    }

    /// Shared raw input buffer (handed to the window layer for writing).
    pub fn buffer(&self) -> Arc<InputBuffer> {
        Arc::clone(&self.buffer)
    }

    /// Current frame input.
    pub fn current(&self) -> &InputFrame {
        &self.current
    }

    /// Binding map.
    pub fn map(&self) -> &InputMap {
        &self.map
    }

    /// Binding map (mutable, for runtime rebinding).
    pub fn map_mut(&mut self) -> &mut InputMap {
        &mut self.map
    }

    /// Converts a physical input into a virtual button press.
    ///
    /// Called by the window layer on a key event; internally consults [`InputMap`] to
    /// decide which virtual buttons are affected.
    pub fn on_key_press(&self, key: crate::binding::KeyCode) {
        for button in VirtualButton::ALL {
            if self
                .map
                .get(button)
                .iter()
                .any(|b| matches!(b, crate::binding::InputBinding::Key(k) if *k == key))
            {
                self.buffer.press(button);
            }
        }
    }

    /// Converts a physical input into a virtual button release.
    pub fn on_key_release(&self, key: crate::binding::KeyCode) {
        for button in VirtualButton::ALL {
            if self
                .map
                .get(button)
                .iter()
                .any(|b| matches!(b, crate::binding::InputBinding::Key(k) if *k == key))
            {
                self.buffer.release(button);
            }
        }
    }

    /// Gamepad button press.
    pub fn on_gamepad_press(&self, button_index: u8) {
        for button in VirtualButton::ALL {
            if self.map.get(button).iter().any(|b| {
                matches!(b, crate::binding::InputBinding::GamepadButton(i) if *i == button_index)
            }) {
                self.buffer.press(button);
            }
        }
    }

    /// Gamepad button release.
    pub fn on_gamepad_release(&self, button_index: u8) {
        for button in VirtualButton::ALL {
            if self.map.get(button).iter().any(|b| {
                matches!(b, crate::binding::InputBinding::GamepadButton(i) if *i == button_index)
            }) {
                self.buffer.release(button);
            }
        }
    }

    /// Consumes the buffer and produces this frame's [`InputFrame`].
    fn sample(&mut self) {
        let (held, press_counts) = self.buffer.take();

        // Clear last frame's edges; the accumulated counts are decided by the buffer.
        self.current.clear_edges();

        for button in VirtualButton::ALL {
            let idx = button.index();
            let state = ButtonState::from_transition(self.prev_held[idx], held[idx]);
            self.current.set(button, state);
            for _ in 0..press_counts[idx] {
                self.current.inc_press(button);
            }
        }

        self.prev_held = held;
    }
}

impl Service for InputManager {
    fn id(&self) -> ServiceId {
        ServiceId::new("input")
    }

    fn attach(&mut self, _ctx: &mut Context) {
        trace!("input service attached");
    }

    fn update(&mut self, ctx: &mut Context, frame: FrameId, _dt: Fx) {
        self.sample();
        // Broadcast to the simulation layer (subscribers update afterwards in registration order).
        ctx.send(InputFrameEvent {
            frame,
            frame_input: self.current.clone(),
        });
    }
}

/// Per-frame input event.
#[derive(Debug, Clone)]
pub struct InputFrameEvent {
    pub frame: FrameId,
    pub frame_input: InputFrame,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::KeyCode;

    #[test]
    fn press_produces_edge() {
        let mut mgr = InputManager::new(InputMap::default());
        mgr.on_key_press(KeyCode::Space);
        mgr.sample();
        let jump = mgr.current().get(VirtualButton::Jump);
        assert!(jump.pressed, "space should press Jump");
        assert!(jump.held);

        // Second frame: still held but no pressed edge
        mgr.sample();
        let jump = mgr.current().get(VirtualButton::Jump);
        assert!(!jump.pressed);
        assert!(jump.held);

        // Release
        mgr.on_key_release(KeyCode::Space);
        mgr.sample();
        let jump = mgr.current().get(VirtualButton::Jump);
        assert!(jump.released);
        assert!(!jump.held);
    }

    #[test]
    fn short_press_between_ticks_is_not_lost() {
        let mut mgr = InputManager::new(InputMap::default());
        // Press and release within one tick
        mgr.on_key_press(KeyCode::X);
        mgr.on_key_release(KeyCode::X);
        mgr.sample();
        // Held state is false, but the press count should be 1
        assert_eq!(mgr.current().press_count(VirtualButton::Dash), 1);
    }
}
