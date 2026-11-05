//! Map diff: generate hot-reload patches.

use std::collections::HashMap;

use releste_math::Vec2;
use releste_world::EntityId;

use crate::format::{MapDocument, Props, Room, SyncRule, Tileset};

/// Hot-reload patch.
#[derive(Debug, Clone)]
pub enum Patch {
    AddEntity(releste_world::EntityData),
    RemoveEntity(EntityId),
    UpdateEntity { id: EntityId, props: Props },
    MoveEntity { id: EntityId, pos: Vec2 },
    ReplaceTiles(Tileset),
    UpdateSync(Vec<SyncRule>),
}

/// Compare two maps and produce a patch list.
pub fn diff(old: &MapDocument, new: &MapDocument) -> Vec<Patch> {
    let mut patches = Vec::new();

    // Compare sync rules
    if old.sync_rules != new.sync_rules {
        patches.push(Patch::UpdateSync(new.sync_rules.clone()));
    }

    // Compare each room
    let old_by_name: HashMap<&str, &Room> =
        old.rooms.iter().map(|r| (r.name.as_str(), r)).collect();
    let new_by_name: HashMap<&str, &Room> =
        new.rooms.iter().map(|r| (r.name.as_str(), r)).collect();

    // Iteration order follows "old map order -> new map order", **not HashSet**:
    // the patch sequence must be deterministic, otherwise the same input gives a different order on every diff.
    // Handle changes and removals of existing rooms first, then fill in the added rooms.
    for old_room in &old.rooms {
        match new_by_name.get(old_room.name.as_str()) {
            Some(&new_room) => patches.extend(diff_room(old_room, new_room)),
            // The room was removed entirely: all of its entities disappear with it.
            None => patches.extend(old_room.entities.iter().map(|e| Patch::RemoveEntity(e.id))),
        }
    }

    for new_room in &new.rooms {
        // Existing rooms were handled in the previous loop; only collect the added ones here.
        if old_by_name.contains_key(new_room.name.as_str()) {
            continue;
        }
        // A new room: the whole tileset + all entities.
        patches.push(Patch::ReplaceTiles(new_room.tiles.clone()));
        patches.extend(
            new_room
                .entities
                .iter()
                .map(|e| Patch::AddEntity(e.clone())),
        );
    }

    patches
}

/// Room-level diff.
fn diff_room(old: &Room, new: &Room) -> Vec<Patch> {
    let mut patches = Vec::new();

    // Tileset change
    if old.tiles != new.tiles {
        patches.push(Patch::ReplaceTiles(new.tiles.clone()));
    }

    // Entity diff
    let old_refs: Vec<EntityRef> = old
        .entities
        .iter()
        .map(|e| EntityRef {
            id: e.id,
            kind: e.kind.clone(),
            x: e.x,
            y: e.y,
            width: e.width,
            height: e.height,
            origin_x: e.origin_x,
            origin_y: e.origin_y,
            props: Props::from_entity(e),
            nodes: e.nodes.clone(),
        })
        .collect();
    let new_refs: Vec<EntityRef> = new
        .entities
        .iter()
        .map(|e| EntityRef {
            id: e.id,
            kind: e.kind.clone(),
            x: e.x,
            y: e.y,
            width: e.width,
            height: e.height,
            origin_x: e.origin_x,
            origin_y: e.origin_y,
            props: Props::from_entity(e),
            nodes: e.nodes.clone(),
        })
        .collect();

    patches.extend(diff_entities(&old_refs, &new_refs));
    patches
}

/// Entity-internal comparison.
///
/// Carries **all** fields needed to build a patch. Miss one and [`Patch::AddEntity`]
/// silently drops the corresponding data -- previously `kind` was hard-coded to an empty string, so every
/// entity added by hot reload shared the same fallback `EntityKindId`.
struct EntityRef {
    id: EntityId,
    kind: String,
    x: f32,
    y: f32,
    width: Option<i32>,
    height: Option<i32>,
    origin_x: f32,
    origin_y: f32,
    props: Props,
    /// Nodes take part in the diff too: `introCrusher`'s landing point and `wire`'s polyline depend on them.
    nodes: Vec<(f32, f32)>,
}

/// Entity diff.
fn diff_entities(old: &[EntityRef], new: &[EntityRef]) -> Vec<Patch> {
    let old_map: HashMap<EntityId, &EntityRef> = old.iter().map(|e| (e.id, e)).collect();
    let new_map: HashMap<EntityId, &EntityRef> = new.iter().map(|e| (e.id, e)).collect();

    let mut patches = Vec::new();

    // All three passes iterate in slice order, **not HashSet**: the patch sequence must be
    // deterministic, otherwise the same input gives patches in a different order on every diff.
    for e in new {
        if !old_map.contains_key(&e.id) {
            patches.push(Patch::AddEntity(releste_world::EntityData {
                id: e.id,
                kind: e.kind.clone(),
                x: e.x,
                y: e.y,
                width: e.width,
                height: e.height,
                origin_x: e.origin_x,
                origin_y: e.origin_y,
                properties: e.props.fields.clone(),
                nodes: e.nodes.clone(),
            }));
        }
    }

    for e in old {
        if !new_map.contains_key(&e.id) {
            patches.push(Patch::RemoveEntity(e.id));
        }
    }

    for old_e in old {
        let Some(new_e) = new_map.get(&old_e.id) else {
            continue;
        };
        if (old_e.x - new_e.x).abs() > f32::EPSILON || (old_e.y - new_e.y).abs() > f32::EPSILON {
            patches.push(Patch::MoveEntity {
                id: old_e.id,
                pos: Vec2::from_f32s(new_e.x, new_e.y),
            });
        }
        if old_e.props != new_e.props {
            patches.push(Patch::UpdateEntity {
                id: old_e.id,
                props: new_e.props.clone(),
            });
        }
    }

    patches
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::RectDef;
    use releste_world::EntityData;

    fn doc(rooms: Vec<Room>) -> MapDocument {
        MapDocument {
            area: "Test".into(),
            rooms,
            sync_rules: vec![],
            style: crate::format::MapStyle::default(),
        }
    }

    fn room(name: &str, entities: Vec<EntityData>) -> Room {
        Room {
            name: name.to_string(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: 320.0,
                h: 180.0,
            },
            tiles: Tileset::new("Gameplay", 4, 4),
            bg: Tileset::new("Gameplay", 4, 4),
            entities,
            triggers: vec![],
            script: None,
            bg_decals: vec![],
            fg_decals: vec![],
            bg_scenery: crate::format::Tileset::default(),
            fg_scenery: crate::format::Tileset::default(),
        }
    }

    fn entity(id: u64, kind: &str, x: f32, y: f32) -> EntityData {
        EntityData::new(kind, EntityId::new(id), x, y)
    }

    /// Pick out the payload of every `AddEntity`.
    fn added(patches: &[Patch]) -> Vec<&EntityData> {
        patches
            .iter()
            .filter_map(|p| match p {
                Patch::AddEntity(d) => Some(d),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn empty_diff() {
        let a = MapDocument {
            area: "Test".into(),
            rooms: vec![],
            sync_rules: vec![],
            style: crate::format::MapStyle::default(),
        };
        let b = a.clone();
        let patches = diff(&a, &b);
        assert!(patches.is_empty());
    }

    /// A new room must not be silently ignored -- both tiles and entities must go into the patch.
    #[test]
    fn added_room_emits_replace_tiles_and_add_entity() {
        let old = doc(vec![]);
        let new = doc(vec![room("r1", vec![entity(1, "wall", 8.0, 8.0)])]);

        let patches = diff(&old, &new);
        assert!(
            patches.iter().any(|p| matches!(p, Patch::ReplaceTiles(_))),
            "a new room should emit ReplaceTiles, got {patches:?}"
        );

        let list = added(&patches);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, EntityId::new(1));
        assert_eq!(list[0].kind, "wall");
    }

    /// Removing a room must retract all of its entities, otherwise they stay in the world forever.
    #[test]
    fn removed_room_emits_remove_entity_for_every_entity() {
        let old = doc(vec![room(
            "r1",
            vec![entity(1, "wall", 8.0, 8.0), entity(2, "door", 64.0, 8.0)],
        )]);
        let new = doc(vec![]);

        let patches = diff(&old, &new);
        let removed: Vec<EntityId> = patches
            .iter()
            .filter_map(|p| match p {
                Patch::RemoveEntity(id) => Some(*id),
                _ => None,
            })
            .collect();
        assert_eq!(removed, vec![EntityId::new(1), EntityId::new(2)]);
    }

    /// `AddEntity` must restore kind and geometry and must not drop fields.
    #[test]
    fn add_entity_preserves_kind_and_geometry() {
        let mut grown = entity(7, "crystal", 4.0, 4.0);
        grown.width = Some(8);
        grown.height = Some(16);
        grown.origin_x = 1.5;
        grown.origin_y = 2.5;

        let old = doc(vec![room("r1", vec![])]);
        let new = doc(vec![room("r1", vec![grown])]);

        let patches = diff(&old, &new);
        let list = added(&patches);
        assert_eq!(
            list.len(),
            1,
            "expected exactly one AddEntity, got {patches:?}"
        );

        let d = list[0];
        assert_eq!(d.kind, "crystal");
        assert_eq!(d.width, Some(8));
        assert_eq!(d.height, Some(16));
        assert_eq!(d.origin_x, 1.5);
        assert_eq!(d.origin_y, 2.5);
    }

    /// The patch sequence must be deterministic: diffing the same input repeatedly gives identical results.
    ///
    /// Previously rooms and entities both iterated a `HashSet`, so the order varied with the process's random state.
    #[test]
    fn diff_sequence_is_deterministic() {
        let make = || {
            doc((0u64..24)
                .map(|i| room(&format!("room_{i}"), vec![entity(i, "wall", 0.0, 0.0)]))
                .collect())
        };

        let old = make();
        let mut new = make();
        new.rooms.remove(3); // remove room_3
        new.rooms
            .push(room("brand_new", vec![entity(999, "door", 0.0, 0.0)]));

        let render = |p: &[Patch]| p.iter().map(|p| format!("{p:?}")).collect::<Vec<String>>();

        let expected = render(&diff(&old, &new));
        assert!(
            !expected.is_empty(),
            "sanity: this case should produce patches"
        );
        for _ in 0..16 {
            assert_eq!(
                render(&diff(&old, &new)),
                expected,
                "diff order is unstable"
            );
        }
    }
}
