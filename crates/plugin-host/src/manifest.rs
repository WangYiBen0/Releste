//! `plugin.toml` — the plugin manifest.
//!
//! A manifest is **data, not code**: it is parsed without instantiating the
//! artifact, which is what lets the host decide what to load before it ever
//! calls `dlopen` (ROADMAP Appendix C rule 7).

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// When a plugin materializes, as written in `load = "…"` (ROADMAP 3.x).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LoadMode {
    /// Load at startup. The default.
    #[default]
    Eager,
    /// Load the first time some fiber `inject`s it.
    OnDemand,
    /// Load when `maps/<ns>/` or `resources/<ns>/` is first opened.
    WhenNamespace(String),
}

impl<'de> serde::Deserialize<'de> for LoadMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let raw = String::deserialize(deserializer)?;
        match raw.as_str() {
            "eager" => Ok(Self::Eager),
            "on-demand" => Ok(Self::OnDemand),
            other => match other.strip_prefix("when-namespace:") {
                Some(ns) if !ns.is_empty() => Ok(Self::WhenNamespace(ns.to_string())),
                _ => Err(D::Error::unknown_variant(
                    other,
                    &["eager", "on-demand", "when-namespace:<ns>"],
                )),
            },
        }
    }
}

/// A parsed `plugin.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    /// Plugin name; also the directory name under `plugins/`.
    pub name: String,
    /// Plugin version, informational only.
    #[serde(default)]
    pub version: String,
    /// ABI version the artifact was built against. The host rejects a mismatch.
    pub abi: u32,
    /// Artifact file name relative to the plugin directory.
    pub entry: String,
    /// When the plugin materializes.
    #[serde(default)]
    pub load: LoadMode,
    /// Services this plugin makes available.
    #[serde(default)]
    pub provide: Vec<String>,
    /// Services it needs. Reactive: losing one unloads the fiber (D.4).
    #[serde(default)]
    pub inject: Vec<String>,
}

impl Manifest {
    /// Read and parse `plugin.toml` at `path`.
    ///
    /// Nothing in the artifact is loaded or executed.
    pub fn from_file(path: &Path) -> Result<Self, ManifestError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ManifestError::Read(path.display().to_string(), e))?;
        toml::from_str(&text).map_err(|source| ManifestError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    /// Parse manifest text that is not backed by a file.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let m: Manifest = toml::from_str(text).map_err(|source| ManifestError::Parse {
            path: "<inline>".to_string(),
            source,
        })?;
        if m.name.is_empty() {
            return Err(ManifestError::EmptyName);
        }
        if m.entry.is_empty() {
            return Err(ManifestError::EmptyEntry);
        }
        Ok(m)
    }

    /// Absolute path of the artifact named by [`Manifest::entry`].
    pub fn entry_path(&self, dir: &Path) -> PathBuf {
        dir.join(&self.entry)
    }
}

/// Why a manifest could not be used.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The file could not be read.
    #[error("cannot read manifest {0}: {1}")]
    Read(String, #[source] std::io::Error),

    /// The file is not a valid `plugin.toml`.
    #[error("invalid manifest {path}: {source}")]
    Parse {
        /// Where the manifest came from, for the error message.
        path: String,
        /// The underlying TOML error.
        #[source]
        source: toml::de::Error,
    },

    /// `name` was empty.
    #[error("manifest has an empty `name`")]
    EmptyName,

    /// `entry` was empty.
    #[error("manifest has an empty `entry`")]
    EmptyEntry,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_complete_manifest() {
        let text = r#"
name    = "player"
version = "0.1.0"
abi     = 1
entry   = "player.so"
load    = "eager"
provide = ["player", "spawn"]
inject  = ["render", "audio"]
"#;
        let m = Manifest::parse(text).expect("parses");
        assert_eq!(m.name, "player");
        assert_eq!(m.abi, 1);
        assert_eq!(m.entry, "player.so");
        assert_eq!(m.load, LoadMode::Eager);
        assert_eq!(m.provide, vec!["player", "spawn"]);
        assert_eq!(m.inject, vec!["render", "audio"]);
    }

    #[test]
    fn optional_fields_default() {
        let m = Manifest::parse("name = \"x\"\nabi = 1\nentry = \"x.so\"\n").expect("parses");
        assert_eq!(m.version, "");
        assert_eq!(m.load, LoadMode::Eager);
        assert!(m.provide.is_empty());
        assert!(m.inject.is_empty());
    }

    #[test]
    fn rejects_missing_required_field() {
        assert!(Manifest::parse("name = \"x\"\nabi = 1\n").is_err());
    }

    #[test]
    fn rejects_empty_name() {
        assert!(matches!(
            Manifest::parse("name = \"\"\nabi = 1\nentry = \"x.so\"\n"),
            Err(ManifestError::EmptyName)
        ));
    }
}
