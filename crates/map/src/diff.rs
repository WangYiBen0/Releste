//! 地图 diff：生成热重载补丁。

use std::collections::HashMap;

use reles_math::Vec2;
use reles_world::EntityId;

use crate::format::{MapDocument, Props, Room, SyncRule, Tileset};

/// 热重载补丁。
#[derive(Debug, Clone)]
pub enum Patch {
    AddEntity(reles_world::EntityData),
    RemoveEntity(EntityId),
    UpdateEntity { id: EntityId, props: Props },
    MoveEntity { id: EntityId, pos: Vec2 },
    ReplaceTiles(Tileset),
    UpdateSync(Vec<SyncRule>),
}

/// 比对两份地图，生成补丁列表。
pub fn diff(old: &MapDocument, new: &MapDocument) -> Vec<Patch> {
    let mut patches = Vec::new();

    // 比较同步规则
    if old.sync_rules != new.sync_rules {
        patches.push(Patch::UpdateSync(new.sync_rules.clone()));
    }

    // 比较每个房间
    let old_by_name: HashMap<&str, &Room> =
        old.rooms.iter().map(|r| (r.name.as_str(), r)).collect();
    let new_by_name: HashMap<&str, &Room> =
        new.rooms.iter().map(|r| (r.name.as_str(), r)).collect();

    // 遍历顺序取「旧地图顺序 → 新地图顺序」，**不走 HashSet**：
    // 补丁序列必须确定，否则同一份输入每次 diff 都得到不同顺序。
    // 先处理已有房间的变更与删除，再补上新增房间。
    for old_room in &old.rooms {
        match new_by_name.get(old_room.name.as_str()) {
            Some(&new_room) => patches.extend(diff_room(old_room, new_room)),
            // 房间被整体移除：其全部实体随之消失。
            None => patches.extend(old_room.entities.iter().map(|e| Patch::RemoveEntity(e.id))),
        }
    }

    for new_room in &new.rooms {
        // 已存在的房间在上一个循环里处理过，这里只收新增的。
        if old_by_name.contains_key(new_room.name.as_str()) {
            continue;
        }
        // 新房间：瓦片整套 + 全部实体。
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

/// 房间级 diff。
fn diff_room(old: &Room, new: &Room) -> Vec<Patch> {
    let mut patches = Vec::new();

    // Tile 集变更
    if old.tiles != new.tiles {
        patches.push(Patch::ReplaceTiles(new.tiles.clone()));
    }

    // 实体 diff
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
        })
        .collect();

    patches.extend(diff_entities(&old_refs, &new_refs));
    patches
}

/// 实体内部比对。
///
/// 承载生成补丁所需的**全部**字段。少一个，[`Patch::AddEntity`]
/// 就会静默丢掉对应数据 —— 此前 `kind` 被硬编码成空串，导致热重载
/// 新增的所有实体共用同一个 fallback `EntityKindId`。
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
}

/// 实体 diff。
fn diff_entities(old: &[EntityRef], new: &[EntityRef]) -> Vec<Patch> {
    let old_map: HashMap<EntityId, &EntityRef> = old.iter().map(|e| (e.id, e)).collect();
    let new_map: HashMap<EntityId, &EntityRef> = new.iter().map(|e| (e.id, e)).collect();

    let mut patches = Vec::new();

    // 三段都按 slice 出现顺序遍历，**不走 HashSet**：补丁序列必须
    // 确定，否则同一份输入每次 diff 都会得到不同顺序的补丁。
    for e in new {
        if !old_map.contains_key(&e.id) {
            patches.push(Patch::AddEntity(reles_world::EntityData {
                id: e.id,
                kind: e.kind.clone(),
                x: e.x,
                y: e.y,
                width: e.width,
                height: e.height,
                origin_x: e.origin_x,
                origin_y: e.origin_y,
                properties: e.props.fields.clone(),
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
    use reles_world::EntityData;

    fn doc(rooms: Vec<Room>) -> MapDocument {
        MapDocument {
            area: "Test".into(),
            rooms,
            sync_rules: vec![],
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
        }
    }

    fn entity(id: u64, kind: &str, x: f32, y: f32) -> EntityData {
        EntityData::new(kind, EntityId::new(id), x, y)
    }

    /// 挑出所有 `AddEntity` 的负载。
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
        };
        let b = a.clone();
        let patches = diff(&a, &b);
        assert!(patches.is_empty());
    }

    /// 新增房间不能被静默忽略 —— 瓦片和实体都要进补丁。
    #[test]
    fn added_room_emits_replace_tiles_and_add_entity() {
        let old = doc(vec![]);
        let new = doc(vec![room("r1", vec![entity(1, "wall", 8.0, 8.0)])]);

        let patches = diff(&old, &new);
        assert!(
            patches.iter().any(|p| matches!(p, Patch::ReplaceTiles(_))),
            "新增房间应带 ReplaceTiles, got {patches:?}"
        );

        let list = added(&patches);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, EntityId::new(1));
        assert_eq!(list[0].kind, "wall");
    }

    /// 删除房间要把它的全部实体撤下，否则它们会永远留在世界里。
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

    /// `AddEntity` 必须还原 kind 与几何信息，不能丢字段。
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

    /// 补丁序列必须确定：同一份输入反复 diff 结果完全一致。
    ///
    /// 此前房间和实体都走 `HashSet` 迭代，顺序随进程随机状态变化。
    #[test]
    fn diff_sequence_is_deterministic() {
        let make = || {
            doc((0u64..24)
                .map(|i| room(&format!("room_{i}"), vec![entity(i, "wall", 0.0, 0.0)]))
                .collect())
        };

        let old = make();
        let mut new = make();
        new.rooms.remove(3); // 删除 room_3
        new.rooms
            .push(room("brand_new", vec![entity(999, "door", 0.0, 0.0)]));

        let render = |p: &[Patch]| p.iter().map(|p| format!("{p:?}")).collect::<Vec<String>>();

        let expected = render(&diff(&old, &new));
        assert!(!expected.is_empty(), "sanity: 本例应产生补丁");
        for _ in 0..16 {
            assert_eq!(render(&diff(&old, &new)), expected, "diff 顺序不稳定");
        }
    }
}
