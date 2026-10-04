//! 引擎原生的 tileset 定义。
//!
//! 这些定义由 `content-pipeline` 在 build time 从原版 `ForegroundTiles.xml`
//! 转换而来 —— XML 是原始格式，只在 build time 解析（AGENTS.md §4.6）；
//! 运行时只读这份 TOML。
//!
//! # Autotiling 约定（ Celeste `Autotiler.cs`）
//!
//! 每块固体瓦片探测自己的 3×3 邻居，把邻接情况与 tileset 的掩码列表
//! 匹配，得到它在 tileset 纹理里的 `(col, row)`；渲染时采样
//! `(col * 8, row * 8)` 起的 8×8。掩码按通配符数升序匹配（最具体的先）。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Celeste tile 字母表：`0` 为空，其余为 tile 索引 + 1。
///
/// 地图瓦片格子里存的是这张表的**索引**；tileset 定义按**字符**索引。
pub const TILE_ALPHABET: &str = "0123456789abcdefghijklmnopqrstuvwxyz";

/// 地图瓦片值（字母表索引）→ tileset id 字符。`0`（空）→ `'0'`。
pub fn tile_char(value: u8) -> char {
    TILE_ALPHABET.chars().nth(value as usize).unwrap_or('0')
}

/// 一条邻接掩码：9 个位置 + 匹配时可用的 `(col, row)` 列表。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaskEntry {
    /// 9 个位置，布局 `[TL, T, TR, L, C, R, BL, B, BR]`：
    /// `0` = 空，`1` = 固体，`2` = 任意（通配）。
    pub mask: [u8; 9],
    /// 匹配时可选的 tileset `(col, row)`。
    pub tiles: Vec<(u32, u32)>,
}

/// 一个 tileset 的定义。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TilesetDef {
    /// tileset 纹理路径（图集内 sprite 名，如 `tilesets/snow`）。
    pub path: String,
    /// 邻接掩码列表，按通配符数升序（最具体的先匹配）。
    #[serde(default)]
    pub masks: Vec<MaskEntry>,
    /// 9 个邻居全部连上时用的 `(col, row)`（center）。
    #[serde(default)]
    pub center: Vec<(u32, u32)>,
    /// 有 2-away 邻居探测失败时用的 `(col, row)`（padding）。
    #[serde(default)]
    pub padding: Vec<(u32, u32)>,
    /// 本 tileset 不视为固体的 tile id（`*` = 全部）。
    #[serde(default)]
    pub ignores: Vec<char>,
}

impl TilesetDef {
    /// 通配符数（掩码匹配的排序键）。
    pub fn wildcards(&self) -> usize {
        self.masks.iter().map(|m| m.wildcards()).sum()
    }

    /// `TerrainType.Ignore`（`Autotiler.cs`）：同 id 恒连；不同 id 连，
    /// 除非在 ignores 里或 `*`（全部）。
    pub fn connects(&self, other: char) -> bool {
        if self.ignores.contains(&'*') {
            return false;
        }
        !self.ignores.contains(&other)
    }
}

impl MaskEntry {
    /// 通配符（`2`）个数。
    pub fn wildcards(&self) -> usize {
        self.mask.iter().filter(|&&v| v == 2).count()
    }

    /// 邻接数组是否匹配本掩码（非通配位置必须相等）。
    pub fn matches(&self, adjacency: &[u8; 9]) -> bool {
        self.mask
            .iter()
            .zip(adjacency.iter())
            .all(|(mask, adj)| *mask == 2 || *mask == *adj)
    }
}

/// 全部 tileset 定义，按 tile id（单字符字符串）索引。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TilesetsDoc {
    /// tile id → 定义。`"0"`（空）不在里面。
    #[serde(rename = "tileset")]
    pub tilesets: BTreeMap<String, TilesetDef>,
}

impl TilesetsDoc {
    /// 从 TOML 读取（content-pipeline 的产物）。
    pub fn from_file(path: &Path) -> Result<Self, TilesetsError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| TilesetsError::Read(path.display().to_string(), e))?;
        toml::from_str(&text).map_err(|source| TilesetsError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    /// tile id 的定义。
    pub fn def(&self, id: char) -> Option<&TilesetDef> {
        self.tilesets.get(&id.to_string())
    }
}

/// tileset 定义加载失败。
#[derive(Debug, thiserror::Error)]
pub enum TilesetsError {
    /// 文件读不了。
    #[error("cannot read tileset definitions {0}: {1}")]
    Read(String, #[source] std::io::Error),

    /// 不是合法的 TOML。
    #[error("invalid tileset definitions {path}: {source}")]
    Parse {
        /// 文件路径，用于报错。
        path: String,
        /// TOML 错误。
        #[source]
        source: toml::de::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_char_maps_the_alphabet() {
        assert_eq!(tile_char(0), '0');
        assert_eq!(tile_char(1), '1');
        assert_eq!(tile_char(3), '3');
        assert_eq!(tile_char(10), 'a');
        assert_eq!(tile_char(35), 'z');
    }

    #[test]
    fn mask_matching_respects_wildcards() {
        let entry = MaskEntry {
            mask: [2, 0, 2, 1, 1, 1, 2, 1, 2],
            tiles: vec![(0, 0)],
        };
        // x0x-111-x1x：上边必须空，左/右/下必须是固体（上边缘瓦片）。
        let adj = [0, 0, 0, 1, 1, 1, 1, 1, 1];
        assert!(entry.matches(&adj));
        let adj = [0, 1, 0, 1, 1, 1, 1, 1, 1];
        assert!(!entry.matches(&adj), "上边是固体时不匹配 x0x");
    }

    #[test]
    fn ignores_block_connecting() {
        let def = TilesetDef {
            ignores: vec!['g'],
            ..Default::default()
        };
        assert!(def.connects('1'));
        assert!(!def.connects('g'));
        let all = TilesetDef {
            ignores: vec!['*'],
            ..Default::default()
        };
        assert!(!all.connects('1'), "`*` blocks every other id");
    }

    #[test]
    fn toml_roundtrip_keeps_mask_order() {
        let doc = TilesetsDoc {
            tilesets: BTreeMap::from([(
                "3".into(),
                TilesetDef {
                    path: "tilesets/snow".into(),
                    masks: vec![MaskEntry {
                        mask: [2, 0, 2, 1, 1, 1, 2, 1, 2],
                        tiles: vec![(0, 0), (1, 0)],
                    }],
                    center: vec![(0, 2)],
                    padding: vec![(0, 3)],
                    ignores: vec![],
                },
            )]),
        };
        let text = toml::to_string(&doc).expect("serializes");
        let back: TilesetsDoc = toml::from_str(&text).expect("parses back");
        assert_eq!(back, doc);
        assert_eq!(back.def('3').expect("def").path, "tilesets/snow");
    }
}
