//! 图集（`.meta`）转换。
//!
//! # 源格式
//! Celeste 的 `.meta` 是 Monocle `Atlas` 的二进制序列化。
//! 字节布局（与反编译源码 `Atlas.ReadAtlasData` 的
//! `Packer` / `PackerNoAtlas` 分支一致）：
//!
//! ```text
//! i32    version        （忽略）
//! string images         （打包命令行，忽略）
//! i32    unknown        （忽略）
//! i16    texture_count
//! repeat texture_count:
//!     string texture_name
//!     i16    sprite_count
//!     repeat sprite_count:
//!         string name
//!         i16 x, y, w, h
//!         i16 offset_x, offset_y, frame_w, frame_h
//! [optional]
//!     string tag            （"LINKS"）
//!     i16    link_count
//!     repeat link_count: string key, string value
//! ```
//!
//! 其中 `string` 是 7-bit varint 长度 + ASCII 字节。
//!
//! # 纹理载荷
//! `Packer` 用的 `.data` 是 `[i32 width][i32 height][Crunch 压缩像素]`。
//! **Crunch 解码尚未实现**（见 AGENTS.md §13 待办）：本工具会写出描述
//! 与原始载荷，并报告需要 Crunch 才能还原像素。

use std::path::{Path, PathBuf};

use crate::error::PipelineError;

/// 纹理载荷来源。
///
/// 与 Monocle `Atlas.ReadAtlasData` 的分支对应：
/// - `Packer`：所有 sprite 共用一个 `<name>0.data`。
/// - `PackerNoAtlas`：**每个 sprite 一个** `<name>/<sprite>.data`。
/// - 其它：独立 PNG。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextureSource {
    /// 所有 sprite 共享的打包图集 `name + "0.data"`（`Packer`）。
    PackedCrunch(PathBuf),
    /// 所有 sprite 共享的单个 `name + ".data"`。
    Crunch(PathBuf),
    /// 每个 sprite 一个 `<name>/<sprite>.data`（`PackerNoAtlas`）。
    PerSpriteDir(PathBuf),
    /// 独立 PNG。
    Png(PathBuf),
}

impl TextureSource {
    /// 该组纹理的根路径（目录或文件）。
    pub fn path(&self) -> &Path {
        match self {
            TextureSource::PackedCrunch(p)
            | TextureSource::Crunch(p)
            | TextureSource::PerSpriteDir(p)
            | TextureSource::Png(p) => p,
        }
    }

    /// 磁盘上的文件扩展名（`PerSpriteDir` 为 `data`）。
    pub fn payload_ext(&self) -> &'static str {
        match self {
            TextureSource::PackedCrunch(_)
            | TextureSource::Crunch(_)
            | TextureSource::PerSpriteDir(_) => "data",
            TextureSource::Png(_) => "png",
        }
    }

    /// 描述文件里的 `kind` 字段。
    pub fn kind(&self) -> &'static str {
        match self {
            TextureSource::PackedCrunch(_) => "packed_crunch",
            TextureSource::Crunch(_) => "crunch",
            TextureSource::PerSpriteDir(_) => "per_sprite_crunch",
            TextureSource::Png(_) => "png",
        }
    }

    /// 是否为需 Crunch 解码的载荷。
    pub fn is_crunch(&self) -> bool {
        !matches!(self, TextureSource::Png(_))
    }

    /// 取某个 sprite 对应的磁盘文件。
    ///
    /// 仅 `PerSpriteDir` 会按 sprite 名分文件；其它返回共享路径。
    pub fn file_for_sprite(&self, sprite_name: &str) -> PathBuf {
        match self {
            TextureSource::PerSpriteDir(dir) => {
                dir.join(format!("{sprite_name}.{}", self.payload_ext()))
            }
            other => other.path().to_path_buf(),
        }
    }
}

/// 单个 sprite 区域。
#[derive(Debug, Clone, PartialEq)]
pub struct SpriteRegion {
    /// sprite 名（如 `"10_a-00"`）。
    pub name: String,
    /// 图集内矩形。
    pub x: i16,
    pub y: i16,
    pub w: i16,
    pub h: i16,
    /// 绘制原点偏移（源文件中的 `offset`，MTexture 里取负）。
    pub origin_x: i16,
    pub origin_y: i16,
    /// 逻辑帧尺寸。
    pub frame_w: i16,
    pub frame_h: i16,
}

/// 一个纹理及其 sprite 列表。
#[derive(Debug, Clone, PartialEq)]
pub struct AtlasTexture {
    pub name: String,
    pub source: TextureSource,
    pub sprites: Vec<SpriteRegion>,
    /// 纹理像素尺寸（从 `.data` 头部读取；PNG 在后续阶段填充）。
    pub width: Option<i32>,
    pub height: Option<i32>,
}

/// 解析结果。
#[derive(Debug, Clone, PartialEq)]
pub struct AtlasBuild {
    /// 格式版本（源文件头部）。
    pub version: i32,
    /// 打包命令（诊断用）。
    pub images: String,
    pub textures: Vec<AtlasTexture>,
    /// `LINKS` 段（别名映射）。
    pub links: Vec<(String, String)>,
}

/// 读取 .NET `BinaryReader` 风格的 7-bit 长度前缀字符串。
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

    // 源文件为 ASCII；用 lossy 以免个别字节导致整体失败。
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
    /// 解析 `.meta` 字节。
    ///
    /// `meta_path` 用于解析纹理相对路径。
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

        // 可选 LINKS 段
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

    /// 从文件读取并解析。
    pub fn from_file(meta_path: &Path) -> Result<Self, PipelineError> {
        let data =
            std::fs::read(meta_path).map_err(|e| PipelineError::io(meta_path.display(), e))?;
        Self::parse(meta_path, &data)
    }

    /// sprite 总数。
    pub fn sprite_count(&self) -> usize {
        self.textures.iter().map(|t| t.sprites.len()).sum()
    }
}

/// 推断纹理来源（对应源格式的 `Packer` / `PackerNoAtlas` 分支）。
fn resolve_texture_source(dir: &Path, name: &str) -> TextureSource {
    let normalized = name.replace('\\', "/");

    // Packer：所有 sprite 共用一个 `<name>0.data`
    let packed = dir.join(format!("{normalized}0.data"));
    if packed.exists() {
        return TextureSource::PackedCrunch(packed);
    }

    // 单纹理 `<name>.data`
    let single = dir.join(format!("{normalized}.data"));
    if single.exists() {
        return TextureSource::Crunch(single);
    }

    // PackerNoAtlas：`<name>/` 目录，每个 sprite 一个 `<name>/<sprite>.data`
    let group = dir.join(&normalized);
    if group.is_dir() {
        return TextureSource::PerSpriteDir(group);
    }

    // 回退：独立 PNG
    TextureSource::Png(dir.join(&normalized))
}

/// 读取 Celeste `.data` 纹理头部（`[i32 width][i32 height][payload]`）。
///
/// 返回 `(width, height)`。**不**解码像素载荷（Crunch，待办）。
pub fn read_data_header(path: &Path) -> Result<(i32, i32), PipelineError> {
    let data = std::fs::read(path).map_err(|e| PipelineError::io(path.display(), e))?;
    if data.len() < 8 {
        return Err(PipelineError::parse(
            "data",
            path.display(),
            "file shorter than 8-byte header",
        ));
    }
    let w = i32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let h = i32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个最小 `.meta` 字节序列。
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
