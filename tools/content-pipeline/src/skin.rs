//! Skin packs: an Everest Skin Mod Helper skin converted into engine-native assets.
//!
//! A skin pack is an Everest mod directory that ships
//! `SkinModHelperConfig.yaml` (`references/SkinModHelper/Code/SkinModHelperModule.cs`
//! reads it) plus its atlas images and `Graphics/Sprites.xml`. This module reads those
//! **original** formats and nothing else does (AGENTS.md 4.6: original formats are
//! build-time only).
//!
//! What a skin can express here is what a pure asset pack can: a whole replacement
//! sprite set (the player's `Character_ID`, per
//! `references/SkinModHelperPlus.../SkinsSystem`: `GFX.SpriteBank.CreateOn(self,
//! value.Character_ID)`), the other scene sprites it lists (`OtherSprite_Path`), and the
//! per-dash hair colours of the base helper
//! (`references/SkinModHelper/Code/SkinModHelperModule.cs:116-120`). Anything needing
//! simulation (a Foxeline tail, a Hyperline colour algorithm) belongs in a plugin.
//!
//! The manifest is YAML, so this module is the only place `serde_yaml` is used.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::PipelineError;

/// One entry of `SkinModHelperConfig.yaml`.
///
/// Source: `references/SkinModHelper/Code/SkinModHelperConfig.cs:5-19` plus the
/// `SkinModHelperPlus` extension fields (`Character_ID`, `OtherSprite_Path`,
/// `OtherSprite_ExPath`), read in `references/Hyperline/Source/Imports/
/// SkinModHelperPlus.cs` and the decompiled `SkinsSystem`.
#[derive(Debug, Clone, Deserialize)]
pub struct SkinConfigEntry {
    /// Display name and the id suffix used for `<spriteId>_<SkinName>` aliases.
    #[serde(rename = "SkinName")]
    pub skin_name: String,
    /// The `Graphics/Sprites.xml` sprite id that becomes the player (`"niko"`). When
    /// absent this entry only reskins other scene sprites.
    #[serde(rename = "Character_ID", default)]
    pub character_id: Option<String>,
    /// Atlas namespace of the other-sprite set (e.g. `"Zaro/niko"`), whose
    /// `Sprites.xml`/atlas live under `Graphics/Atlases/<namespace>/` and
    /// `Graphics/<namespace>/Sprites.xml`.
    #[serde(rename = "OtherSprite_Path", default)]
    pub other_sprite_path: Option<String>,
    /// Like [`SkinConfigEntry::other_sprite_path`] but for a texture pack that must reach
    /// every namespace (`OtherSprite_ExPath`).
    #[serde(rename = "OtherSprite_ExPath", default)]
    pub other_sprite_ex_path: Option<String>,
}

/// The raw `SkinModHelperConfig.yaml`: a list of skin entries.
#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub struct SkinConfigFile {
    pub entries: Vec<SkinConfigEntry>,
}

impl SkinConfigFile {
    /// Read and parse `SkinModHelperConfig.yaml`.
    pub fn from_file(path: &Path) -> Result<Self, PipelineError> {
        let text =
            std::fs::read_to_string(path).map_err(|e| PipelineError::io(path.display(), e))?;
        serde_yaml::from_str(&text).map_err(|e| {
            PipelineError::parse("SkinModHelperConfig.yaml", path.display(), e.to_string())
        })
    }
}

/// The hair colours that the base helper applies per dash count.
///
/// Source: `references/SkinModHelper/Code/SkinModHelperModule.cs:116-120` seeds
/// `[0]=44B7FF` (used), `[1]=AC3232` (normal), `[2]=FF6DEF` (two dashes); a
/// `SkinModHelperConfig.yaml` `HairColors` list then overrides individual counts
/// (124-134) and upper counts inherit the last set value (137-141). This engine has one
/// dash in the Prologue, so 0 and 1 are the ones that matter, but all three are kept.
pub fn default_hair_colors() -> BTreeMap<u8, [u8; 3]> {
    BTreeMap::from([
        (0, [0x44, 0xB7, 0xFF]),
        (1, [0xAC, 0x32, 0x32]),
        (2, [0xFF, 0x6D, 0xEF]),
    ])
}

/// Parse `RRGGBB` (no `#`) into RGB bytes.
pub fn parse_hex_color(hex: &str) -> Option<[u8; 3]> {
    let b = hex.as_bytes();
    if b.len() != 6 || !b.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some([r, g, b])
}

/// A candidate skin pack directory: any mod dir that has a
/// `SkinModHelperConfig.yaml` at its root.
pub fn find_skin_packs(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.join("SkinModHelperConfig.yaml").is_file() {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_colours_parse_and_reject_bad_input() {
        assert_eq!(parse_hex_color("44B7FF"), Some([0x44, 0xB7, 0xFF]));
        assert_eq!(parse_hex_color("#44B7FF"), None);
        assert_eq!(parse_hex_color("44B7F"), None);
        assert_eq!(parse_hex_color("GGGGGG"), None);
    }

    #[test]
    fn yaml_matches_the_real_niko_pack_shape() {
        // Shape copied from references/Celeste-with-Everest/Mods/
        // Niko_-_Celeste_Skin-Helper.zip -> SkinModHelperConfig.yaml.
        let text = r#"
- SkinName: "Zaro_Niko"
  Player_List: true
  Silhouette_List: false
  Character_ID: "niko"
  OtherSprite_Path: "Zaro/niko"
  SkinDialogKey: "Zaro_Niko"
- SkinName: "Zaro_Niko_TP"
  General_List: true
  OtherSprite_ExPath: "Zaro/niko_texturepack"
"#;
        let file: SkinConfigFile = serde_yaml::from_str(text).unwrap();
        assert_eq!(file.entries.len(), 2);
        assert_eq!(file.entries[0].skin_name, "Zaro_Niko");
        assert_eq!(file.entries[0].character_id.as_deref(), Some("niko"));
        assert_eq!(
            file.entries[0].other_sprite_path.as_deref(),
            Some("Zaro/niko")
        );
        assert_eq!(file.entries[1].character_id, None);
        assert_eq!(
            file.entries[1].other_sprite_ex_path.as_deref(),
            Some("Zaro/niko_texturepack")
        );
    }

    #[test]
    fn default_hair_colors_match_the_helper_seed() {
        let c = default_hair_colors();
        assert_eq!(c.get(&0), Some(&[0x44, 0xB7, 0xFF]));
        assert_eq!(c.get(&1), Some(&[0xAC, 0x32, 0x32]));
        assert_eq!(c.get(&2), Some(&[0xFF, 0x6D, 0xEF]));
    }
}
