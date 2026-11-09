//! Atlas descriptor (`.atlas`) loading.
//!
//! The format is produced by `tools/content-pipeline`: one texture + one TOML
//! descriptor (sprite list, rect, offset, origin).
//!
//! **No runtime parsing of `.meta` / `.xnb` / `.bin`** (AGENTS.md 4.6) -
//! only our own format is read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use releste_math::Rect;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Texture payload kind.
///
/// `tools/content-pipeline` decodes packed `.data` pages into PNG offline, so
/// in practice a descriptor carries [`TextureKind::Png`]. The remaining variants
/// describe raw `Celeste` `.data` payloads that are copied verbatim and are **not**
/// pixel-RLE-decoded by the pipeline yet (AGENTS.md 13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextureKind {
    /// Standalone PNG (directly decodable).
    Png,
    /// Shared `.data` payload (pixel RLE, decoded offline by the pipeline).
    Crunch,
    /// Packed atlas page `name0.data` (pixel RLE).
    PackedCrunch,
    /// One `.data` per sprite (pixel RLE); needs runtime per-sprite binding.
    PerSpriteCrunch,
}

impl TextureKind {
    /// Whether it decodes to pixels directly.
    ///
    /// Only PNG does: `.data` payloads are byte-run encoded and must be
    /// converted offline (AGENTS.md 4.6).
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
            releste_math::Vec2::from_f32s(f32::from(self.x), f32::from(self.y)),
            releste_math::Vec2::from_f32s(f32::from(self.x + self.w), f32::from(self.y + self.h)),
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
    /// sprite name (lowercase) -> (texture index, sprite index)
    index: HashMap<String, (usize, usize)>,
    /// Aliases (the `LINKS` section).
    aliases: HashMap<String, String>,
    /// Asset root directory (`atlas/`).
    root: PathBuf,
}

impl SpriteAtlas {
    /// Loads from a file and builds the index.
    ///
    /// `root` is the `atlas/` directory, used to resolve absolute texture payload paths.
    pub fn load(path: &Path, root: &Path) -> Result<Self, AtlasError> {
        let descriptor = AtlasDescriptor::from_file(path)?;
        Ok(Self::new(descriptor, root))
    }

    /// Builds the index from an already-parsed descriptor.
    pub fn new(descriptor: AtlasDescriptor, root: &Path) -> Self {
        let mut index = HashMap::new();
        for (ti, tex) in descriptor.textures.iter().enumerate() {
            for (si, sprite) in tex.sprites.iter().enumerate() {
                // Sprite lookups are case-insensitive (matching Celeste).
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

    /// The descriptor (read-only).
    pub fn descriptor(&self) -> &AtlasDescriptor {
        &self.descriptor
    }

    /// Atlas name.
    pub fn name(&self) -> &str {
        &self.descriptor.name
    }

    /// Looks up a sprite by name.
    ///
    /// Separator-agnostic: a tileset definition writes `tilesets/snow`, the atlas writes `tilesets\snow`.
    pub fn sprite(&self, name: &str) -> Option<(&TextureGroup, &Sprite)> {
        let key = name.to_lowercase().replace('/', "\\");
        let (ti, si) = *self.index.get(&key)?;
        let tex = self.descriptor.textures.get(ti)?;
        let sprite = tex.sprites.get(si)?;
        Some((tex, sprite))
    }

    /// Looks up a sprite by name, returning an error if it is not found.
    pub fn require(&self, name: &str) -> Result<(&TextureGroup, &Sprite), AtlasError> {
        self.sprite(name)
            .ok_or_else(|| AtlasError::SpriteNotFound(name.to_string()))
    }

    /// Absolute path of the payload for this sprite.
    ///
    /// `PerSpriteCrunch` uses the sprite's own `source`; the others use the group-level `source`.
    pub fn payload_path(&self, tex: &TextureGroup, sprite: &Sprite) -> PathBuf {
        match (&tex.kind, &sprite.source) {
            (TextureKind::PerSpriteCrunch, Some(s)) => self.join_root(s),
            _ => self.group_payload_path(tex),
        }
    }

    /// Absolute path of the payload for this texture group.
    ///
    /// Descriptor paths are prefixed with `atlas/`; strip it, then join onto the atlas root directory.
    pub fn group_payload_path(&self, tex: &TextureGroup) -> PathBuf {
        self.join_root(&tex.source)
    }

    /// Joins a descriptor-relative path onto the atlas root directory.
    fn join_root(&self, rel: &str) -> PathBuf {
        let rel = rel.strip_prefix("atlas/").unwrap_or(rel);
        self.root.join(rel)
    }

    /// Resolves an alias (`LINKS`) to the real sprite name.
    pub fn resolve_alias<'a>(&'a self, name: &'a str) -> &'a str {
        self.aliases
            .get(&name.to_lowercase())
            .map(String::as_str)
            .unwrap_or(name)
    }

    /// All sprite names.
    pub fn sprite_names(&self) -> impl Iterator<Item = &str> {
        self.descriptor
            .textures
            .iter()
            .flat_map(|t| t.sprites.iter().map(|s| s.name.as_str()))
    }
}

/// Resolves the texture of one animation frame exactly like Monocle's
/// `Atlas.GetAtlasSubtextureFromAtlasAt` (`pureCeleste.cs:14311-14329`):
///
/// - `index == 0` first tries the **exact** `prefix`. Player animations whose single
///   texture is named after their path (`characters/player/duck`, `characters/player/bubble`)
///   only resolve here -- they have no `duck00` (`14313-14316`);
/// - otherwise `prefix + index` is tried with 0 to 5 extra leading zeros and the first hit
///   wins (`idle` 1 -> `idle1`, `idle01`, ... -> `idle01`; `14317-14326`);
/// - no hit at all -> `None`. Vanilla throws `Can't find sprite ...` at `10467`; callers
///   warn and keep the frame slot instead.
///
/// `exists` must be the atlas' own case-insensitive lookup.
pub fn frame_name_at(exists: impl Fn(&str) -> bool, prefix: &str, index: usize) -> Option<String> {
    if index == 0 && exists(prefix) {
        return Some(prefix.to_string());
    }
    let mut text = index.to_string();
    for _ in 0..6 {
        let name = format!("{prefix}{text}");
        if exists(&name) {
            return Some(name);
        }
        text.insert(0, '0');
    }
    None
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
    fn frame_name_at_prefers_the_exact_key_for_index_zero() {
        // The player's `duck` animation (`Loop id="duck" path="duck" frames="0"`) has a
        // single texture named after the path: no `duck00` exists.
        let exists = |n: &str| {
            matches!(
                n,
                "characters\\player\\duck"
                    | "characters\\player\\idle00"
                    | "characters\\player\\idle01"
            )
        };
        assert_eq!(
            frame_name_at(exists, "characters\\player\\duck", 0).as_deref(),
            Some("characters\\player\\duck")
        );
        // Without an exact key the padded lookup still runs for index 0.
        assert_eq!(
            frame_name_at(exists, "characters\\player\\idle", 0).as_deref(),
            Some("characters\\player\\idle00")
        );
        assert_eq!(
            frame_name_at(exists, "characters\\player\\idle", 1).as_deref(),
            Some("characters\\player\\idle01")
        );
        assert_eq!(frame_name_at(exists, "characters\\player\\idle", 2), None);
    }

    #[test]
    fn frame_name_at_tries_every_zero_padding_level() {
        let one = |n: &str| n == "sprite0";
        let three = |n: &str| n == "sprite003";
        assert_eq!(frame_name_at(one, "sprite", 0).as_deref(), Some("sprite0"));
        assert_eq!(
            frame_name_at(three, "sprite", 3).as_deref(),
            Some("sprite003")
        );
        assert_eq!(frame_name_at(|_| false, "sprite", 7), None);
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
