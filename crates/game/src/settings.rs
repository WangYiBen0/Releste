//! User settings: key bindings and the player skin (the counterpart of vanilla
//! `Settings.Instance`).
//!
//! Vanilla stores bindings in the settings under the user directory, and
//! `SetDefaultKeyboardControls(reset)` (`Settings.cs:161`, called from `AfterLoad`
//! at `Settings.cs:151`) falls back to the defaults when a binding is missing: arrow
//! keys move, C jumps, X dashes, Z/V/LeftShift grabs, Enter pauses. This engine
//! persists them in TOML (the stack spec forbids JSON): a missing section or an
//! unknown key name falls back per action and warns; it never panics
//! (AGENTS.md 15.8/15.9).
//!
//! # Layers
//!
//! Settings are read as a stack, the later layer winning field by field:
//!
//! 1. **Compiled-in default**, `crates/game/config.toml`, embedded with
//!    `include_str!`. This is the only place the shipped values live: the Rust
//!    sources do not hardcode a key or a skin name, so changing the defaults is a
//!    data change. `--write-config` seeds a user file from this layer.
//! 2. **User configuration**, the file in the user configuration directory:
//!
//!    | platform | path |
//!    |---|---|
//!    | Linux / BSD | `$XDG_CONFIG_HOME/releste/config.toml` (fallback `~/.config/releste/config.toml`) |
//!    | macOS | `~/Library/Application Support/releste/config.toml` |
//!    | Windows | `%APPDATA%\releste\config.toml` |
//!
//! 3. **Per-install override**, `<root>/settings.toml` next to the asset root. It
//!    beats the user configuration on purpose (a portable install carries its own
//!    settings), and nothing ever writes it: an install directory may be read-only.
//!
//! `--config <PATH>` or `RELESTE_CONFIG_FILE` replaces layers 2 and 3 with that one
//! file (still on top of the compiled-in default), for tests and for tooling.
//!
//! Every layer is re-read on change while the game runs, so editing the skin name
//! swaps the reskin without a restart (see `crate::app`).
//!
//! Key names are the `Debug` name of `winit::keyboard::KeyCode` (e.g. `"KeyC"`,
//! `"ArrowLeft"`): physical keys (scancode semantics), matching vanilla
//! `TranslateKeys` (101708)'s "bind by physical position".

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;
use tracing::{error, info, warn};
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
/// The shipped default, embedded at build time. Editing this file changes the
/// defaults for every install; the Rust sources carry no key or skin values.
pub const BUILTIN_TOML: &str = include_str!("../config.toml");
/// Label used for the compiled-in layer in logs and diagnostics.
pub const BUILTIN_NAME: &str = "<builtin config.toml>";

/// Where the config lives, and whether that path was chosen explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLocation {
    /// Canonical path: the write target, and the user-level read source.
    pub path: PathBuf,
    /// True when `--config` or `RELESTE_CONFIG_FILE` picked the path, in which case
    /// `<root>/settings.toml` and the user file are not consulted separately.
    pub explicit: bool,
}

impl ConfigLocation {
    /// The files read on top of the compiled-in default, lowest precedence first.
    ///
    /// Missing files are not an error (a layer that does not exist contributes
    /// nothing), so the caller filters by existence.
    pub fn layers(&self, root: &Path) -> Vec<PathBuf> {
        if self.explicit {
            return vec![self.path.clone()];
        }
        let mut files = vec![self.path.clone()];
        let local = root.join(LOCAL_FILE_NAME);
        if local != self.path {
            files.push(local);
        }
        files
    }

    /// The highest-precedence file for `root`, for logging: `<root>/settings.toml`
    /// when it exists, the canonical path otherwise, an explicit choice as-is.
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
/// consulted here -- see [`ConfigLocation::layers`].
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

impl KeyBindings {
    fn empty() -> Self {
        KeyBindings {
            left: Vec::new(),
            right: Vec::new(),
            up: Vec::new(),
            down: Vec::new(),
            jump: Vec::new(),
            dash: Vec::new(),
            grab: Vec::new(),
        }
    }
}

impl Default for KeyBindings {
    /// The bindings shipped in `config.toml` (vanilla defaults, 101602-101651).
    fn default() -> Self {
        builtin().keys.clone()
    }
}

/// Everything the settings file configures.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    /// Key bindings for the current layer stack.
    pub keys: KeyBindings,
    /// Player reskin namespace installed by `smh2releste`; empty means vanilla.
    /// Matches SkinModHelper's `Character_ID`, which prefixes every player texture
    /// (`SkinModHelperModule.cs:205-222`).
    pub skin: String,
}

impl Settings {
    fn empty() -> Self {
        Settings {
            keys: KeyBindings::empty(),
            skin: String::new(),
        }
    }

    /// Read the settings for `location`, merging every layer over the compiled-in
    /// default (`ConfigLocation::layers`). Missing files are skipped silently, an
    /// unreadable or invalid file warns and is skipped, and a field that yields no
    /// usable value keeps the value from the layer below.
    pub fn load_resolved(location: &ConfigLocation, root: &Path) -> Self {
        let mut settings = builtin().clone();
        for file in location.layers(root) {
            if !file.exists() {
                continue;
            }
            if file != location.path && !location.explicit {
                info!(
                    file = %file.display(),
                    config = %location.path.display(),
                    "using the local settings.toml, which takes precedence over the user config"
                );
            }
            settings.merge_file(&file);
        }
        settings
    }

    /// Read one file on top of the compiled-in default.
    ///
    /// Test-only helper: the game itself always goes through
    /// [`Settings::load_resolved`], which also covers an explicit `--config` path.
    #[cfg(test)]
    pub fn load(file: &Path) -> Self {
        let mut settings = builtin().clone();
        settings.merge_file(file);
        settings
    }

    fn merge_file(&mut self, file: &Path) {
        let text = match std::fs::read_to_string(file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                warn!(path = %file.display(), error = %e, "config file is unreadable; ignoring this layer");
                return;
            }
        };
        match toml::from_str::<SettingsToml>(&text) {
            Ok(layer) => self.apply_layer(&layer, file),
            Err(e) => {
                warn!(path = %file.display(), error = %e, "config file is invalid; ignoring this layer");
            }
        }
    }

    /// Overlay one parsed layer: a present field wins when it yields at least one
    /// usable key (or a non-empty skin name), otherwise the layer below is kept.
    fn apply_layer(&mut self, layer: &SettingsToml, source: &Path) {
        apply_action(&layer.keys.left, &mut self.keys.left, "left", source);
        apply_action(&layer.keys.right, &mut self.keys.right, "right", source);
        apply_action(&layer.keys.up, &mut self.keys.up, "up", source);
        apply_action(&layer.keys.down, &mut self.keys.down, "down", source);
        apply_action(&layer.keys.jump, &mut self.keys.jump, "jump", source);
        apply_action(&layer.keys.dash, &mut self.keys.dash, "dash", source);
        apply_action(&layer.keys.grab, &mut self.keys.grab, "grab", source);
        // An empty name is not an override: it means "this layer says nothing about
        // the skin", so a local file that only touches keys keeps the user's skin.
        if let Some(name) = &layer.skin.name {
            let name = name.trim();
            if !name.is_empty() {
                self.skin = name.to_string();
            }
        }
    }

    /// Write these settings to `file`, creating the parent directory if needed.
    ///
    /// This is the write half of the config surface: a rebinding UI saves here, and
    /// `releste --write-config` seeds a file. The output is deterministic (fixed key
    /// order, no maps), so re-saving unchanged settings produces identical bytes,
    /// which keeps the hot-reload watcher quiet.
    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        if let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(file, self.to_toml())
    }

    /// Render the config file body for these settings.
    pub fn to_toml(&self) -> String {
        fn names(keys: &[KeyCode]) -> String {
            keys.iter()
                .map(|k| format!("\"{k:?}\""))
                .collect::<Vec<_>>()
                .join(", ")
        }
        fn quote(value: &str) -> String {
            let mut out = String::with_capacity(value.len() + 2);
            out.push('"');
            for ch in value.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    other => out.push(other),
                }
            }
            out.push('"');
            out
        }
        format!(
            "# Releste configuration.\n\
             #\n\
             # Key names are winit `KeyCode` names and bind by physical position\n\
             # (vanilla `TranslateKeys`, pureCeleste.cs:101708). An action may list\n\
             # several keys; the first match wins.\n\
             #\n\
             # This file is one layer of a stack: the compiled-in default is below\n\
             # it, and <root>/settings.toml wins over it. Editing any of them while\n\
             # the game runs takes effect without a restart; delete a line to fall\n\
             # back to the value below.\n\
             \n\
             [keys]\n\
             left = [{}]\n\
             right = [{}]\n\
             up = [{}]\n\
             down = [{}]\n\
             jump = [{}]\n\
             dash = [{}]\n\
             grab = [{}]\n\
             \n\
             # Player reskin namespace installed by `smh2releste`; empty means vanilla.\n\
             [skin]\n\
             name = {}\n",
            names(&self.keys.left),
            names(&self.keys.right),
            names(&self.keys.up),
            names(&self.keys.down),
            names(&self.keys.jump),
            names(&self.keys.dash),
            names(&self.keys.grab),
            quote(&self.skin),
        )
    }
}

/// The compiled-in layer, parsed once.
pub fn builtin() -> &'static Settings {
    static BUILTIN: OnceLock<Settings> = OnceLock::new();
    BUILTIN.get_or_init(|| {
        let layer: SettingsToml = match toml::from_str(BUILTIN_TOML) {
            Ok(layer) => layer,
            Err(e) => {
                error!(error = %e, "the compiled-in config.toml is invalid; every binding will be empty");
                SettingsToml::default()
            }
        };
        let mut settings = Settings::empty();
        settings.apply_layer(&layer, Path::new(BUILTIN_NAME));
        settings
    })
}

fn apply_action(list: &Option<Vec<String>>, slot: &mut Vec<KeyCode>, action: &str, source: &Path) {
    let Some(names) = list else {
        return;
    };
    let keys: Vec<KeyCode> = names.iter().filter_map(|n| parse_key(n)).collect();
    if keys.is_empty() {
        warn!(
            action,
            source = %source.display(),
            "config layer binds no usable keys; keeping the value from the layer below"
        );
        return;
    }
    *slot = keys;
}

/// The serializable shape of one settings layer.
#[derive(Debug, Default, Clone, Deserialize)]
struct KeysToml {
    left: Option<Vec<String>>,
    right: Option<Vec<String>>,
    up: Option<Vec<String>>,
    down: Option<Vec<String>>,
    jump: Option<Vec<String>>,
    dash: Option<Vec<String>>,
    grab: Option<Vec<String>>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct SkinToml {
    name: Option<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct SettingsToml {
    #[serde(default)]
    keys: KeysToml,
    #[serde(default)]
    skin: SkinToml,
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

    /// The shipped default carries the vanilla bindings and no skin, and it parses
    /// without warnings -- the Rust sources must not hardcode these values.
    #[test]
    fn builtin_config_matches_the_vanilla_defaults() {
        let builtin = builtin();
        assert_eq!(builtin.keys.left, vec![KeyCode::ArrowLeft]);
        assert_eq!(builtin.keys.right, vec![KeyCode::ArrowRight]);
        assert_eq!(builtin.keys.up, vec![KeyCode::ArrowUp]);
        assert_eq!(builtin.keys.down, vec![KeyCode::ArrowDown]);
        assert_eq!(builtin.keys.jump, vec![KeyCode::KeyC]);
        assert_eq!(builtin.keys.dash, vec![KeyCode::KeyX]);
        assert_eq!(
            builtin.keys.grab,
            vec![KeyCode::KeyZ, KeyCode::KeyV, KeyCode::ShiftLeft]
        );
        assert_eq!(builtin.skin, "", "vanilla has no reskin");
        // `Default` is the compiled-in layer, not a second copy of the values.
        assert_eq!(KeyBindings::default(), builtin.keys);
    }

    /// Missing file -> the compiled-in default.
    #[test]
    fn missing_file_falls_back_to_the_builtin_default() {
        let root = tmp("missing");
        assert_eq!(Settings::load(&root.join(CONFIG_FILE_NAME)), *builtin());
    }

    /// A valid override wins; a bad key name keeps the layer below; a missing
    /// action keeps the layer below.
    #[test]
    fn a_layer_overrides_only_the_fields_it_sets() {
        let root = tmp("load");
        std::fs::write(
            root.join(CONFIG_FILE_NAME),
            "[keys]\njump = [\"Space\", \"KeyC\"]\nleft = [\"KeyA\"]\nright = [\"Nope\"]\n",
        )
        .unwrap();
        let settings = Settings::load(&root.join(CONFIG_FILE_NAME));
        assert_eq!(settings.keys.jump, vec![KeyCode::Space, KeyCode::KeyC]);
        assert_eq!(settings.keys.left, vec![KeyCode::KeyA]);
        assert_eq!(
            settings.keys.right,
            vec![KeyCode::ArrowRight],
            "bad key -> layer below"
        );
        assert_eq!(
            settings.keys.up,
            vec![KeyCode::ArrowUp],
            "missing -> layer below"
        );
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

    /// `<root>/settings.toml` beats the user config field by field; an explicit
    /// `--config` replaces both of them.
    #[test]
    fn the_local_file_beats_the_user_file_and_explicit_replaces_both() {
        let root = tmp("precedence");
        let user = root.join("user").join(CONFIG_FILE_NAME);
        let local = root.join(LOCAL_FILE_NAME);

        let location = ConfigLocation {
            path: user.clone(),
            explicit: false,
        };
        assert_eq!(location.layers(&root), vec![user.clone(), local.clone()]);

        std::fs::create_dir_all(user.parent().expect("user config parent")).unwrap();
        std::fs::write(
            &user,
            "[keys]\njump = [\"Space\"]\n[skin]\nname = \"Foxeline\"\n",
        )
        .unwrap();
        let user_only = Settings::load_resolved(&location, &root);
        assert_eq!(user_only.keys.jump, vec![KeyCode::Space]);
        assert_eq!(user_only.skin, "Foxeline");

        // The local file only sets one field: the rest survives from the user file.
        std::fs::write(&local, "[keys]\njump = [\"KeyW\"]\n").unwrap();
        let merged = Settings::load_resolved(&location, &root);
        assert_eq!(merged.keys.jump, vec![KeyCode::KeyW], "local beats user");
        assert_eq!(merged.skin, "Foxeline", "untouched field falls through");

        // An explicit path replaces the user and local layers.
        let explicit = ConfigLocation {
            path: user.clone(),
            explicit: true,
        };
        assert_eq!(explicit.layers(&root), vec![user.clone()]);
        let only = Settings::load_resolved(&explicit, &root);
        assert_eq!(only.keys.jump, vec![KeyCode::Space], "local is ignored");
    }

    /// A high layer with no usable value must not wipe the layer below.
    #[test]
    fn a_broken_high_layer_keeps_the_lower_value() {
        let root = tmp("broken");
        let user = root.join(CONFIG_FILE_NAME);
        let local = root.join(LOCAL_FILE_NAME);
        std::fs::write(
            &user,
            "[keys]\njump = [\"Space\"]\n[skin]\nname = \"Foxeline\"\n",
        )
        .unwrap();
        std::fs::write(&local, "[keys]\njump = [\"Nope\"]\n[skin]\nname = \"\"\n").unwrap();
        let location = ConfigLocation {
            path: user,
            explicit: false,
        };
        let merged = Settings::load_resolved(&location, &root);
        assert_eq!(merged.keys.jump, vec![KeyCode::Space]);
        assert_eq!(merged.skin, "Foxeline", "an empty name is not an override");
    }

    /// Saving creates missing directories and round-trips through `load`.
    #[test]
    fn save_then_load_round_trips() {
        let root = tmp("roundtrip");
        let file = root.join("nested").join(CONFIG_FILE_NAME);
        let settings = Settings {
            keys: KeyBindings {
                left: vec![KeyCode::KeyA],
                right: vec![KeyCode::KeyD],
                up: vec![KeyCode::KeyW],
                down: vec![KeyCode::KeyS],
                jump: vec![KeyCode::Space],
                dash: vec![KeyCode::ShiftRight],
                grab: vec![KeyCode::KeyZ, KeyCode::KeyV],
            },
            skin: "Foxeline".to_string(),
        };
        settings.save(&file).expect("save");
        assert_eq!(Settings::load(&file), settings);
        // Deterministic output: saving again writes the same bytes.
        let first = std::fs::read_to_string(&file).unwrap();
        settings.save(&file).expect("save again");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), first);
        assert!(first.contains("[skin]"), "the skin section must persist");
    }
}
