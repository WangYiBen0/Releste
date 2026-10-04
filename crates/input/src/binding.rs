//! Key bindings: physical input → virtual buttons.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::state::VirtualButton;

/// Physical key code (a self-contained enum decoupled from the window layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyCode {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,
    ArrowLeft,
    ArrowRight,
    ArrowUp,
    ArrowDown,
    Space,
    Enter,
    Escape,
    Tab,
    Shift,
    Control,
    Backspace,
    Comma,
    Period,
    Slash,
    Semicolon,
    Quote,
    Backquote,
    BracketLeft,
    BracketRight,
    Minus,
    Equal,
}

/// Binding of a single physical input source.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputBinding {
    /// Keyboard key.
    Key(KeyCode),
    /// Gamepad button (index defined by the gamepad layer).
    GamepadButton(u8),
    /// Gamepad axis + direction (`axis` index, `positive` means the positive direction).
    GamepadAxis { axis: u8, positive: bool },
}

/// Binding map: a virtual button may bind multiple physical inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputMap {
    bindings: HashMap<VirtualButton, Vec<InputBinding>>,
}

impl Default for InputMap {
    /// Default bindings (Celeste style).
    fn default() -> Self {
        use KeyCode::*;
        use VirtualButton as B;

        let mut bindings: HashMap<VirtualButton, Vec<InputBinding>> = HashMap::new();
        let mut bind = |b: VirtualButton, keys: &[KeyCode]| {
            bindings.insert(b, keys.iter().map(|k| InputBinding::Key(*k)).collect());
        };

        bind(B::Left, &[ArrowLeft, A]);
        bind(B::Right, &[ArrowRight, D]);
        bind(B::Up, &[ArrowUp, W]);
        bind(B::Down, &[ArrowDown, S]);
        bind(B::Jump, &[Space, C, J]);
        bind(B::Dash, &[X, K, Shift]);
        bind(B::Grab, &[Z, L, Control]);
        bind(B::Pause, &[Escape]);
        bind(B::Confirm, &[Enter, Space]);
        bind(B::Cancel, &[Escape, Backspace]);
        bind(B::Journal, &[Tab]);
        bind(B::Restart, &[R]);

        InputMap { bindings }
    }
}

impl InputMap {
    /// Empty binding map.
    pub fn new() -> Self {
        InputMap {
            bindings: HashMap::new(),
        }
    }

    /// Binds one physical input to a virtual button (appends).
    pub fn bind(&mut self, button: VirtualButton, binding: InputBinding) {
        self.bindings.entry(button).or_default().push(binding);
    }

    /// Clears all bindings of a button.
    pub fn unbind_all(&mut self, button: VirtualButton) {
        self.bindings.remove(&button);
    }

    /// Queries all physical inputs bound to a virtual button.
    pub fn get(&self, button: VirtualButton) -> &[InputBinding] {
        self.bindings.get(&button).map(Vec::as_slice).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_map_has_all_buttons() {
        let map = InputMap::default();
        for button in VirtualButton::ALL {
            assert!(
                !map.get(button).is_empty(),
                "button {button:?} has no default binding"
            );
        }
    }

    #[test]
    fn bind_adds() {
        let mut map = InputMap::new();
        map.bind(VirtualButton::Jump, InputBinding::Key(KeyCode::Space));
        assert_eq!(map.get(VirtualButton::Jump).len(), 1);
    }
}
