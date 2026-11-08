//! Editor data model: document, selection, undo stack.
//!
//! This layer does not depend on egui, so it can be tested and can be used for
//! scripted editing (for example batch map fixes) in a headless environment.

use std::path::{Path, PathBuf};

use releste_map::{load_map, save_map, MapDocument, RectDef, Room, Tileset};
use releste_math::Vec2;
use releste_world::{EntityData, EntityId, FieldValue};
use thiserror::Error;

/// Editor error.
#[derive(Debug, Error)]
pub enum EditorError {
    #[error("map error: {0}")]
    Map(#[from] releste_map::MapError),
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

/// Default depth of the undo stack.
pub const DEFAULT_UNDO_DEPTH: usize = 200;

/// Selection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Selection {
    /// Nothing selected.
    #[default]
    None,
    /// One entity is selected.
    Entity(EntityId),
    /// A tile rectangle is selected (room index + half-open range).
    Tiles {
        room: usize,
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
    },
}

impl Selection {
    /// Whether an entity is selected.
    pub fn entity(&self) -> Option<EntityId> {
        match self {
            Selection::Entity(id) => Some(*id),
            _ => None,
        }
    }
}

/// A rectangular copy of tile cells for paste.
///
/// Cell values are engine tile ids; 0 means empty. The grid is row-major
/// and room-relative, so pasting works across rooms of any size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileClipboard {
    /// Width in cells.
    pub width: u32,
    /// Height in cells.
    pub height: u32,
    /// Row-major cell values.
    pub cells: Vec<u16>,
}

impl TileClipboard {
    /// The value at (x, y) relative to the clipboard origin, or 0 when out
    /// of range.
    pub fn get(&self, x: u32, y: u32) -> u16 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.cells[(y as usize) * (self.width as usize) + (x as usize)]
    }
}

/// Document snapshot (the undo unit).
#[derive(Debug, Clone, PartialEq)]
struct Snapshot {
    document: MapDocument,
    /// Action description (the UI shows "Undo: Move entity").
    label: String,
}

/// Editor state.
pub struct Editor {
    document: MapDocument,
    path: Option<PathBuf>,
    dirty: bool,
    selection: Selection,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    undo_depth: usize,
    current_room: usize,
    /// In-flight stroke: the pre-stroke document, committed on end_stroke.
    stroke: Option<Snapshot>,
    /// Whether the open stroke actually changed something.
    stroke_touched: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    /// Creates a new empty document.
    pub fn new() -> Self {
        Editor {
            document: MapDocument {
                area: "NewArea".into(),
                rooms: Vec::new(),
                sync_rules: Vec::new(),
                style: releste_map::MapStyle::default(),
            },
            path: None,
            dirty: false,
            selection: Selection::None,
            undo: Vec::new(),
            redo: Vec::new(),
            undo_depth: DEFAULT_UNDO_DEPTH,
            current_room: 0,
            stroke: None,
            stroke_touched: false,
        }
    }

    /// Opens an existing map.
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
            stroke: None,
            stroke_touched: false,
        })
    }

    /// Sets the undo depth.
    pub fn with_undo_depth(mut self, depth: usize) -> Self {
        self.undo_depth = depth.max(1);
        self
    }

    // ---------- Queries ----------

    /// The document (read-only).
    pub fn document(&self) -> &MapDocument {
        &self.document
    }

    /// The document (mutable). **Does not** record undo; only for rendering / read-only sync.
    ///
    /// Call [`Editor::touch`] after modifying.
    pub fn document_mut(&mut self) -> &mut MapDocument {
        &mut self.document
    }

    /// Current file path.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether there are unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Selection.
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Sets the selection (does not record undo).
    pub fn select(&mut self, selection: Selection) {
        self.selection = selection;
    }

    /// Current room index.
    pub fn current_room(&self) -> usize {
        self.current_room
    }

    /// Switches the current room.
    pub fn set_current_room(&mut self, index: usize) -> Result<(), EditorError> {
        if index >= self.document.rooms.len() {
            return Err(EditorError::NoRoom(index));
        }
        self.current_room = index;
        Ok(())
    }

    /// The current room.
    pub fn room(&self) -> Option<&Room> {
        self.document.rooms.get(self.current_room)
    }

    /// Number of undoable steps.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Number of redoable steps.
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Label of the next undo.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    /// Marks the document as modified externally (for example a render-layer sync).
    pub fn touch(&mut self) {
        self.dirty = true;
    }

    // ---------- Saving ----------

    /// Saves to the current path.
    pub fn save(&mut self) -> Result<(), EditorError> {
        let Some(path) = self.path.clone() else {
            return Err(EditorError::Map(releste_map::MapError::Io(
                std::io::Error::new(std::io::ErrorKind::NotFound, "no path set; use save_as"),
            )));
        };
        self.save_as(&path)
    }

    /// Saves as.
    pub fn save_as(&mut self, path: &Path) -> Result<(), EditorError> {
        save_map(&self.document, path)?;
        self.path = Some(path.to_path_buf());
        self.dirty = false;
        Ok(())
    }

    // ---------- Undo / redo ----------

    /// Records one undoable operation.
    ///
    /// Inside an open stroke (see [`Editor::begin_stroke`]) the snapshot is
    /// deferred: the whole stroke collapses into the single snapshot taken
    /// when the stroke began, so dragging a brush across twenty cells is one
    /// undo step, not twenty.
    fn record(&mut self, label: impl Into<String>) {
        let label = label.into();
        if self.stroke.is_some() {
            // The pre-stroke snapshot already holds the document; per-op
            // labels are discarded in favour of the stroke label.
            self.stroke_touched = true;
            self.redo.clear();
            self.dirty = true;
            return;
        }
        self.undo.push(Snapshot {
            document: self.document.clone(),
            label,
        });
        if self.undo.len() > self.undo_depth {
            self.undo.remove(0);
        }
        // A new operation invalidates the redo stack.
        self.redo.clear();
        self.dirty = true;
    }

    /// Begins a multi-operation stroke that undoes as one step.
    ///
    /// Call this when a pointer gesture starts (or a script run begins) and
    /// [`Editor::end_stroke`] when it finishes. Opening a stroke while
    /// another is open commits the previous one first. A stroke that never
    /// changed anything pushes no undo entry.
    pub fn begin_stroke(&mut self, label: impl Into<String>) {
        self.end_stroke();
        self.stroke = Some(Snapshot {
            document: self.document.clone(),
            label: label.into(),
        });
        self.stroke_touched = false;
    }

    /// Commits the open stroke as one undo entry (if it changed anything).
    pub fn end_stroke(&mut self) {
        if let Some(pre) = self.stroke.take() {
            if self.stroke_touched {
                self.undo.push(pre);
                if self.undo.len() > self.undo_depth {
                    self.undo.remove(0);
                }
            }
        }
        self.stroke_touched = false;
    }

    /// Undoes one step.
    pub fn undo(&mut self) -> Result<&str, EditorError> {
        // Undoing mid-stroke commits the stroke first, then undoes it.
        self.end_stroke();
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

    /// Redoes one step.
    pub fn redo(&mut self) -> Result<&str, EditorError> {
        // Redoing mid-stroke commits the stroke first.
        self.end_stroke();
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

    // ---------- Room operations ----------

    /// Adds a room.
    pub fn add_room(&mut self, room: Room) -> Result<usize, EditorError> {
        if self.document.rooms.iter().any(|r| r.name == room.name) {
            return Err(EditorError::DuplicateRoom(room.name));
        }
        self.record(format!("Add room {}", room.name));
        self.document.rooms.push(room);
        Ok(self.document.rooms.len() - 1)
    }

    /// Removes a room.
    pub fn remove_room(&mut self, index: usize) -> Result<Room, EditorError> {
        if index >= self.document.rooms.len() {
            return Err(EditorError::NoRoom(index));
        }
        self.record(format!("Remove room {index}"));
        let room = self.document.rooms.remove(index);
        self.clamp_current_room();
        Ok(room)
    }

    /// Renames a room.
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
        self.record(format!("Rename room {old} -> {name}"));
        self.document.rooms[index].name = name;
        Ok(())
    }

    /// Sets the room bounds.
    ///
    /// If the bounds shrink and the player ends up outside, hot reload rolls back -- but
    /// the editor stage allows it first and the reload after saving validates it.
    pub fn set_room_bounds(&mut self, index: usize, bounds: RectDef) -> Result<(), EditorError> {
        let name = self
            .document
            .rooms
            .get(index)
            .ok_or(EditorError::NoRoom(index))?
            .name
            .clone();
        self.record(format!("Resize room {name} bounds"));
        {
            let room = &mut self.document.rooms[index];
            room.bounds = bounds;
            // Tile grids must always match the room size (one 8 px cell per
            // tile). A stale grid drew a smaller grid inside the enlarged
            // room and saved an invalid map.
            let gw = ((bounds.w / 8.0).round() as u32).max(1);
            let gh = ((bounds.h / 8.0).round() as u32).max(1);
            room.tiles = Self::resized_tileset(&room.tiles, gw, gh);
            room.bg = Self::resized_tileset(&room.bg, gw, gh);
            room.bg_scenery = Self::resized_tileset(&room.bg_scenery, gw, gh);
            room.fg_scenery = Self::resized_tileset(&room.fg_scenery, gw, gh);
        }
        Ok(())
    }

    // ---------- Tile operations ----------

    /// Copies a tile grid into a new grid of the given size, preserving the
    /// overlapping region and zero-filling the rest.
    fn resized_tileset(ts: &Tileset, width: u32, height: u32) -> Tileset {
        let mut next = Tileset::new(ts.atlas.clone(), width, height);
        for y in 0..ts.height.min(height) {
            for x in 0..ts.width.min(width) {
                if let Some(v) = ts.get(x, y) {
                    next.set(x, y, v);
                }
            }
        }
        next
    }

    /// Ensures the current room's tile grid size (grows it when too small, preserving existing content).
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

        self.record("Resize tile grid");

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

    /// Brush: sets one tile.
    pub fn paint_tile(&mut self, x: u32, y: u32, value: u16) -> Result<(), EditorError> {
        let idx = self.current_room;
        let room = self
            .document
            .rooms
            .get(idx)
            .ok_or(EditorError::NoRoom(idx))?;

        let Some(current) = room.tiles.get(x, y) else {
            // Out of bounds: silently ignore (the brush was dragged past the edge).
            return Ok(());
        };
        if current == value {
            return Ok(());
        }

        self.record(format!("Paint tile ({x},{y})"));
        self.document.rooms[idx].tiles.set(x, y, value);
        Ok(())
    }

    /// Eraser: clears one tile.
    pub fn erase_tile(&mut self, x: u32, y: u32) -> Result<(), EditorError> {
        self.paint_tile(x, y, 0)
    }

    /// Fills a rectangular region.
    ///
    /// Returns the number of cells actually changed (0 means no-op).
    pub fn fill_tiles(
        &mut self,
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
        value: u16,
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

        self.record(format!("Fill ({lx},{ly})-({hx},{hy})"));
        let room = &mut self.document.rooms[idx];
        for y in ly..=hy {
            for x in lx..=hx {
                room.tiles.set(x, y, value);
            }
        }
        Ok(changed)
    }

    // ---------- Tile clipboard ----------

    /// Copies an inclusive tile rectangle from one room into a clipboard.
    ///
    /// Returns `None` when the room does not exist; cells outside the room
    /// grid come back as 0 (empty), so partial overlaps are safe.
    pub fn copy_tiles(
        &self,
        room: usize,
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
    ) -> Option<TileClipboard> {
        let tiles = &self.document.rooms.get(room)?.tiles;
        let (lx, hx) = (x0.min(x1), x0.max(x1));
        let (ly, hy) = (y0.min(y1), y0.max(y1));
        let width = hx.saturating_sub(lx) + 1;
        let height = hy.saturating_sub(ly) + 1;
        let mut cells = Vec::with_capacity((width as usize) * (height as usize));
        for y in ly..=hy {
            for x in lx..=hx {
                cells.push(tiles.get(x, y).unwrap_or(0));
            }
        }
        Some(TileClipboard {
            width,
            height,
            cells,
        })
    }

    /// Cuts an inclusive tile rectangle: copies it and clears the source.
    ///
    /// The undo entry is only recorded when something actually changed;
    /// the clipboard is still returned for an all-empty region.
    pub fn cut_tiles(
        &mut self,
        room: usize,
        x0: u32,
        y0: u32,
        x1: u32,
        y1: u32,
    ) -> Option<TileClipboard> {
        let clip = self.copy_tiles(room, x0, y0, x1, y1)?;
        if self.delete_tiles(room, x0, y0, x1, y1) == 0 {
            return Some(clip);
        }
        Some(clip)
    }

    /// Clears an inclusive tile rectangle. Returns the number of cells
    /// actually changed; 0 records no undo entry.
    pub fn delete_tiles(&mut self, room: usize, x0: u32, y0: u32, x1: u32, y1: u32) -> usize {
        let Some(r) = self.document.rooms.get(room) else {
            return 0;
        };
        let (lx, hx) = (x0.min(x1), x0.max(x1));
        let (ly, hy) = (y0.min(y1), y0.max(y1));
        let changed = (ly..=hy)
            .flat_map(|y| (lx..=hx).map(move |x| (x, y)))
            .filter(|(x, y)| r.tiles.get(*x, *y).is_some_and(|v| v != 0))
            .count();
        if changed == 0 {
            return 0;
        }

        self.record(format!("Delete tiles ({lx},{ly})-({hx},{hy})"));
        let r = &mut self.document.rooms[room];
        for y in ly..=hy {
            for x in lx..=hx {
                r.tiles.set(x, y, 0);
            }
        }
        changed
    }

    /// Pastes clipboard cells with the clipboard origin at (x, y).
    ///
    /// Cells that fall outside the room grid are skipped, so a clipboard
    /// can hang over an edge. Returns the number of cells actually
    /// changed; 0 records no undo entry.
    pub fn paste_tiles(&mut self, room: usize, x: u32, y: u32, clip: &TileClipboard) -> usize {
        let Some(r) = self.document.rooms.get(room) else {
            return 0;
        };
        let changed = (0..clip.height)
            .flat_map(|dy| (0..clip.width).map(move |dx| (dx, dy)))
            .filter(|(dx, dy)| {
                let cell = clip.get(*dx, *dy);
                let (gx, gy) = (x.saturating_add(*dx), y.saturating_add(*dy));
                // Any in-bounds cell whose value differs counts as a change,
                // including empty clipboard cells clearing existing tiles.
                r.tiles.get(gx, gy).is_some_and(|v| v != cell)
            })
            .count();
        if changed == 0 {
            return 0;
        }

        self.record(format!("Paste {}x{} tiles", clip.width, clip.height));
        let r = &mut self.document.rooms[room];
        for dy in 0..clip.height {
            for dx in 0..clip.width {
                let (gx, gy) = (x.saturating_add(dx), y.saturating_add(dy));
                r.tiles.set(gx, gy, clip.get(dx, dy));
            }
        }
        changed
    }

    // ---------- Entity operations ----------

    /// Adds an entity.
    pub fn add_entity(&mut self, entity: EntityData) -> Result<EntityId, EditorError> {
        let idx = self.current_room;
        if idx >= self.document.rooms.len() {
            return Err(EditorError::NoRoom(idx));
        }
        self.record(format!("Add entity {}", entity.kind));
        let id = entity.id;
        self.document.rooms[idx].entities.push(entity);
        Ok(id)
    }

    /// Removes an entity.
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

        self.record(format!("Remove entity {id}"));
        let entity = self.document.rooms[idx].entities.remove(pos);
        if self.selection.entity() == Some(id) {
            self.selection = Selection::None;
        }
        Ok(entity)
    }

    /// Moves an entity.
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

        self.record(format!("Move entity {id}"));
        let entity = self.document.rooms[idx]
            .entities
            .iter_mut()
            .find(|e| e.id == id)
            .expect("checked above");
        entity.x = new_x;
        entity.y = new_y;
        Ok(())
    }

    /// Moves an entity without recording an undo step.
    ///
    /// This is the interactive continuation of [`Editor::move_entity`]: the
    /// first frame of a drag gesture records via `move_entity`, and every
    /// following frame calls this so the whole gesture stays one undo entry.
    pub fn set_entity_position(&mut self, id: EntityId, x: f32, y: f32) -> Result<(), EditorError> {
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
        if (entity.x - x).abs() < f32::EPSILON && (entity.y - y).abs() < f32::EPSILON {
            return Ok(());
        }
        let entity = self.document.rooms[idx]
            .entities
            .iter_mut()
            .find(|e| e.id == id)
            .expect("checked above");
        entity.x = x;
        entity.y = y;
        Ok(())
    }

    /// Sets an entity property.
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

        self.record(format!("Set {id}.{key}"));
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
    use releste_math::Vec2;

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
            ..Default::default()
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

        // Filling the same value again -> 0 changes
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
    fn dragging_position_does_not_record_undo() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        let id = e.add_entity(entity("spinner", 10.0, 20.0)).unwrap();
        let before = e.undo_len();
        e.set_entity_position(id, 88.0, 96.0).unwrap();
        assert_eq!(e.undo_len(), before);
        let got = &e.room().unwrap().entities[0];
        assert_eq!(got.x, 88.0);
        assert_eq!(got.y, 96.0);
    }

    #[test]
    fn drag_then_undo_restores_the_pre_move_position() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        let id = e.add_entity(entity("spinner", 10.0, 20.0)).unwrap();
        e.move_entity(id, Vec2::from_f32s(30.0, 40.0)).unwrap();
        e.set_entity_position(id, 50.0, 60.0).unwrap();
        e.undo().unwrap();
        let got = &e.room().unwrap().entities[0];
        assert_eq!(got.x, 10.0);
        assert_eq!(got.y, 20.0);
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
        assert_eq!(e.undo_label(), Some("Add room start"));
    }

    #[test]
    fn save_and_reopen_roundtrip() {
        let dir = std::env::temp_dir().join("releste-editor-test");
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

    #[test]
    fn resizing_room_bounds_resyncs_the_tile_grids() {
        let mut e = Editor::new();
        let mut room = room("r");
        room.bounds = RectDef {
            x: 0.0,
            y: 0.0,
            w: 16.0,
            h: 16.0,
        };
        room.tiles = Tileset::new("Gameplay", 2, 2);
        room.tiles.set(1, 1, 7);
        e.add_room(room).unwrap();

        e.set_room_bounds(
            0,
            RectDef {
                x: 0.0,
                y: 0.0,
                w: 32.0,
                h: 24.0,
            },
        )
        .expect("resize grows");
        let r = e.room().expect("room");
        assert_eq!((r.tiles.width, r.tiles.height), (4, 3));
        assert_eq!(r.tiles.get(1, 1), Some(7));
        assert_eq!((r.bg.width, r.bg.height), (4, 3));

        e.set_room_bounds(
            0,
            RectDef {
                x: 0.0,
                y: 0.0,
                w: 8.0,
                h: 8.0,
            },
        )
        .expect("resize shrinks");
        let r = e.room().expect("room");
        assert_eq!((r.tiles.width, r.tiles.height), (1, 1));
    }

    #[test]
    fn a_paint_stroke_is_one_undo_step() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        e.begin_stroke("Paint tiles");
        e.paint_tile(0, 0, 5).unwrap();
        e.paint_tile(1, 0, 5).unwrap();
        e.paint_tile(2, 0, 5).unwrap();
        e.end_stroke();
        // add_room + the merged stroke.
        assert_eq!(e.undo_len(), 2);
        assert_eq!(e.undo_label(), Some("Paint tiles"));
        e.undo().unwrap();
        let r = e.room().expect("room");
        assert_eq!(r.tiles.get(0, 0), Some(0));
        assert_eq!(r.tiles.get(2, 0), Some(0));
    }

    #[test]
    fn a_drag_fill_stroke_is_one_undo_step() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        e.begin_stroke("Paint tiles");
        e.fill_tiles(0, 0, 3, 2, 9).unwrap();
        e.paint_tile(5, 5, 9).unwrap();
        e.end_stroke();
        // add_room + the merged stroke.
        assert_eq!(e.undo_len(), 2);
        e.undo().unwrap();
        e.redo().unwrap();
        assert_eq!(e.room().expect("room").tiles.get(5, 5), Some(9));
    }

    #[test]
    fn an_unchanged_stroke_pushes_no_undo_entry() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        e.begin_stroke("Paint tiles");
        // Painting the same value everywhere changes nothing.
        e.paint_tile(0, 0, 0).unwrap();
        e.fill_tiles(0, 0, 3, 3, 0).unwrap();
        e.end_stroke();
        // Only add_room; the no-op stroke pushed nothing.
        assert_eq!(e.undo_len(), 1);
    }

    #[test]
    fn undo_during_a_stroke_commits_the_stroke_first() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        e.begin_stroke("Paint tiles");
        e.paint_tile(1, 1, 3).unwrap();
        // Undo while the stroke is still open: the stroke is committed and
        // then immediately undone, so the tile is empty again.
        e.undo().unwrap();
        assert_eq!(e.room().expect("room").tiles.get(1, 1), Some(0));
        // Only add_room remains.
        assert_eq!(e.undo_len(), 1);
    }

    #[test]
    fn copy_cut_delete_paste_roundtrip() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        e.fill_tiles(1, 1, 2, 2, 7).unwrap();

        let clip = e.copy_tiles(0, 1, 1, 2, 2).expect("clip");
        assert_eq!((clip.width, clip.height), (2, 2));
        assert_eq!(clip.get(0, 0), 7);
        assert_eq!(clip.get(1, 1), 7);
        assert_eq!(clip.get(5, 5), 0);

        let cut = e.cut_tiles(0, 1, 1, 2, 2).expect("clip");
        assert_eq!(cut, clip);
        let r = e.room().expect("room");
        assert_eq!(r.tiles.get(1, 1), Some(0));
        assert_eq!(e.undo_len(), 3); // add_room + fill + cut

        e.undo().unwrap(); // undo cut
        assert_eq!(e.room().expect("room").tiles.get(2, 2), Some(7));

        // Paste shifted to (3, 3).
        let pasted = e.paste_tiles(0, 3, 3, &clip);
        assert_eq!(pasted, 4);
        let r = e.room().expect("room");
        assert_eq!(r.tiles.get(3, 3), Some(7));
        assert_eq!(r.tiles.get(4, 4), Some(7));
    }

    #[test]
    fn paste_over_the_edge_skips_outside_cells() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap(); // 8x8 grid
        let clip = TileClipboard {
            width: 2,
            height: 1,
            cells: vec![3, 4],
        };
        // Origin (7, 7): the second cell hangs past the edge.
        let changed = e.paste_tiles(0, 7, 7, &clip);
        assert_eq!(changed, 1);
        let r = e.room().expect("room");
        assert_eq!(r.tiles.get(7, 7), Some(3));
    }

    #[test]
    fn delete_of_an_empty_region_records_nothing() {
        let mut e = Editor::new();
        e.add_room(room("a")).unwrap();
        assert_eq!(e.delete_tiles(0, 0, 0, 3, 3), 0);
        assert_eq!(e.undo_len(), 1); // only add_room
        e.begin_stroke("Delete tiles");
        assert_eq!(e.delete_tiles(0, 0, 0, 3, 3), 0);
        e.end_stroke();
        assert_eq!(e.undo_len(), 1);
    }
}
