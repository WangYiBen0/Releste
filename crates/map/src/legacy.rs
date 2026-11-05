//! **Frozen** copies of historical schema versions.
//!
//! `bincode` is not self-describing (ROADMAP Appendix E.6): touching a single field makes every old file
//! fail to decode. So "reading an old file" must go through **old structs + migration functions**, not through
//! `#[serde(default)]` (which never fires under bincode, E.9 #5).
//!
//! Once the structs here are released they **must not change again**: their byte layout is the on-disk
//! truth. All evolution happens in `crates/map/src/format.rs`, leaving a new copy here.
//!
//! Currently holding:
//! - [`v1`] -- the version before `nodes` / `style` / decals / decoration layers were introduced;
//! - [`v2`] -- the version before backdrop room filtering (`only` / `exclude`) and transition visibility
//!   (`instantIn` / `instantOut`) were introduced.

use serde::{Deserialize, Serialize};

use releste_world::{EntityId, FieldValue};

/// The complete on-disk structure when `MAP_VERSION == 1` (used only to decode old files).
///
/// The field order **corresponds one-to-one** to the declaration order of
/// `crates/map/src/format.rs` at v1; the order is the on-disk layout and must not be adjusted.
///
/// Note that [`v1::EntityData`] is v1's own copy, **not**
/// `releste_world::EntityData`: the latter gained `nodes` in v2, and reusing it directly would make
/// old files read misaligned starting at `nodes`. This is exactly why the "frozen copy" exists.
///
/// The one deliberate deviation: `properties` uses `BTreeMap` instead of the
/// `HashMap` written at v1. bincode encodes **both byte for byte identically** (length prefix + key/value
/// sequence), so the on-disk layout is unchanged; what we gain is no longer introducing `HashMap`'s random order.
pub mod v1 {
    use std::collections::BTreeMap;

    use super::*;

    /// v1 entity data (`releste_world::EntityData` minus `nodes`).
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct EntityData {
        pub id: EntityId,
        pub kind: String,
        pub x: f32,
        pub y: f32,
        pub width: Option<i32>,
        pub height: Option<i32>,
        pub origin_x: f32,
        pub origin_y: f32,
        pub properties: BTreeMap<String, FieldValue>,
    }

    /// On-disk format with the version header.
    #[derive(Debug, Serialize, Deserialize)]
    pub struct MapDisk {
        pub magic: [u8; 8],
        pub version: u32,
        pub document: MapDocument,
    }

    /// The v1 map document.
    #[derive(Debug, Serialize, Deserialize)]
    pub struct MapDocument {
        pub area: String,
        pub rooms: Vec<Room>,
        #[serde(default)]
        pub sync_rules: Vec<SyncRule>,
    }

    /// The v1 room.
    #[derive(Debug, Serialize, Deserialize)]
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
        pub script: Option<std::path::PathBuf>,
    }

    /// The v1 rectangle definition.
    #[derive(Debug, Serialize, Deserialize)]
    pub struct RectDef {
        pub x: f32,
        pub y: f32,
        pub w: f32,
        pub h: f32,
    }

    /// The v1 tile grid.
    #[derive(Debug, Default, Serialize, Deserialize)]
    pub struct Tileset {
        #[serde(default)]
        pub atlas: String,
        #[serde(default)]
        pub width: u32,
        #[serde(default)]
        pub height: u32,
        #[serde(default)]
        pub tiles: Vec<u8>,
    }

    /// The v1 trigger.
    #[derive(Debug, Serialize, Deserialize)]
    pub struct TriggerData {
        pub kind: String,
        pub x: f32,
        pub y: f32,
        pub width: f32,
        pub height: f32,
        #[serde(default)]
        pub properties: BTreeMap<String, String>,
    }

    /// The v1 sync rule.
    #[derive(Debug, Serialize, Deserialize)]
    pub struct SyncRule {
        pub kind: String,
        pub mode: SyncMode,
        #[serde(default)]
        pub flag: Option<String>,
    }

    /// The v1 sync mode.
    #[derive(Debug, Serialize, Deserialize)]
    pub enum SyncMode {
        Flag,
        Full,
        Event,
    }
}

/// The on-disk structure when `MAP_VERSION == 2` (used only to decode old files).
///
/// v2 -> v3 only appended four fields at the end of [`v2::Backdrop`] (room filtering + transition
/// visibility); every other structure is byte for byte identical -- so we directly **alias and reuse**
/// `crate::format`'s types; an alias is isomorphic and introduces no layout drift.
pub mod v2 {
    use super::*;

    /// `releste_world::EntityData` did not change between v2/v3.
    pub use releste_world::EntityData;
    pub type RectDef = crate::format::RectDef;
    pub type Tileset = crate::format::Tileset;
    pub type Decal = crate::format::Decal;
    pub type TriggerData = crate::format::TriggerData;
    pub type SyncRule = crate::format::SyncRule;
    pub type SyncMode = crate::format::SyncMode;
    pub type Room = crate::format::Room;

    /// On-disk format with the version header.
    #[derive(Debug, Serialize, Deserialize)]
    pub struct MapDisk {
        pub magic: [u8; 8],
        pub version: u32,
        pub document: MapDocument,
    }

    /// The v2 map document.
    #[derive(Debug, Serialize, Deserialize)]
    pub struct MapDocument {
        pub area: String,
        pub rooms: Vec<Room>,
        #[serde(default)]
        pub sync_rules: Vec<SyncRule>,
        #[serde(default)]
        pub style: MapStyle,
    }

    /// The v2 style (same as v3, only the backdrop type differs).
    #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
    pub struct MapStyle {
        #[serde(default)]
        pub background_color: [u8; 3],
        #[serde(default)]
        pub backgrounds: Vec<Backdrop>,
        #[serde(default)]
        pub foregrounds: Vec<Backdrop>,
    }

    /// The v2 backdrop: the complete field sequence before the four fields appended in v3.
    ///
    /// The field order corresponds one-to-one to `crates/map/src/format.rs` at v2; must not be adjusted.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Backdrop {
        pub kind: String,
        #[serde(default)]
        pub texture: String,
        #[serde(default)]
        pub atlas: String,
        #[serde(default)]
        pub x: f32,
        #[serde(default)]
        pub y: f32,
        #[serde(default)]
        pub scroll_x: f32,
        #[serde(default)]
        pub scroll_y: f32,
        #[serde(default)]
        pub speed_x: f32,
        #[serde(default)]
        pub speed_y: f32,
        #[serde(default)]
        pub color: [u8; 4],
        #[serde(default)]
        pub loop_x: bool,
        #[serde(default)]
        pub loop_y: bool,
        #[serde(default)]
        pub flip_x: bool,
        #[serde(default)]
        pub flip_y: bool,
        #[serde(default)]
        pub wind: f32,
        #[serde(default)]
        pub blend: String,
    }
}

impl From<v1::SyncMode> for crate::format::SyncMode {
    fn from(value: v1::SyncMode) -> Self {
        match value {
            v1::SyncMode::Flag => crate::format::SyncMode::Flag,
            v1::SyncMode::Full => crate::format::SyncMode::Full,
            v1::SyncMode::Event => crate::format::SyncMode::Event,
        }
    }
}

impl From<crate::format::SyncMode> for v1::SyncMode {
    fn from(value: crate::format::SyncMode) -> Self {
        match value {
            crate::format::SyncMode::Flag => v1::SyncMode::Flag,
            crate::format::SyncMode::Full => v1::SyncMode::Full,
            crate::format::SyncMode::Event => v1::SyncMode::Event,
        }
    }
}

impl From<v1::Tileset> for crate::format::Tileset {
    fn from(value: v1::Tileset) -> Self {
        crate::format::Tileset {
            atlas: value.atlas,
            width: value.width,
            height: value.height,
            // v1 was `Vec<u8>`; v2 widened it to `Vec<u16>` (decoration layer indices can reach 352).
            // Widen element by element: `u8` -> `u16` never loses information.
            tiles: value.tiles.into_iter().map(u16::from).collect(),
        }
    }
}

impl From<v1::EntityData> for releste_world::EntityData {
    fn from(value: v1::EntityData) -> Self {
        // `nodes` is expressiveness that only v2 has: it does not exist in old files, so it is empty after migration.
        releste_world::EntityData {
            id: value.id,
            kind: value.kind,
            x: value.x,
            y: value.y,
            width: value.width,
            height: value.height,
            origin_x: value.origin_x,
            origin_y: value.origin_y,
            properties: value.properties,
            nodes: Vec::new(),
        }
    }
}

/// Migrate a v1 document to v2 (frozen form).
///
/// Migration is a **pure function**: it only moves data, touching neither disk nor global state, so it can
/// be chained (`v0 -> v1 -> v2 -> v3 ...`, ROADMAP E.7).
///
/// Every field added in v2 takes the value "old files had no expressiveness for it" -- empty. Old versions
/// simply had no parallax backgrounds, decals or decoration layers, so a migrated document renders
/// **pixel-identical** to the old interpreter: nothing appears out of nowhere and nothing is lost.
///
/// Sources: `pureCeleste.cs:112593-112618` (`CreateBackdrops`,
/// the `<Style>` at `112428-112449`), `77244-77253` (`TileGrid.Overlay`),
/// `113467-113497` (`bgdecals` / `fgdecals` parsing),
/// `113135-113198` (`EntityData.Nodes`).
pub fn migrate_v1_to_v2(doc: v1::MapDocument) -> v2::MapDocument {
    v2::MapDocument {
        area: doc.area,
        rooms: doc
            .rooms
            .into_iter()
            .map(|room| crate::format::Room {
                name: room.name,
                bounds: crate::format::RectDef {
                    x: room.bounds.x,
                    y: room.bounds.y,
                    w: room.bounds.w,
                    h: room.bounds.h,
                },
                tiles: room.tiles.into(),
                bg: room.bg.into(),
                entities: room.entities.into_iter().map(Into::into).collect(),
                triggers: room
                    .triggers
                    .into_iter()
                    .map(|t| crate::format::TriggerData {
                        kind: t.kind,
                        x: t.x,
                        y: t.y,
                        width: t.width,
                        height: t.height,
                        properties: t.properties,
                    })
                    .collect(),
                script: room.script,
                bg_decals: Vec::new(),
                fg_decals: Vec::new(),
                bg_scenery: crate::format::Tileset::default(),
                fg_scenery: crate::format::Tileset::default(),
            })
            .collect(),
        sync_rules: doc
            .sync_rules
            .into_iter()
            .map(|r| crate::format::SyncRule {
                kind: r.kind,
                mode: r.mode.into(),
                flag: r.flag,
            })
            .collect(),
        style: v2::MapStyle::default(),
    }
}

/// Migrate a v2 document to the current version (v3).
///
/// v3 only appended room filtering and transition visibility to backdrop. Old files have no such
/// expressiveness, so they all take "unset": `only_in` / `exclude_from` empty (visible in all rooms),
/// `instant_in` takes the vanilla default `true` and `instant_out` the default `false`
/// (the field initialization at `pureCeleste.cs:109411-109418`) -- the migrated rendering is
/// pixel-identical to the v2 interpreter.
pub fn migrate_v2_to_v3(doc: v2::MapDocument) -> crate::format::MapDocument {
    fn backdrop(bd: v2::Backdrop) -> crate::format::Backdrop {
        crate::format::Backdrop {
            kind: bd.kind,
            texture: bd.texture,
            atlas: bd.atlas,
            x: bd.x,
            y: bd.y,
            scroll_x: bd.scroll_x,
            scroll_y: bd.scroll_y,
            speed_x: bd.speed_x,
            speed_y: bd.speed_y,
            color: bd.color,
            loop_x: bd.loop_x,
            loop_y: bd.loop_y,
            flip_x: bd.flip_x,
            flip_y: bd.flip_y,
            wind: bd.wind,
            blend: bd.blend,
            only_in: Vec::new(),
            exclude_from: Vec::new(),
            instant_in: true,
            instant_out: false,
        }
    }
    crate::format::MapDocument {
        area: doc.area,
        rooms: doc.rooms,
        sync_rules: doc.sync_rules,
        style: crate::format::MapStyle {
            background_color: doc.style.background_color,
            backgrounds: doc.style.backgrounds.into_iter().map(backdrop).collect(),
            foregrounds: doc.style.foregrounds.into_iter().map(backdrop).collect(),
        },
    }
}

/// Migrate a document of any version to the current version.
///
/// Chained: every added version adds one `v_n -> v_{n+1}` branch, and files older than N versions
/// can still be read without special cases (ROADMAP E.7).
pub fn migrate(version: u32, bytes: &[u8]) -> Result<crate::format::MapDocument, crate::MapError> {
    match version {
        0 => Err(crate::MapError::UnsupportedVersion { found: version }),
        1 => {
            let disk: v1::MapDisk = bincode::deserialize(bytes).map_err(crate::MapError::Parse)?;
            if disk.magic != *crate::format::MAP_MAGIC {
                return Err(crate::MapError::InvalidMagic);
            }
            Ok(migrate_v2_to_v3(migrate_v1_to_v2(disk.document)))
        }
        2 => {
            let disk: v2::MapDisk = bincode::deserialize(bytes).map_err(crate::MapError::Parse)?;
            if disk.magic != *crate::format::MAP_MAGIC {
                return Err(crate::MapError::InvalidMagic);
            }
            Ok(migrate_v2_to_v3(disk.document))
        }
        v => Err(crate::MapError::UnsupportedVersion { found: v }),
    }
}

/// Construction helpers for fixtures and tests.
///
/// Kept outside `cfg(test)`: the `tests/` integration tests and the fixture generator both need it to write
/// "real old files". Fixtures are produced by **downgrading** the current schema, so they never rot.
pub mod fixtures {
    use super::*;
    use crate::format::MapDocument as Current;

    /// v2 `u16` tile index -> v1 `u8`, kept only when it fits.
    ///
    /// This is a downgrade direction **only for test fixtures**: real migration only goes v1 -> v2.
    fn narrowed(tiles: &[u16]) -> Vec<u8> {
        tiles
            .iter()
            .map(|t| u8::try_from(*t).unwrap_or(0))
            .collect()
    }

    /// Drop what v2 added from the current document to get its v1 form.
    pub fn downgrade(doc: &Current) -> v1::MapDocument {
        v1::MapDocument {
            area: doc.area.clone(),
            rooms: doc
                .rooms
                .iter()
                .map(|r| v1::Room {
                    name: r.name.clone(),
                    bounds: v1::RectDef {
                        x: r.bounds.x,
                        y: r.bounds.y,
                        w: r.bounds.w,
                        h: r.bounds.h,
                    },
                    tiles: v1::Tileset {
                        atlas: r.tiles.atlas.clone(),
                        width: r.tiles.width,
                        height: r.tiles.height,
                        // Reverse narrowing: v2 -> v1 is lossless only when every index <= 255.
                        // The fixture uses its own narrow grid, so it never hits this.
                        tiles: narrowed(&r.tiles.tiles),
                    },
                    bg: v1::Tileset {
                        atlas: r.bg.atlas.clone(),
                        width: r.bg.width,
                        height: r.bg.height,
                        tiles: narrowed(&r.bg.tiles),
                    },
                    entities: r
                        .entities
                        .iter()
                        .map(|e| v1::EntityData {
                            id: e.id,
                            kind: e.kind.clone(),
                            x: e.x,
                            y: e.y,
                            width: e.width,
                            height: e.height,
                            origin_x: e.origin_x,
                            origin_y: e.origin_y,
                            properties: e.properties.clone(),
                        })
                        .collect(),
                    triggers: r
                        .triggers
                        .iter()
                        .map(|t| v1::TriggerData {
                            kind: t.kind.clone(),
                            x: t.x,
                            y: t.y,
                            width: t.width,
                            height: t.height,
                            properties: t.properties.clone(),
                        })
                        .collect(),
                    script: r.script.clone(),
                })
                .collect(),
            sync_rules: doc
                .sync_rules
                .iter()
                .map(|r| v1::SyncRule {
                    kind: r.kind.clone(),
                    mode: r.mode.clone().into(),
                    flag: r.flag.clone(),
                })
                .collect(),
        }
    }

    /// Write a v1 document to on-disk bytes (including the version header).
    pub fn to_v1_bytes(doc: &Current) -> Vec<u8> {
        let disk = v1::MapDisk {
            magic: *crate::format::MAP_MAGIC,
            version: 1,
            document: downgrade(doc),
        };
        bincode::serialize(&disk).expect("v1 fixture must serialize")
    }

    /// Write a document to bytes at an arbitrary version number (for "future version" fixtures).
    pub fn to_bytes_with_version(doc: &Current, version: u32) -> Vec<u8> {
        let disk = v1::MapDisk {
            magic: *crate::format::MAP_MAGIC,
            version,
            document: downgrade(doc),
        };
        bincode::serialize(&disk).expect("fixture must serialize")
    }

    /// Drop the four backdrop fields added in v3 from the current document to get its v2 form.
    pub fn downgrade_to_v2(doc: &Current) -> v2::MapDocument {
        fn backdrop(bd: &crate::format::Backdrop) -> v2::Backdrop {
            v2::Backdrop {
                kind: bd.kind.clone(),
                texture: bd.texture.clone(),
                atlas: bd.atlas.clone(),
                x: bd.x,
                y: bd.y,
                scroll_x: bd.scroll_x,
                scroll_y: bd.scroll_y,
                speed_x: bd.speed_x,
                speed_y: bd.speed_y,
                color: bd.color,
                loop_x: bd.loop_x,
                loop_y: bd.loop_y,
                flip_x: bd.flip_x,
                flip_y: bd.flip_y,
                wind: bd.wind,
                blend: bd.blend.clone(),
            }
        }
        v2::MapDocument {
            area: doc.area.clone(),
            rooms: doc.rooms.clone(),
            sync_rules: doc.sync_rules.clone(),
            style: v2::MapStyle {
                background_color: doc.style.background_color,
                backgrounds: doc.style.backgrounds.iter().map(backdrop).collect(),
                foregrounds: doc.style.foregrounds.iter().map(backdrop).collect(),
            },
        }
    }

    /// Write a v2 document to on-disk bytes (including the version header) -- the golden fixture for the v3 migration.
    pub fn to_v2_bytes(doc: &Current) -> Vec<u8> {
        let disk = v2::MapDisk {
            magic: *crate::format::MAP_MAGIC,
            version: 2,
            document: downgrade_to_v2(doc),
        };
        bincode::serialize(&disk).expect("v2 fixture must serialize")
    }

    /// The part that **must** be field-for-field equal after migration (except fields added in v2).
    ///
    /// Used by migration tests: current document -> downgrade to v1 bytes -> migrate back to v2; every field
    /// that v1 already had must be unchanged. `nodes` (added in v2) does not take part in the comparison.
    pub fn assert_v1_fields_survive(before: &Current, after: &Current) -> Result<(), String> {
        if before.area != after.area {
            return Err(format!("area: {:?} != {:?}", before.area, after.area));
        }
        if before.rooms.len() != after.rooms.len() {
            return Err(format!(
                "room count: {} != {}",
                before.rooms.len(),
                after.rooms.len()
            ));
        }
        for (a, b) in before.rooms.iter().zip(after.rooms.iter()) {
            if a.name != b.name {
                return Err(format!("room name: {} != {}", a.name, b.name));
            }
            if a.bounds != b.bounds {
                return Err(format!("room {} bounds differ", a.name));
            }
            if a.tiles != b.tiles {
                return Err(format!("room {} tiles differ", a.name));
            }
            if a.bg != b.bg {
                return Err(format!("room {} bg differ", a.name));
            }
            if a.triggers != b.triggers {
                return Err(format!("room {} triggers differ", a.name));
            }
            if a.script != b.script {
                return Err(format!("room {} script differ", a.name));
            }
            if a.entities.len() != b.entities.len() {
                return Err(format!("room {} entity count differs", a.name));
            }
            for (ea, eb) in a.entities.iter().zip(b.entities.iter()) {
                if ea.id != eb.id
                    || ea.kind != eb.kind
                    || ea.x != eb.x
                    || ea.y != eb.y
                    || ea.width != eb.width
                    || ea.height != eb.height
                    || ea.origin_x != eb.origin_x
                    || ea.origin_y != eb.origin_y
                    || ea.properties != eb.properties
                {
                    return Err(format!("entity {} in room {} differs", ea.id, a.name));
                }
            }
        }
        if before.sync_rules != after.sync_rules {
            return Err("sync_rules differ after migration".to_string());
        }
        Ok(())
    }
}
