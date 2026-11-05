//! World: the ECS container.
//!
//! Manages all runtime entities. Provides ID-based lookup APIs for hot reload.

use std::collections::HashMap;

use slotmap::{DefaultKey, SlotMap};

use crate::entity::Entity;
use crate::factory::EntityKindId;
use crate::id::EntityId;
use crate::schema::FieldValue;
use releste_math::Rect;

/// World -- the runtime container for all entities.
///
/// Entities are stored in a `SlotMap`, doubly indexed by [`EntityId`] (stable
/// ID) and [`EntityKindId`] (kind ID).
#[derive(Debug, Default)]
pub struct World {
    entities: SlotMap<DefaultKey, Entity>,
    /// Stable ID -> SlotMap Key.
    id_to_key: HashMap<EntityId, DefaultKey>,
    /// Kind ID -> SlotMap Keys of all entities of that kind.
    kind_index: HashMap<EntityKindId, Vec<DefaultKey>>,
    /// Player entity ID (used for push-out during hot reload conflict
    /// resolution).
    player: Option<EntityId>,
}

impl World {
    /// Create an empty world.
    pub fn new() -> Self {
        World {
            entities: SlotMap::with_key(),
            id_to_key: HashMap::new(),
            kind_index: HashMap::new(),
            player: None,
        }
    }

    /// Insert an entity.
    pub fn insert(&mut self, entity: Entity) -> EntityId {
        let id = entity.id;
        let kind = entity.kind_id;
        let key = self.entities.insert(entity);
        self.id_to_key.insert(id, key);
        self.kind_index.entry(kind).or_default().push(key);
        id
    }

    /// Remove an entity.
    pub fn remove(&mut self, id: EntityId) -> Option<Entity> {
        let key = self.id_to_key.remove(&id)?;
        let entity = self.entities.remove(key)?;
        if let Some(list) = self.kind_index.get_mut(&entity.kind_id) {
            list.retain(|k| *k != key);
        }
        Some(entity)
    }

    /// Look up by stable ID.
    pub fn get(&self, id: EntityId) -> Option<&Entity> {
        self.id_to_key
            .get(&id)
            .and_then(|key| self.entities.get(*key))
    }

    /// Look up a mutable reference by stable ID.
    pub fn get_mut(&mut self, id: EntityId) -> Option<&mut Entity> {
        self.id_to_key
            .get(&id)
            .and_then(|key| self.entities.get_mut(*key))
    }

    /// Find all entities of a kind.
    pub fn by_kind(&self, kind: EntityKindId) -> impl Iterator<Item = &Entity> {
        self.kind_index
            .get(&kind)
            .into_iter()
            .flat_map(|keys| keys.iter())
            .filter_map(|key| self.entities.get(*key))
    }

    /// Iterate over all entities.
    pub fn all(&self) -> impl Iterator<Item = &Entity> {
        self.entities.values()
    }

    /// Total number of entities.
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Whether the world is empty.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Remove all entities.
    pub fn clear(&mut self) {
        self.entities.clear();
        self.id_to_key.clear();
        self.kind_index.clear();
    }

    /// Whether the given stable ID exists.
    pub fn contains(&self, id: EntityId) -> bool {
        self.id_to_key.contains_key(&id)
    }

    /// Set the player entity ID.
    pub fn set_player(&mut self, id: EntityId) {
        self.player = Some(id);
    }

    /// The player entity ID.
    pub fn player(&self) -> Option<EntityId> {
        self.player
    }

    /// World AABB of an entity.
    ///
    /// Falls back to 8x8 units (roughly half a tile) when the size is missing,
    /// so the hot reload push-out logic always has a usable collision box.
    pub fn bounds(&self, id: EntityId) -> Option<releste_math::Rect> {
        let e = self.get(id)?;
        let x = get_f32(e, "x").unwrap_or(0.0);
        let y = get_f32(e, "y").unwrap_or(0.0);
        let w = get_i32(e, "width").map(|v| v as f32).unwrap_or(8.0);
        let h = get_i32(e, "height").map(|v| v as f32).unwrap_or(8.0);
        Some(releste_math::Rect::new(
            releste_math::Vec2::from_f32s(x, y),
            releste_math::Vec2::from_f32s(x + w, y + h),
        ))
    }

    /// World AABB of the player.
    pub fn player_bounds(&self) -> Option<releste_math::Rect> {
        self.bounds(self.player?)
    }

    /// Resolve overlap between the player and other entities (push the player
    /// out).
    ///
    /// Hot reload conflict resolution strategy 1 (AGENTS.md 7.4): when the
    /// player overlaps a newly added entity, push the player out rather than
    /// ejecting or getting stuck.
    ///
    /// Returns the list of entity IDs judged to be "push sources".
    pub fn resolve_overlap(&mut self) -> Vec<EntityId> {
        let Some(player_id) = self.player else {
            return Vec::new();
        };
        let Some(player_rect) = self.entity_rect(player_id) else {
            return Vec::new();
        };

        // Collect the entities overlapping the player (excluding the player).
        let overlapping: Vec<(EntityId, Rect)> = self
            .entities
            .values()
            .filter(|e| e.id != player_id)
            .filter_map(|e| self.entity_rect(e.id).map(|r| (e.id, r)))
            .filter(|(_, r)| rects_overlap(*r, player_rect))
            .collect();

        if overlapping.is_empty() {
            return Vec::new();
        }

        // Push out one by one: pick the minimum translation axis.
        let mut player_pos = player_rect.min;
        for (_id, other) in &overlapping {
            player_pos = push_out(player_pos, player_rect.size(), *other);
        }

        if let Some(p) = self.get_mut(player_id) {
            p.set("x", FieldValue::Float(player_pos.x.to_f32()));
            p.set("y", FieldValue::Float(player_pos.y.to_f32()));
        }

        overlapping.into_iter().map(|(id, _)| id).collect()
    }

    /// Read the AABB of an entity (internal; see [`World::bounds`] for the
    /// public version).
    fn entity_rect(&self, id: EntityId) -> Option<releste_math::Rect> {
        self.bounds(id)
    }
}

/// Read an f32 property.
fn get_f32(e: &Entity, key: &str) -> Option<f32> {
    match e.get(key)? {
        FieldValue::Float(v) => Some(*v),
        FieldValue::Int(v) => Some(*v as f32),
        _ => None,
    }
}

/// Read an i32 property.
fn get_i32(e: &Entity, key: &str) -> Option<i32> {
    match e.get(key)? {
        FieldValue::Int(v) => Some(*v),
        FieldValue::Float(v) => Some(*v as i32),
        _ => None,
    }
}

/// AABB overlap test.
fn rects_overlap(a: releste_math::Rect, b: releste_math::Rect) -> bool {
    a.min.x <= b.max.x && a.max.x >= b.min.x && a.min.y <= b.max.y && a.max.y >= b.min.y
}

/// Push `pos` (with size `size`) out of `blocker` along the minimum
/// translation axis.
fn push_out(
    pos: releste_math::Vec2,
    size: releste_math::Vec2,
    blocker: releste_math::Rect,
) -> releste_math::Vec2 {
    if !rects_overlap(releste_math::Rect::from_origin_size(pos, size), blocker) {
        return pos;
    }

    let dx_left = blocker.min.x - (pos.x + size.x); // displacement toward the left
    let dx_right = blocker.max.x - pos.x; // displacement toward the right
    let dy_up = blocker.min.y - (pos.y + size.y); // displacement upward
    let dy_down = blocker.max.y - pos.y; // displacement downward

    let candidates = [
        (
            dx_left.abs(),
            releste_math::Vec2::new(dx_left, releste_math::Fx::zero()),
        ),
        (
            dx_right.abs(),
            releste_math::Vec2::new(dx_right, releste_math::Fx::zero()),
        ),
        (
            dy_up.abs(),
            releste_math::Vec2::new(releste_math::Fx::zero(), dy_up),
        ),
        (
            dy_down.abs(),
            releste_math::Vec2::new(releste_math::Fx::zero(), dy_down),
        ),
    ];

    let (_, delta) = candidates
        .into_iter()
        .min_by(|a, b| a.0.cmp(&b.0))
        .expect("candidates is non-empty");
    pos + delta
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::Entity;
    use crate::id::EntityId;

    #[test]
    fn insert_and_get() {
        let mut world = World::new();
        let id = EntityId::new(1);
        let kind = EntityKindId::new(10);
        let entity = Entity::new(id, kind);
        world.insert(entity);
        assert!(world.get(id).is_some());
        assert_eq!(world.len(), 1);
    }

    #[test]
    fn by_kind_index() {
        let mut world = World::new();
        let kind_a = EntityKindId::new(10);
        let kind_b = EntityKindId::new(20);
        world.insert(Entity::new(EntityId::new(1), kind_a));
        world.insert(Entity::new(EntityId::new(2), kind_a));
        world.insert(Entity::new(EntityId::new(3), kind_b));

        assert_eq!(world.by_kind(kind_a).count(), 2);
        assert_eq!(world.by_kind(kind_b).count(), 1);
    }

    #[test]
    fn remove_updates_indices() {
        let mut world = World::new();
        let id = EntityId::new(1);
        let kind = EntityKindId::new(10);
        world.insert(Entity::new(id, kind));
        assert!(world.remove(id).is_some());
        assert!(world.get(id).is_none());
        assert_eq!(world.by_kind(kind).count(), 0);
    }
}
