//! World：ECS 容器。
//!
//! 管理所有运行时实体。热重载时提供按 ID 查询的 API。

use std::collections::HashMap;

use slotmap::{DefaultKey, SlotMap};

use crate::entity::Entity;
use crate::factory::EntityKindId;
use crate::id::EntityId;
use crate::schema::FieldValue;
use reles_math::Rect;

/// World — 所有实体的运行时容器。
///
/// 实体以 `SlotMap` 存储，按 [`EntityId`]（稳定 ID）和
/// [`EntityKindId`]（种类 ID）双重索引。
#[derive(Debug, Default)]
pub struct World {
    entities: SlotMap<DefaultKey, Entity>,
    /// 稳定 ID → SlotMap Key。
    id_to_key: HashMap<EntityId, DefaultKey>,
    /// 种类 ID → 该种类所有实体的 SlotMap Key 列表。
    kind_index: HashMap<EntityKindId, Vec<DefaultKey>>,
    /// 玩家实体 ID（热重载冲突解决时用于推离）。
    player: Option<EntityId>,
}

impl World {
    /// 创建空世界。
    pub fn new() -> Self {
        World {
            entities: SlotMap::with_key(),
            id_to_key: HashMap::new(),
            kind_index: HashMap::new(),
            player: None,
        }
    }

    /// 插入实体。
    pub fn insert(&mut self, entity: Entity) -> EntityId {
        let id = entity.id;
        let kind = entity.kind_id;
        let key = self.entities.insert(entity);
        self.id_to_key.insert(id, key);
        self.kind_index.entry(kind).or_default().push(key);
        id
    }

    /// 移除实体。
    pub fn remove(&mut self, id: EntityId) -> Option<Entity> {
        let key = self.id_to_key.remove(&id)?;
        let entity = self.entities.remove(key)?;
        if let Some(list) = self.kind_index.get_mut(&entity.kind_id) {
            list.retain(|k| *k != key);
        }
        Some(entity)
    }

    /// 按稳定 ID 查找。
    pub fn get(&self, id: EntityId) -> Option<&Entity> {
        self.id_to_key
            .get(&id)
            .and_then(|key| self.entities.get(*key))
    }

    /// 按稳定 ID 查找可变引用。
    pub fn get_mut(&mut self, id: EntityId) -> Option<&mut Entity> {
        self.id_to_key
            .get(&id)
            .and_then(|key| self.entities.get_mut(*key))
    }

    /// 按种类查找所有实体。
    pub fn by_kind(&self, kind: EntityKindId) -> impl Iterator<Item = &Entity> {
        self.kind_index
            .get(&kind)
            .into_iter()
            .flat_map(|keys| keys.iter())
            .filter_map(|key| self.entities.get(*key))
    }

    /// 遍历所有实体。
    pub fn all(&self) -> impl Iterator<Item = &Entity> {
        self.entities.values()
    }

    /// 实体总数。
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// 清除所有实体。
    pub fn clear(&mut self) {
        self.entities.clear();
        self.id_to_key.clear();
        self.kind_index.clear();
    }

    /// 是否存在指定稳定 ID。
    pub fn contains(&self, id: EntityId) -> bool {
        self.id_to_key.contains_key(&id)
    }

    /// 设置玩家实体 ID。
    pub fn set_player(&mut self, id: EntityId) {
        self.player = Some(id);
    }

    /// 玩家实体 ID。
    pub fn player(&self) -> Option<EntityId> {
        self.player
    }

    /// 某实体的世界 AABB。
    ///
    /// 缺少尺寸时回退为 8x8 单位（约等于半个 tile），保证
    /// 热重载的推离逻辑总有一个可用的碰撞盒。
    pub fn bounds(&self, id: EntityId) -> Option<reles_math::Rect> {
        let e = self.get(id)?;
        let x = get_f32(e, "x").unwrap_or(0.0);
        let y = get_f32(e, "y").unwrap_or(0.0);
        let w = get_i32(e, "width").map(|v| v as f32).unwrap_or(8.0);
        let h = get_i32(e, "height").map(|v| v as f32).unwrap_or(8.0);
        Some(reles_math::Rect::new(
            reles_math::Vec2::from_f32s(x, y),
            reles_math::Vec2::from_f32s(x + w, y + h),
        ))
    }

    /// 玩家的世界 AABB。
    pub fn player_bounds(&self) -> Option<reles_math::Rect> {
        self.bounds(self.player?)
    }

    /// 解决玩家与其它实体的重叠（把玩家推离）。
    ///
    /// 热重载冲突处理策略 1（AGENTS.md §7.4）：玩家与新增实体
    /// 重叠时推离，而不是弹出或卡住。
    ///
    /// 返回被判定为"推挤源"的实体 ID 列表。
    pub fn resolve_overlap(&mut self) -> Vec<EntityId> {
        let Some(player_id) = self.player else {
            return Vec::new();
        };
        let Some(player_rect) = self.entity_rect(player_id) else {
            return Vec::new();
        };

        // 收集与玩家重叠的实体（排除玩家自身）。
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

        // 逐个推离：取最小平移轴。
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

    /// 读取实体的 AABB（内部使用；公开版本见 [`World::bounds`]）。
    fn entity_rect(&self, id: EntityId) -> Option<reles_math::Rect> {
        self.bounds(id)
    }
}

/// 读取 f32 属性。
fn get_f32(e: &Entity, key: &str) -> Option<f32> {
    match e.get(key)? {
        FieldValue::Float(v) => Some(*v),
        FieldValue::Int(v) => Some(*v as f32),
        _ => None,
    }
}

/// 读取 i32 属性。
fn get_i32(e: &Entity, key: &str) -> Option<i32> {
    match e.get(key)? {
        FieldValue::Int(v) => Some(*v),
        FieldValue::Float(v) => Some(*v as i32),
        _ => None,
    }
}

/// AABB 重叠判定。
fn rects_overlap(a: reles_math::Rect, b: reles_math::Rect) -> bool {
    a.min.x <= b.max.x && a.max.x >= b.min.x && a.min.y <= b.max.y && a.max.y >= b.min.y
}

/// 把 `pos`（尺寸 `size`）沿最小平移轴推出 `blocker`。
fn push_out(
    pos: reles_math::Vec2,
    size: reles_math::Vec2,
    blocker: reles_math::Rect,
) -> reles_math::Vec2 {
    if !rects_overlap(reles_math::Rect::from_origin_size(pos, size), blocker) {
        return pos;
    }

    let dx_left = blocker.min.x - (pos.x + size.x); // 推向左侧的位移
    let dx_right = blocker.max.x - pos.x; // 推向右侧的位移
    let dy_up = blocker.min.y - (pos.y + size.y); // 推向上方的位移
    let dy_down = blocker.max.y - pos.y; // 推向下方的位移

    let candidates = [
        (
            dx_left.abs(),
            reles_math::Vec2::new(dx_left, reles_math::Fx::zero()),
        ),
        (
            dx_right.abs(),
            reles_math::Vec2::new(dx_right, reles_math::Fx::zero()),
        ),
        (
            dy_up.abs(),
            reles_math::Vec2::new(reles_math::Fx::zero(), dy_up),
        ),
        (
            dy_down.abs(),
            reles_math::Vec2::new(reles_math::Fx::zero(), dy_down),
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
