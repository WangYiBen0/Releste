//! egui 界面：把 [`Editor`] 模型渲染成编辑器 UI。

use egui::{Color32, Pos2, Rect as EguiRect, Sense, Stroke, Vec2 as EguiVec2};

use reles_world::EntityId;
use reles_world::{FieldValue, Schema};

use crate::model::{Editor, Selection};
use crate::schema_panel::{SchemaPanel, WidgetKind};

/// 一个 tile 在画布上的像素尺寸。
pub const TILE_PX: f32 = 16.0;

/// UI 状态（不属于文档，因此不进撤销栈）。
pub struct EditorUi {
    /// 当前画笔的 tile 值。
    pub brush: u8,
    /// 当前放置的实体种类。
    pub entity_kind: String,
    /// 画布缩放。
    pub zoom: f32,
    /// 画布平移。
    pub pan: egui::Vec2,
    /// 状态栏消息。
    pub status: String,
    /// 正在绘制的起点（用于矩形填充）。
    paint_origin: Option<(u32, u32)>,
    /// 每个实体种类的表单（缓存，避免每帧重编译）。
    panels: std::collections::HashMap<String, SchemaPanel>,
}

impl Default for EditorUi {
    fn default() -> Self {
        EditorUi {
            brush: 1,
            entity_kind: "player".into(),
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            status: "就绪".into(),
            paint_origin: None,
            panels: std::collections::HashMap::new(),
        }
    }
}

impl EditorUi {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册某实体种类的 Schema（首次遇到时编译）。
    pub fn register_schema(&mut self, kind: &str, schema: &Schema) {
        self.panels
            .entry(kind.to_string())
            .or_insert_with(|| SchemaPanel::compile(kind, schema));
    }

    /// 渲染整个界面。
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
                    "未保存*"
                } else {
                    "已保存"
                };
                ui.label(dirty);
                ui.separator();
                if let Some(p) = editor.path() {
                    ui.label(p.display().to_string());
                } else {
                    ui.label("(未命名)");
                }
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| self.canvas(ui, editor));
    }

    /// 菜单栏。
    fn menu_bar(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("文件", |ui| {
                    if ui.button("新建").clicked() {
                        *editor = Editor::new();
                        self.status = "新建地图".into();
                        ui.close_menu();
                    }
                    if ui.button("打开…").clicked() {
                        self.status = "打开：请通过命令行传入路径".into();
                        ui.close_menu();
                    }
                    if ui.button("保存").clicked() {
                        match editor.save() {
                            Ok(()) => self.status = "已保存".into(),
                            Err(e) => self.status = format!("保存失败：{e}"),
                        }
                        ui.close_menu();
                    }
                    if ui.button("另存为…").clicked() {
                        self.status = "另存为：请通过命令行传入路径".into();
                        ui.close_menu();
                    }
                });

                ui.menu_button("编辑", |ui| {
                    let can_undo = editor.undo_len() > 0;
                    let label = editor
                        .undo_label()
                        .map(|l| format!("撤销 {l}"))
                        .unwrap_or_else(|| "撤销".to_string());
                    if ui.add_enabled(can_undo, egui::Button::new(label)).clicked() {
                        if let Err(e) = editor.undo() {
                            self.status = format!("撤销失败：{e}");
                        }
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(editor.redo_len() > 0, egui::Button::new("重做"))
                        .clicked()
                    {
                        if let Err(e) = editor.redo() {
                            self.status = format!("重做失败：{e}");
                        }
                        ui.close_menu();
                    }
                });

                ui.menu_button("房间", |ui| {
                    if ui.button("新增房间").clicked() {
                        let name = format!("room_{}", editor.document().rooms.len());
                        let room = reles_map::Room {
                            name,
                            bounds: reles_map::RectDef {
                                x: 0.0,
                                y: 0.0,
                                w: 320.0,
                                h: 180.0,
                            },
                            tiles: reles_map::Tileset::new("Gameplay", 20, 12),
                            bg: reles_map::Tileset::default(),
                            entities: Vec::new(),
                            triggers: Vec::new(),
                            script: None,
                        };
                        match editor.add_room(room) {
                            Ok(i) => {
                                let _ = editor.set_current_room(i);
                                self.status = "已新增房间".into();
                            }
                            Err(e) => self.status = format!("新增失败：{e}"),
                        }
                        ui.close_menu();
                    }
                });
            });
        });
    }

    /// 左侧：房间列表 + 画笔 / 实体调色板。
    fn room_panel(&mut self, ui: &mut egui::Ui, editor: &mut Editor) {
        ui.heading("房间");
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
        ui.heading("Tile 画笔");
        ui.add(egui::Slider::new(&mut self.brush, 0..=35).text("tile 值"));
        ui.label("左键绘制 · 拖动填充矩形 · 右键擦除");

        ui.separator();
        ui.heading("实体");
        ui.text_edit_singleline(&mut self.entity_kind);
        if ui.button("在当前房间中心放置").clicked() {
            let kind = self.entity_kind.clone();
            let (x, y) = editor
                .room()
                .map(|r| (r.bounds.x + r.bounds.w / 2.0, r.bounds.y + r.bounds.h / 2.0))
                .unwrap_or((0.0, 0.0));
            // 稳定 ID 由地图作者/管线决定；编辑器用位置 + 种类散列占位。
            let id = EntityId::generate(0, &kind, 0, x as i32, y as i32);
            let entity = reles_world::EntityData::new(kind, id, x, y);
            match editor.add_entity(entity) {
                Ok(id) => {
                    editor.select(Selection::Entity(id));
                    self.status = "已放置实体".into();
                }
                Err(e) => self.status = format!("放置失败：{e}"),
            }
        }
    }

    /// 右侧：Schema 驱动的属性面板。
    fn inspector_panel(&mut self, ui: &mut egui::Ui, editor: &mut Editor) {
        ui.heading("检视器");
        let Some(id) = editor.selection().entity() else {
            ui.label("未选中实体");
            return;
        };

        let Some((kind, x, y, props)) = editor.room().and_then(|r| {
            r.entities
                .iter()
                .find(|e| e.id == id)
                .map(|e| (e.kind.clone(), e.x, e.y, e.properties.clone()))
        }) else {
            ui.label("选中的实体已不存在");
            return;
        };

        ui.label(format!("种类：{kind}"));
        ui.label(format!("ID：{id}"));

        // 位置：直接可编辑（每次改动进撤销栈）。
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
            let _ = editor.move_entity(id, reles_math::Vec2::from_f32s(nx, ny));
        }

        ui.separator();

        // 若已注册该种类的 Schema，生成表单；否则列出原始键值。
        match self.panels.get_mut(&kind) {
            Some(panel) => {
                for i in 0..panel.fields.len() {
                    let name = panel.fields[i].name.clone();
                    let widget = panel.fields[i].widget.clone();

                    // 实体上的实际值优先（Schema 默认值只作回退）。
                    if let Some(v) = props.get(&name) {
                        panel.fields[i].value = v.clone();
                    }

                    ui.label(&panel.fields[i].label);
                    let mut value = panel.fields[i].value.clone();
                    if render_widget(ui, &name, &widget, &mut value) {
                        panel.fields[i].value = value.clone();
                        if let Err(e) = editor.set_entity_prop(id, &name, value) {
                            self.status = format!("写入失败：{e}");
                        }
                    }
                }
            }
            None => {
                ui.label("（该种类未注册 Schema，显示原始属性）");
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

    /// 中间：tile 画布 + 实体标记。
    fn canvas(&mut self, ui: &mut egui::Ui, editor: &mut Editor) {
        // 先快照渲染所需数据，之后才能可变借用 `editor`。
        let Some((tiles_w, tiles_h, filled, entities, selected_id)) = editor.room().map(|room| {
            let w = room.tiles.width.max(1) as usize;
            let h = room.tiles.height.max(1) as usize;
            let filled: Vec<(usize, u8)> = (0..h)
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
            ui.centered_and_justified(|ui| ui.label("没有房间：请先新建一个"));
            return;
        };

        let size = EguiVec2::new(
            tiles_w as f32 * TILE_PX * self.zoom,
            tiles_h as f32 * TILE_PX * self.zoom,
        );
        let (response, painter) =
            ui.allocate_painter(size.max(EguiVec2::splat(64.0)), Sense::click_and_drag());
        let origin = response.rect.min + self.pan;

        // 背景
        painter.rect_filled(response.rect, 0.0, Color32::from_gray(24));

        // tiles
        for (idx, value) in &filled {
            let x = idx % tiles_w;
            let y = idx / tiles_w;
            let rect = tile_rect(origin, x, y, self.zoom);
            // 用 tile 值派生一个可区分的颜色（真实渲染由 reles-render 负责）。
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

        // 网格线
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

        // 实体标记
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

        // 交互：绘制 / 擦除
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

        // 选中实体（tile 坐标 1:1 对应世界坐标）
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

/// tile 在屏幕上的矩形。
fn tile_rect(origin: Pos2, x: usize, y: usize, zoom: f32) -> EguiRect {
    let s = TILE_PX * zoom;
    let min = Pos2::new(origin.x + x as f32 * s, origin.y + y as f32 * s);
    EguiRect::from_min_size(min, EguiVec2::splat(s))
}

/// 实体在屏幕上的矩形（至少 1 格可见）。
fn entity_rect(origin: Pos2, x: f32, y: f32, width: i32, height: i32, zoom: f32) -> EguiRect {
    let s = TILE_PX * zoom;
    let w = (width as f32).max(1.0);
    let h = (height as f32).max(1.0);
    EguiRect::from_min_size(
        Pos2::new(origin.x + x * s, origin.y + y * s),
        EguiVec2::new(w * s, h * s),
    )
}

/// 屏幕坐标 → tile 坐标。
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

/// 渲染单个控件。返回值是否发生变化。
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
                ui.label("（类型不匹配：期望 Int）");
                false
            }
        }
        WidgetKind::FloatInput { min, max, speed } => {
            if let FieldValue::Float(v) = value {
                ui.add(egui::DragValue::new(v).range(*min..=*max).speed(*speed))
                    .changed()
            } else {
                ui.label("（类型不匹配：期望 Float）");
                false
            }
        }
        WidgetKind::TextInput => {
            if let FieldValue::String(v) = value {
                ui.text_edit_singleline(v).changed()
            } else {
                ui.label("（类型不匹配：期望 String）");
                false
            }
        }
        WidgetKind::Checkbox => {
            if let FieldValue::Bool(v) = value {
                ui.checkbox(v, "").changed()
            } else {
                ui.label("（类型不匹配：期望 Bool）");
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
                ui.label("（类型不匹配：期望 Enum）");
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
                ui.label("（类型不匹配：期望 Color）");
                false
            }
        }
        WidgetKind::EntityRefPicker => {
            if let FieldValue::EntityRef(v) = value {
                ui.add(egui::DragValue::new(v).speed(1.0)).changed()
            } else {
                ui.label("（类型不匹配：期望 EntityRef）");
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
                ui.label("（类型不匹配：期望 Vec2）");
                false
            }
        }
    }
}
