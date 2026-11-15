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
/// Source: `pureCeleste.cs:111066` `public const string Hair = "characters/player/hair00"`;
/// `pureCeleste.cs:111071` `bangs = GFX.Game.GetAtlasSubtextures("characters/player/bangs")`.
const VANILLA_HAIR: &str = crate::sprites::PLAYER_HAIR_SPRITE;
const VANILLA_BANGS: [&str; 3] = [
    crate::sprites::PLAYER_BANGS_SPRITE,
    "characters\\player\\bangs01",
    "characters\\player\\bangs02",
];

/// A selectable player hair skin: an atlas sub-namespace prepended to the vanilla
/// `characters/player/...` hair names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayerHairSkin {
    /// Atlas path prefix inserted before `characters\...` (a `Gameplay/<skin>/` atlas
    /// group). The vanilla player uses the empty string.
    prefix: &'static str,
}

impl PlayerHairSkin {
    /// The vanilla player character: no prefix, so names match the existing constants.
    pub const VANILLA: Self = Self { prefix: "" };

    /// A skin whose hair lives under the given atlas sub-namespace prefix
    /// (e.g. `"Foxeline"` for `Gameplay/Foxeline/`).
    ///
    /// Unused in the running binary because no skin is selectable yet (the atlas ships
    /// no alt-hair group, and skin selection is the next step); it is the constructor a
    /// future `settings.toml` `skin.prefix` wiring will call, and is exercised by the
    /// unit tests below. AGENTS.md 5.1 allows a scoped `#[allow]` with a clear reason.
    #[allow(dead_code)]
    pub const fn with_prefix(prefix: &'static str) -> Self {
        Self { prefix }
    }

    /// The resolved hair-tail texture name for this skin. The vanilla skin returns the
    /// existing `PLAYER_HAIR_SPRITE` constants unchanged; a prefixed skin returns
    /// `<prefix>\characters\player\hair00`.
    pub fn hair(&self) -> Cow<'static, str> {
        if self.prefix.is_empty() {
            Cow::Borrowed(VANILLA_HAIR)
        } else {
            Cow::Owned(format!("{}\\characters\\player\\hair00", self.prefix,))
        }
    }

    /// The resolved bangs texture name for this skin at hair `frame` (clamped to
    /// `0..2`), mirroring `bangs[Sprite.HairFrame]` (`pureCeleste.cs:111186, 111196`).
    pub fn bangs(&self, frame: usize) -> Cow<'static, str> {
        let idx = frame.min(2);
        if self.prefix.is_empty() {
            Cow::Borrowed(VANILLA_BANGS[idx])
        } else {
            Cow::Owned(format!(
                "{}\\characters\\player\\bangs0{}",
                self.prefix, idx,
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
        let fox = PlayerHairSkin::with_prefix("Foxeline");
        assert_eq!(fox.hair(), "Foxeline\\characters\\player\\hair00");
        assert_eq!(fox.bangs(0), "Foxeline\\characters\\player\\bangs00");
        assert_eq!(fox.bangs(1), "Foxeline\\characters\\player\\bangs01");
        assert_eq!(fox.bangs(2), "Foxeline\\characters\\player\\bangs02");
        // Like vanilla's `HairFrame` metadata, the frame index is clamped to 0..2:
        // a walk-loop frame index of 3+ still resolves to bangs02.
        assert_eq!(fox.bangs(3), "Foxeline\\characters\\player\\bangs02");
    }
}
