//! Atlas (`.meta`) conversion.
//!
//! # Source format
//! Celeste's `.meta` is the binary serialization of Monocle `Atlas`.
//! Byte layout (matching the `Packer` / `PackerNoAtlas` branches of
//! the decompiled `Atlas.ReadAtlasData`):
//!
//! ```text
//! i32    version        (ignored)
//! string images         (packing command line, ignored)
//! i32    unknown        (ignored)
//! i16    texture_count
//! repeat texture_count:
//!     string texture_name
//!     i16    sprite_count
//!     repeat sprite_count:
//!         string name
//!         i16 x, y, w, h
//!         i16 offset_x, offset_y, frame_w, frame_h
//! [optional]
//!     string tag            ("LINKS")
//!     i16    link_count
//!     repeat link_count: string key, string value
//! ```
//!
//! where `string` is a 7-bit varint length + ASCII bytes.
//!
//! # Texture payload
//! The `.data` used by `Packer` / `PackerNoAtlas` is **not Crunch** -- the Monocle branch called
//! `Crunch*` actually reads a `.png`. `.data` is a per-command **pixel RLE**;
//! see [`crate::data`] for decoding. This tool decodes it to PNG offline; the runtime only knows `png`.

use std::path::{Path, PathBuf};

use crate::error::PipelineError;

/// Texture payload source.
///
/// Corresponds to the branches of Monocle `Atlas.ReadAtlasData`:
/// - `Packer`: all sprites share one `<name>0.data`.
/// - `PackerNoAtlas`: **one per sprite**, `<name>/<sprite>.data`.
/// - Anything else: a standalone PNG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextureSource {
    /// The packed atlas shared by all sprites, `name + "0.data"` (`Packer`).
    PackedCrunch(PathBuf),
    /// A single `name + ".data"` shared by all sprites.
    Crunch(PathBuf),
    /// One `<name>/<sprite>.data` per sprite (`PackerNoAtlas`).
    PerSpriteDir(PathBuf),
    /// A standalone PNG.
    Png(PathBuf),
}

impl TextureSource {
    /// Root path of this group of textures (a directory or a file).
    pub fn path(&self) -> &Path {
        match self {
            TextureSource::PackedCrunch(p)
            | TextureSource::Crunch(p)
            | TextureSource::PerSpriteDir(p)
            | TextureSource::Png(p) => p,
        }
    }

    /// File extension on disk (`data` for `PerSpriteDir`).
    pub fn payload_ext(&self) -> &'static str {
        match self {
            TextureSource::PackedCrunch(_)
            | TextureSource::Crunch(_)
            | TextureSource::PerSpriteDir(_) => "data",
            TextureSource::Png(_) => "png",
        }
    }

    /// The `kind` field in the description file.
    pub fn kind(&self) -> &'static str {
        match self {
            TextureSource::PackedCrunch(_) => "packed_crunch",
            TextureSource::Crunch(_) => "crunch",
            TextureSource::PerSpriteDir(_) => "per_sprite_crunch",
            TextureSource::Png(_) => "png",
        }
    }

    /// Whether this is a `.data` payload that needs transcoding (not a standalone PNG).
    pub fn is_packer_data(&self) -> bool {
        !matches!(self, TextureSource::Png(_))
    }

    /// Get the disk file for one sprite.
    ///
    /// Only `PerSpriteDir` splits files by sprite name; the others return the shared path.
    pub fn file_for_sprite(&self, sprite_name: &str) -> PathBuf {
        match self {
            TextureSource::PerSpriteDir(dir) => {
                dir.join(format!("{sprite_name}.{}", self.payload_ext()))
            }
            other => other.path().to_path_buf(),
        }
    }
}

/// A single sprite region.
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteRegion {
    /// sprite name (e.g. `"10_a-00"`).
    pub name: String,
    /// Rectangle inside the atlas.
    pub x: i16,
    pub y: i16,
    pub w: i16,
    pub h: i16,
    /// Draw-origin offset (the source file's `offset`; negated in MTexture).
    pub origin_x: i16,
    pub origin_y: i16,
    /// Logical frame size.
    pub frame_w: i16,
    pub frame_h: i16,
}

/// One texture and its sprite list.
#[derive(Debug, Clone, PartialEq)]
pub struct AtlasTexture {
    pub name: String,
    pub source: TextureSource,
    pub sprites: Vec<SpriteRegion>,
    /// Texture pixel dimensions (read from the `.data` header; filled in later for PNG).
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// Parse result.
#[derive(Debug, Clone, PartialEq)]
pub struct AtlasBuild {
    /// Format version (source file header).
    pub version: i32,
    /// Packing command (for diagnostics).
    pub images: String,
    pub textures: Vec<AtlasTexture>,
    /// The `LINKS` section (alias map).
    pub links: Vec<(String, String)>,
}

/// Read a .NET `BinaryReader`-style 7-bit length-prefixed string.
fn read_string(cur: &mut usize, data: &[u8]) -> Result<String, PipelineError> {
    let mut len: u32 = 0;
    let mut shift = 0;
    loop {
        let byte = *data.get(*cur).ok_or_else(|| {
            PipelineError::parse("meta", "<buffer>", "unexpected EOF in string length")
        })?;
        *cur += 1;
        len |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 28 {
            return Err(PipelineError::parse(
                "meta",
                "<buffer>",
                "varint length too large",
            ));
        }
    }

    let end = *cur + len as usize;
    let bytes = data
        .get(*cur..end)
        .ok_or_else(|| PipelineError::parse("meta", "<buffer>", "unexpected EOF in string body"))?;
    *cur = end;

    // The source files are ASCII; use lossy so a single stray byte does not fail everything.
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn read_i32(cur: &mut usize, data: &[u8]) -> Result<i32, PipelineError> {
    let end = *cur + 4;
    let b = data
        .get(*cur..end)
        .ok_or_else(|| PipelineError::parse("meta", "<buffer>", "unexpected EOF in i32"))?;
    *cur = end;
    Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_i16(cur: &mut usize, data: &[u8]) -> Result<i16, PipelineError> {
    let end = *cur + 2;
    let b = data
        .get(*cur..end)
        .ok_or_else(|| PipelineError::parse("meta", "<buffer>", "unexpected EOF in i16"))?;
    *cur = end;
    Ok(i16::from_le_bytes([b[0], b[1]]))
}

impl AtlasBuild {
    /// Parse `.meta` bytes.
    ///
    /// `meta_path` is used to resolve relative texture paths.
    pub fn parse(meta_path: &Path, data: &[u8]) -> Result<Self, PipelineError> {
        let dir = meta_path.parent().unwrap_or_else(|| Path::new("."));
        let mut cur = 0usize;

        let version = read_i32(&mut cur, data)?;
        let images = read_string(&mut cur, data)?;
        let _unknown = read_i32(&mut cur, data)?;

        let texture_count = read_i16(&mut cur, data)?;
        if texture_count < 0 {
            return Err(PipelineError::parse(
                "meta",
                meta_path.display(),
                format!("negative texture count: {texture_count}"),
            ));
        }

        let mut textures = Vec::with_capacity(texture_count as usize);
        for _ in 0..texture_count {
            let name = read_string(&mut cur, data)?;
            let sprite_count = read_i16(&mut cur, data)?;
            if sprite_count < 0 {
                return Err(PipelineError::parse(
                    "meta",
                    meta_path.display(),
                    format!("negative sprite count for texture {name}: {sprite_count}"),
                ));
            }

            let mut sprites = Vec::with_capacity(sprite_count as usize);
            for _ in 0..sprite_count {
                let sprite_name = read_string(&mut cur, data)?;
                let x = read_i16(&mut cur, data)?;
                let y = read_i16(&mut cur, data)?;
                let w = read_i16(&mut cur, data)?;
                let h = read_i16(&mut cur, data)?;
                let offset_x = read_i16(&mut cur, data)?;
                let offset_y = read_i16(&mut cur, data)?;
                let frame_w = read_i16(&mut cur, data)?;
                let frame_h = read_i16(&mut cur, data)?;
                sprites.push(SpriteRegion {
                    name: sprite_name,
                    x,
                    y,
                    w,
                    h,
                    origin_x: offset_x,
                    origin_y: offset_y,
                    frame_w,
                    frame_h,
                });
            }

            let source = resolve_texture_source(dir, &name);
            textures.push(AtlasTexture {
                name,
                source,
                sprites,
                width: None,
                height: None,
            });
        }

        // Optional LINKS section
        let mut links = Vec::new();
        if cur < data.len() {
            let tag = read_string(&mut cur, data)?;
            if tag == "LINKS" {
                let n = read_i16(&mut cur, data)?;
                for _ in 0..n.max(0) {
                    let key = read_string(&mut cur, data)?;
                    let value = read_string(&mut cur, data)?;
                    links.push((key, value));
                }
            }
        }

        Ok(AtlasBuild {
            version,
            images,
            textures,
            links,
        })
    }

    /// Read and parse from a file.
    pub fn from_file(meta_path: &Path) -> Result<Self, PipelineError> {
        let data =
            std::fs::read(meta_path).map_err(|e| PipelineError::io(meta_path.display(), e))?;
        Self::parse(meta_path, &data)
    }

    /// Total sprite count.
    pub fn sprite_count(&self) -> usize {
        self.textures.iter().map(|t| t.sprites.len()).sum()
    }
}

/// Infer the texture source (corresponds to the `Packer` / `PackerNoAtlas` branches of the source format).
fn resolve_texture_source(dir: &Path, name: &str) -> TextureSource {
    let normalized = name.replace('\\', "/");

    // Packer: all sprites share one `<name>0.data`
    let packed = dir.join(format!("{normalized}0.data"));
    if packed.exists() {
        return TextureSource::PackedCrunch(packed);
    }

    // Single texture `<name>.data`
    let single = dir.join(format!("{normalized}.data"));
    if single.exists() {
        return TextureSource::Crunch(single);
    }

    // PackerNoAtlas: a `<name>/` directory with one `<name>/<sprite>.data` per sprite
    let group = dir.join(&normalized);
    if group.is_dir() {
        return TextureSource::PerSpriteDir(group);
    }

    // Fallback: standalone PNG
    TextureSource::Png(dir.join(&normalized))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal `.meta` byte sequence.
    fn build_meta() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&5i32.to_le_bytes());
        let images = "src";
        out.push(images.len() as u8);
        out.extend_from_slice(images.as_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        // 1 texture
        out.extend_from_slice(&1i16.to_le_bytes());
        let tex = "Gui";
        out.push(tex.len() as u8);
        out.extend_from_slice(tex.as_bytes());
        // 2 sprites
        out.extend_from_slice(&2i16.to_le_bytes());
        for (name, x, y) in [("a", 1i16, 2i16), ("b", 3, 4)] {
            out.push(name.len() as u8);
            out.extend_from_slice(name.as_bytes());
            for v in [x, y, 10, 20, -1, -2, 12, 24] {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }

    #[test]
    fn parses_meta() {
        let bytes = build_meta();
        let build = AtlasBuild::parse(Path::new("/tmp/Gui.meta"), &bytes).unwrap();
        assert_eq!(build.version, 5);
        assert_eq!(build.images, "src");
        assert_eq!(build.textures.len(), 1);
        assert_eq!(build.textures[0].name, "Gui");
        assert_eq!(build.sprite_count(), 2);

        let a = &build.textures[0].sprites[0];
        assert_eq!(a.name, "a");
        assert_eq!((a.x, a.y, a.w, a.h), (1, 2, 10, 20));
        assert_eq!((a.origin_x, a.origin_y), (-1, -2));
        assert_eq!((a.frame_w, a.frame_h), (12, 24));
    }

    #[test]
    fn parses_links_section() {
        let mut bytes = build_meta();
        let tag = "LINKS";
        bytes.push(tag.len() as u8);
        bytes.extend_from_slice(tag.as_bytes());
        bytes.extend_from_slice(&1i16.to_le_bytes());
        for s in ["alias", "target"] {
            bytes.push(s.len() as u8);
            bytes.extend_from_slice(s.as_bytes());
        }
        let build = AtlasBuild::parse(Path::new("/tmp/Gui.meta"), &bytes).unwrap();
        assert_eq!(build.links, vec![("alias".into(), "target".into())]);
    }

    #[test]
    fn truncated_meta_errors() {
        assert!(AtlasBuild::parse(Path::new("/tmp/x.meta"), &[5, 0]).is_err());
    }
}
