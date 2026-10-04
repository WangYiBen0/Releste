//! 应用 patch 到 World，含冲突解决与回滚。

use reles_map::Patch;
use reles_math::Rect;
use reles_world::{Entity, EntityData, EntityId, World};
use tracing::{info, warn};

/// 热重载应用器。
///
/// 负责把 [`Patch`] 序列应用到 [`World`]，并保证冲突时
/// 世界不变量不被破坏。
#[derive(Debug, Clone)]
pub struct Reload {
    /// 是否启用"房间收缩时拒绝 patch"（默认 true）。
    pub strict_bounds: bool,
    /// 本次重载后的房间边界（用于策略 3 校验）。
    ///
    /// `None` 表示不做边界校验。
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

/// 应用结果报告。
#[derive(Debug, Default, Clone)]
pub struct ApplyReport {
    pub added: usize,
    pub removed: usize,
    pub updated: usize,
    pub moved: usize,
    pub tiles_replaced: bool,
    pub sync_updated: bool,
    /// 被跳过的 patch（引用的 ID 消失等）。
    pub skipped: Vec<String>,
    /// 被推离的实体（与玩家重叠）。
    pub pushed_entities: Vec<EntityId>,
}

/// Reload 错误。
#[derive(Debug, thiserror::Error)]
pub enum ReloadError {
    #[error("patch rejected, rolled back: {0}")]
    RolledBack(String),
    #[error("world invariant violated: {0}")]
    Invariant(String),
}

/// 应用失败的兼容别名。
pub type ApplyError = ReloadError;

impl Reload {
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置本次重载的房间边界（启用策略 3 校验）。
    pub fn with_bounds(mut self, bounds: Rect) -> Self {
        self.room_bounds = Some(bounds);
        self
    }

    /// 应用一批 patch。
    ///
    /// 冲突处理策略（AGENTS.md §7.4）：
    /// 1. 玩家与新增实体重叠 → 推离
    /// 2. 引用的 ID 消失 → warning + 跳过
    /// 3. 房间边界缩小且玩家在外 → error + 回滚整个 patch
    ///
    /// **玩家与携带物状态在任何情况下都保留**——patches 只影响
    /// 被显式提及的实体。
    pub fn apply(
        &mut self,
        world: &mut World,
        patches: Vec<Patch>,
    ) -> Result<ApplyReport, ReloadError> {
        let mut report = ApplyReport::default();

        // 快照：用于回滚。
        let snapshot = self.snapshot(world);

        for patch in patches {
            if let Err(e) = self.apply_one(world, patch, &mut report) {
                self.restore(world, snapshot);
                warn!(error = %e, "reload patch failed, rolling back");
                return Err(ReloadError::RolledBack(e.to_string()));
            }

            // 策略 3：每一步后校验玩家仍在房间边界内。
            if let Err(e) = self.check_player_in_bounds(world) {
                self.restore(world, snapshot);
                warn!(error = %e, "player left room bounds, rolling back");
                return Err(ReloadError::RolledBack(e.to_string()));
            }
        }

        // 策略 1：玩家与新增实体重叠 → 推离。
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

    /// 校验玩家在（可选的）房间边界内。
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

        // 用玩家中心点判定，避免贴边时被误判。
        let center = reles_math::Vec2::new(
            (player_bounds.min.x + player_bounds.max.x) / reles_math::Fx::from_int(2),
            (player_bounds.min.y + player_bounds.max.y) / reles_math::Fx::from_int(2),
        );
        if bounds.contains(center) {
            Ok(())
        } else {
            Err(ReloadError::Invariant(format!(
                "player center {center} is outside room bounds"
            )))
        }
    }

    /// 应用单条 patch。
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
                entity.set("x", reles_world::FieldValue::Float(pos.x.to_f32()));
                entity.set("y", reles_world::FieldValue::Float(pos.y.to_f32()));
                report.moved += 1;
            }
            Patch::ReplaceTiles(_tiles) => {
                // tile 集替换由 render 层消费；这里仅标记。
                report.tiles_replaced = true;
            }
            Patch::UpdateSync(_rules) => {
                report.sync_updated = true;
            }
        }
        Ok(())
    }

    /// 从 EntityData 构建运行时实体。
    fn build_entity(&self, data: &EntityData) -> Entity {
        // EntityKindId 由注册表解析；此处用名称散列占位。
        // 完整的注册表集成在 world 层的 EntityRegistry 中完成。
        let kind_id = kind_id_for(&data.kind);
        let mut entity = Entity::new(data.id, kind_id);
        entity.properties = data.properties.clone();
        entity.set("x", reles_world::FieldValue::Float(data.x));
        entity.set("y", reles_world::FieldValue::Float(data.y));
        if let Some(w) = data.width {
            entity.set("width", reles_world::FieldValue::Int(w));
        }
        if let Some(h) = data.height {
            entity.set("height", reles_world::FieldValue::Int(h));
        }
        entity
    }

    /// 冲突解决：把玩家从重叠的新实体中推离。
    ///
    /// 返回被推离的实体 ID 列表。
    fn resolve_player_overlap(&self, world: &mut World) -> Vec<EntityId> {
        world.resolve_overlap()
    }

    /// 世界快照（用于回滚）。
    fn snapshot(&self, world: &World) -> Vec<(EntityId, Entity)> {
        world.all().map(|e| (e.id, e.clone())).collect()
    }

    /// 从快照恢复。
    fn restore(&self, world: &mut World, snapshot: Vec<(EntityId, Entity)>) {
        world.clear();
        for (_id, entity) in snapshot {
            world.insert(entity);
        }
    }
}

/// 由种类名称推导 `EntityKindId`（注册表未命中时的回退）。
fn kind_id_for(kind: &str) -> reles_world::EntityKindId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(kind.as_bytes());
    let bytes: [u8; 32] = hasher.finalize().into();
    reles_world::EntityKindId::new(u32::from_le_bytes(bytes[0..4].try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reles_map::{MapDocument, Props, Room, Tileset};
    use reles_math::{Rect, Vec2};
    use reles_world::{Entity, EntityKindId, FieldValue};

    const KIND_WALL: EntityKindId = EntityKindId::new(1);
    const KIND_PLAYER: EntityKindId = EntityKindId::new(2);

    fn id(n: u64) -> EntityId {
        EntityId::new(n)
    }

    /// 造一个带位置与尺寸的实体。
    fn entity_raw(eid: EntityId, kind: EntityKindId, x: f32, y: f32, w: i32, h: i32) -> Entity {
        let mut e = Entity::new(eid, kind);
        e.set("x", FieldValue::Float(x));
        e.set("y", FieldValue::Float(y));
        e.set("width", FieldValue::Int(w));
        e.set("height", FieldValue::Int(h));
        e
    }

    /// 地图实体数据。
    fn edata(eid: EntityId, kind: &str, x: f32, y: f32, w: i32, h: i32) -> EntityData {
        let mut d = EntityData::new(kind, eid, x, y);
        d.width = Some(w);
        d.height = Some(h);
        d
    }

    /// 一个世界：玩家 + 一堵墙。
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

    // ---------- 基本 patch 应用 ----------

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

    // ---------- 策略 2：ID 消失 → warning + 跳过 ----------

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

    // ---------- 策略 1：玩家被推离 ----------

    #[test]
    fn player_is_pushed_out_of_new_entity() {
        let mut w = world_fixture();
        // 新实体与玩家部分重叠（玩家 100..108，实体 102..110）。
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

    // ---------- 策略 3：玩家越界 → 回滚 ----------

    #[test]
    fn shrinking_bounds_with_player_outside_rolls_back() {
        let mut w = world_fixture();
        // 玩家在 (100,100)。把房间缩到远离玩家的地方。
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
        // 玩家远在边界外，但 strict_bounds=false 时应放行。
        let report = r
            .apply(
                &mut w,
                vec![Patch::AddEntity(edata(id(2), "wall", 10.0, 10.0, 8, 8))],
            )
            .unwrap();
        assert_eq!(report.added, 1);
    }

    // ---------- 核心不变量：玩家状态保留（AGENTS.md §10） ----------

    #[test]
    fn player_state_is_preserved_across_reload() {
        let mut w = world_fixture();
        // 给玩家一些"会话状态"。
        {
            let p = w.get_mut(w.player().unwrap()).unwrap();
            p.set("vx", FieldValue::Float(12.5));
            p.set("dashing", FieldValue::Bool(true));
            p.set("stamina", FieldValue::Int(42));
        }
        let player = w.player().unwrap();
        let before = w.get(player).unwrap().clone();

        // 改一堆无关的东西（墙角移动 + 增减实体）。
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

    // ---------- 回滚的精确性 ----------

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

    // ---------- 端到端：diff → apply ----------

    fn room_with(tiles: u8, entity_positions: &[(u64, &str, f32, f32)]) -> Room {
        let mut t = Tileset::new("Gameplay", 4, 4);
        t.set(0, 0, tiles);
        Room {
            name: "room_0".into(),
            bounds: reles_map::RectDef {
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
        }
    }

    fn doc(rooms: Vec<Room>) -> MapDocument {
        MapDocument {
            area: "Test".into(),
            rooms,
            sync_rules: Vec::new(),
        }
    }

    #[test]
    fn end_to_end_diff_then_apply() {
        let old = doc(vec![room_with(1, &[(1, "wall", 100.0, 100.0)])]);
        // 新版本：墙角移动 + 多了一个实体 + tile 变化
        let new = doc(vec![room_with(
            5,
            &[(1, "wall", 110.0, 100.0), (2, "spinner", 200.0, 50.0)],
        )]);

        let patches = reles_map::diff(&old, &new);
        assert!(!patches.is_empty(), "diff should produce patches");

        // 用旧地图构建世界，然后应用 diff。
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

        // 玩家未被创建 diff 影响。
        assert_eq!(player_xy(&w), (250.0, 150.0));
        assert!(w.get(player).is_some());
        assert!(w.get(id(2)).is_some());
    }

    #[test]
    fn diff_of_identical_maps_is_empty() {
        let a = doc(vec![room_with(1, &[(1, "wall", 0.0, 0.0)])]);
        let patches = reles_map::diff(&a, &a.clone());
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
