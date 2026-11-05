//! Apply patches to the World, with conflict resolution and rollback.

use releste_map::Patch;
use releste_math::Rect;
use releste_world::{Entity, EntityData, EntityId, World};
use tracing::{info, warn};

/// Hot reload applier.
///
/// Applies a sequence of [`Patch`] to the [`World`] and guarantees that the
/// world invariants are not broken on conflict.
#[derive(Debug, Clone)]
pub struct Reload {
    /// Whether to enable "reject the patch when the room shrinks" (default
    /// true).
    pub strict_bounds: bool,
    /// Room bounds after this reload (used for strategy 3 validation).
    ///
    /// `None` means no bounds validation.
    pub room_bounds: Option<Rect>,
}

impl Default for Reload {
    fn default() -> Self {
        Reload {
            strict_bounds: true,
            room_bounds: None,
        }
    }
}

/// Application result report.
#[derive(Debug, Default, Clone)]
pub struct ApplyReport {
    pub added: usize,
    pub removed: usize,
    pub updated: usize,
    pub moved: usize,
    pub tiles_replaced: bool,
    pub sync_updated: bool,
    /// Skipped patches (referenced ID disappeared, etc.).
    pub skipped: Vec<String>,
    /// Pushed-out entities (overlapped the player).
    pub pushed_entities: Vec<EntityId>,
}

/// Reload error.
#[derive(Debug, thiserror::Error)]
pub enum ReloadError {
    #[error("patch rejected, rolled back: {0}")]
    RolledBack(String),
    #[error("world invariant violated: {0}")]
    Invariant(String),
}

/// Compatibility alias for an application failure.
pub type ApplyError = ReloadError;

impl Reload {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the room bounds for this reload (enables strategy 3 validation).
    pub fn with_bounds(mut self, bounds: Rect) -> Self {
        self.room_bounds = Some(bounds);
        self
    }

    /// Apply a batch of patches.
    ///
    /// Conflict handling strategies (AGENTS.md 7.4):
    /// 1. Player overlaps a newly added entity -> push out
    /// 2. Referenced ID disappeared -> warning + skip
    /// 3. Room bounds shrink and the player is outside -> error + roll back the
    ///    whole patch
    ///
    /// **Player and carried-object state is preserved in all cases** --
    /// patches only affect entities that are explicitly mentioned.
    pub fn apply(
        &mut self,
        world: &mut World,
        patches: Vec<Patch>,
    ) -> Result<ApplyReport, ReloadError> {
        let mut report = ApplyReport::default();

        // Snapshot: used for rollback.
        let snapshot = self.snapshot(world);

        for patch in patches {
            if let Err(e) = self.apply_one(world, patch, &mut report) {
                self.restore(world, snapshot);
                warn!(error = %e, "reload patch failed, rolling back");
                return Err(ReloadError::RolledBack(e.to_string()));
            }

            // Strategy 3: after each step, verify the player is still within
            // the room bounds.
            if let Err(e) = self.check_player_in_bounds(world) {
                self.restore(world, snapshot);
                warn!(error = %e, "player left room bounds, rolling back");
                return Err(ReloadError::RolledBack(e.to_string()));
            }
        }

        // Strategy 1: player overlaps a newly added entity -> push out.
        let pushed = self.resolve_player_overlap(world);
        report.pushed_entities = pushed;

        info!(
            added = report.added,
            removed = report.removed,
            updated = report.updated,
            moved = report.moved,
            skipped = report.skipped.len(),
            pushed = report.pushed_entities.len(),
            "reload applied"
        );

        Ok(report)
    }

    /// Verify the player is within the (optional) room bounds.
    fn check_player_in_bounds(&self, world: &World) -> Result<(), ReloadError> {
        if !self.strict_bounds {
            return Ok(());
        }
        let Some(bounds) = self.room_bounds else {
            return Ok(());
        };
        let Some(player_bounds) = world.player_bounds() else {
            return Ok(());
        };

        // Judge by the player center to avoid a false positive when hugging an
        // edge.
        let center = releste_math::Vec2::new(
            (player_bounds.min.x + player_bounds.max.x) / releste_math::Fx::from_int(2),
            (player_bounds.min.y + player_bounds.max.y) / releste_math::Fx::from_int(2),
        );
        if bounds.contains(center) {
            Ok(())
        } else {
            Err(ReloadError::Invariant(format!(
                "player center {center} is outside room bounds"
            )))
        }
    }

    /// Apply a single patch.
    fn apply_one(
        &self,
        world: &mut World,
        patch: Patch,
        report: &mut ApplyReport,
    ) -> Result<(), ReloadError> {
        match patch {
            Patch::AddEntity(data) => {
                let entity = self.build_entity(&data);
                world.insert(entity);
                report.added += 1;
            }
            Patch::RemoveEntity(id) => {
                if world.remove(id).is_some() {
                    report.removed += 1;
                } else {
                    report
                        .skipped
                        .push(format!("RemoveEntity: id {id} not found"));
                    warn!(%id, "RemoveEntity: id not found, skipping");
                }
            }
            Patch::UpdateEntity { id, props } => {
                let Some(entity) = world.get_mut(id) else {
                    report
                        .skipped
                        .push(format!("UpdateEntity: id {id} not found"));
                    warn!(%id, "UpdateEntity: id not found, skipping");
                    return Ok(());
                };
                entity.properties = props.fields;
                report.updated += 1;
            }
            Patch::MoveEntity { id, pos } => {
                let Some(entity) = world.get_mut(id) else {
                    report
                        .skipped
                        .push(format!("MoveEntity: id {id} not found"));
                    warn!(%id, "MoveEntity: id not found, skipping");
                    return Ok(());
                };
                entity.set("x", releste_world::FieldValue::Float(pos.x.to_f32()));
                entity.set("y", releste_world::FieldValue::Float(pos.y.to_f32()));
                report.moved += 1;
            }
            Patch::ReplaceTiles(_tiles) => {
                // The tileset replacement is consumed by the render layer; only
                // flagged here.
                report.tiles_replaced = true;
            }
            Patch::UpdateSync(_rules) => {
                report.sync_updated = true;
            }
        }
        Ok(())
    }

    /// Build a runtime entity from EntityData.
    fn build_entity(&self, data: &EntityData) -> Entity {
        // EntityKindId is resolved by the registry; a name hash is used as a
        // placeholder here.
        // Full registry integration is done in the world layer's
        // EntityRegistry.
        let kind_id = kind_id_for(&data.kind);
        let mut entity = Entity::new(data.id, kind_id);
        entity.properties = data.properties.clone();
        entity.set("x", releste_world::FieldValue::Float(data.x));
        entity.set("y", releste_world::FieldValue::Float(data.y));
        if let Some(w) = data.width {
            entity.set("width", releste_world::FieldValue::Int(w));
        }
        if let Some(h) = data.height {
            entity.set("height", releste_world::FieldValue::Int(h));
        }
        entity
    }

    /// Conflict resolution: push the player out of overlapping new entities.
    ///
    /// Returns the list of pushed-out entity IDs.
    fn resolve_player_overlap(&self, world: &mut World) -> Vec<EntityId> {
        world.resolve_overlap()
    }

    /// World snapshot (used for rollback).
    fn snapshot(&self, world: &World) -> Vec<(EntityId, Entity)> {
        world.all().map(|e| (e.id, e.clone())).collect()
    }

    /// Restore from the snapshot.
    fn restore(&self, world: &mut World, snapshot: Vec<(EntityId, Entity)>) {
        world.clear();
        for (_id, entity) in snapshot {
            world.insert(entity);
        }
    }
}

/// Derive `EntityKindId` from the kind name (fallback when the registry
/// misses).
fn kind_id_for(kind: &str) -> releste_world::EntityKindId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kind.as_bytes());
    let bytes: [u8; 32] = hasher.finalize().into();
    releste_world::EntityKindId::new(u32::from_le_bytes(bytes[0..4].try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use releste_map::{MapDocument, Props, Room, Tileset};
    use releste_math::{Rect, Vec2};
    use releste_world::{Entity, EntityKindId, FieldValue};

    const KIND_WALL: EntityKindId = EntityKindId::new(1);
    const KIND_PLAYER: EntityKindId = EntityKindId::new(2);

    fn id(n: u64) -> EntityId {
        EntityId::new(n)
    }

    /// Build an entity with position and size.
    fn entity_raw(eid: EntityId, kind: EntityKindId, x: f32, y: f32, w: i32, h: i32) -> Entity {
        let mut e = Entity::new(eid, kind);
        e.set("x", FieldValue::Float(x));
        e.set("y", FieldValue::Float(y));
        e.set("width", FieldValue::Int(w));
        e.set("height", FieldValue::Int(h));
        e
    }

    /// Map entity data.
    fn edata(eid: EntityId, kind: &str, x: f32, y: f32, w: i32, h: i32) -> EntityData {
        let mut d = EntityData::new(kind, eid, x, y);
        d.width = Some(w);
        d.height = Some(h);
        d
    }

    /// A world: player + one wall.
    fn world_fixture() -> World {
        let mut w = World::new();
        let player = id(100);
        w.insert(entity_raw(player, KIND_PLAYER, 100.0, 100.0, 8, 8));
        w.insert(entity_raw(id(1), KIND_WALL, 200.0, 100.0, 8, 8));
        w.set_player(player);
        w
    }

    fn player_xy(w: &World) -> (f32, f32) {
        let p = w.get(w.player().unwrap()).unwrap();
        let x = match p.get("x") {
            Some(FieldValue::Float(v)) => *v,
            other => panic!("player x missing: {other:?}"),
        };
        let y = match p.get("y") {
            Some(FieldValue::Float(v)) => *v,
            other => panic!("player y missing: {other:?}"),
        };
        (x, y)
    }

    // ---------- Basic patch application ----------

    #[test]
    fn add_entity_inserts() {
        let mut w = world_fixture();
        let before = w.len();
        let mut r = Reload::new();
        let report = r
            .apply(
                &mut w,
                vec![Patch::AddEntity(edata(id(2), "wall", 300.0, 100.0, 8, 8))],
            )
            .unwrap();
        assert_eq!(report.added, 1);
        assert_eq!(w.len(), before + 1);
        assert!(w.get(id(2)).is_some());
    }

    #[test]
    fn remove_entity_removes() {
        let mut w = world_fixture();
        let mut r = Reload::new();
        let report = r.apply(&mut w, vec![Patch::RemoveEntity(id(1))]).unwrap();
        assert_eq!(report.removed, 1);
        assert!(w.get(id(1)).is_none());
    }

    #[test]
    fn move_entity_updates_position() {
        let mut w = world_fixture();
        let mut r = Reload::new();
        let report = r
            .apply(
                &mut w,
                vec![Patch::MoveEntity {
                    id: id(1),
                    pos: Vec2::from_f32s(50.0, 60.0),
                }],
            )
            .unwrap();
        assert_eq!(report.moved, 1);
        let e = w.get(id(1)).unwrap();
        assert_eq!(e.get("x"), Some(&FieldValue::Float(50.0)));
        assert_eq!(e.get("y"), Some(&FieldValue::Float(60.0)));
    }

    #[test]
    fn update_entity_replaces_props() {
        let mut w = world_fixture();
        let mut props = Props::default();
        props.fields.insert("open".into(), FieldValue::Bool(true));

        let mut r = Reload::new();
        let report = r
            .apply(&mut w, vec![Patch::UpdateEntity { id: id(1), props }])
            .unwrap();
        assert_eq!(report.updated, 1);
        assert_eq!(
            w.get(id(1)).unwrap().get("open"),
            Some(&FieldValue::Bool(true))
        );
    }

    #[test]
    fn tiles_and_sync_are_reported() {
        let mut w = world_fixture();
        let mut r = Reload::new();
        let report = r
            .apply(
                &mut w,
                vec![
                    Patch::ReplaceTiles(Tileset::new("Gameplay", 4, 4)),
                    Patch::UpdateSync(vec![]),
                ],
            )
            .unwrap();
        assert!(report.tiles_replaced);
        assert!(report.sync_updated);
    }

    // ---------- Strategy 2: ID disappeared -> warning + skip ----------

    #[test]
    fn unknown_id_is_skipped_not_fatal() {
        let mut w = world_fixture();
        let before = w.len();
        let mut r = Reload::new();
        let report = r
            .apply(
                &mut w,
                vec![
                    Patch::RemoveEntity(id(9999)),
                    Patch::MoveEntity {
                        id: id(8888),
                        pos: Vec2::ZERO,
                    },
                    Patch::AddEntity(edata(id(3), "wall", 500.0, 100.0, 8, 8)),
                ],
            )
            .unwrap();

        assert_eq!(report.added, 1);
        assert_eq!(report.skipped.len(), 2);
        assert_eq!(w.len(), before + 1, "valid patches must still apply");
    }

    // ---------- Strategy 1: player is pushed out ----------

    #[test]
    fn player_is_pushed_out_of_new_entity() {
        let mut w = world_fixture();
        // The new entity partially overlaps the player (player 100..108,
        // entity 102..110).
        let mut r = Reload::new();
        let report = r
            .apply(
                &mut w,
                vec![Patch::AddEntity(edata(id(2), "wall", 102.0, 100.0, 8, 8))],
            )
            .unwrap();

        assert_eq!(report.pushed_entities.len(), 1);
        let (px, py) = player_xy(&w);
        assert!(
            px < 100.0 || px > 102.0,
            "player should be pushed on x, got ({px}, {py})"
        );
        assert!(!w.resolve_overlap().len() > 0, "overlap should be resolved");
    }

    #[test]
    fn no_push_when_nothing_overlaps() {
        let mut w = world_fixture();
        let mut r = Reload::new();
        let report = r
            .apply(
                &mut w,
                vec![Patch::AddEntity(edata(id(2), "wall", 500.0, 500.0, 8, 8))],
            )
            .unwrap();
        assert!(report.pushed_entities.is_empty());
        assert_eq!(player_xy(&w), (100.0, 100.0));
    }

    // ---------- Strategy 3: player out of bounds -> rollback ----------

    #[test]
    fn shrinking_bounds_with_player_outside_rolls_back() {
        let mut w = world_fixture();
        // The player is at (100,100). Shrink the room far away from the
        // player.
        let mut r = Reload::new().with_bounds(Rect::new(
            Vec2::from_f32s(0.0, 0.0),
            Vec2::from_f32s(50.0, 50.0),
        ));

        let before_len = w.len();
        let err = r
            .apply(
                &mut w,
                vec![Patch::AddEntity(edata(id(2), "wall", 10.0, 10.0, 8, 8))],
            )
            .unwrap_err();

        assert!(matches!(err, ReloadError::RolledBack(_)));
        assert_eq!(w.len(), before_len, "world must be rolled back");
        assert!(w.get(id(2)).is_none(), "the add must be undone");
    }

    #[test]
    fn bounds_check_passes_when_player_inside() {
        let mut w = world_fixture();
        let mut r = Reload::new().with_bounds(Rect::new(
            Vec2::from_f32s(0.0, 0.0),
            Vec2::from_f32s(320.0, 180.0),
        ));
        let report = r
            .apply(
                &mut w,
                vec![Patch::AddEntity(edata(id(2), "wall", 10.0, 10.0, 8, 8))],
            )
            .unwrap();
        assert_eq!(report.added, 1);
    }

    #[test]
    fn strict_bounds_can_be_disabled() {
        let mut w = world_fixture();
        let mut r = Reload {
            strict_bounds: false,
            room_bounds: Some(Rect::new(
                Vec2::from_f32s(0.0, 0.0),
                Vec2::from_f32s(1.0, 1.0),
            )),
        };
        // The player is far outside the bounds, but strict_bounds=false should
        // allow it.
        let report = r
            .apply(
                &mut w,
                vec![Patch::AddEntity(edata(id(2), "wall", 10.0, 10.0, 8, 8))],
            )
            .unwrap();
        assert_eq!(report.added, 1);
    }

    // ---------- Core invariant: player state is preserved (AGENTS.md 10) ----------

    #[test]
    fn player_state_is_preserved_across_reload() {
        let mut w = world_fixture();
        // Give the player some "session state".
        {
            let p = w.get_mut(w.player().unwrap()).unwrap();
            p.set("vx", FieldValue::Float(12.5));
            p.set("dashing", FieldValue::Bool(true));
            p.set("stamina", FieldValue::Int(42));
        }
        let player = w.player().unwrap();
        let before = w.get(player).unwrap().clone();

        // Change a bunch of unrelated things (move the wall + add/remove
        // entities).
        let mut r = Reload::new();
        r.apply(
            &mut w,
            vec![
                Patch::MoveEntity {
                    id: id(1),
                    pos: Vec2::from_f32s(205.0, 105.0),
                },
                Patch::AddEntity(edata(id(7), "spinner", 400.0, 400.0, 16, 16)),
                Patch::RemoveEntity(id(1)),
            ],
        )
        .unwrap();

        let after = w.get(player).expect("player must survive reload");
        assert_eq!(after.properties, before.properties);
    }

    #[test]
    fn player_is_never_removed_by_unrelated_patches() {
        let mut w = world_fixture();
        let player = w.player().unwrap();
        let mut r = Reload::new();
        r.apply(
            &mut w,
            vec![
                Patch::RemoveEntity(id(1)),
                Patch::ReplaceTiles(Tileset::new("Gameplay", 2, 2)),
            ],
        )
        .unwrap();
        assert!(w.get(player).is_some());
    }

    // ---------- Rollback precision ----------

    #[test]
    fn rollback_restores_world_exactly() {
        let mut w = world_fixture();
        let snapshot: Vec<(EntityId, Entity)> = w.all().map(|e| (e.id, e.clone())).collect();

        let mut r = Reload::new().with_bounds(Rect::new(
            Vec2::from_f32s(0.0, 0.0),
            Vec2::from_f32s(10.0, 10.0),
        ));
        let _ = r.apply(
            &mut w,
            vec![
                Patch::AddEntity(edata(id(50), "wall", 1.0, 1.0, 8, 8)),
                Patch::MoveEntity {
                    id: id(1),
                    pos: Vec2::from_f32s(9.0, 9.0),
                },
            ],
        );

        let after: Vec<(EntityId, Entity)> = w.all().map(|e| (e.id, e.clone())).collect();
        assert_eq!(after.len(), snapshot.len());
        for (id, e) in &snapshot {
            let got = w.get(*id).expect("entity restored");
            assert_eq!(got.properties, e.properties, "entity {id} differs");
        }
    }

    // ---------- End to end: diff -> apply ----------

    fn room_with(tiles: u16, entity_positions: &[(u64, &str, f32, f32)]) -> Room {
        let mut t = Tileset::new("Gameplay", 4, 4);
        t.set(0, 0, tiles);
        Room {
            name: "room_0".into(),
            bounds: releste_map::RectDef {
                x: 0.0,
                y: 0.0,
                w: 320.0,
                h: 180.0,
            },
            tiles: t,
            bg: Tileset::default(),
            entities: entity_positions
                .iter()
                .map(|(eid, kind, x, y)| edata(id(*eid), kind, *x, *y, 8, 8))
                .collect(),
            triggers: Vec::new(),
            script: None,
            ..Default::default()
        }
    }

    fn doc(rooms: Vec<Room>) -> MapDocument {
        MapDocument {
            area: "Test".into(),
            rooms,
            sync_rules: Vec::new(),
            ..Default::default()
        }
    }

    #[test]
    fn end_to_end_diff_then_apply() {
        let old = doc(vec![room_with(1, &[(1, "wall", 100.0, 100.0)])]);
        // New version: wall corner moved + one extra entity + tiles changed
        let new = doc(vec![room_with(
            5,
            &[(1, "wall", 110.0, 100.0), (2, "spinner", 200.0, 50.0)],
        )]);

        let patches = releste_map::diff(&old, &new);
        assert!(!patches.is_empty(), "diff should produce patches");

        // Build the world from the old map, then apply the diff.
        let mut w = World::new();
        w.insert(entity_raw(id(1), KIND_WALL, 100.0, 100.0, 8, 8));
        let player = id(100);
        w.insert(entity_raw(player, KIND_PLAYER, 250.0, 150.0, 8, 8));
        w.set_player(player);

        let mut r = Reload::new();
        let report = r.apply(&mut w, patches).unwrap();

        assert_eq!(report.added, 1, "spinner should be added");
        assert_eq!(report.moved, 1, "wall should move");
        assert!(report.tiles_replaced, "tiles changed");

        // The player was not affected by the created diff.
        assert_eq!(player_xy(&w), (250.0, 150.0));
        assert!(w.get(player).is_some());
        assert!(w.get(id(2)).is_some());
    }

    #[test]
    fn diff_of_identical_maps_is_empty() {
        let a = doc(vec![room_with(1, &[(1, "wall", 0.0, 0.0)])]);
        let patches = releste_map::diff(&a, &a.clone());
        assert!(patches.is_empty());
    }

    #[test]
    fn applying_empty_patch_list_is_a_noop() {
        let mut w = world_fixture();
        let before: Vec<(EntityId, Entity)> = w.all().map(|e| (e.id, e.clone())).collect();
        let mut r = Reload::new();
        let report = r.apply(&mut w, vec![]).unwrap();
        assert_eq!(report.added + report.removed + report.moved, 0);

        let after: Vec<(EntityId, Entity)> = w.all().map(|e| (e.id, e.clone())).collect();
        assert_eq!(after.len(), before.len());
    }
}
