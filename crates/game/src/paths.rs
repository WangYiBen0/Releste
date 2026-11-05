//! Runtime path resolution.
//!
//! Everything is resolved relative to the executable (or `--root`), never a hard-coded absolute path --
//! AGENTS.md 3 / ROADMAP Appendix C.

use std::path::{Path, PathBuf};

/// A resolved resource root directory.
#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
    /// `maps/` (the dev tree falls back to `assets/maps/`).
    maps: PathBuf,
    /// `atlas/` directory (the second argument of `SpriteAtlas::load`).
    atlas: PathBuf,
    /// `plugins/`.
    plugins: PathBuf,
}

impl Paths {
    /// Resolve the root directory and locate the subdirectories.
    ///
    /// `--root` always applies unconditionally. Otherwise choose among executable directory -> repo root (dev
    /// build) -> current directory: the **first candidate that actually has maps** wins -- merely "looking
    /// like" a resource root is not enough: a build source tree in the nix store carries an empty-shell `maps/` and
    /// a tracked `plugins/`, so it looks like a root but cannot run. If none works, fall back to
    /// the first lookalike, so later loading fails with the **full path**.
    pub fn resolve(explicit: Option<&Path>) -> Self {
        if let Some(root) = explicit {
            return Self::under(root);
        }
        let mut candidates: Vec<PathBuf> = Vec::new();
        candidates.extend(Self::exe_root());
        candidates.extend(Self::dev_root());
        candidates.push(PathBuf::from("."));
        Self::pick(&candidates)
    }

    /// Pick a root from the candidates: one with actual maps wins, otherwise the "looks like a resource root" one.
    fn pick(candidates: &[PathBuf]) -> Self {
        let chosen = candidates
            .iter()
            .find(|root| has_maps(&maps_dir_under(root)))
            .or_else(|| candidates.iter().find(|root| looks_like_root(root)));
        Self::under(chosen.unwrap_or(&PathBuf::from(".")))
    }

    /// Build Paths under `root` (the concrete choices for maps/atlas/plugins).
    fn under(root: &Path) -> Self {
        let maps = maps_dir_under(root);
        let atlas = if root.join("atlas").is_dir() {
            root.join("atlas")
        } else if root.join("resources").is_dir() {
            // Appendix C: resources/<namespace>/atlas -- take the first namespace.
            first_dir(&root.join("resources"))
                .map(|ns| ns.join("atlas"))
                .unwrap_or_else(|| root.join("assets").join("atlas"))
        } else {
            root.join("assets").join("atlas")
        };
        let plugins = root.join("plugins");

        Paths {
            root: root.to_path_buf(),
            maps,
            atlas,
            plugins,
        }
    }

    /// The executable directory, returned only when it looks like a resource root.
    fn exe_root() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?.to_path_buf();
        looks_like_root(&dir).then_some(dir)
    }

    /// Source tree root (a dev build started via `cargo run`).
    fn dev_root() -> Option<PathBuf> {
        // `crates/game` -> `crates` -> repo root.
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest.parent()?.parent()?;
        looks_like_root(root).then(|| root.to_path_buf())
    }

    /// The resource root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `maps/` directory.
    pub fn maps(&self) -> &Path {
        &self.maps
    }

    /// `atlas/` directory (for parsing `.atlas` payloads).
    pub fn atlas_dir(&self) -> &Path {
        &self.atlas
    }

    /// `plugins/` directory.
    pub fn plugins(&self) -> &Path {
        &self.plugins
    }

    /// Hot-reload watch directory (which is `maps/`).
    pub fn watch_dir(&self) -> &Path {
        &self.maps
    }

    /// Full path of one map. `name` may include `.map` or omit it.
    pub fn map(&self, name: &str) -> PathBuf {
        let file = if name.ends_with(".map") {
            name.to_string()
        } else {
            format!("{name}.map")
        };
        if Path::new(&file).is_absolute() {
            PathBuf::from(file)
        } else {
            self.maps.join(file)
        }
    }

    /// Full path of one atlas descriptor (the `.atlas` matches the directory name at its level).
    pub fn atlas(&self, name: &str) -> PathBuf {
        self.atlas.join(format!("{name}.atlas"))
    }

    /// Full path of the tileset definition (same directory as the atlas, a content-pipeline product).
    pub fn tilesets(&self) -> PathBuf {
        self.atlas.join("tilesets.toml")
    }

    /// **Background** tile definitions (converted from `Graphics/BackgroundTiles.xml`).
    ///
    /// This and [`Paths::tilesets`] are two independent tables: the same id maps to different
    /// textures on each side (two independent vanilla `Autotiler`s, `pureCeleste.cs:110116-110117`).
    pub fn bg_tilesets(&self) -> PathBuf {
        self.atlas.join("bg_tilesets.toml")
    }
}

fn looks_like_root(dir: &Path) -> bool {
    ["assets", "maps", "plugins", "resources"]
        .iter()
        .any(|d| dir.join(d).is_dir())
}

/// The map directory actually usable under `root`: the release tree `maps/<namespace>/` wins, falling back to
/// the dev tree's `assets/maps/` -- whichever directory **actually has maps** is used.
fn maps_dir_under(root: &Path) -> PathBuf {
    let namespaced = root.join("maps");
    let legacy = root.join("assets").join("maps");
    if has_maps(&namespaced) {
        namespaced
    } else if has_maps(&legacy) {
        legacy
    } else if namespaced.is_dir() {
        namespaced
    } else {
        legacy
    }
}

/// Whether a directory has any `.map` (checks one level of subdirectories, enough to cover `maps/<namespace>/`).
fn has_maps(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            std::fs::read_dir(&path)
                .map(|nested| nested.flatten().any(|e| is_map(&e.path())))
                .unwrap_or(false)
        } else {
            is_map(&path)
        }
    })
}

fn is_map(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "map")
}

fn first_dir(dir: &Path) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    entries.sort();
    entries.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_path_appends_extension_once() {
        let mut p = Paths::resolve(Some(Path::new("/nonexistent-root")));
        p.maps = PathBuf::from("/maps");
        assert_eq!(p.map("0-Intro"), PathBuf::from("/maps/0-Intro.map"));
        assert_eq!(p.map("0-Intro.map"), PathBuf::from("/maps/0-Intro.map"));
    }

    #[test]
    fn absolute_map_is_not_rewritten() {
        let mut p = Paths::resolve(Some(Path::new("/nonexistent-root")));
        p.maps = PathBuf::from("/maps");
        assert_eq!(p.map("/tmp/a.map"), PathBuf::from("/tmp/a.map"));
    }

    #[test]
    fn atlas_dir_is_the_atlas_directory() {
        let p = Paths::resolve(Some(Path::new("/nonexistent-root")));
        assert!(
            p.atlas_dir().ends_with("assets/atlas"),
            "{:?}",
            p.atlas_dir()
        );
        assert!(p.atlas("Gameplay").ends_with("assets/atlas/Gameplay.atlas"));
    }

    /// When the exe/dev root "looks like" a resource root (empty-shell maps/ + tracked plugins/) but has
    /// no loadable map, it must yield to the candidate that **actually has maps** (repo root/cwd).
    ///
    /// nix run scenario: the build source tree in the store is that "lookalike" root; the old implementation
    /// picked it and reported "map does not exist", even though the repo was right in cwd.
    #[test]
    fn a_root_without_any_map_yields_to_one_that_has_maps() {
        let lookalike = std::env::temp_dir().join("releste-paths-lookalike");
        let real = std::env::temp_dir().join("releste-paths-real-root");
        let _ = std::fs::remove_dir_all(&lookalike);
        let _ = std::fs::remove_dir_all(&real);
        // The store source tree's shape: empty maps/, tracked plugins/<name>/.
        std::fs::create_dir_all(lookalike.join("maps")).unwrap();
        std::fs::create_dir_all(lookalike.join("plugins/clock")).unwrap();
        // The repo root's shape: assets/maps/ has a real map.
        std::fs::create_dir_all(real.join("assets/maps")).unwrap();
        std::fs::write(real.join("assets/maps/0-Intro.map"), b"RELMAP").unwrap();

        let picked = Paths::pick(&[lookalike.clone(), real.clone()]);
        assert_eq!(picked.root(), real, "the root with actual maps must win");
        assert!(picked.maps().starts_with(&real), "{:?}", picked.maps());

        let _ = std::fs::remove_dir_all(&lookalike);
        let _ = std::fs::remove_dir_all(&real);
    }

    /// Dev tree: `maps/` is an empty shell, so it should fall back to `assets/maps/`.
    #[test]
    fn maps_dir_falls_back_when_the_namespaced_dir_is_empty() {
        let root = std::env::temp_dir().join("releste-paths-empty-namespaced");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("maps")).unwrap();
        std::fs::create_dir_all(root.join("assets/maps")).unwrap();
        std::fs::write(root.join("assets/maps/0-Intro.map"), b"RELMAP").unwrap();

        let p = Paths::resolve(Some(&root));
        assert!(
            p.maps().ends_with("assets/maps"),
            "expected the legacy dir, got {:?}",
            p.maps()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Release tree: when `maps/<namespace>/` has maps it must beat the empty `assets/maps/`.
    #[test]
    fn maps_dir_prefers_the_namespace_layout_when_it_has_maps() {
        let root = std::env::temp_dir().join("releste-paths-namespaced");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("maps/Gameplay")).unwrap();
        std::fs::create_dir_all(root.join("assets/maps")).unwrap();
        std::fs::write(root.join("maps/Gameplay/0-Intro.map"), b"RELMAP").unwrap();

        let p = Paths::resolve(Some(&root));
        assert!(p.maps().ends_with("maps"), "{:?}", p.maps());
        assert_eq!(
            p.map("Gameplay/0-Intro"),
            root.join("maps/Gameplay/0-Intro.map")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
