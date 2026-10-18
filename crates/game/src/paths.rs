//! 运行期路径解析。
//!
//! 一切都相对可执行文件（或 `--root`）解析，绝不写死绝对路径 ——
//! AGENTS.md §3 / ROADMAP Appendix C。

use std::path::{Path, PathBuf};

/// 已解析的资源根目录。
#[derive(Debug, Clone)]
pub struct Paths {
    root: PathBuf,
    /// `maps/`（开发树回落到 `assets/maps/`）。
    maps: PathBuf,
    /// `atlas/` 目录（`SpriteAtlas::load` 的第二个参数）。
    atlas: PathBuf,
    /// `plugins/`。
    plugins: PathBuf,
}

impl Paths {
    /// 解析根目录并定位子目录。
    ///
    /// `--root` 永远无条件生效。否则按 可执行文件目录 → 仓库根（开发
    /// 构建）→ 当前目录 挑选：**第一个真有地图的**候选赢 —— 光"长得
    /// 像"资源根不够：nix store 里的构建源码树带着空壳 `maps/` 和
    /// tracked 的 `plugins/`，看起来像根却跑不起来。全都不可用时退回
    /// 第一个长得像的，让后续加载带着**完整路径**报错。
    pub fn resolve(explicit: Option<&Path>) -> Self {
        if let Some(root) = explicit {
            return Self::under(&root.to_path_buf());
        }
        let mut candidates: Vec<PathBuf> = Vec::new();
        candidates.extend(Self::exe_root());
        candidates.extend(Self::dev_root());
        candidates.push(PathBuf::from("."));
        Self::pick(&candidates)
    }

    /// 从候选里挑根：真有地图者优先，退而求其次选"长得像资源根"的。
    fn pick(candidates: &[PathBuf]) -> Self {
        let chosen = candidates
            .iter()
            .find(|root| has_maps(&maps_dir_under(root)))
            .or_else(|| candidates.iter().find(|root| looks_like_root(root)));
        Self::under(chosen.unwrap_or(&PathBuf::from(".")))
    }

    /// 在 `root` 下生成 Paths（maps/atlas/plugins 的具体取舍）。
    fn under(root: &Path) -> Self {
        let maps = maps_dir_under(root);
        let atlas = if root.join("atlas").is_dir() {
            root.join("atlas")
        } else if root.join("resources").is_dir() {
            // Appendix C: resources/<namespace>/atlas —— 先取第一个 namespace。
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

    /// 可执行文件目录，仅当它看起来像个资源根时才返回。
    fn exe_root() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?.to_path_buf();
        looks_like_root(&dir).then_some(dir)
    }

    /// 源码树根（`cargo run` 起来的开发构建）。
    fn dev_root() -> Option<PathBuf> {
        // `crates/game` → `crates` → 仓库根。
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = manifest.parent()?.parent()?;
        looks_like_root(root).then(|| root.to_path_buf())
    }

    /// 资源根。
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `maps/` 目录。
    pub fn maps(&self) -> &Path {
        &self.maps
    }

    /// `atlas/` 目录（解析 `.atlas` 载荷用）。
    pub fn atlas_dir(&self) -> &Path {
        &self.atlas
    }

    /// `plugins/` 目录。
    pub fn plugins(&self) -> &Path {
        &self.plugins
    }

    /// 热重载监听目录（就是 `maps/`）。
    pub fn watch_dir(&self) -> &Path {
        &self.maps
    }

    /// 一张地图的完整路径。`name` 允许带 `.map` 或不带。
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

    /// 一个图集描述的完整路径（`.atlas` 与它同级的目录名一致）。
    pub fn atlas(&self, name: &str) -> PathBuf {
        self.atlas.join(format!("{name}.atlas"))
    }

    /// tileset 定义的完整路径（与图集同目录，content-pipeline 的产物）。
    pub fn tilesets(&self) -> PathBuf {
        self.atlas.join("tilesets.toml")
    }

    /// **背景**瓦片定义（由 `Graphics/BackgroundTiles.xml` 转换）。
    ///
    /// 与 [`Paths::tilesets`] 是两张独立的表：同一个 id 在两边是不同
    /// 贴图（原版两个独立的 `Autotiler`，`pureCeleste.cs:110116-110117`）。
    pub fn bg_tilesets(&self) -> PathBuf {
        self.atlas.join("bg_tilesets.toml")
    }
}

fn looks_like_root(dir: &Path) -> bool {
    ["assets", "maps", "plugins", "resources"]
        .iter()
        .any(|d| dir.join(d).is_dir())
}

/// `root` 下实际可用的地图目录：发布树 `maps/<namespace>/` 优先，回落
/// 开发树的 `assets/maps/` —— 哪个目录**真有地图**就用哪个。
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

/// 目录里有没有 `.map`（看一层子目录，够覆盖 `maps/<namespace>/`）。
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

    /// exe/dev 根"长得像"资源根（空壳 maps/ + tracked plugins/）但没有
    /// 一张可加载的地图时，必须让位给**真有地图**的候选（仓库根/cwd）。
    ///
    /// nix run 场景：store 里的构建源码树就是那个"长得像"的根，旧实现
    /// 选中它后直接报"地图不存在"，哪怕仓库就在 cwd。
    #[test]
    fn a_root_without_any_map_yields_to_one_that_has_maps() {
        let lookalike = std::env::temp_dir().join("releste-paths-lookalike");
        let real = std::env::temp_dir().join("releste-paths-real-root");
        let _ = std::fs::remove_dir_all(&lookalike);
        let _ = std::fs::remove_dir_all(&real);
        // store 源码树的形状：空 maps/、tracked 的 plugins/<name>/。
        std::fs::create_dir_all(lookalike.join("maps")).unwrap();
        std::fs::create_dir_all(lookalike.join("plugins/clock")).unwrap();
        // 仓库根的形状：assets/maps/ 里有真地图。
        std::fs::create_dir_all(real.join("assets/maps")).unwrap();
        std::fs::write(real.join("assets/maps/0-Intro.map"), b"RELMAP").unwrap();

        let picked = Paths::pick(&[lookalike.clone(), real.clone()]);
        assert_eq!(picked.root(), real, "the root with actual maps must win");
        assert!(picked.maps().starts_with(&real), "{:?}", picked.maps());

        let _ = std::fs::remove_dir_all(&lookalike);
        let _ = std::fs::remove_dir_all(&real);
    }

    /// 开发树：`maps/` 是空壳，应退到 `assets/maps/`。
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

    /// 发布树：`maps/<namespace>/` 里有地图时必须赢过空的 `assets/maps/`。
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
