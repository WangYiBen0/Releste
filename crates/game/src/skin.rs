//! A reskin of the playable character's hair (bangs + tail), ported faithfully from
//! the Everest Skin Mod Helper's hair override.
//!
//! SkinModHelper replaces the vanilla hair textures by prepending a skin-specific
//! atlas path and falling back to the vanilla textures when the prefixed path is
//! absent (`references/SkinModHelper/Code/SkinModHelperModule.cs:205-222`):
//!
//! ```csharp
//! string newBangsPath = GetUniquePath() + "characters/player/bangs";
//! string newHairPath  = GetUniquePath() + "characters/player/hair00";
//! if (GFX.Game.Has(newHairPath)) return GFX.Game[newHairPath];
//! return orig(self, index);
//! ```
//!
//! This module mirrors that contract in engine form. The vanilla skin keeps an empty
//! prefix, so its resolved names are byte-for-byte the existing constants; a
//! non-default skin (e.g. the `Foxeline` `Gameplay/Foxeline/` atlas group,
//! `references/Foxeline/Graphics/Atlases/Gameplay/Foxeline/bangs/bangs00.png`)
//! prepends the prefix to the `<prefix>/characters/player/...` names. A caller still
//! resolves each name against the session atlas and falls back to the vanilla name
//! when the prefixed one is missing, exactly as the mod's `GFX.Game.Has` check does.

use std::borrow::Cow;

/// The vanilla hair atlas names, as in `crate::sprites` (`PLAYER_HAIR_SPRITE`,
/// `PLAYER_BANGS_SPRITE`), with the three bangs frames spelled out.
///
/// Source: `references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:11` `public const string Hair = "characters/player/hair00"`;
/// `references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:37` `bangs = GFX.Game.GetAtlasSubtextures("characters/player/bangs")`.
const VANILLA_HAIR: &str = crate::sprites::PLAYER_HAIR_SPRITE;
const VANILLA_BANGS: [&str; 3] = [
    crate::sprites::PLAYER_BANGS_SPRITE,
    "characters\\player\\bangs01",
    "characters\\player\\bangs02",
];

/// A selectable player hair skin: an atlas sub-namespace prepended to the vanilla
/// `characters/player/...` hair names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerHairSkin {
    /// Atlas path prefix inserted before `characters\...` (a `Gameplay/<skin>/` atlas
    /// group). The vanilla player uses the empty string.
    prefix: Cow<'static, str>,
}

impl PlayerHairSkin {
    /// The vanilla player character: no prefix, so names match the existing constants.
    pub const VANILLA: Self = Self {
        prefix: Cow::Borrowed(""),
    };

    /// Build a skin from a runtime-provided prefix (e.g. a CLI `--skin` argument, or a
    /// future `settings.toml` value), so the selection does not have to be a
    /// compile-time constant. An empty prefix is the vanilla skin.
    pub fn from_prefix(prefix: String) -> Self {
        Self {
            prefix: if prefix.is_empty() {
                Cow::Borrowed("")
            } else {
                Cow::Owned(prefix)
            },
        }
    }

    /// The atlas prefix this skin prepends to the vanilla player names; empty for the
    /// vanilla player. Used when the skin has to be re-resolved from settings (hot
    /// reload) rather than kept as a name.
    pub fn prefix(&self) -> &str {
        self.prefix.as_ref()
    }

    /// The resolved hair-tail texture name for this skin. The vanilla skin returns the
    /// existing `PLAYER_HAIR_SPRITE` constants unchanged; a prefixed skin returns
    /// `<prefix>\characters\player\hair00`.
    pub fn hair(&self) -> Cow<'static, str> {
        if self.prefix.is_empty() {
            Cow::Borrowed(VANILLA_HAIR)
        } else {
            Cow::Owned(format!(
                "{}\\characters\\player\\hair00",
                self.prefix.as_ref(),
            ))
        }
    }

    /// The resolved bangs texture name for this skin at hair `frame` (clamped to
    /// `0..2`), mirroring `bangs[Sprite.HairFrame]` (`references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:132, references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:142`).
    pub fn bangs(&self, frame: usize) -> Cow<'static, str> {
        let idx = frame.min(2);
        if self.prefix.is_empty() {
            Cow::Borrowed(VANILLA_BANGS[idx])
        } else {
            Cow::Owned(format!(
                "{}\\characters\\player\\bangs0{}",
                self.prefix.as_ref(),
                idx,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vanilla_skin_resolves_to_the_existing_constants() {
        let vanilla = PlayerHairSkin::VANILLA;
        assert_eq!(vanilla.hair(), crate::sprites::PLAYER_HAIR_SPRITE);
        // The constants live in `sprites`, which spells the default bangs frame out the
        // same way this module does.
        assert_eq!(vanilla.bangs(0), crate::sprites::PLAYER_BANGS_SPRITE);
    }

    #[test]
    fn prefixed_skin_prepends_the_prefix_to_every_hair_name() {
        let fox = PlayerHairSkin::from_prefix("Foxeline".to_owned());
        assert_eq!(fox.hair(), "Foxeline\\characters\\player\\hair00");
        assert_eq!(fox.bangs(0), "Foxeline\\characters\\player\\bangs00");
        assert_eq!(fox.bangs(1), "Foxeline\\characters\\player\\bangs01");
        assert_eq!(fox.bangs(2), "Foxeline\\characters\\player\\bangs02");
        // Like vanilla's `HairFrame` metadata, the frame index is clamped to 0..2:
        // a walk-loop frame index of 3+ still resolves to bangs02.
        assert_eq!(fox.bangs(3), "Foxeline\\characters\\player\\bangs02");
    }
}
