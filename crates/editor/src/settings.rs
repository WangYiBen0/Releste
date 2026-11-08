//! Persisted editor settings: fallback font choice and UI scale.
//!
//! The file lives at `$XDG_CONFIG_HOME/releste/editor.toml` (or
//! `$HOME/.config/releste/editor.toml`). Environment variables documented in
//! [`crate::fonts`] override the stored values so a one-off launch can always
//! bypass the saved configuration. Loading never fails hard: a missing file is
//! the default, a broken file falls back to defaults with a warning.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Root of the persisted settings document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Fallback font selection.
    #[serde(default)]
    pub font: FontSection,
    /// Interface preferences.
    #[serde(default)]
    pub ui: UiSection,
}

/// The chosen fallback font. `path = None` means auto-detect.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FontSection {
    /// Absolute path of the font file, when the user picked one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Face index inside a collection (`.ttc`).
    #[serde(default)]
    pub index: u32,
}

/// Interface preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiSection {
    /// Global interface zoom factor.
    #[serde(default = "default_ui_scale")]
    pub scale: f32,
}

fn default_ui_scale() -> f32 {
    1.0
}

impl Default for UiSection {
    fn default() -> Self {
        Self { scale: 1.0 }
    }
}

impl Settings {
    /// Loads the settings file, falling back to defaults (plus environment
    /// overrides) when it is missing or unreadable.
    pub fn load() -> Self {
        let mut settings = match config_path() {
            Some(path) => match std::fs::read_to_string(&path) {
                Ok(text) => match toml::from_str::<Settings>(&text) {
                    Ok(parsed) => parsed,
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "broken settings file; using defaults");
                        Settings::default()
                    }
                },
                Err(_) => Settings::default(),
            },
            None => Settings::default(),
        };
        settings.apply_env_overrides();
        settings.ui.scale = settings.ui.scale.clamp(0.75, 2.5);
        settings
    }

    /// Writes the settings to the config file, returning its path.
    pub fn save(&self) -> Result<PathBuf, String> {
        let path = config_path()
            .ok_or_else(|| "no config directory (set HOME or XDG_CONFIG_HOME)".to_string())?;
        let body = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, body).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// The runtime font configuration implied by these settings.
    pub fn font_config(&self) -> crate::fonts::FontConfig {
        crate::fonts::FontConfig {
            path: self.font.path.as_deref().map(PathBuf::from),
            index: self.font.index,
            scale: crate::fonts::DEFAULT_SCALE,
            y_offset: 0.0,
        }
    }

    /// Environment variables win over the stored file (one-off overrides).
    fn apply_env_overrides(&mut self) {
        if let Ok(raw) = std::env::var(crate::fonts::FONT_ENV) {
            let path = PathBuf::from(raw.trim());
            if path.is_file() {
                self.font.path = Some(path.to_string_lossy().into_owned());
            } else {
                tracing::warn!(path = %raw, "RELESTE_UI_FONT does not name a readable file");
            }
        }
        if let Ok(raw) = std::env::var(crate::fonts::FONT_INDEX_ENV) {
            if let Ok(index) = raw.trim().parse::<u32>() {
                self.font.index = index;
            }
        }
    }
}

/// Locates the settings file; `None` when no home directory is known.
pub fn config_path() -> Option<PathBuf> {
    let base = match std::env::var("XDG_CONFIG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => {
            let home = std::env::var("HOME").ok()?;
            if home.is_empty() {
                return None;
            }
            PathBuf::from(home).join(".config")
        }
    };
    Some(base.join("releste").join("editor.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_auto_detect_at_scale_one() {
        let settings = Settings::default();
        assert!(settings.font.path.is_none());
        assert_eq!(settings.font.index, 0);
        assert!((settings.ui.scale - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn missing_scale_field_defaults_to_one() {
        let parsed: Settings = toml::from_str(
            "[font]
index = 2
",
        )
        .expect("parse");
        assert_eq!(parsed.font.index, 2);
        assert!(parsed.font.path.is_none());
        assert!((parsed.ui.scale - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn round_trips_through_toml() {
        let mut settings = Settings::default();
        settings.font.path = Some("/fonts/SourceHanSans.ttc".into());
        settings.font.index = 17;
        settings.ui.scale = 1.15;
        let text = toml::to_string_pretty(&settings).expect("serialize");
        let back: Settings = toml::from_str(&text).expect("deserialize");
        assert_eq!(back.font.path.as_deref(), Some("/fonts/SourceHanSans.ttc"));
        assert_eq!(back.font.index, 17);
        assert!((back.ui.scale - 1.15).abs() < f32::EPSILON);
    }

    #[test]
    fn broken_file_falls_back_to_defaults_instead_of_panicking() {
        let parsed: Result<Settings, _> = toml::from_str("this is not toml <<<");
        assert!(parsed.is_err());
        let settings = Settings::default();
        assert!(settings.font.path.is_none());
    }
}
