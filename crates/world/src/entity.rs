//! Entity and component data.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::factory::EntityKindId;
use crate::id::EntityId;
use crate::schema::FieldValue;

/// Runtime entity.
///
/// Built by `EntityFactory::build` and stored in `World`.
#[derive(Debug, Clone)]
pub struct Entity {
    pub id: EntityId,
    pub kind_id: EntityKindId,
    pub properties: BTreeMap<String, FieldValue>,
}

impl Entity {
    pub fn new(id: EntityId, kind_id: EntityKindId) -> Self {
        Entity {
            id,
            kind_id,
            properties: BTreeMap::new(),
        }
    }

    /// Get a property.
    pub fn get(&self, key: &str) -> Option<&FieldValue> {
        self.properties.get(key)
    }

    /// Set a property.
    pub fn set(&mut self, key: impl Into<String>, value: FieldValue) {
        self.properties.insert(key.into(), value);
    }
}

/// Entity data from a map (before it is built into an [`Entity`]).
///
/// This structure can be deserialized from a `.map` file.
///
/// **Field order is the on-disk layout** (bincode is not self-describing,
/// ROADMAP Appendix E.6): new fields may only be appended to the end, and the
/// `legacy` module keeps a frozen copy for each historical version.
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
    /// The property table uses `BTreeMap`: `HashMap` iteration order is random
    /// per process, which would serialize the same map to different bytes
    /// (ROADMAP E.8 / E.9 #3).
    pub properties: BTreeMap<String, FieldValue>,
    /// Entity nodes (appended in v2, the `node` child element).
    ///
    /// Vanilla `EntityData.Nodes` (`pureCeleste.cs:113135-113198`); the three
    /// most common consumers: `IntroCrusher` landing point
    /// (`106324-106327`), `Wire` polyline (`106674-106704`), `Bridge` piers
    /// (`107679`).
    ///
    /// The coordinates are in the **same coordinate space** as `x`/`y` (room
    /// local, may extend beyond the room bounds): vanilla consumers write
    /// `data.Nodes[0] + offset` (offset = room offset, added only once), fully
    /// symmetric with `data.Position + offset` -- nodes are absolute room
    /// coordinates, **not** offsets relative to the entity position.
    #[serde(default)]
    pub nodes: Vec<(f32, f32)>,
}

impl EntityData {
    pub fn new(kind: impl Into<String>, id: EntityId, x: f32, y: f32) -> Self {
        EntityData {
            id,
            kind: kind.into(),
            x,
            y,
            width: None,
            height: None,
            origin_x: 0.0,
            origin_y: 0.0,
            properties: BTreeMap::new(),
            nodes: Vec::new(),
        }
    }
}
