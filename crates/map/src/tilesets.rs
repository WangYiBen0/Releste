//! Engine-native tileset definitions.
//!
//! These definitions are converted by `content-pipeline` at build time from the vanilla
//! `ForegroundTiles.xml` -- XML is the original format and is only parsed at build time (AGENTS.md 4.6);
//! at runtime only this TOML is read.
//!
//! # Autotiling convention (Celeste `Autotiler.cs`)
//!
//! Every solid tile probes its own 3x3 neighborhood and matches the adjacency pattern against the
//! tileset's mask list to get its `(col, row)` in the tileset texture; rendering samples the 8x8
//! starting at `(col * 8, row * 8)`. Masks match in ascending wildcard count (most specific first).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Celeste tile alphabet: `0` is empty, everything else is tile index + 1.
///
/// A map tile cell stores an **index** into this table; tileset definitions are indexed by **character**.
pub const TILE_ALPHABET: &str = "0123456789abcdefghijklmnopqrstuvwxyz";

/// Map tile value (alphabet index) -> tileset id character. `0` (empty) -> `'0'`.
pub fn tile_char(value: u16) -> char {
    TILE_ALPHABET.chars().nth(value as usize).unwrap_or('0')
}

/// One adjacency mask: 9 positions + the `(col, row)` list available on a match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaskEntry {
    /// 9 positions, laid out `[TL, T, TR, L, C, R, BL, B, BR]`:
    /// `0` = empty, `1` = solid, `2` = any (wildcard).
    pub mask: [u8; 9],
    /// The tileset `(col, row)` choices on a match.
    pub tiles: Vec<(u32, u32)>,
}

/// Definition of one tileset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TilesetDef {
    /// Tileset texture path (sprite name inside the atlas, e.g. `tilesets/snow`).
    pub path: String,
    /// Adjacency mask list, in ascending wildcard count (most specific matches first).
    #[serde(default)]
    pub masks: Vec<MaskEntry>,
    /// The `(col, row)` used when all nine neighbors connect (center).
    #[serde(default)]
    pub center: Vec<(u32, u32)>,
    /// The `(col, row)` used when a 2-away neighbor probe fails (padding).
    #[serde(default)]
    pub padding: Vec<(u32, u32)>,
    /// Tile ids this tileset does not treat as solid (`*` = all).
    #[serde(default)]
    pub ignores: Vec<char>,
}

impl TilesetDef {
    /// Wildcard count (the sort key for mask matching).
    pub fn wildcards(&self) -> usize {
        self.masks.iter().map(|m| m.wildcards()).sum()
    }

    /// `TerrainType.Ignore` (`Autotiler.cs`): the same id always connects; a different id connects,
    /// unless it is in ignores or is `*` (all).
    pub fn connects(&self, other: char) -> bool {
        if self.ignores.contains(&'*') {
            return false;
        }
        !self.ignores.contains(&other)
    }
}

impl MaskEntry {
    /// Number of wildcards (`2`).
    pub fn wildcards(&self) -> usize {
        self.mask.iter().filter(|&&v| v == 2).count()
    }

    /// Whether the adjacency array matches this mask (non-wildcard positions must be equal).
    pub fn matches(&self, adjacency: &[u8; 9]) -> bool {
        self.mask
            .iter()
            .zip(adjacency.iter())
            .all(|(mask, adj)| *mask == 2 || *mask == *adj)
    }
}

/// All tileset definitions, indexed by tile id (single-character string).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TilesetsDoc {
    /// tile id -> definition. `"0"` (empty) is not in there.
    #[serde(rename = "tileset")]
    pub tilesets: BTreeMap<String, TilesetDef>,
}

impl TilesetsDoc {
    /// Read from TOML (the content-pipeline output).
    pub fn from_file(path: &Path) -> Result<Self, TilesetsError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| TilesetsError::Read(path.display().to_string(), e))?;
        toml::from_str(&text).map_err(|source| TilesetsError::Parse {
            path: path.display().to_string(),
            source,
        })
    }

    /// The definition for a tile id.
    pub fn def(&self, id: char) -> Option<&TilesetDef> {
        self.tilesets.get(&id.to_string())
    }
}

/// Tileset definition loading failed.
#[derive(Debug, thiserror::Error)]
pub enum TilesetsError {
    /// The file cannot be read.
    #[error("cannot read tileset definitions {0}: {1}")]
    Read(String, #[source] std::io::Error),

    /// Not valid TOML.
    #[error("invalid tileset definitions {path}: {source}")]
    Parse {
        /// The file path, for the error message.
        path: String,
        /// The TOML error.
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
        // x0x-111-x1x: the top must be empty and the left/right/bottom must be solid (top-edge tile).
        let adj = [0, 0, 0, 1, 1, 1, 1, 1, 1];
        assert!(entry.matches(&adj));
        let adj = [0, 1, 0, 1, 1, 1, 1, 1, 1];
        assert!(
            !entry.matches(&adj),
            "does not match x0x when the top is solid"
        );
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
