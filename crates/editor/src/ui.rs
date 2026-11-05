//! egui interface: renders the [`Editor`] model as the editor UI.

use egui::{Color32, Pos2, Rect as EguiRect, Sense, Stroke, Vec2 as EguiVec2};

use releste_world::EntityId;
use releste_world::{FieldValue, Schema};

use crate::model::{Editor, Selection};
use crate::schema_panel::{SchemaPanel, WidgetKind};

/// Pixel size of one tile on the canvas.
pub const TILE_PX: f32 = 16.0;

/// UI state (not part of the document, so it is not in the undo stack).
pub struct EditorUi {
    /// Current brush tile value.
    /// The brush tile value.
    ///
    /// `u16`: decoration-layer indices go up to 352 (the flat index across the
    /// whole `tilesets/scenery`), which does not fit in `u8`.
    pub brush: u16,
    /// Entity kind currently being placed.
    pub entity_kind: String,
    /// Canvas zoom.
    pub zoom: f32,
    /// Canvas pan.
    pub pan: egui::Vec2,
    /// Status bar message.
    pub status: String,
    /// Origin of the current paint stroke (used for rectangle fills).
    paint_origin: Option<(u32, u32)>,
    /// Form per entity kind (cached to avoid recompiling every frame).
    panels: std::collections::HashMap<String, SchemaPanel>,
}

impl Default for EditorUi {
    fn default() -> Self {
        EditorUi {
            brush: 1,
            entity_kind: "player".into(),
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            status: "Ready".into(),
            paint_origin: None,
            panels: std::collections::HashMap::new(),
        }
    }
}

impl EditorUi {
    /// Creates a new one.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers the Schema for an entity kind (compiled on first encounter).
    pub fn register_schema(&mut self, kind: &str, schema: &Schema) {
        self.panels
            .entry(kind.to_string())
            .or_insert_with(|| SchemaPanel::compile(kind, schema));
    }

    /// Renders the whole interface.
    pub fn show(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        self.menu_bar(ctx, editor);
        egui::SidePanel::left("rooms").show(ctx, |ui| self.room_panel(ui, editor));
        egui::SidePanel::right("inspector")
            .default_width(260.0)
            .show(ctx, |ui| self.inspector_panel(ui, editor));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
                ui.separator();
                let dirty = if editor.is_dirty() {
                    "Unsaved*"
                } else {
                    "Saved"
                };
                ui.label(dirty);
                ui.separator();
                if let Some(p) = editor.path() {
                    ui.label(p.display().to_string());
                } else {
                    ui.label("(untitled)");
                }
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| self.canvas(ui, editor));
    }

    /// Menu bar.
    fn menu_bar(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("New").clicked() {
                        *editor = Editor::new();
                        self.status = "New map".into();
                        ui.close_menu();
                    }
                    if ui.button("Open...").clicked() {
                        self.status = "Open: pass a path on the command line".into();
                        ui.close_menu();
                    }
                    if ui.button("Save").clicked() {
                        match editor.save() {
                            Ok(()) => self.status = "Saved".into(),
                            Err(e) => self.status = format!("Save failed: {e}"),
                        }
                        ui.close_menu();
                    }
                    if ui.button("Save As...").clicked() {
                        self.status = "Save As: pass a path on the command line".into();
                        ui.close_menu();
                    }
                });

                ui.menu_button("Edit", |ui| {
                    let can_undo = editor.undo_len() > 0;
                    let label = editor
                        .undo_label()
                        .map(|l| format!("Undo {l}"))
                        .unwrap_or_else(|| "Undo".to_string());
                    if ui.add_enabled(can_undo, egui::Button::new(label)).clicked() {
                        if let Err(e) = editor.undo() {
                            self.status = format!("Undo failed: {e}");
                        }
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(editor.redo_len() > 0, egui::Button::new("Redo"))
                        .clicked()
                    {
                        if let Err(e) = editor.redo() {
                            self.status = format!("Redo failed: {e}");
                        }
                        ui.close_menu();
                    }
                });

                ui.menu_button("Room", |ui| {
                    if ui.button("Add room").clicked() {
                        let name = format!("room_{}", editor.document().rooms.len());
                        let room = releste_map::Room {
                            name,
                            bounds: releste_map::RectDef {
                                x: 0.0,
                                y: 0.0,
                                w: 320.0,
                                h: 180.0,
                            },
                            tiles: releste_map::Tileset::new("Gameplay", 20, 12),
                            bg: releste_map::Tileset::default(),
                            entities: Vec::new(),
                            triggers: Vec::new(),
                            script: None,
                            bg_decals: Vec::new(),
                            fg_decals: Vec::new(),
                            bg_scenery: releste_map::Tileset::default(),
                            fg_scenery: releste_map::Tileset::default(),
                        };
                        match editor.add_room(room) {
                            Ok(i) => {
                                let _ = editor.set_current_room(i);
                                self.status = "Room added".into();
                            }
                            Err(e) => self.status = format!("Add failed: {e}"),
                        }
                        ui.close_menu();
                    }
                });
            });
        });
    }

    /// Left side: room list + brush / entity palette.
    fn room_panel(&mut self, ui: &mut egui::Ui, editor: &mut Editor) {
        ui.heading("Rooms");
        let rooms: Vec<String> = editor
            .document()
            .rooms
            .iter()
            .map(|r| r.name.clone())
            .collect();
        let current = editor.current_room();

        egui::ScrollArea::vertical()
            .max_height(180.0)
            .show(ui, |ui| {
                for (i, name) in rooms.iter().enumerate() {
                    if ui.selectable_label(i == current, name).clicked() {
                        let _ = editor.set_current_room(i);
                    }
                }
            });

        ui.separator();
        ui.heading("Tile brush");
        ui.add(egui::Slider::new(&mut self.brush, 0..=35).text("tile value"));
        ui.label("Left click paints - drag to fill a rectangle - right click erases");

        ui.separator();
        ui.heading("Entities");
        ui.text_edit_singleline(&mut self.entity_kind);
        if ui.button("Place at current room center").clicked() {
            let kind = self.entity_kind.clone();
            let (x, y) = editor
                .room()
                .map(|r| (r.bounds.x + r.bounds.w / 2.0, r.bounds.y + r.bounds.h / 2.0))
                .unwrap_or((0.0, 0.0));
            // Stable IDs are decided by the map author / pipeline; the editor uses a placeholder hash of position + kind.
            let id = EntityId::generate(0, &kind, 0, x as i32, y as i32);
            let entity = releste_world::EntityData::new(kind, id, x, y);
            match editor.add_entity(entity) {
                Ok(id) => {
                    editor.select(Selection::Entity(id));
                    self.status = "Entity placed".into();
                }
                Err(e) => self.status = format!("Place failed: {e}"),
            }
        }
    }

    /// Right side: the Schema-driven property panel.
    fn inspector_panel(&mut self, ui: &mut egui::Ui, editor: &mut Editor) {
        ui.heading("Inspector");
        let Some(id) = editor.selection().entity() else {
            ui.label("No entity selected");
            return;
        };

        let Some((kind, x, y, props)) = editor.room().and_then(|r| {
            r.entities
                .iter()
                .find(|e| e.id == id)
                .map(|e| (e.kind.clone(), e.x, e.y, e.properties.clone()))
        }) else {
            ui.label("The selected entity no longer exists");
            return;
        };

        ui.label(format!("Kind: {kind}"));
        ui.label(format!("ID: {id}"));

        // Position: directly editable (each change goes on the undo stack).
        let mut nx = x;
        let mut ny = y;
        let changed = ui
            .horizontal(|ui| {
                let a = ui.add(egui::DragValue::new(&mut nx).speed(1.0).prefix("x "));
                let b = ui.add(egui::DragValue::new(&mut ny).speed(1.0).prefix("y "));
                a.changed() || b.changed()
            })
            .inner;

        if changed {
            let _ = editor.move_entity(id, releste_math::Vec2::from_f32s(nx, ny));
        }

        ui.separator();

        // If a Schema for this kind is registered, build a form; otherwise list the raw key-values.
        match self.panels.get_mut(&kind) {
            Some(panel) => {
                for i in 0..panel.fields.len() {
                    let name = panel.fields[i].name.clone();
                    let widget = panel.fields[i].widget.clone();

                    // The entity's actual value wins (the Schema default is only a fallback).
                    if let Some(v) = props.get(&name) {
                        panel.fields[i].value = v.clone();
                    }

                    ui.label(&panel.fields[i].label);
                    let mut value = panel.fields[i].value.clone();
                    if render_widget(ui, &name, &widget, &mut value) {
                        panel.fields[i].value = value.clone();
                        if let Err(e) = editor.set_entity_prop(id, &name, value) {
                            self.status = format!("Write failed: {e}");
                        }
                    }
                }
            }
            None => {
                ui.label("(no Schema registered for this kind; showing raw properties)");
                egui::Grid::new("raw_props").show(ui, |ui| {
                    for (k, v) in &props {
                        ui.label(k);
                        ui.label(format!("{v:?}"));
                        ui.end_row();
                    }
                });
            }
        }
    }

    /// Center: the tile canvas + entity markers.
    fn canvas(&mut self, ui: &mut egui::Ui, editor: &mut Editor) {
        // Snapshot the data needed for rendering first, so `editor` can be mutably borrowed afterwards.
        let Some((tiles_w, tiles_h, filled, entities, selected_id)) = editor.room().map(|room| {
            let w = room.tiles.width.max(1) as usize;
            let h = room.tiles.height.max(1) as usize;
            let filled: Vec<(usize, u16)> = (0..h)
                .flat_map(|y| (0..w).map(move |x| (y * w + x, (x, y))))
                .filter_map(|(idx, (x, y))| {
                    room.tiles
                        .get(x as u32, y as u32)
                        .filter(|v| *v != 0)
                        .map(|v| (idx, v))
                })
                .collect();
            let entities: Vec<(EntityId, f32, f32, i32, i32)> = room
                .entities
                .iter()
                .map(|e| (e.id, e.x, e.y, e.width.unwrap_or(1), e.height.unwrap_or(1)))
                .collect();
            (w, h, filled, entities, editor.selection().entity())
        }) else {
            ui.centered_and_justified(|ui| ui.label("No room: create one first"));
            return;
        };

        let size = EguiVec2::new(
            tiles_w as f32 * TILE_PX * self.zoom,
            tiles_h as f32 * TILE_PX * self.zoom,
        );
        let (response, painter) =
            ui.allocate_painter(size.max(EguiVec2::splat(64.0)), Sense::click_and_drag());
        let origin = response.rect.min + self.pan;

        // Background
        painter.rect_filled(response.rect, 0.0, Color32::from_gray(24));

        // tiles
        for (idx, value) in &filled {
            let x = idx % tiles_w;
            let y = idx / tiles_w;
            let rect = tile_rect(origin, x, y, self.zoom);
            // Derive a distinguishable color from the tile value (real rendering is releste-render's job).
            let hue = (*value as f32) / 36.0;
            painter.rect_filled(
                rect,
                0.0,
                Color32::from_rgb(
                    (60.0 + 180.0 * hue) as u8,
                    (120.0 + 80.0 * (1.0 - hue)) as u8,
                    (200.0 - 120.0 * hue) as u8,
                ),
            );
        }

        // Grid lines
        for x in 0..=tiles_w {
            let px = origin.x + x as f32 * TILE_PX * self.zoom;
            painter.line_segment(
                [Pos2::new(px, origin.y), Pos2::new(px, origin.y + size.y)],
                Stroke::new(0.5_f32, Color32::from_gray(48)),
            );
        }
        for y in 0..=tiles_h {
            let py = origin.y + y as f32 * TILE_PX * self.zoom;
            painter.line_segment(
                [Pos2::new(origin.x, py), Pos2::new(origin.x + size.x, py)],
                Stroke::new(0.5_f32, Color32::from_gray(48)),
            );
        }

        // Entity markers
        for (id, ex, ey, ew, eh) in &entities {
            let rect = entity_rect(origin, *ex, *ey, *ew, *eh, self.zoom);
            let selected = selected_id == Some(*id);
            painter.rect_stroke(
                rect,
                0.0,
                Stroke::new(
                    if selected { 2.5_f32 } else { 1.0_f32 },
                    if selected {
                        Color32::YELLOW
                    } else {
                        Color32::LIGHT_RED
                    },
                ),
                egui::StrokeKind::Outside,
            );
        }

        // Interaction: paint / erase
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some((tx, ty)) = screen_to_tile(origin, pos, self.zoom) {
                let erase = ui.input(|i| i.pointer.secondary_down());
                if response.drag_started() || response.clicked() {
                    self.paint_origin = Some((tx, ty));
                }
                if response.dragged() {
                    if let Some((ox, oy)) = self.paint_origin {
                        let value = if erase { 0 } else { self.brush };
                        let _ = editor.fill_tiles(ox, oy, tx, ty, value);
                    }
                } else {
                    let _ = if erase {
                        editor.erase_tile(tx, ty)
                    } else {
                        editor.paint_tile(tx, ty, self.brush)
                    };
                }
            }
        }
        if response.drag_stopped() {
            self.paint_origin = None;
        }

        // Select an entity (tile coordinates map 1:1 to world coordinates)
        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                if let Some((tx, ty)) = screen_to_tile(origin, pos, self.zoom) {
                    let (wx, wy) = (tx as f32, ty as f32);
                    if let Some((id, ..)) = entities
                        .iter()
                        .find(|(_, ex, ey, ..)| (ex - wx).abs() <= 1.0 && (ey - wy).abs() <= 1.0)
                    {
                        editor.select(Selection::Entity(*id));
                    }
                }
            }
        }
    }
}

/// A tile's rectangle on screen.
fn tile_rect(origin: Pos2, x: usize, y: usize, zoom: f32) -> EguiRect {
    let s = TILE_PX * zoom;
    let min = Pos2::new(origin.x + x as f32 * s, origin.y + y as f32 * s);
    EguiRect::from_min_size(min, EguiVec2::splat(s))
}

/// An entity's rectangle on screen (at least 1 cell visible).
fn entity_rect(origin: Pos2, x: f32, y: f32, width: i32, height: i32, zoom: f32) -> EguiRect {
    let s = TILE_PX * zoom;
    let w = (width as f32).max(1.0);
    let h = (height as f32).max(1.0);
    EguiRect::from_min_size(
        Pos2::new(origin.x + x * s, origin.y + y * s),
        EguiVec2::new(w * s, h * s),
    )
}

/// Screen coordinates -> tile coordinates.
fn screen_to_tile(origin: Pos2, pos: Pos2, zoom: f32) -> Option<(u32, u32)> {
    let s = TILE_PX * zoom;
    if s <= 0.0 {
        return None;
    }
    let dx = (pos.x - origin.x) / s;
    let dy = (pos.y - origin.y) / s;
    if dx < 0.0 || dy < 0.0 {
        return None;
    }
    Some((dx as u32, dy as u32))
}

/// Renders a single widget. Returns whether the value changed.
fn render_widget(
    ui: &mut egui::Ui,
    name: &str,
    widget: &WidgetKind,
    value: &mut FieldValue,
) -> bool {
    match widget {
        WidgetKind::IntInput { min, max } => {
            if let FieldValue::Int(v) = value {
                ui.add(egui::DragValue::new(v).range(*min..=*max)).changed()
            } else {
                ui.label("(type mismatch: expected Int)");
                false
            }
        }
        WidgetKind::FloatInput { min, max, speed } => {
            if let FieldValue::Float(v) = value {
                ui.add(egui::DragValue::new(v).range(*min..=*max).speed(*speed))
                    .changed()
            } else {
                ui.label("(type mismatch: expected Float)");
                false
            }
        }
        WidgetKind::TextInput => {
            if let FieldValue::String(v) = value {
                ui.text_edit_singleline(v).changed()
            } else {
                ui.label("(type mismatch: expected String)");
                false
            }
        }
        WidgetKind::Checkbox => {
            if let FieldValue::Bool(v) = value {
                ui.checkbox(v, "").changed()
            } else {
                ui.label("(type mismatch: expected Bool)");
                false
            }
        }
        WidgetKind::Combo { options } => {
            if let FieldValue::Enum(idx) = value {
                let mut current = *idx;
                let mut changed = false;
                egui::ComboBox::from_id_salt(name)
                    .selected_text(
                        options
                            .get(current)
                            .cloned()
                            .unwrap_or_else(|| "?".to_string()),
                    )
                    .show_ui(ui, |ui| {
                        for (i, opt) in options.iter().enumerate() {
                            if ui.selectable_value(&mut current, i, opt).clicked() {
                                changed = true;
                            }
                        }
                    });
                *idx = current;
                changed
            } else {
                ui.label("(type mismatch: expected Enum)");
                false
            }
        }
        WidgetKind::ColorPicker => {
            if let FieldValue::Color(rgba) = value {
                let mut c = Color32::from_rgba_unmultiplied(rgba[0], rgba[1], rgba[2], rgba[3]);
                let changed = ui.color_edit_button_srgba(&mut c).changed();
                if changed {
                    *rgba = [c.r(), c.g(), c.b(), c.a()];
                }
                changed
            } else {
                ui.label("(type mismatch: expected Color)");
                false
            }
        }
        WidgetKind::EntityRefPicker => {
            if let FieldValue::EntityRef(v) = value {
                ui.add(egui::DragValue::new(v).speed(1.0)).changed()
            } else {
                ui.label("(type mismatch: expected EntityRef)");
                false
            }
        }
        WidgetKind::Vec2Input => {
            if let FieldValue::Vec2 { x, y } = value {
                ui.horizontal(|ui| {
                    let a = ui.add(egui::DragValue::new(x).speed(0.5).prefix("x "));
                    let b = ui.add(egui::DragValue::new(y).speed(0.5).prefix("y "));
                    a.changed() || b.changed()
                })
                .inner
            } else {
                ui.label("(type mismatch: expected Vec2)");
                false
            }
        }
    }
}
