//! `.map` file format definition.
//!
//! The on-disk layout is the field declaration order (bincode is not self-describing, ROADMAP Appendix E.6):
//! **append only at the end, never insert in the middle, never delete**. Any change is a
//! [`MAP_VERSION`] bump plus one frozen copy kept in [`crate::legacy`] plus
//! one `v_n -> v_{n+1}` migration.
//!
//! Appended in v2 relative to v1 (all at the end of their respective structs):
//! - [`MapDocument::style`] -- area-level parallax background (`<Style>`, `pureCeleste.cs:112428-112449`);
//! - [`Room::bg_decals`] / [`Room::fg_decals`] -- decals (`113467-113497`);
//! - [`Room::bg_scenery`] / [`Room::fg_scenery`] -- `Scenery` decoration layer overrides (`77244-77253`);
//! - [`EntityData::nodes`] -- entity nodes (the `introCrusher` endpoint, the `wire` polyline, the `bridge` piers).
//!
//! Appended in v3 relative to v2 (all at the end of [`Backdrop`]):
//! - [`Backdrop::only_in`] / [`Backdrop::exclude_from`] -- room filtering
//!   (vanilla `Backdrop.OnlyIn` / `ExcludeFrom`, `109406-109409`, from
//!   `only=` / `exclude=`, `112860-112873`; wildcards are expanded at conversion time, see
//!   `ParseLevelsList`, `113019-113036`);
//! - [`Backdrop::instant_in`] / [`Backdrop::instant_out`] -- transition visibility
//!   (vanilla `InstantIn` / `InstantOut`, `109411-109418`, from
//!   `instantIn=` / `instantOut=`, `112895-112905`).
//!
//! At the same time `properties` / `Props.fields` moves from `HashMap` to `BTreeMap`:
//! `HashMap` iterates in `RandomState` order, so two serializations of the same map
//! differ byte for byte and content hashing is therefore unusable (ROADMAP E.8 / E.9 #3),
//! and since `HashMap`'s `PartialEq` ignores order the roundtrip test in AGENTS.md 9 never caught it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use releste_math::{Rect, Vec2};
use releste_world::{EntityData, FieldValue};

/// Magic bytes: `"RELMAP\0\0"`
pub const MAP_MAGIC: &[u8; 8] = b"RELMAP\x00\x00";

/// Current `.map` format version.
///
/// v3: room filtering (`only` / `exclude`) and transition visibility for backdrops
/// (`instantIn` / `instantOut`).
pub const MAP_VERSION: u32 = 3;

/// Complete structure of a `.map` file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MapDocument {
    pub area: String,
    pub rooms: Vec<Room>,
    /// Sync rules (present in v1 already; the declaration position stays the same).
    #[serde(default)]
    pub sync_rules: Vec<SyncRule>,
    /// Area-level style: clear color and parallax background (appended in v2).
    #[serde(default)]
    pub style: MapStyle,
}

/// Style of a single map (from the vanilla `<Style>` element in `MapData`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MapStyle {
    /// Clear color (RGB).
    ///
    /// Vanilla default is `Color.Black`; only `<Style color="...">` overrides it
    /// (default at `pureCeleste.cs:112310`, override at `112432-112435`).
    #[serde(default)]
    pub background_color: [u8; 3],
    /// Backgrounds drawn **after** the world (vanilla `<Backgrounds>`).
    #[serde(default)]
    pub backgrounds: Vec<Backdrop>,
    /// Foregrounds drawn **before** the world (vanilla `<Foregrounds>`).
    #[serde(default)]
    pub foregrounds: Vec<Backdrop>,
}

/// One background / foreground layer.
///
/// `kind` is the vanilla element name (`"parallax"`, `"snowBg"`, `"snowFg"`...);
/// the engine decides behavior from it; every field not written in the file falls back to the vanilla default.
///
/// Default value sources: `pureCeleste.cs:109341-109430` (`Backdrop` field defaults),
/// `112620-112900` (`ParseBackdrop` per-attribute overrides).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Backdrop {
    /// Element name (compared lowercased, same as `ParseBackdrop`).
    pub kind: String,
    /// Texture name (`parallax texture="..."`).
    #[serde(default)]
    pub texture: String,
    /// Source atlas of the texture: `"game"` / `"gui"` / `"misc"` (`112635-112646`).
    #[serde(default)]
    pub atlas: String,
    /// Position (`x` / `y`, default 0).
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    /// Scroll factor (`scrollx` / `scrolly`, default 1 -- i.e. it follows the camera).
    #[serde(default)]
    pub scroll_x: f32,
    #[serde(default)]
    pub scroll_y: f32,
    /// Self-drift speed (`speedx` / `speedy`, default 0).
    #[serde(default)]
    pub speed_x: f32,
    #[serde(default)]
    pub speed_y: f32,
    /// Color (`color`, default white).
    #[serde(default)]
    pub color: [u8; 4],
    /// Whether to loop on X / Y (`loopx` / `loopy`; vanilla default is **true for both**).
    #[serde(default)]
    pub loop_x: bool,
    #[serde(default)]
    pub loop_y: bool,
    /// Whether it is flipped (`flipx` / `flipy`).
    #[serde(default)]
    pub flip_x: bool,
    #[serde(default)]
    pub flip_y: bool,
    /// Wind multiplier (`wind`).
    #[serde(default)]
    pub wind: f32,
    /// Blend mode: `"alphablend"` (default) or `"additive"` (`112658-112673`).
    #[serde(default)]
    pub blend: String,
    /// Render only in these rooms (appended in v3; vanilla `Backdrop.OnlyIn`, `109406`,
    /// `only=`, `112860-112873`). Wildcards were already expanded to concrete room names at conversion time.
    #[serde(default)]
    pub only_in: Vec<String>,
    /// Do not render in these rooms (appended in v3; vanilla `Backdrop.ExcludeFrom`, `109409`,
    /// `exclude=`, `112855-112860`).
    #[serde(default)]
    pub exclude_from: Vec<String>,
    /// Immediately visible when a transition cuts in (appended in v3; vanilla `InstantIn`, default `true`,
    /// `109411`, `112895-112900`).
    #[serde(default = "default_true")]
    pub instant_in: bool,
    /// Immediately hidden when a transition cuts out (appended in v3; vanilla `InstantOut`, default `false`,
    /// `109413`, `112900-112905`).
    #[serde(default)]
    pub instant_out: bool,
}

/// serde default helper: `true`.
fn default_true() -> bool {
    true
}

impl Backdrop {
    /// Vanilla default color: white (`112793`).
    pub const WHITE: [u8; 4] = [255, 255, 255, 255];

    /// Create a new one (an empty color means "use the vanilla default white").
    pub fn new(kind: impl Into<String>) -> Self {
        Backdrop {
            kind: kind.into(),
            texture: String::new(),
            atlas: "game".to_string(),
            x: 0.0,
            y: 0.0,
            scroll_x: 1.0,
            scroll_y: 1.0,
            speed_x: 0.0,
            speed_y: 0.0,
            color: [0, 0, 0, 0],
            loop_x: true,
            loop_y: true,
            flip_x: false,
            flip_y: false,
            wind: 0.0,
            blend: "alphablend".to_string(),
            only_in: Vec::new(),
            exclude_from: Vec::new(),
            instant_in: true,
            instant_out: false,
        }
    }

    /// Effective color: all-zero ("unset") is treated as the vanilla default white.
    pub fn effective_color(&self) -> [u8; 4] {
        if self.color == [0, 0, 0, 0] {
            Self::WHITE
        } else {
            self.color
        }
    }
}

/// A single decal (vanilla `DecalData`, `pureCeleste.cs:113199-113206`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decal {
    /// Texture name (relative to `decals/`, may carry a `.png` suffix; vanilla strips it).
    pub texture: String,
    pub x: f32,
    pub y: f32,
    /// Scale; `scale_x == -1` means horizontal mirror (vanilla feeds it straight to `SpriteBatch.Draw`'s `scale`).
    pub scale_x: f32,
    pub scale_y: f32,
}

/// A single room.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub name: String,
    pub bounds: RectDef,
    pub tiles: Tileset,
    #[serde(default)]
    pub bg: Tileset,
    #[serde(default)]
    pub entities: Vec<EntityData>,
    #[serde(default)]
    pub triggers: Vec<TriggerData>,
    #[serde(default)]
    pub script: Option<PathBuf>,
    /// Background decals (vanilla `bgdecals`, Depth 9000, `115083-115086`).
    #[serde(default)]
    pub bg_decals: Vec<Decal>,
    /// Foreground decals (vanilla `fgdecals`, Depth -10500, `115079-115082`).
    #[serde(default)]
    pub fg_decals: Vec<Decal>,
    /// `bgtiles` decoration layer (`Scenery` atlas, `77244-77248`).
    #[serde(default)]
    pub bg_scenery: Tileset,
    /// `fgtiles` decoration layer (same as above, `77249-77254`).
    #[serde(default)]
    pub fg_scenery: Tileset,
}

impl Room {
    /// World coordinate of the room's top-left corner.
    pub fn origin(&self) -> Vec2 {
        Vec2::from_f32s(self.bounds.x, self.bounds.y)
    }
}

/// Rectangle definition (f32, map coordinates).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct RectDef {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectDef {
    pub fn to_rect(self) -> Rect {
        Rect::new(
            Vec2::from_f32s(self.x, self.y),
            Vec2::from_f32s(self.x + self.w, self.y + self.h),
        )
    }
}

/// Tile set definition.
///
/// Row-major grid of tile indices; `0` means empty. The meaning of an index is
/// decided by `atlas` (the engine interprets it in the render layer; the map layer does not understand art).
///
/// The convention that `0` is "empty" is Releste's own (AGENTS.md 4.2: the map layer does not
/// understand art); the converter is responsible for squashing the vanilla `-1` ("do not override") into `0`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Tileset {
    /// Texture atlas name (e.g. `"Gameplay"`).
    #[serde(default)]
    pub atlas: String,
    /// Grid width (in tiles).
    #[serde(default)]
    pub width: u32,
    /// Grid height (in tiles).
    #[serde(default)]
    pub height: u32,
    /// Row-major tile indices; the length should be `width * height`.
    ///
    /// `u16` rather than `u8`: decoration layer indices count the whole `tilesets/scenery`
    /// (the flat index of `Tileset.this[int]`, `pureCeleste.cs:15215-15226`);
    /// the vanilla image has 360 cells (30x12), and `5-MirrorTemple` uses indices up to 352.
    /// The solid layers (`solids`/`bg`) use the 0-9a-z alphabet, max 35, which fits.
    #[serde(default)]
    pub tiles: Vec<u16>,
}

impl Tileset {
    /// Create a new grid.
    pub fn new(atlas: impl Into<String>, width: u32, height: u32) -> Self {
        Tileset {
            atlas: atlas.into(),
            width,
            height,
            tiles: vec![0; (width as usize) * (height as usize)],
        }
    }

    /// Get a tile.
    pub fn get(&self, x: u32, y: u32) -> Option<u16> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.tiles
            .get((y as usize) * (self.width as usize) + (x as usize))
            .copied()
    }

    /// Set a tile.
    pub fn set(&mut self, x: u32, y: u32, value: u16) {
        if x < self.width && y < self.height {
            let idx = (y as usize) * (self.width as usize) + (x as usize);
            if let Some(slot) = self.tiles.get_mut(idx) {
                *slot = value;
            }
        }
    }

    /// Whether there is any non-empty tile.
    pub fn is_empty(&self) -> bool {
        self.tiles.iter().all(|t| *t == 0)
    }
}

/// Trigger data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TriggerData {
    pub kind: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
}

/// Sync rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncRule {
    pub kind: String,
    pub mode: SyncMode,
    #[serde(default)]
    pub flag: Option<String>,
}

/// Sync mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    Flag,
    Full,
    Event,
}

/// Entity property set (used by diff).
///
/// `BTreeMap` rather than `HashMap`: see the byte-determinism note at the top of this module.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Props {
    pub fields: BTreeMap<String, FieldValue>,
}

impl Props {
    /// Extract properties from entity data.
    pub fn from_entity(entity: &EntityData) -> Self {
        Props {
            fields: entity.properties.clone(),
        }
    }
}
