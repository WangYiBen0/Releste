//! User settings: key bindings (the counterpart of vanilla `Settings.Instance`).
//!
//! Vanilla stores bindings in the settings under the user directory, and
//! `SetDefaultKeyboardControls(reset)` (`pureCeleste.cs:101595-101706`) falls back
//! to the defaults when a binding is missing: arrow keys move, C jumps, X dashes,
//! Z/V/LeftShift grabs, Enter pauses. This engine persists them in TOML (the stack
//! spec forbids JSON): a missing section or an unknown key name falls back to the
//! vanilla default for that action and warns; it never panics (AGENTS.md 15.8/15.9).
//!
//! # Where the file lives
//!
//! Reading checks these in order, first hit wins:
//!
//! 1. `--config <PATH>` on the command line,
//! 2. the `RELESTE_CONFIG_FILE` environment variable,
//! 3. `<root>/settings.toml` -- a portable or per-install override next to the
//!    asset root; it beats the user configuration on purpose,
//! 4. the user configuration directory:
//!
//!    | platform | path |
//!    |---|---|
//!    | Linux / BSD | `$XDG_CONFIG_HOME/releste/config.toml` (fallback `~/.config/releste/config.toml`) |
//!    | macOS | `~/Library/Application Support/releste/config.toml` |
//!    | Windows | `%APPDATA%\releste\config.toml` |
//!
//! 5. `<root>/config.toml` when no home directory is known at all (portable
//!    install, or a test).
//!
//! Writing (a future rebinding menu, or `--config`-driven tooling) always uses the
//! path from 1/2/4/5 -- never `<root>/settings.toml`, which may live in a
//! read-only install directory.
//!
//! Key names are the `Debug` name of `winit::keyboard::KeyCode` (e.g. `"KeyC"`,
//! `"ArrowLeft"`): physical keys (scancode semantics), matching vanilla
//! `TranslateKeys` (101708)'s "bind by physical position".

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{info, warn};
use winit::keyboard::KeyCode;

/// File name inside the user configuration directory.
pub const CONFIG_FILE_NAME: &str = "config.toml";
/// Sub-directory inside the user configuration directory.
pub const CONFIG_DIR_NAME: &str = "releste";
/// Environment variable that overrides the config file location.
pub const CONFIG_ENV: &str = "RELESTE_CONFIG_FILE";
/// Per-install override next to the asset root. Read-only as far as the engine is
/// concerned: it takes precedence over the user configuration, but nothing writes
/// it (an install directory may be read-only).
pub const LOCAL_FILE_NAME: &str = "settings.toml";

/// Where the config lives, and whether that path was chosen explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLocation {
    /// Canonical path: the write target, and the read source unless a local
    /// override exists.
    pub path: PathBuf,
    /// True when `--config` or `RELESTE_CONFIG_FILE` picked the path, in which case
    /// `<root>/settings.toml` is ignored.
    pub explicit: bool,
}

impl ConfigLocation {
    /// The file to read: an explicit choice as-is, otherwise `<root>/settings.toml`
    /// when that exists, otherwise the canonical path.
    pub fn read_path(&self, root: &Path) -> PathBuf {
        if self.explicit {
            return self.path.clone();
        }
        let local = root.join(LOCAL_FILE_NAME);
        if local.exists() {
            local
        } else {
            self.path.clone()
        }
    }
}

/// Resolve the canonical config path.
///
/// `cli` is the `--config` value, `env` the `RELESTE_CONFIG_FILE` value, `xdg`
/// `$XDG_CONFIG_HOME`, `home` `$HOME` (or `%APPDATA%` on Windows), and `root` the
/// asset root used as a last resort. Everything is passed in so the precedence is
/// testable without touching the process environment. `<root>/settings.toml` is not
/// consulted here -- see [`ConfigLocation::read_path`].
pub fn resolve_config(
    cli: Option<&Path>,
    env: Option<&OsStr>,
    xdg: Option<&OsStr>,
    home: Option<&OsStr>,
    root: &Path,
) -> ConfigLocation {
    if let Some(path) = cli.filter(|p| !p.as_os_str().is_empty()) {
        return ConfigLocation {
            path: path.to_path_buf(),
            explicit: true,
        };
    }
    if let Some(value) = env.filter(|v| !v.is_empty()) {
        return ConfigLocation {
            path: PathBuf::from(value),
            explicit: true,
        };
    }
    if let Some(base) = xdg.filter(|v| !v.is_empty()) {
        return ConfigLocation {
            path: Path::new(base).join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME),
            explicit: false,
        };
    }
    if let Some(home) = home.filter(|v| !v.is_empty()) {
        let home = Path::new(home);
        let path = if cfg!(target_os = "windows") {
            home.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME)
        } else if cfg!(target_os = "macos") {
            home.join("Library")
                .join("Application Support")
                .join(CONFIG_DIR_NAME)
                .join(CONFIG_FILE_NAME)
        } else {
            home.join(".config")
                .join(CONFIG_DIR_NAME)
                .join(CONFIG_FILE_NAME)
        };
        return ConfigLocation {
            path,
            explicit: false,
        };
    }
    ConfigLocation {
        path: root.join(CONFIG_FILE_NAME),
        explicit: false,
    }
}

/// [`resolve_config`] fed from the real process environment and `--config`.
pub fn config_location(cli: Option<&Path>, root: &Path) -> ConfigLocation {
    let env = std::env::var_os(CONFIG_ENV);
    let xdg = std::env::var_os("XDG_CONFIG_HOME");
    let home = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA")
    } else {
        std::env::var_os("HOME")
    };
    resolve_config(cli, env.as_deref(), xdg.as_deref(), home.as_deref(), root)
}

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
    /// Read bindings for `location`, honouring the `<root>/settings.toml` override.
    ///
    /// A missing file is not an error: the vanilla defaults apply (the `reset: false`
    /// branch of 101595). A missing section, or a key name that does not parse, falls
    /// back per action with a `warn` -- never silently, never a panic.
    pub fn load_resolved(location: &ConfigLocation, root: &Path) -> Self {
        let file = location.read_path(root);
        if file != location.path {
            info!(
                file = %file.display(),
                config = %location.path.display(),
                "using the local settings.toml, which takes precedence over the user config"
            );
        }
        Self::load(&file)
    }

    /// Read bindings from one file. See [`KeyBindings::load_resolved`] for the
    /// precedence rules.
    pub fn load(file: &Path) -> Self {
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(_) => {
                // A missing config file is not an error -- use the vanilla defaults (the
                // `reset: false` branch of 101595: do nothing if a binding is non-empty).
                return KeyBindings::default();
            }
        };
        let parsed: SettingsToml = match toml::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                warn!(path = %file.display(), error = %e, "config file is invalid; using vanilla defaults");
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
                                "config file binds no usable keys; using the vanilla default"
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

    /// Write these bindings to `file`, creating the parent directory if needed.
    ///
    /// This is the write half of the config surface: a rebinding UI saves here, and
    /// `releste --config <path>`-driven scripts can seed a file. The output is
    /// deterministic (fixed key order, no maps), so re-saving an unchanged config
    /// produces identical bytes.
    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(file, self.to_toml())
    }

    /// Render the config file body for these bindings.
    pub fn to_toml(&self) -> String {
        fn names(keys: &[KeyCode]) -> String {
            keys.iter()
                .map(|k| format!("\"{k:?}\""))
                .collect::<Vec<_>>()
                .join(", ")
        }
        format!(
            "# Releste configuration.\n\
             #\n\
             # Key names are winit `KeyCode` names and bind by physical position\n\
             # (vanilla `TranslateKeys`, pureCeleste.cs:101708). An action may list\n\
             # several keys; the first match wins.\n\
             #\n\
             # Reload by restarting the game. Delete a line to fall back to that\n\
             # action's vanilla default.\n\
             \n\
             [keys]\n\
             left = [{}]\n\
             right = [{}]\n\
             up = [{}]\n\
             down = [{}]\n\
             jump = [{}]\n\
             dash = [{}]\n\
             grab = [{}]\n",
            names(&self.left),
            names(&self.right),
            names(&self.up),
            names(&self.down),
            names(&self.jump),
            names(&self.dash),
            names(&self.grab),
        )
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
            warn!(
                key = name,
                "unknown key name in the config file; ignoring it"
            );
            return None;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("releste-settings-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Missing file -> vanilla defaults (arrow keys + C).
    #[test]
    fn missing_file_falls_back_to_vanilla_defaults() {
        let root = tmp("missing");
        let b = KeyBindings::load(&root.join(CONFIG_FILE_NAME));
        assert_eq!(b, KeyBindings::default());
    }

    /// Valid overrides take effect; a bad key name warns and is ignored; a missing
    /// action falls back to its own vanilla default.
    #[test]
    fn config_overrides_and_falls_back_per_action() {
        let root = tmp("load");
        std::fs::write(
            root.join(CONFIG_FILE_NAME),
            "[keys]\njump = [\"Space\", \"KeyC\"]\nleft = [\"KeyA\"]\nright = [\"Nope\"]\n",
        )
        .unwrap();
        let b = KeyBindings::load(&root.join(CONFIG_FILE_NAME));
        assert_eq!(b.jump, vec![KeyCode::Space, KeyCode::KeyC]);
        assert_eq!(b.left, vec![KeyCode::KeyA]);
        assert_eq!(
            b.right,
            vec![KeyCode::ArrowRight],
            "bad key -> vanilla default"
        );
        assert_eq!(b.up, vec![KeyCode::ArrowUp], "missing -> vanilla default");
    }

    /// Canonical path precedence: `--config` > env > `XDG_CONFIG_HOME` > home > root.
    #[test]
    fn canonical_path_precedence() {
        let root = Path::new("/assets");
        let cli = Path::new("/cli/config.toml");
        let env = OsStr::new("/env/config.toml");
        let xdg = OsStr::new("/xdg");
        let home = OsStr::new("/home/u");

        let loc = resolve_config(Some(cli), Some(env), Some(xdg), Some(home), root);
        assert_eq!(loc.path, cli, "--config wins");
        assert!(loc.explicit, "--config is explicit");

        let loc = resolve_config(None, Some(env), Some(xdg), Some(home), root);
        assert_eq!(loc.path, Path::new("/env/config.toml"), "env wins over XDG");
        assert!(loc.explicit, "the env var is explicit");

        let loc = resolve_config(None, None, Some(xdg), Some(home), root);
        assert_eq!(loc.path, Path::new("/xdg/releste/config.toml"));
        assert!(!loc.explicit);

        let loc = resolve_config(None, None, None, Some(home), root);
        assert_eq!(loc.path, Path::new("/home/u/.config/releste/config.toml"));

        let loc = resolve_config(None, None, None, None, root);
        assert_eq!(loc.path, Path::new("/assets/config.toml"));

        // An exported-but-empty variable must behave as unset.
        let loc = resolve_config(
            Some(Path::new("")),
            Some(OsStr::new("")),
            Some(xdg),
            Some(home),
            root,
        );
        assert_eq!(loc.path, Path::new("/xdg/releste/config.toml"));
        assert!(!loc.explicit);
    }

    /// `<root>/settings.toml` beats the user config, but an explicit `--config` or
    /// `RELESTE_CONFIG_FILE` is used exactly as given.
    #[test]
    fn local_settings_toml_takes_precedence_over_the_user_config() {
        let root = tmp("precedence");
        let user = root.join("user").join(CONFIG_FILE_NAME);

        let default = ConfigLocation {
            path: user.clone(),
            explicit: false,
        };
        assert_eq!(
            default.read_path(&root),
            user,
            "no local file -> user config"
        );

        let local = root.join(LOCAL_FILE_NAME);
        std::fs::write(&local, "[keys]\njump = [\"Space\"]\n").unwrap();
        assert_eq!(default.read_path(&root), local, "local file wins");

        let explicit = ConfigLocation {
            path: user.clone(),
            explicit: true,
        };
        assert_eq!(
            explicit.read_path(&root),
            user,
            "--config/RELESTE_CONFIG_FILE ignore the local file"
        );

        // And loading through the location reads the local file, so the binding wins.
        let bindings = KeyBindings::load_resolved(&default, &root);
        assert_eq!(bindings.jump, vec![KeyCode::Space]);
    }

    /// Saving creates missing directories and round-trips through `load`.
    #[test]
    fn save_then_load_round_trips() {
        let root = tmp("roundtrip");
        let file = root.join("nested").join(CONFIG_FILE_NAME);
        let bindings = KeyBindings {
            left: vec![KeyCode::KeyA],
            right: vec![KeyCode::KeyD],
            up: vec![KeyCode::KeyW],
            down: vec![KeyCode::KeyS],
            jump: vec![KeyCode::Space],
            dash: vec![KeyCode::ShiftRight],
            grab: vec![KeyCode::KeyZ, KeyCode::KeyV],
        };
        bindings.save(&file).expect("save");
        assert_eq!(KeyBindings::load(&file), bindings);
        // Deterministic output: saving again writes the same bytes.
        let first = std::fs::read_to_string(&file).unwrap();
        bindings.save(&file).expect("save again");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), first);
    }
}
