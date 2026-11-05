//! User settings: key bindings (the counterpart of vanilla `Settings.Instance`).
//!
//! Vanilla stores bindings in the settings under the user directory, and `SetDefaultKeyboardControls(reset)`
//! (`pureCeleste.cs:101595-101706`) falls back to the defaults when a binding is missing: arrow keys move,
//! C jumps, X dashes, Z/V/LeftShift grabs, Enter pauses. This engine persists them in `settings.toml`
//! (the stack spec forbids JSON): a missing section/unknown key name in the file -> fall back to the vanilla defaults and warn,
//! and never panic (AGENTS.md 15.8/15.9). File location `<root>/settings.toml`
//! (resolved by `Paths` and writable at runtime -- the future rebinding menu edits it directly).
//!
//! Key names = the `Debug` name of `winit::keyboard::KeyCode` (e.g. `"KeyC"`,
//! `"ArrowLeft"`), storing physical keys (scancode semantics), matching vanilla `TranslateKeys`
//! (101708)'s "bind by physical position".

use std::path::Path;

use serde::Deserialize;
use tracing::warn;
use winit::keyboard::KeyCode;

/// A set of key bindings: action -> physical key list (an action can bind multiple keys, vanilla
/// `Binding.Add(Keys...)` semantics; the Grab binding at 101622 binds three keys).
#[derive(Debug, Clone, PartialEq)]
pub struct KeyBindings {
    pub left: Vec<KeyCode>,
    pub right: Vec<KeyCode>,
    pub up: Vec<KeyCode>,
    pub down: Vec<KeyCode>,
    pub jump: Vec<KeyCode>,
    /// Dash (vanilla default X, `pureCeleste.cs:101632`).
    pub dash: Vec<KeyCode>,
    /// Grab (vanilla default Z/V/LeftShift, `pureCeleste.cs:101622`).
    pub grab: Vec<KeyCode>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        // Vanilla defaults (101602-101651). Grab (Z/V/LeftShift, 101622) and
        // dash (X, 101632) are added once Phase 4.1 is implemented.
        KeyBindings {
            left: vec![KeyCode::ArrowLeft],
            right: vec![KeyCode::ArrowRight],
            up: vec![KeyCode::ArrowUp],
            down: vec![KeyCode::ArrowDown],
            jump: vec![KeyCode::KeyC],
            dash: vec![KeyCode::KeyX],
            grab: vec![KeyCode::KeyZ, KeyCode::KeyV, KeyCode::ShiftLeft],
        }
    }
}

/// The serializable shape of the settings file.
#[derive(Debug, Default, Deserialize)]
struct KeysToml {
    left: Option<Vec<String>>,
    right: Option<Vec<String>>,
    up: Option<Vec<String>>,
    down: Option<Vec<String>>,
    jump: Option<Vec<String>>,
    dash: Option<Vec<String>>,
    grab: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct SettingsToml {
    #[serde(default)]
    keys: KeysToml,
}

impl KeyBindings {
    /// Read bindings from `<root>/settings.toml`; when the file/section/key name is missing or invalid
    /// fall back per item to the vanilla defaults (with a `warn`, not silently).
    pub fn load(paths_root: &Path) -> Self {
        let file = paths_root.join("settings.toml");
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(_) => {
                // A missing settings file is not an error -- use the vanilla defaults (the
                // `reset: false` branch of 101595: do nothing if a binding is non-empty).
                return KeyBindings::default();
            }
        };
        let parsed: SettingsToml = match toml::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                warn!(path = %file.display(), error = %e, "settings.toml is invalid; using vanilla defaults");
                return KeyBindings::default();
            }
        };
        let k = parsed.keys;
        let map =
            |list: &Option<Vec<String>>, fallback: Vec<KeyCode>, action: &str| -> Vec<KeyCode> {
                match list {
                    None => fallback,
                    Some(names) => {
                        let keys: Vec<KeyCode> =
                            names.iter().filter_map(|n| parse_key(n)).collect();
                        if keys.is_empty() {
                            warn!(
                                action,
                                "settings.toml binds no usable keys; using vanilla default"
                            );
                            fallback
                        } else {
                            keys
                        }
                    }
                }
            };
        KeyBindings {
            left: map(&k.left, vec![KeyCode::ArrowLeft], "left"),
            right: map(&k.right, vec![KeyCode::ArrowRight], "right"),
            up: map(&k.up, vec![KeyCode::ArrowUp], "up"),
            down: map(&k.down, vec![KeyCode::ArrowDown], "down"),
            jump: map(&k.jump, vec![KeyCode::KeyC], "jump"),
            dash: map(&k.dash, vec![KeyCode::KeyX], "dash"),
            grab: map(
                &k.grab,
                vec![KeyCode::KeyZ, KeyCode::KeyV, KeyCode::ShiftLeft],
                "grab",
            ),
        }
    }
}

/// `"ArrowLeft"` / `"KeyC"` / `"Escape"` ... -> `KeyCode`.
/// The `Debug` name of `KeyCode` is used as the key name (matching what the user writes in the settings file).
fn parse_key(name: &str) -> Option<KeyCode> {
    Some(match name {
        "ArrowLeft" => KeyCode::ArrowLeft,
        "ArrowRight" => KeyCode::ArrowRight,
        "ArrowUp" => KeyCode::ArrowUp,
        "ArrowDown" => KeyCode::ArrowDown,
        "KeyA" => KeyCode::KeyA,
        "KeyC" => KeyCode::KeyC,
        "KeyD" => KeyCode::KeyD,
        "KeyS" => KeyCode::KeyS,
        "KeyV" => KeyCode::KeyV,
        "KeyW" => KeyCode::KeyW,
        "KeyX" => KeyCode::KeyX,
        "KeyZ" => KeyCode::KeyZ,
        "Space" => KeyCode::Space,
        "Enter" => KeyCode::Enter,
        "Escape" => KeyCode::Escape,
        "Tab" => KeyCode::Tab,
        "Backspace" => KeyCode::Backspace,
        "ShiftLeft" => KeyCode::ShiftLeft,
        "ShiftRight" => KeyCode::ShiftRight,
        _ => {
            warn!(key = name, "unknown key name in settings.toml; ignoring it");
            return None;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Missing file -> vanilla defaults (arrow keys + C).
    #[test]
    fn missing_settings_file_falls_back_to_vanilla_defaults() {
        let root = std::env::temp_dir().join("releste-settings-missing");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let b = KeyBindings::load(&root);
        assert_eq!(b, KeyBindings::default());
    }

    /// Valid overrides take effect; a bad key name warns and is ignored; an empty list falls back to the default.
    #[test]
    fn settings_file_overrides_and_falls_back_per_action() {
        let root = std::env::temp_dir().join("releste-settings-load");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.toml"),
            "[keys]\njump = [\"Space\", \"KeyC\"]\nleft = [\"KeyA\"]\nright = [\"Nope\"]\n",
        )
        .unwrap();
        let b = KeyBindings::load(&root);
        assert_eq!(b.jump, vec![KeyCode::Space, KeyCode::KeyC]);
        assert_eq!(b.left, vec![KeyCode::KeyA]);
        assert_eq!(
            b.right,
            vec![KeyCode::ArrowRight],
            "bad key -> vanilla default"
        );
        assert_eq!(b.up, vec![KeyCode::ArrowUp], "missing -> vanilla default");
    }
}
