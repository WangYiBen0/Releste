//! 编辑器数据模型：文档、选区、撤销栈。
//!
//! 这一层不依赖 egui，因此可以被测试，也能在 headless 环境
//! 用于脚本化编辑（如批量修图）。

use std::path::{Path, PathBuf};

use reles_map::{load_map, save_map, MapDocument, RectDef, Room, Tileset};
use reles_math::Vec2;
use reles_world::{EntityData, EntityId, FieldValue};
use thiserror::Error;

/// 编辑器错误。
#[derive(Debug, Error)]
pub enum EditorError {
    #[error("map error: {0}")]
    Map(#[from] reles_map::MapError),
    #[error("no room at index {0}")]
    NoRoom(usize),
    #[error("no entity with id {0}")]
    NoEntity(EntityId),
    #[error("undo stack is empty")]
    NothingToUndo,
    #[error("redo stack is empty")]
    NothingToRedo,
    #[error("a room named `{0}` already exists")]
    DuplicateRoom(String),
}

/// 撤销栈的默认深度。
pub const DEFAULT_UNDO_DEPTH: usize = 200;

/// 选区。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Selection {
    /// 无选中。
    #[default]
    None,
    /// 选中一个实体。
    Entity(EntityId),
    /// 选中 tile 矩形（房间索引 + 半开区间）。
    Tiles {
        room: usize,
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
    },
}

impl Selection {
    /// 是否选中了实体。
    pub fn entity(&self) -> Option<EntityId> {
        match self {
            Selection::Entity(id) => Some(*id),
            _ => None,
        }
    }
}

/// 文档快照（撤销单位）。
#[derive(Debug, Clone, PartialEq)]
struct Snapshot {
    document: MapDocument,
    /// 操作描述（UI 显示 "撤销：移动实体"）。
    label: String,
}

/// 编辑器状态。
pub struct Editor {
    document: MapDocument,
    path: Option<PathBuf>,
    dirty: bool,
    selection: Selection,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    undo_depth: usize,
    current_room: usize,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// 新建空文档。
    pub fn new() -> Self {
        Editor {
            document: MapDocument {
                area: "NewArea".into(),
                rooms: Vec::new(),
                sync_rules: Vec::new(),
            },
            path: None,
            dirty: false,
            selection: Selection::None,
            undo: Vec::new(),
            redo: Vec::new(),
            undo_depth: DEFAULT_UNDO_DEPTH,
            current_room: 0,
        }
    }

    /// 打开已有地图。
    pub fn open(path: &Path) -> Result<Self, EditorError> {
        let document = load_map(path)?;
        Ok(Editor {
            document,
            path: Some(path.to_path_buf()),
            dirty: false,
            selection: Selection::None,
            undo: Vec::new(),
            redo: Vec::new(),
            undo_depth: DEFAULT_UNDO_DEPTH,
            current_room: 0,
        })
    }

    /// 设置撤销深度。
    pub fn with_undo_depth(mut self, depth: usize) -> Self {
        self.undo_depth = depth.max(1);
        self
    }

    // ---------- 查询 ----------

    /// 文档（只读）。
    pub fn document(&self) -> &MapDocument {
        &self.document
    }

    /// 文档（可变）。**不会**记录撤销；仅供渲染 / 只读同步使用。
    ///
    /// 修改后请调用 [`Editor::touch`]。
    pub fn document_mut(&mut self) -> &mut MapDocument {
        &mut self.document
    }

    /// 当前文件路径。
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// 是否有未保存修改。
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 选区。
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// 设置选区（不记录撤销）。
    pub fn select(&mut self, selection: Selection) {
        self.selection = selection;
    }

    /// 当前房间索引。
    pub fn current_room(&self) -> usize {
        self.current_room
    }

    /// 切换当前房间。
    pub fn set_current_room(&mut self, index: usize) -> Result<(), EditorError> {
        if index >= self.document.rooms.len() {
            return Err(EditorError::NoRoom(index));
        }
        self.current_room = index;
        Ok(())
    }

    /// 当前房间。
    pub fn room(&self) -> Option<&Room> {
        self.document.rooms.get(self.current_room)
    }

    /// 可撤销步数。
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// 可重做步数。
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// 下一步撤销的标签。
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    /// 标记文档已被外部修改（如渲染层同步）。
    pub fn touch(&mut self) {
        self.dirty = true;
    }

    // ---------- 保存 ----------

    /// 保存到当前路径。
    pub fn save(&mut self) -> Result<(), EditorError> {
        let Some(path) = self.path.clone() else {
            return Err(EditorError::Map(reles_map::MapError::Io(
                std::io::Error::new(std::io::ErrorKind::NotFound, "no path set; use save_as"),
            )));
        };
        self.save_as(&path)
    }

    /// 另存为。
    pub fn save_as(&mut self, path: &Path) -> Result<(), EditorError> {
        save_map(&self.document, path)?;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        Ok(())
    }

    // ---------- 撤销 / 重做 ----------

    /// 记录一次可撤销操作。
    fn record(&mut self, label: impl Into<String>) {
        let label = label.into();
        self.undo.push(Snapshot {
            document: self.document.clone(),
            label,
        });
        if self.undo.len() > self.undo_depth {
            self.undo.remove(0);
        }
        // 新操作使重做栈失效。
        self.redo.clear();
        self.dirty = true;
    }

    /// 撤销一步。
    pub fn undo(&mut self) -> Result<&str, EditorError> {
        let snapshot = self.undo.pop().ok_or(EditorError::NothingToUndo)?;
        let current = Snapshot {
            document: std::mem::replace(&mut self.document, snapshot.document),
            label: snapshot.label.clone(),
        };
        self.redo.push(current);
        self.dirty = true;
        self.clamp_current_room();
        Ok(self.redo.last().map(|s| s.label.as_str()).unwrap_or(""))
    }

    /// 重做一步。
    pub fn redo(&mut self) -> Result<&str, EditorError> {
        let snapshot = self.redo.pop().ok_or(EditorError::NothingToRedo)?;
        let current = Snapshot {
            document: std::mem::replace(&mut self.document, snapshot.document),
            label: snapshot.label.clone(),
        };
        self.undo.push(current);
        self.dirty = true;
        self.clamp_current_room();
        Ok(self.redo.last().map(|s| s.label.as_str()).unwrap_or(""))
    }

    fn clamp_current_room(&mut self) {
        if self.document.rooms.is_empty() {
            self.current_room = 0;
        } else if self.current_room >= self.document.rooms.len() {
            self.current_room = self.document.rooms.len() - 1;
        }
    }

    // ---------- 房间操作 ----------

    /// 新增房间。
    pub fn add_room(&mut self, room: Room) -> Result<usize, EditorError> {
        if self.document.rooms.iter().any(|r| r.name == room.name) {
            return Err(EditorError::DuplicateRoom(room.name));
        }
        self.record(format!("新增房间 {}", room.name));
        self.document.rooms.push(room);
        Ok(self.document.rooms.len() - 1)
    }

    /// 删除房间。
    pub fn remove_room(&mut self, index: usize) -> Result<Room, EditorError> {
        if index >= self.document.rooms.len() {
            return Err(EditorError::NoRoom(index));
        }
        self.record(format!("删除房间 {index}"));
        let room = self.document.rooms.remove(index);
        self.clamp_current_room();
        Ok(room)
    }

    /// 重命名房间。
    pub fn rename_room(
        &mut self,
        index: usize,
        name: impl Into<String>,
    ) -> Result<(), EditorError> {
        let name = name.into();
        if self.document.rooms.iter().any(|r| r.name == name) {
            return Err(EditorError::DuplicateRoom(name));
        }
        let old = self
            .document
            .rooms
            .get(index)
            .ok_or(EditorError::NoRoom(index))?
            .name
            .clone();
        self.record(format!("重命名房间 {old} → {name}"));
        self.document.rooms[index].name = name;
        Ok(())
    }

    /// 设置房间边界。
    ///
    /// 缩小边界时若玩家在外面，热重载会回滚——但编辑器阶段
    /// 先允许，由保存后的 reload 校验。
    pub fn set_room_bounds(&mut self, index: usize, bounds: RectDef) -> Result<(), EditorError> {
        let name = self
            .document
            .rooms
            .get(index)
            .ok_or(EditorError::NoRoom(index))?
            .name
            .clone();
        self.record(format!("调整房间 {name} 边界"));
        self.document.rooms[index].bounds = bounds;
        Ok(())
    }

    // ---------- Tile 操作 ----------

    /// 确保当前房间的 tile 网格尺寸（不足则扩容，保留已有内容）。
    pub fn ensure_tile_grid(&mut self, width: u32, height: u32) -> Result<(), EditorError> {
        let idx = self.current_room;
        let room = self
            .document
            .rooms
            .get(idx)
            .ok_or(EditorError::NoRoom(idx))?;

        if room.tiles.width == width && room.tiles.height == height {
            return Ok(());
        }

        let atlas = room.tiles.atlas.clone();
        let old_w = room.tiles.width;
        let old_h = room.tiles.height;
        let old_tiles = room.tiles.tiles.clone();

        self.record("调整 tile 网格");

        let mut next = Tileset::new(atlas, width, height);
        for y in 0..old_h.min(height) {
            for x in 0..old_w.min(width) {
                let v = old_tiles
                    .get((y as usize) * (old_w as usize) + (x as usize))
                    .copied()
                    .unwrap_or(0);
                next.set(x, y, v);
            }
        }
        self.document.rooms[idx].tiles = next;
        Ok(())
    }

    /// 画笔：设置一个 tile。
    pub fn paint_tile(&mut self, x: u32, y: u32, value: u8) -> Result<(), EditorError> {
        let idx = self.current_room;
        let room = self
            .document
            .rooms
            .get(idx)
            .ok_or(EditorError::NoRoom(idx))?;

        let Some(current) = room.tiles.get(x, y) else {
            // 越界静默忽略（刷子拖到边界外）。
            return Ok(());
        };
        if current == value {
            return Ok(());
        }

        self.record(format!("绘制 tile ({x},{y})"));
        self.document.rooms[idx].tiles.set(x, y, value);
        Ok(())
    }

    /// 橡皮：清除一个 tile。
    pub fn erase_tile(&mut self, x: u32, y: u32) -> Result<(), EditorError> {
        self.paint_tile(x, y, 0)
    }

    /// 填充矩形区域。
    ///
    /// 返回实际改动的格数（0 表示 no-op）。
    pub fn fill_tiles(
        &mut self,
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
        value: u8,
    ) -> Result<usize, EditorError> {
        let idx = self.current_room;
        let room = self
            .document
            .rooms
            .get(idx)
            .ok_or(EditorError::NoRoom(idx))?;

        let (lx, hx) = (x0.min(x1), x0.max(x1));
        let (ly, hy) = (y0.min(y1), y0.max(y1));

        let mut changed = 0usize;
        for y in ly..=hy {
            for x in lx..=hx {
                if room.tiles.get(x, y).is_some_and(|v| v != value) {
                    changed += 1;
                }
            }
        }
        if changed == 0 {
            return Ok(0);
        }

        self.record(format!("填充 ({lx},{ly})-({hx},{hy})"));
        let room = &mut self.document.rooms[idx];
        for y in ly..=hy {
            for x in lx..=hx {
                room.tiles.set(x, y, value);
            }
        }
        Ok(changed)
    }

    // ---------- 实体操作 ----------

    /// 加入实体。
    pub fn add_entity(&mut self, entity: EntityData) -> Result<EntityId, EditorError> {
        let idx = self.current_room;
        if idx >= self.document.rooms.len() {
            return Err(EditorError::NoRoom(idx));
        }
        self.record(format!("新增实体 {}", entity.kind));
        let id = entity.id;
        self.document.rooms[idx].entities.push(entity);
        Ok(id)
    }

    /// 移除实体。
    pub fn remove_entity(&mut self, id: EntityId) -> Result<EntityData, EditorError> {
        let idx = self.current_room;
        let room = self
            .document
            .rooms
            .get(idx)
            .ok_or(EditorError::NoRoom(idx))?;
        let pos = room
            .entities
            .iter()
            .position(|e| e.id == id)
            .ok_or(EditorError::NoEntity(id))?;

        self.record(format!("删除实体 {id}"));
        let entity = self.document.rooms[idx].entities.remove(pos);
        if self.selection.entity() == Some(id) {
            self.selection = Selection::None;
        }
        Ok(entity)
    }

    /// 移动实体。
    pub fn move_entity(&mut self, id: EntityId, pos: Vec2) -> Result<(), EditorError> {
        let idx = self.current_room;
        let room = self
            .document
            .rooms
            .get(idx)
            .ok_or(EditorError::NoRoom(idx))?;
        let entity = room
            .entities
            .iter()
            .find(|e| e.id == id)
            .ok_or(EditorError::NoEntity(id))?;

        let new_x = pos.x.to_f32();
        let new_y = pos.y.to_f32();
        if (entity.x - new_x).abs() < f32::EPSILON && (entity.y - new_y).abs() < f32::EPSILON {
            return Ok(());
        }

        self.record(format!("移动实体 {id}"));
        let entity = self.document.rooms[idx]
            .entities
            .iter_mut()
            .find(|e| e.id == id)
            .expect("checked above");
        entity.x = new_x;
        entity.y = new_y;
        Ok(())
    }

    /// 修改实体属性。
    pub fn set_entity_prop(
        &mut self,
        id: EntityId,
        key: impl Into<String>,
        value: FieldValue,
    ) -> Result<(), EditorError> {
        let key = key.into();
        let idx = self.current_room;
        let room = self
            .document
            .rooms
            .get(idx)
            .ok_or(EditorError::NoRoom(idx))?;
        let entity = room
            .entities
            .iter()
            .find(|e| e.id == id)
            .ok_or(EditorError::NoEntity(id))?;
        if entity.properties.get(&key) == Some(&value) {
            return Ok(());
        }

        self.record(format!("修改 {id}.{key}"));
        let entity = self.document.rooms[idx]
            .entities
            .iter_mut()
            .find(|e| e.id == id)
            .expect("checked above");
        entity.properties.insert(key, value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reles_math::Vec2;

    fn room(name: &str) -> Room {
        Room {
            name: name.into(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: 320.0,
                h: 180.0,
            },
            tiles: Tileset::new("Gameplay", 8, 8),
            bg: Tileset::default(),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
        }
    }

    fn entity(kind: &str, x: f32, y: f32) -> EntityData {
        EntityData::new(kind, EntityId::new(1), x, y)
    }

    #[test]
    fn new_document_is_clean() {
        let e = Editor::new();
        assert!(!e.is_dirty());
        assert!(e.document().rooms.is_empty());
    }

    #[test]
    fn add_room_marks_dirty_and_records_undo() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        assert!(e.is_dirty());
        assert_eq!(e.undo_len(), 1);
        assert_eq!(e.document().rooms.len(), 1);
    }

    #[test]
    fn duplicate_room_name_is_rejected() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        assert!(matches!(
            e.add_room(room("a")),
            Err(EditorError::DuplicateRoom(_))
        ));
    }

    #[test]
    fn undo_restores_previous_state() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        e.add_room(room("b")).unwrap();
        assert_eq!(e.document().rooms.len(), 2);

        e.undo().unwrap();
        assert_eq!(e.document().rooms.len(), 1);
        assert_eq!(e.redo_len(), 1);

        e.redo().unwrap();
        assert_eq!(e.document().rooms.len(), 2);
    }

    #[test]
    fn undo_on_empty_stack_errors() {
        let mut e = Editor::new();
        assert!(matches!(e.undo(), Err(EditorError::NothingToUndo)));
        assert!(matches!(e.redo(), Err(EditorError::NothingToRedo)));
    }

    #[test]
    fn new_edit_clears_redo_stack() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        e.undo().unwrap();
        assert_eq!(e.redo_len(), 1);
        e.add_room(room("c")).unwrap();
        assert_eq!(e.redo_len(), 0, "new edit must invalidate redo");
    }

    #[test]
    fn undo_depth_is_bounded() {
        let mut e = Editor::new().with_undo_depth(3);
        for i in 0..10 {
            e.add_room(room(&format!("r{i}"))).unwrap();
        }
        assert_eq!(e.undo_len(), 3);
    }

    #[test]
    fn paint_tile_changes_value_and_is_undoable() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        e.paint_tile(1, 2, 42).unwrap();
        assert_eq!(e.room().unwrap().tiles.get(1, 2), Some(42));

        e.undo().unwrap();
        assert_eq!(e.room().unwrap().tiles.get(1, 2), Some(0));
    }

    #[test]
    fn painting_same_value_is_a_noop() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        e.paint_tile(0, 0, 5).unwrap();
        let before = e.undo_len();
        e.paint_tile(0, 0, 5).unwrap();
        assert_eq!(e.undo_len(), before, "no-op paint must not record undo");
    }

    #[test]
    fn painting_out_of_bounds_is_ignored() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        let before = e.undo_len();
        e.paint_tile(999, 999, 7).unwrap();
        assert_eq!(
            e.undo_len(),
            before,
            "out-of-bounds paint must not record undo"
        );
    }

    #[test]
    fn erase_clears_tile() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        e.paint_tile(3, 3, 9).unwrap();
        e.erase_tile(3, 3).unwrap();
        assert_eq!(e.room().unwrap().tiles.get(3, 3), Some(0));
    }

    #[test]
    fn fill_reports_changed_count_and_is_undoable() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        let n = e.fill_tiles(0, 0, 2, 2, 4).unwrap();
        assert_eq!(n, 9);

        // 再填同样的值 → 0 变化
        assert_eq!(e.fill_tiles(0, 0, 2, 2, 4).unwrap(), 0);

        e.undo().unwrap();
        assert_eq!(e.room().unwrap().tiles.get(0, 0), Some(0));
    }

    #[test]
    fn fill_normalizes_reversed_coordinates() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        let n = e.fill_tiles(2, 2, 0, 0, 3).unwrap();
        assert_eq!(n, 9);
        assert_eq!(e.room().unwrap().tiles.get(1, 1), Some(3));
    }

    #[test]
    fn resize_tile_grid_preserves_existing() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        e.paint_tile(1, 1, 7).unwrap();
        e.ensure_tile_grid(16, 16).unwrap();
        let tiles = &e.room().unwrap().tiles;
        assert_eq!((tiles.width, tiles.height), (16, 16));
        assert_eq!(
            tiles.get(1, 1),
            Some(7),
            "existing tiles must survive resize"
        );
    }

    #[test]
    fn entity_add_move_and_remove() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        let id = e.add_entity(entity("spinner", 10.0, 20.0)).unwrap();

        e.move_entity(id, Vec2::from_f32s(30.0, 40.0)).unwrap();
        let got = &e.room().unwrap().entities[0];
        assert_eq!(got.x, 30.0);
        assert_eq!(got.y, 40.0);

        e.remove_entity(id).unwrap();
        assert!(e.room().unwrap().entities.is_empty());
    }

    #[test]
    fn moving_to_same_position_is_noop() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        let id = e.add_entity(entity("spinner", 10.0, 20.0)).unwrap();
        let before = e.undo_len();
        e.move_entity(id, Vec2::from_f32s(10.0, 20.0)).unwrap();
        assert_eq!(e.undo_len(), before);
    }

    #[test]
    fn removing_unknown_entity_errors() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        assert!(matches!(
            e.remove_entity(EntityId::new(999)),
            Err(EditorError::NoEntity(_))
        ));
    }

    #[test]
    fn set_entity_prop_records_undo() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        let id = e.add_entity(entity("door", 0.0, 0.0)).unwrap();

        e.set_entity_prop(id, "open", FieldValue::Bool(true))
            .unwrap();
        assert_eq!(
            e.room().unwrap().entities[0].properties.get("open"),
            Some(&FieldValue::Bool(true))
        );

        e.undo().unwrap();
        assert_eq!(e.room().unwrap().entities[0].properties.get("open"), None);
    }

    #[test]
    fn removing_selected_entity_clears_selection() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        let id = e.add_entity(entity("x", 0.0, 0.0)).unwrap();
        e.select(Selection::Entity(id));
        e.remove_entity(id).unwrap();
        assert_eq!(e.selection(), &Selection::None);
    }

    #[test]
    fn undo_label_describes_the_action() {
        let mut e = Editor::new();
        e.add_room(room("start")).unwrap();
        assert_eq!(e.undo_label(), Some("新增房间 start"));
    }

    #[test]
    fn save_and_reopen_roundtrip() {
        let dir = std::env::temp_dir().join("reles-editor-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.map");

        let mut e = Editor::new();
        e.add_room(room("only")).unwrap();
        e.paint_tile(2, 2, 11).unwrap();
        e.add_entity(entity("player", 5.0, 6.0)).unwrap();
        e.save_as(&path).unwrap();
        assert!(!e.is_dirty());

        let reopened = Editor::open(&path).unwrap();
        assert_eq!(reopened.document().rooms.len(), 1);
        assert_eq!(reopened.room().unwrap().tiles.get(2, 2), Some(11));
        assert_eq!(reopened.room().unwrap().entities.len(), 1);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn save_without_path_errors() {
        let mut e = Editor::new();
        assert!(e.save().is_err());
    }

    #[test]
    fn current_room_is_clamped_after_removal() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        e.add_room(room("b")).unwrap();
        e.set_current_room(1).unwrap();
        e.remove_room(1).unwrap();
        assert_eq!(e.current_room(), 0);
    }

    #[test]
    fn set_current_room_rejects_out_of_range() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        assert!(matches!(e.set_current_room(5), Err(EditorError::NoRoom(5))));
    }
}
