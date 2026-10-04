//! 地图 diff：生成热重载补丁。

use std::collections::{HashMap, HashSet};

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

    // 所有房间名
    let all_names: HashSet<&str> = old_by_name
        .keys()
        .copied()
        .chain(new_by_name.keys().copied())
        .collect();

    for name in all_names {
        match (old_by_name.get(name), new_by_name.get(name)) {
            (Some(old_r), Some(new_r)) => {
                patches.extend(diff_room(old_r, new_r));
            }
            (None, Some(_new_r)) => {
                // 新房间：所有实体作为 AddEntity 添加
                // (TODO: 更完整的房间 diff)
            }
            (Some(_old_r), None) => {
                // 整个房间被移除
                // (TODO: 发 RemoveEntity 给所有旧实体)
            }
            (None, None) => unreachable!(),
        }
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
            x: e.x,
            y: e.y,
            props: Props::from_entity(e),
        })
        .collect();
    let new_refs: Vec<EntityRef> = new
        .entities
        .iter()
        .map(|e| EntityRef {
            id: e.id,
            x: e.x,
            y: e.y,
            props: Props::from_entity(e),
        })
        .collect();

    patches.extend(diff_entities(&old_refs, &new_refs));
    patches
}

/// 实体内部比对。
struct EntityRef {
    id: EntityId,
    x: f32,
    y: f32,
    props: Props,
}

/// 实体 diff。
fn diff_entities(old: &[EntityRef], new: &[EntityRef]) -> Vec<Patch> {
    let old_map: HashMap<EntityId, &EntityRef> = old.iter().map(|e| (e.id, e)).collect();
    let new_map: HashMap<EntityId, &EntityRef> = new.iter().map(|e| (e.id, e)).collect();

    let old_ids: HashSet<EntityId> = old_map.keys().copied().collect();
    let new_ids: HashSet<EntityId> = new_map.keys().copied().collect();

    let mut patches = Vec::new();

    // 新增
    for id in new_ids.difference(&old_ids) {
        let e = new_map[id];
        // 通过世界来构建完整实体
        patches.push(Patch::AddEntity(reles_world::EntityData {
            id: e.id,
            kind: String::new(),
            x: e.x,
            y: e.y,
            width: None,
            height: None,
            origin_x: 0.0,
            origin_y: 0.0,
            properties: e.props.fields.clone(),
        }));
    }

    // 移除
    for id in old_ids.difference(&new_ids) {
        patches.push(Patch::RemoveEntity(*id));
    }

    // 变更
    for id in old_ids.intersection(&new_ids) {
        let old_e = old_map[id];
        let new_e = new_map[id];
        if (old_e.x - new_e.x).abs() > f32::EPSILON || (old_e.y - new_e.y).abs() > f32::EPSILON {
            patches.push(Patch::MoveEntity {
                id: *id,
                pos: Vec2::from_f32s(new_e.x, new_e.y),
            });
        }
        if old_e.props != new_e.props {
            patches.push(Patch::UpdateEntity {
                id: *id,
                props: new_e.props.clone(),
            });
        }
    }

    patches
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
