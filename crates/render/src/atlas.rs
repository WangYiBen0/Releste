//! Atlas descriptor (`.atlas`) loading.
//!
//! The format is produced by `tools/content-pipeline`: one texture + one TOML
//! descriptor (sprite list, rect, offset, origin).
//!
//! **No runtime parsing of `.meta` / `.xnb` / `.bin`** (AGENTS.md §4.6) —
//! only our own format is read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use reles_math::Rect;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Texture payload kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextureKind {
    /// Standalone PNG (directly decodable).
    Png,
    /// Shared Crunch-compressed `.data`.
    Crunch,
    /// Packed atlas `name0.data` (Crunch).
    PackedCrunch,
    /// One `.data` per sprite (Crunch).
    PerSpriteCrunch,
}

impl TextureKind {
    /// Whether it decodes to pixels directly.
    pub fn is_decodable(self) -> bool {
        matches!(self, TextureKind::Png)
    }
}

/// A single sprite region.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sprite {
    pub name: String,
    pub x: i16,
    pub y: i16,
    pub w: i16,
    pub h: i16,
    /// Draw origin (already positive, i.e. MTexture's offset).
    pub origin_x: i16,
    pub origin_y: i16,
    pub frame_w: i16,
    pub frame_h: i16,
    /// Only used by `per_sprite_crunch`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl Sprite {
    /// Source rectangle within the atlas.
    pub fn rect(&self) -> Rect {
        Rect::new(
            reles_math::Vec2::from_f32s(f32::from(self.x), f32::from(self.y)),
            reles_math::Vec2::from_f32s(f32::from(self.x + self.w), f32::from(self.y + self.h)),
        )
    }

    /// Draw origin (relative to the top-left corner).
    pub fn origin(&self) -> (f32, f32) {
        (f32::from(self.origin_x), f32::from(self.origin_y))
    }

    /// Logical frame size.
    pub fn frame_size(&self) -> (f32, f32) {
        (f32::from(self.frame_w), f32::from(self.frame_h))
    }
}

/// A texture group.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextureGroup {
    pub name: String,
    pub kind: TextureKind,
    /// Payload path relative to `atlas/`.
    pub source: String,
    #[serde(default)]
    pub width: Option<i32>,
    #[serde(default)]
    pub height: Option<i32>,
    #[serde(default)]
    pub sprites: Vec<Sprite>,
}

/// An `.atlas` file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtlasDescriptor {
    pub name: String,
    #[serde(default)]
    pub version: i32,
    #[serde(default)]
    pub links: Vec<(String, String)>,
    pub textures: Vec<TextureGroup>,
}

/// Load error.
#[derive(Debug, Error)]
pub enum AtlasError {
    #[error("failed to read {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
    #[error("sprite `{0}` not found")]
    SpriteNotFound(String),
    #[error("texture group `{0}` not found")]
    GroupNotFound(String),
}

impl AtlasDescriptor {
    /// Parses from TOML text.
    pub fn from_str(path: &str, text: &str) -> Result<Self, AtlasError> {
        toml::from_str(text).map_err(|source| AtlasError::Parse {
            path: path.to_string(),
            source,
        })
    }

    /// Parses from a file.
    pub fn from_file(path: &Path) -> Result<Self, AtlasError> {
        let text = std::fs::read_to_string(path).map_err(|source| AtlasError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_str(&path.display().to_string(), &text)
    }

    /// Total sprite count.
    pub fn sprite_count(&self) -> usize {
        self.textures.iter().map(|t| t.sprites.len()).sum()
    }

    /// Whether Crunch decoding is required to render.
    pub fn needs_crunch(&self) -> bool {
        self.textures.iter().any(|t| !t.kind.is_decodable())
    }
}

/// Parsed sprite index (for runtime lookups).
#[derive(Debug, Clone)]
pub struct SpriteAtlas {
    descriptor: AtlasDescriptor,
    /// sprite name (lowercase) → (texture index, sprite index)
    index: HashMap<String, (usize, usize)>,
    /// Aliases (the `LINKS` section).
    aliases: HashMap<String, String>,
    /// Asset root directory (`atlas/`).
    root: PathBuf,
}

impl SpriteAtlas {
    /// 从文件加载并构建索引。
    ///
    /// `root` 是 `atlas/` 目录，用于解析纹理载荷的绝对路径。
    pub fn load(path: &Path, root: &Path) -> Result<Self, AtlasError> {
        let descriptor = AtlasDescriptor::from_file(path)?;
        Ok(Self::new(descriptor, root))
    }

    /// 从已解析的描述构建索引。
    pub fn new(descriptor: AtlasDescriptor, root: &Path) -> Self {
        let mut index = HashMap::new();
        for (ti, tex) in descriptor.textures.iter().enumerate() {
            for (si, sprite) in tex.sprites.iter().enumerate() {
                // sprite 查询大小写不敏感（与 Celeste 一致）。
                index.insert(sprite.name.to_lowercase(), (ti, si));
            }
        }

        let aliases = descriptor
            .links
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect();

        SpriteAtlas {
            descriptor,
            index,
            aliases,
            root: root.to_path_buf(),
        }
    }

    /// 描述（只读）。
    pub fn descriptor(&self) -> &AtlasDescriptor {
        &self.descriptor
    }

    /// 图集名。
    pub fn name(&self) -> &str {
        &self.descriptor.name
    }

    /// 按名查 sprite。
    pub fn sprite(&self, name: &str) -> Option<(&TextureGroup, &Sprite)> {
        let key = name.to_lowercase();
        let (ti, si) = *self.index.get(&key)?;
        let tex = self.descriptor.textures.get(ti)?;
        let sprite = tex.sprites.get(si)?;
        Some((tex, sprite))
    }

    /// 按名查 sprite，未找到返回错误。
    pub fn require(&self, name: &str) -> Result<(&TextureGroup, &Sprite), AtlasError> {
        self.sprite(name)
            .ok_or_else(|| AtlasError::SpriteNotFound(name.to_string()))
    }

    /// 该 sprite 载荷的绝对路径。
    ///
    /// `PerSpriteCrunch` 用 sprite 自己的 `source`，其它用组级 `source`。
    pub fn payload_path(&self, tex: &TextureGroup, sprite: &Sprite) -> PathBuf {
        let rel = match (&tex.kind, &sprite.source) {
            (TextureKind::PerSpriteCrunch, Some(s)) => s.clone(),
            _ => tex.source.clone(),
        };
        // 描述里的路径以 `atlas/` 为前缀，去掉它再拼 root。
        let rel = rel.strip_prefix("atlas/").unwrap_or(&rel);
        self.root.join(rel)
    }

    /// 解析别名（`LINKS`）到真实 sprite 名。
    pub fn resolve_alias<'a>(&'a self, name: &'a str) -> &'a str {
        self.aliases
            .get(&name.to_lowercase())
            .map(String::as_str)
            .unwrap_or(name)
    }

    /// 所有 sprite 名。
    pub fn sprite_names(&self) -> impl Iterator<Item = &str> {
        self.descriptor
            .textures
            .iter()
            .flat_map(|t| t.sprites.iter().map(|s| s.name.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
name = "Checkpoints"
version = 5

[[textures]]
name = "Checkpoints"
kind = "per_sprite_crunch"
source = "atlas/Checkpoints/Checkpoints"

[[textures.sprites]]
name = "10_a-00"
x = 0
y = 0
w = 120
h = 80
origin_x = 4
origin_y = 8
frame_w = 120
frame_h = 80
source = "atlas/Checkpoints/Checkpoints/10_a-00.data"
"#;

    #[test]
    fn parses_descriptor() {
        let d = AtlasDescriptor::from_str("sample", SAMPLE).unwrap();
        assert_eq!(d.name, "Checkpoints");
        assert_eq!(d.version, 5);
        assert_eq!(d.sprite_count(), 1);
        assert_eq!(d.textures[0].kind, TextureKind::PerSpriteCrunch);
        assert!(!d.textures[0].kind.is_decodable());
        assert!(d.needs_crunch());
    }

    #[test]
    fn sprite_lookup_is_case_insensitive() {
        let d = AtlasDescriptor::from_str("sample", SAMPLE).unwrap();
        let atlas = SpriteAtlas::new(d, Path::new("/assets"));
        assert!(atlas.sprite("10_a-00").is_some());
        assert!(atlas.sprite("10_A-00").is_some());
        assert!(atlas.sprite("nope").is_none());
    }

    #[test]
    fn payload_path_for_per_sprite() {
        let d = AtlasDescriptor::from_str("sample", SAMPLE).unwrap();
        let atlas = SpriteAtlas::new(d, Path::new("/assets"));
        let (tex, sprite) = atlas.require("10_a-00").unwrap();
        let p = atlas.payload_path(tex, sprite);
        assert_eq!(
            p.to_string_lossy(),
            "/assets/Checkpoints/Checkpoints/10_a-00.data"
        );
    }

    #[test]
    fn sprite_geometry() {
        let d = AtlasDescriptor::from_str("sample", SAMPLE).unwrap();
        let atlas = SpriteAtlas::new(d, Path::new("/assets"));
        let (_, s) = atlas.require("10_a-00").unwrap();
        assert_eq!(s.origin(), (4.0, 8.0));
        assert_eq!(s.frame_size(), (120.0, 80.0));
        let r = s.rect();
        assert_eq!(r.width().to_f32(), 120.0);
        assert_eq!(r.height().to_f32(), 80.0);
    }

    #[test]
    fn aliases_resolve() {
        let mut d = AtlasDescriptor::from_str("sample", SAMPLE).unwrap();
        d.links.push(("alias".into(), "10_a-00".into()));
        let atlas = SpriteAtlas::new(d, Path::new("/assets"));
        assert_eq!(atlas.resolve_alias("alias"), "10_a-00");
        assert_eq!(atlas.resolve_alias("other"), "other");
    }

    #[test]
    fn missing_sprite_reports_name() {
        let d = AtlasDescriptor::from_str("sample", SAMPLE).unwrap();
        let atlas = SpriteAtlas::new(d, Path::new("/assets"));
        match atlas.require("ghost") {
            Err(AtlasError::SpriteNotFound(n)) => assert_eq!(n, "ghost"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn packed_group_shares_payload() {
        let src = r#"
name = "Gameplay"
version = 5

[[textures]]
name = "Gameplay"
kind = "packed_crunch"
source = "atlas/Gameplay/Gameplay0.data"

[[textures.sprites]]
name = "tileset"
x = 1
y = 2
w = 8
h = 8
origin_x = 0
origin_y = 0
frame_w = 8
frame_h = 8
"#;
        let d = AtlasDescriptor::from_str("g", src).unwrap();
        let atlas = SpriteAtlas::new(d, Path::new("/assets"));
        let (tex, sprite) = atlas.require("tileset").unwrap();
        assert_eq!(sprite.source, None);
        assert_eq!(
            atlas.payload_path(tex, sprite).to_string_lossy(),
            "/assets/Gameplay/Gameplay0.data"
        );
    }
}
