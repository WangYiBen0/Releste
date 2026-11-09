//! egui interface: renders the [`Editor`] model as the editor UI.

use egui::{Color32, Pos2, Rect as EguiRect, Sense, Stroke, Vec2 as EguiVec2};

use releste_world::EntityId;
use releste_world::{FieldValue, Schema};

use crate::dialog::{DialogMode, FileDialog};
use crate::l10n::{self, Localizer};
use crate::model::{Editor, Selection};
use crate::schema_panel::{SchemaPanel, WidgetKind};
use crate::tiles::TileRenderer;
use tracing::warn;

/// Screen pixels spanned by one tile cell in the canvas world view.
///
/// The canvas works in world pixels (1 world px = `zoom` screen px) and a
/// Celeste tile is `crate::tiles::TILE_PX` = 8 source pixels, so a cell spans
/// exactly 8 world px. Drawing cells at any other scale desynchronises the
/// picture from the click transform (a 3x mismatch once shipped: tiles were
/// drawn 24 px wide while clicks divided by 8, so every stroke landed three
/// cells away from the pointer).
const CANVAS_CELL_PX: f32 = crate::tiles::TILE_PX as f32;

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
    /// Active left-panel tool (tiles vs entities).
    tool: Tool,
    /// Entity kind armed for canvas placement (tool = Entities).
    armed_entity: Option<String>,
    /// Canvas zoom.
    pub zoom: f32,
    /// Canvas pan.
    pub pan: egui::Vec2,
    /// Status bar message.
    pub status: String,
    /// Last cell painted or erased in the open stroke; the next drag frame
    /// draws a line from it (Bresenham) so fast movement leaves no gaps.
    stroke_last: Option<(u32, u32)>,
    /// Entity being dragged: (id, grab offset in room-local pixels). The
    /// offset keeps the grab point under the cursor while the entity follows.
    dragging_entity: Option<(EntityId, f32, f32)>,
    /// Form per entity kind (cached to avoid recompiling every frame).
    panels: std::collections::HashMap<String, SchemaPanel>,
    /// Active localisation.
    localizer: Localizer,
    /// Catalog root the switchable locales are read from.
    catalog_root: std::path::PathBuf,
    /// Locales discovered at startup, shown by the language switcher.
    available_locales: Vec<crate::l10n::Locale>,
    /// Tile art loaded from `assets/atlas`; `None` until a map names an atlas.
    tiles: Option<TileRenderer>,
    /// Asset root used to load tile art.
    assets_root: std::path::PathBuf,
    /// egui textures, one per decoded atlas page.
    tile_textures: Vec<egui::TextureHandle>,
    /// The open file dialog, if any.
    dialog: Option<FileDialog>,
    /// Directory the dialogs start in.
    map_dir: std::path::PathBuf,
    /// Pending request from the dialog for the app host to act on.
    pending_io: Option<PendingIo>,
    /// Whether the inline room-rename editor is open.
    renaming_room: bool,
    /// The room name being edited.
    room_name_edit: String,
    /// Persisted editor settings (fallback font + UI scale).
    settings: crate::settings::Settings,
    /// Whether the font settings window is open.
    settings_open: bool,
    /// CJK font candidates for the settings picker, discovered once.
    font_choices: Option<Vec<crate::fonts::FontCandidate>>,
    /// Half-open tile marquee origin: (room index, x, y).
    marquee: Option<(usize, u32, u32)>,
    /// Shift + primary drag: the whole gesture is a marquee selection,
    /// whatever tool is active.
    shift_marquee: bool,
    /// Drag-move of an existing tile selection (Select tool).
    tile_drag: Option<TileDragMove>,
    /// Alt-drag room gesture: move it, or resize it from a border edge.
    room_drag: Option<RoomDrag>,
    /// Unsaved-changes prompt shown on close (armed by the app layer).
    /// `pub(crate)`: the windowing layer arms it in `CloseRequested`.
    pub(crate) close_prompt: bool,
    /// The prompt was resolved with approval to exit the app.
    /// `pub(crate)`: the windowing layer polls it in `about_to_wait`.
    pub(crate) exit_approved: bool,
    /// Tile clipboard shared by copy / cut / paste.
    tile_clipboard: Option<crate::model::TileClipboard>,
    /// Hovered tile: (room index, x, y). The paste target.
    cursor_tile: Option<(usize, u32, u32)>,
    /// Whether a paint / erase gesture is open (one gesture = one undo).
    stroke_open: bool,
    /// Lua script console state.
    script_console: crate::script::ScriptConsole,
    /// Visual font scale applied to egui style (addresses "too thin").
    ui_font_scale: f32,
    /// Re-fit the whole map into the viewport on the next frame.
    fit_view: bool,
    /// Two-cell checkerboard for the void around rooms.
    checker: Option<egui::TextureHandle>,
}

/// A file operation the UI asked for, executed by the host after the frame.
///
/// The UI cannot open or save by itself: `Editor::open` returns a new `Editor`
/// and the host owns the window title and the asset root, so the request is
/// queued and applied once the egui borrow is released.
#[derive(Debug, Clone)]
pub enum PendingIo {
    /// Open this path, replacing the current document.
    Open(std::path::PathBuf),
    /// Save the document to this path.
    SaveAs(std::path::PathBuf),
}

impl Default for EditorUi {
    fn default() -> Self {
        Self::with_locale(crate::l10n::Locale::default())
    }
}

impl EditorUi {
    /// Creates a new one, starting in the `en_US` source locale.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a UI starting in `locale`, loading catalogs from the catalog
    /// root discovered for this process (see `l10n::find_catalog_root`).
    pub fn with_locale(locale: crate::l10n::Locale) -> Self {
        let root = l10n::find_catalog_root().unwrap_or_else(|| std::path::PathBuf::from("locales"));
        Self::with_locale_at(locale, root)
    }

    /// Creates a UI starting in `locale`, reading catalogs from `root`.
    pub fn with_locale_at(locale: crate::l10n::Locale, root: std::path::PathBuf) -> Self {
        let localizer = Localizer::load(&root, &locale);
        let available_locales = l10n::discover(&root);
        let settings = crate::settings::Settings::load();
        let mut ui = EditorUi {
            brush: 1,
            entity_kind: "player".into(),
            tool: Tool::Tiles,
            armed_entity: None,
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            status: String::new(),
            stroke_last: None,
            dragging_entity: None,
            panels: std::collections::HashMap::new(),
            localizer,
            catalog_root: root,
            available_locales,
            tiles: None,
            assets_root: std::path::PathBuf::from("assets"),
            tile_textures: Vec::new(),
            dialog: None,
            map_dir: std::path::PathBuf::from("maps"),
            pending_io: None,
            renaming_room: false,
            room_name_edit: String::new(),
            settings_open: false,
            font_choices: None,
            marquee: None,
            shift_marquee: false,
            tile_drag: None,
            room_drag: None,
            close_prompt: false,
            exit_approved: false,
            tile_clipboard: None,
            cursor_tile: None,
            stroke_open: false,
            script_console: crate::script::ScriptConsole::default(),
            ui_font_scale: settings.ui.scale,
            fit_view: false,
            checker: None,
            settings,
        };
        ui.status = ui.t("status.ready").to_string();
        ui
    }

    /// Sets the asset root and loads tile art for `atlas_name`.
    ///
    /// Called by the host once it knows where `assets/` is and which atlas the
    /// open map uses. Failure to load art is not fatal: the canvas falls back to
    /// flat placeholders so editing still works on a checkout without assets.
    pub fn load_tile_art(&mut self, assets_root: std::path::PathBuf, atlas_name: &str) {
        self.assets_root = assets_root;
        self.tiles = Some(TileRenderer::load(&self.assets_root, atlas_name));
    }

    /// Sets the directory the Open / Save As dialogs start in.
    pub fn set_map_dir(&mut self, dir: std::path::PathBuf) {
        self.map_dir = dir;
    }

    /// Takes a queued file operation for the host to perform.
    pub fn take_pending_io(&mut self) -> Option<PendingIo> {
        self.pending_io.take()
    }

    /// Replaces the status-bar message (used by the host after a file operation).
    pub fn set_status(&mut self, message: String) {
        self.status = message;
    }

    /// Resolves a key in the active locale (the `t` shorthand).
    ///
    /// Returns an owned `String` deliberately: a borrowed `&str` would keep
    /// `self` immutably borrowed for as long as the label lives, which does not
    /// work inside the `egui` closures that also need `&mut self` (the status
    /// bar, the menu handlers). The allocation is a few dozen bytes per frame
    /// and UI code is explicitly exempt from the hot-path budget (AGENTS.md 8/9).
    pub fn t(&self, key: &str) -> String {
        self.localizer.lookup(key).to_string()
    }

    /// Resolves a key and substitutes `{name}` placeholders.
    pub fn tf(&self, key: &str, args: &[(&str, String)]) -> String {
        self.localizer.format(key, args)
    }

    /// The active locale.
    pub fn locale(&self) -> &crate::l10n::Locale {
        self.localizer.locale()
    }

    /// The localizer driving the interface language.
    pub fn localizer(&self) -> &Localizer {
        &self.localizer
    }

    /// Switches the active language, re-reading catalogs from disk.
    ///
    /// No restart is required, which keeps the "hot-swappable" property the rest
    /// of the project has (AGENTS.md section 3).
    pub fn set_locale(&mut self, locale: crate::l10n::Locale) {
        self.localizer = Localizer::load(&self.catalog_root, &locale);
        // Status messages are already-translated snapshots, so a language
        // switch resets the transient message rather than showing a
        // half-translated one.
        self.status = self.t("status.ready").to_string();
    }

    /// The locales offered by the language switcher.
    pub fn available_locales(&self) -> &[crate::l10n::Locale] {
        &self.available_locales
    }

    /// Registers the Schema for an entity kind (compiled on first encounter).
    pub fn register_schema(&mut self, kind: &str, schema: &Schema) {
        self.panels
            .entry(kind.to_string())
            .or_insert_with(|| SchemaPanel::compile(kind, schema));
    }

    /// Renders the whole interface.
    pub fn show(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        let scale = self.ui_font_scale.clamp(0.75, 2.5);
        ctx.set_zoom_factor(scale);
        self.menu_bar(ctx, editor);
        self.handle_shortcuts(ctx, editor);
        egui::SidePanel::left("rooms")
            .default_width(200.0)
            .show(ctx, |ui| self.room_panel(ui, editor));
        egui::SidePanel::right("inspector")
            .default_width(260.0)
            .show(ctx, |ui| self.inspector_panel(ui, editor));
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(&self.status);
                ui.separator();
                // "Saved" is only meaningful for a document that has a path:
                // an untouched untitled buffer has never been written, so
                // claiming it is saved would be a lie.
                if editor.is_dirty() {
                    ui.label(self.t("status.unsaved"));
                    ui.separator();
                } else if editor.path().is_some() {
                    ui.label(self.t("status.saved"));
                    ui.separator();
                }
                if let Some(p) = editor.path() {
                    ui.label(p.display().to_string());
                } else {
                    ui.label(self.t("status.untitled"));
                }
                ui.separator();
                ui.label(format!("{}%", (self.zoom * 100.0).round() as i32));
                if ui.small_button(self.t("status.reset_view")).clicked() {
                    self.fit_view = true;
                }
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            // Loenn-style: a fixed viewport with pan / zoom, no scrollbars.
            self.canvas(ui, editor);
        });
        self.dialog_window(ctx);
        self.settings_window(ctx);
        self.script_window(ctx, editor);
        self.close_prompt_window(ctx, editor);
    }

    /// Renders the Open / Save As browser when it is open.
    fn dialog_window(&mut self, ctx: &egui::Context) {
        // Resolve every localized string BEFORE taking `dialog` mutably: `t()`
        // borrows self immutably, and the dialog borrow is exclusive.
        let open_label = self.t("menu.file.open");
        let save_as_label = self.t("menu.file.save_as");
        let up_label = self.t("dialog.up");
        let filename_label = self.t("dialog.filename");
        let open_btn = self.t("dialog.open");
        let save_btn = self.t("dialog.save");
        let cancel_label = self.t("dialog.cancel");

        let Some(dialog) = self.dialog.as_mut() else {
            return;
        };

        let title = match dialog.mode() {
            DialogMode::Open => open_label,
            DialogMode::SaveAs => save_as_label,
        };
        let mut open = true;
        let mut close = false;
        let mut confirmed: Option<std::path::PathBuf> = None;

        egui::Window::new(title)
            .open(&mut open)
            .resizable(true)
            .default_width(460.0)
            .default_height(360.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui.button(&up_label).clicked() {
                        dialog.up();
                    }
                    ui.label(dialog.dir().display().to_string());
                });
                if let Some(err) = dialog.error() {
                    ui.colored_label(Color32::LIGHT_RED, err);
                }
                ui.separator();

                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        // Collect first: the closure cannot borrow dialog mutably
                        // while iterating its entries.
                        let entries: Vec<(String, bool)> = dialog
                            .entries()
                            .iter()
                            .map(|e| (e.name.clone(), e.is_dir))
                            .collect();
                        for (name, is_dir) in entries {
                            let label = if is_dir {
                                format!("[ {name} ]")
                            } else {
                                name.clone()
                            };
                            if ui.selectable_label(false, label).clicked() {
                                if is_dir {
                                    dialog.enter(&name);
                                } else {
                                    dialog.select(&name);
                                }
                            }
                        }
                    });

                ui.separator();
                if dialog.mode() == DialogMode::SaveAs {
                    ui.horizontal(|ui| {
                        ui.label(&filename_label);
                        ui.text_edit_singleline(dialog.filename_mut());
                    });
                } else if let Some(sel) = dialog.selected() {
                    ui.label(sel.display().to_string());
                }

                ui.horizontal(|ui| {
                    let can = dialog.pending_path().is_some();
                    let confirm_label = match dialog.mode() {
                        DialogMode::Open => open_btn.clone(),
                        DialogMode::SaveAs => save_btn.clone(),
                    };
                    if ui
                        .add_enabled(can, egui::Button::new(confirm_label))
                        .clicked()
                    {
                        confirmed = dialog.confirm();
                    }
                    if ui.button(&cancel_label).clicked() {
                        close = true;
                    }
                });
            });

        if let Some(path) = confirmed {
            self.pending_io = Some(match dialog.mode() {
                DialogMode::Open => PendingIo::Open(path),
                DialogMode::SaveAs => PendingIo::SaveAs(path),
            });
            // A freshly opened document should show its whole map.
            self.fit_view = true;
            close = true;
        }
        if close || !open {
            self.dialog = None;
        }
    }

    /// Menu bar.
    fn menu_bar(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        egui::TopBottomPanel::top("menu").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button(self.t("menu.file"), |ui| {
                    if ui.button(self.t("menu.file.new")).clicked() {
                        *editor = Editor::new();
                        self.fit_view = true;
                        self.status = self.t("status.new_map").to_string();
                        ui.close_menu();
                    }
                    if ui.button(self.t("menu.file.open")).clicked() {
                        self.dialog = Some(FileDialog::new(
                            DialogMode::Open,
                            self.map_dir.clone(),
                            "map",
                        ));
                        ui.close_menu();
                    }
                    if ui.button(self.t("menu.file.save")).clicked() {
                        match editor.save() {
                            Ok(()) => self.status = self.t("status.saved").to_string(),
                            Err(e) => {
                                self.status =
                                    self.tf("status.save_failed", &[("error", e.to_string())])
                            }
                        }
                        ui.close_menu();
                    }
                    if ui.button(self.t("menu.file.save_as")).clicked() {
                        let mut dialog =
                            FileDialog::new(DialogMode::SaveAs, self.map_dir.clone(), "map");
                        if let Some(name) = editor
                            .path()
                            .and_then(|p| p.file_name())
                            .and_then(|n| n.to_str())
                        {
                            *dialog.filename_mut() = name.to_string();
                        }
                        self.dialog = Some(dialog);
                        ui.close_menu();
                    }
                });

                ui.menu_button(self.t("menu.edit"), |ui| {
                    let can_undo = editor.undo_len() > 0;
                    // The undo label itself is model-layer data (see
                    // Editor::record) and stays language-neutral; only the frame
                    // around it is translated.
                    let label = match editor.undo_label() {
                        Some(l) => self.tf("menu.edit.undo_with", &[("label", l.to_string())]),
                        None => self.t("menu.edit.undo").to_string(),
                    };
                    if ui.add_enabled(can_undo, egui::Button::new(label)).clicked() {
                        if let Err(e) = editor.undo() {
                            self.status =
                                self.tf("status.undo_failed", &[("error", e.to_string())]);
                        }
                        ui.close_menu();
                    }
                    if ui
                        .add_enabled(
                            editor.redo_len() > 0,
                            egui::Button::new(self.t("menu.edit.redo")),
                        )
                        .clicked()
                    {
                        if let Err(e) = editor.redo() {
                            self.status =
                                self.tf("status.redo_failed", &[("error", e.to_string())]);
                        }
                        ui.close_menu();
                    }
                    ui.separator();
                    let has_tile_sel = matches!(editor.selection(), Selection::Tiles { .. });
                    let copy_btn =
                        egui::Button::new(self.t("menu.edit.copy")).shortcut_text("Ctrl+C");
                    if ui.add_enabled(has_tile_sel, copy_btn).clicked() {
                        self.copy_selection(editor);
                        ui.close_menu();
                    }
                    let cut_btn =
                        egui::Button::new(self.t("menu.edit.cut")).shortcut_text("Ctrl+X");
                    if ui.add_enabled(has_tile_sel, cut_btn).clicked() {
                        self.cut_selection(editor);
                        ui.close_menu();
                    }
                    let paste_btn =
                        egui::Button::new(self.t("menu.edit.paste")).shortcut_text("Ctrl+V");
                    if ui
                        .add_enabled(self.tile_clipboard.is_some(), paste_btn)
                        .clicked()
                    {
                        self.paste_clipboard(editor);
                        ui.close_menu();
                    }
                    let delete_btn =
                        egui::Button::new(self.t("menu.edit.delete")).shortcut_text("Del");
                    if ui.add_enabled(has_tile_sel, delete_btn).clicked() {
                        self.delete_selection(editor);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(self.t("menu.script")).clicked() {
                        self.script_console.open = true;
                        ui.close_menu();
                    }
                });

                ui.menu_button(self.t("menu.room"), |ui| {
                    if ui.button(self.t("menu.room.add")).clicked() {
                        let name = format!("room_{}", editor.document().rooms.len());
                        let room = releste_map::Room {
                            name,
                            // A Celeste-standard 40x23 room: the bounds must
                            // stay a multiple of the 8 px tile so the tile
                            // grid matches the room outline from the start.
                            bounds: releste_map::RectDef {
                                x: 0.0,
                                y: 0.0,
                                w: 320.0,
                                h: 184.0,
                            },
                            tiles: releste_map::Tileset::new("Gameplay", 40, 23),
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
                                let name =
                                    editor.room().map(|r| r.name.clone()).unwrap_or_default();
                                self.status = self.tf("status.room_added", &[("name", name)]);
                            }
                            Err(e) => {
                                self.status =
                                    self.tf("status.room_add_failed", &[("error", e.to_string())])
                            }
                        }
                        ui.close_menu();
                    }
                });

                ui.menu_button(self.t("menu.view"), |ui| {
                    if ui.button(self.t("menu.view.reset")).clicked() {
                        self.fit_view = true;
                        self.status = self.t("status.reset_view").to_string();
                        ui.close_menu();
                    }
                    ui.separator();
                    ui.label(self.t("menu.view.font_scale"));
                    let mut scale = self.ui_font_scale;
                    if ui
                        .add(egui::Slider::new(&mut scale, 0.85..=1.8).step_by(0.05))
                        .changed()
                    {
                        self.ui_font_scale = scale;
                    }
                    ui.separator();
                    if ui.button(self.t("menu.view.test_map")).clicked() {
                        self.test_in_game(editor);
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button(self.t("menu.settings")).clicked() {
                        self.settings_open = true;
                        ui.close_menu();
                    }
                });

                // Language switcher: re-reads the catalog from disk, no restart.
                let active = self.locale().to_string();
                ui.menu_button(format!("{} ({active})", self.t("menu.language")), |ui| {
                    let locales = self.available_locales.clone();
                    for locale in locales {
                        let name = locale.to_string();
                        if ui.selectable_label(name == active, &name).clicked() {
                            self.set_locale(locale);
                            ui.close_menu();
                        }
                    }
                });
            });
        });
    }

    /// Renders the font settings window: fallback font picker, face index
    /// and UI scale. Every change applies immediately and persists to the
    /// settings file (see [`crate::settings`]).
    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let Some(config) = crate::settings::config_path() else {
            // Nowhere to persist; the window would lie about saving.
            self.settings_open = false;
            return;
        };
        let choices = self
            .font_choices
            .take()
            .unwrap_or_else(crate::fonts::font_candidates);
        let mut changed = false;
        let mut open = self.settings_open;
        egui::Window::new(self.t("settings.title"))
            .open(&mut open)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(self.t("settings.font"));
                    let selected = self.settings.font.path.clone();
                    let label = selected
                        .as_deref()
                        .map(|p| {
                            std::path::Path::new(p)
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or(p)
                                .to_string()
                        })
                        .unwrap_or_else(|| self.t("settings.auto"));
                    egui::ComboBox::from_id_salt("font_settings_combo")
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(selected.is_none(), self.t("settings.auto"))
                                .clicked()
                            {
                                self.settings.font.path = None;
                                changed = true;
                            }
                            for choice in choices.iter() {
                                let stem = choice
                                    .path
                                    .file_stem()
                                    .and_then(|s| s.to_str())
                                    .unwrap_or("")
                                    .to_string();
                                let path_text = choice.path.to_string_lossy().into_owned();
                                let is_selected = selected.as_deref() == Some(path_text.as_str());
                                if ui.selectable_label(is_selected, stem).clicked() {
                                    self.settings.font.path = Some(path_text);
                                    self.settings.font.index =
                                        crate::fonts::preferred_face_index(&choice.path)
                                            .min(choice.faces.saturating_sub(1));
                                    changed = true;
                                }
                            }
                        });
                });
                // The face index is only meaningful for an explicit file.
                if self.settings.font.path.is_some() {
                    let faces = self
                        .settings
                        .font
                        .path
                        .as_deref()
                        .map(std::path::Path::new)
                        .map(crate::fonts::face_count)
                        .unwrap_or(1);
                    ui.horizontal(|ui| {
                        ui.label(self.t("settings.face"));
                        let mut face = self.settings.font.index;
                        if ui
                            .add(egui::DragValue::new(&mut face).range(0..=faces.saturating_sub(1)))
                            .changed()
                        {
                            self.settings.font.index = face;
                            changed = true;
                        }
                        ui.weak(format!("/ {faces}"));
                    });
                }
                ui.separator();
                ui.label(self.t("settings.scale"));
                let mut scale = self.ui_font_scale;
                if ui
                    .add(egui::Slider::new(&mut scale, 0.85..=1.8).step_by(0.05))
                    .changed()
                {
                    self.ui_font_scale = scale;
                    changed = true;
                }
                ui.separator();
                if ui.button(self.t("settings.reset")).clicked() {
                    self.settings = crate::settings::Settings::default();
                    self.ui_font_scale = 1.0;
                    changed = true;
                }
                ui.weak(self.tf("settings.hint", &[("path", config.display().to_string())]));
                if choices.is_empty() {
                    ui.weak(self.t("settings.no_fonts"));
                }
            });
        self.settings_open = open;
        self.font_choices = Some(choices);
        if changed {
            self.apply_font_settings(ctx);
        }
    }

    /// Re-installs the fallback font from the current settings and persists
    /// them. Failures are reported on the status bar, never swallowed.
    fn apply_font_settings(&mut self, ctx: &egui::Context) {
        self.settings.ui.scale = self.ui_font_scale;
        crate::fonts::install_cjk_fallback_with(ctx, &self.settings.font_config());
        match crate::settings::Settings::save(&self.settings) {
            Ok(path) => {
                tracing::info!(path = %path.display(), "editor settings saved");
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot save editor settings");
                self.status = self.tf("status.settings_save_failed", &[("error", e)]);
            }
        }
    }

    /// Visual tile palette (Loenn-style): one clickable art cell per tileset
    /// id from the tileset definitions, plus the eraser in front. Falls back
    /// to flat swatches when there is no atlas art.
    /// The tile palette: every tileset texture of the map's atlas, drawn in
    /// full (like Loenn's picker) so the art is recognizable. A single
    /// autotile cell would just be an unidentifiable dark fragment.
    fn tile_palette(&mut self, ui: &mut egui::Ui) {
        self.ensure_tile_textures(ui.ctx());
        let Some(tiles) = self.tiles.as_ref() else {
            ui.weak(self.t("panel.no_tile_art"));
            return;
        };
        // Owned copies: neither the defs nor the texture handles may borrow
        // `self` while the mutable widget pass runs below.
        let mut values: Vec<u16> = tiles
            .defs()
            .tilesets
            .keys()
            .filter_map(|k| parse_tile_key(k))
            .collect();
        values.sort_unstable();
        let textures = self.tile_textures.clone();
        let mut brush = self.brush;
        if ui
            .selectable_label(brush == 0, self.t("panel.tool_eraser"))
            .clicked()
        {
            // Update the local copy: it is written back to `self.brush` after
            // the grid below and would otherwise clobber this assignment.
            brush = 0;
        }
        ui.add_space(2.0);
        egui::ScrollArea::vertical()
            .max_height(240.0)
            .id_salt("tile_palette")
            .show(ui, |ui| {
                // Columns follow the panel width: widening the panel shows
                // more entries per row instead of stretching. The count is
                // measured HERE, not inside the grid: a Grid's child Ui only
                // reports the remaining width of its current cell, which
                // collapses every row to a single column.
                let cols = (((ui.available_width() - 12.0) / (PALETTE_CELL.x + 6.0)).floor()
                    as usize)
                    .max(1);
                egui::Grid::new("tile_palette_grid")
                    .spacing([6.0, 6.0])
                    .show(ui, |ui| {
                        let mut column = 0usize;
                        for value in values {
                            brush = palette_cell(ui, tiles, &textures, value, brush);
                            column += 1;
                            if column >= cols {
                                ui.end_row();
                                column = 0;
                            }
                        }
                    });
            });
        self.brush = brush;
    }

    /// Entity kind palette (Loenn-style): click a registered kind to arm it,
    /// then click the canvas to place it. Right-click cancels.
    fn entity_palette(&mut self, ui: &mut egui::Ui) {
        let mut kinds: Vec<String> = self.panels.keys().cloned().collect();
        kinds.sort();
        if kinds.is_empty() {
            ui.weak(self.t("panel.entity_no_kinds"));
        } else {
            egui::ScrollArea::vertical()
                .max_height(140.0)
                .id_salt("entity_palette")
                .show(ui, |ui| {
                    for kind in &kinds {
                        let armed = self.armed_entity.as_deref() == Some(kind.as_str());
                        if ui.selectable_label(armed, kind).clicked() {
                            self.armed_entity = Some(kind.clone());
                            self.entity_kind = kind.clone();
                        }
                    }
                });
        }
        if let Some(kind) = self.armed_entity.clone() {
            ui.strong(self.tf("panel.entity_armed", &[("kind", kind)]));
        }
        // Keep the in-room list usable in entity mode too.
        ui.add_space(2.0);
        ui.label(self.t("panel.entity_select_hint"));
    }

    /// Left side: room list + brush / entity palette.
    fn room_panel(&mut self, ui: &mut egui::Ui, editor: &mut Editor) {
        ui.heading(self.t("panel.rooms"));
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
                        editor.select(Selection::None);
                    }
                }
            });

        // Room actions: previously only reachable from dead model code.
        if editor.room().is_some() {
            ui.horizontal(|ui| {
                if ui.button(self.t("panel.room_rename")).clicked() {
                    self.renaming_room = true;
                    self.room_name_edit = editor.room().map(|r| r.name.clone()).unwrap_or_default();
                }
                if ui.button(self.t("panel.room_delete")).clicked() {
                    match editor.remove_room(editor.current_room()) {
                        Ok(_) => self.status = self.t("status.room_removed"),
                        Err(e) => {
                            self.status =
                                self.tf("status.room_remove_failed", &[("error", e.to_string())])
                        }
                    }
                }
            });
            if self.renaming_room {
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.room_name_edit);
                    if ui.button(self.t("dialog.ok")).clicked() {
                        let idx = editor.current_room();
                        match editor.rename_room(idx, self.room_name_edit.clone()) {
                            Ok(()) => {
                                self.renaming_room = false;
                                self.status = self.tf(
                                    "status.room_renamed",
                                    &[("name", self.room_name_edit.clone())],
                                );
                            }
                            Err(e) => {
                                self.status = self
                                    .tf("status.room_rename_failed", &[("error", e.to_string())])
                            }
                        }
                    }
                    if ui.button(self.t("dialog.cancel")).clicked() {
                        self.renaming_room = false;
                    }
                });
            }

            // Room bounds, in world units.
            if let Some(room) = editor.room() {
                let mut b = room.bounds;
                let mut changed = false;
                ui.collapsing(self.t("panel.room_bounds"), |ui| {
                    ui.horizontal(|ui| {
                        changed |= ui
                            .add(egui::DragValue::new(&mut b.x).speed(1.0).prefix("x "))
                            .changed();
                        changed |= ui
                            .add(egui::DragValue::new(&mut b.y).speed(1.0).prefix("y "))
                            .changed();
                    });
                    ui.horizontal(|ui| {
                        changed |= ui
                            .add(egui::DragValue::new(&mut b.w).speed(1.0).prefix("w "))
                            .changed();
                        changed |= ui
                            .add(egui::DragValue::new(&mut b.h).speed(1.0).prefix("h "))
                            .changed();
                    });
                    ui.label(egui::RichText::new(self.t("panel.room_drag_hint")).weak());
                });
                if changed {
                    let idx = editor.current_room();
                    if let Err(e) = editor.set_room_bounds(idx, b) {
                        self.status = self.tf("status.write_failed", &[("error", e.to_string())]);
                    }
                }
            }
        }

        ui.separator();
        // Loenn-style tool tabs: pick tiles or place entities.
        ui.horizontal(|ui| {
            if ui
                .selectable_label(self.tool == Tool::Tiles, self.t("panel.tool_tiles"))
                .clicked()
            {
                self.tool = Tool::Tiles;
            }
            if ui
                .selectable_label(self.tool == Tool::Entities, self.t("panel.tool_entities"))
                .clicked()
            {
                self.tool = Tool::Entities;
            }
            if ui
                .selectable_label(self.tool == Tool::Select, self.t("panel.tool_select"))
                .clicked()
            {
                self.tool = Tool::Select;
            }
        });
        match self.tool {
            Tool::Tiles => {
                ui.heading(self.t("panel.tile_brush"));
                self.tile_palette(ui);
                ui.label(self.t("panel.brush_hint"));
                ui.separator();
                ui.heading(self.t("panel.entities"));
                ui.label(self.t("panel.entity_select_hint"));
            }
            Tool::Entities => {
                ui.heading(self.t("panel.entities"));
                self.entity_palette(ui);
                ui.label(self.t("panel.entity_place_hint"));
            }
            Tool::Select => {
                ui.heading(self.t("panel.select_tool"));
                ui.label(self.t("panel.select_hint"));
            }
        }
        // List entities in the current room for quick selection.
        ui.strong(self.t("panel.entity_list"));
        if let Some(room) = editor.room() {
            let ents: Vec<(releste_world::EntityId, String)> = room
                .entities
                .iter()
                .map(|e| (e.id, e.kind.clone()))
                .collect();
            let selected_id = editor.selection().entity();
            egui::ScrollArea::vertical()
                .max_height(120.0)
                .id_salt("entity_list")
                .show(ui, |ui| {
                    for (id, kind) in &ents {
                        let is_sel = selected_id == Some(*id);
                        let label = format!("{} ({})", kind, id.raw() & 0xFFFF);
                        if ui.selectable_label(is_sel, label).clicked() {
                            editor.select(Selection::Entity(*id));
                        }
                    }
                    if ents.is_empty() {
                        ui.weak("(none)");
                    }
                });
        }
        ui.add_space(4.0);
        ui.text_edit_singleline(&mut self.entity_kind);
        if ui.button(self.t("panel.place_entity")).clicked() {
            let kind = self.entity_kind.clone();
            let (x, y) = editor
                .room()
                .map(|r| (r.bounds.w / 2.0, r.bounds.h / 2.0))
                .unwrap_or((0.0, 0.0));
            // Stable IDs are decided by the map author / pipeline; the editor uses a placeholder hash of position + kind.
            let id = EntityId::generate(0, &kind, 0, x as i32, y as i32);
            let entity = releste_world::EntityData::new(kind, id, x, y);
            match editor.add_entity(entity) {
                Ok(id) => {
                    editor.select(Selection::Entity(id));
                    self.status = self.t("status.entity_placed").to_string();
                }
                Err(e) => self.status = self.tf("status.place_failed", &[("error", e.to_string())]),
            }
        }
    }

    /// Right side: the Schema-driven property panel.
    fn inspector_panel(&mut self, ui: &mut egui::Ui, editor: &mut Editor) {
        ui.heading(self.t("panel.inspector"));
        let Some(id) = editor.selection().entity() else {
            ui.label(self.t("inspector.no_selection"));
            return;
        };

        let Some((kind, x, y, props)) = editor.room().and_then(|r| {
            r.entities
                .iter()
                .find(|e| e.id == id)
                .map(|e| (e.kind.clone(), e.x, e.y, e.properties.clone()))
        }) else {
            ui.label(self.t("inspector.entity_gone"));
            return;
        };

        ui.label(self.tf("inspector.kind", &[("kind", kind.clone())]));
        ui.label(self.tf("inspector.id", &[("id", id.to_string())]));
        if ui.button(self.t("inspector.delete_entity")).clicked() {
            match editor.remove_entity(id) {
                Ok(_) => self.status = self.t("status.entity_removed"),
                Err(e) => {
                    self.status = self.tf("status.remove_failed", &[("error", e.to_string())])
                }
            }
            return;
        }

        // Position: directly editable (each change goes on the undo stack).
        let mut nx = x;
        let mut ny = y;
        let changed = ui
            .horizontal(|ui| {
                let px = self.t("inspector.axis_x").to_string();
                let py = self.t("inspector.axis_y").to_string();
                let a = ui.add(egui::DragValue::new(&mut nx).speed(1.0).prefix(px));
                let b = ui.add(egui::DragValue::new(&mut ny).speed(1.0).prefix(py));
                a.changed() || b.changed()
            })
            .inner;

        if changed {
            let _ = editor.move_entity(id, releste_math::Vec2::from_f32s(nx, ny));
        }

        ui.separator();

        // If a Schema for this kind is registered, build a form; otherwise list the raw key-values.
        // The three fields are borrowed separately so the panel map can be
        // mutable while the localizer and the status line stay reachable.
        let localizer = &self.localizer;
        let status = &mut self.status;
        let panels = &mut self.panels;
        match panels.get_mut(&kind) {
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
                    if render_widget(ui, &name, &widget, &mut value, localizer) {
                        panel.fields[i].value = value.clone();
                        if let Err(e) = editor.set_entity_prop(id, &name, value) {
                            *status = localizer
                                .format("status.write_failed", &[("error", e.to_string())]);
                        }
                    }
                }
            }
            None => {
                ui.label(localizer.lookup("inspector.no_schema"));
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

    /// Uploads decoded atlas pages to egui once, lazily.
    ///
    /// Textures are cached in `tile_textures`; a page that failed to decode is
    /// skipped so its index stays aligned with `TileRenderer::pages`.
    fn ensure_tile_textures(&mut self, ctx: &egui::Context) {
        let Some(tiles) = &self.tiles else {
            return;
        };
        if !self.tile_textures.is_empty() || !tiles.has_art() {
            return;
        }

        // egui refuses (and panics) on a texture larger than the backend's
        // limit, so a 4096x4096 atlas page on a 2048-capable adapter would
        // crash the editor. Downscale proportionally instead: the UV maths in
        // `canvas` is normalised, so shrinking the bitmap keeps every cell
        // aligned while the art stays visible.
        let max_side = ctx.input(|i| i.max_texture_side).max(1);
        let mut textures = Vec::new();
        for page in tiles.pages() {
            if page.width == 0 || page.height == 0 {
                // Placeholder so page indices stay aligned with the atlas.
                let img = egui::ColorImage::new([1, 1], Color32::TRANSPARENT);
                textures.push(ctx.load_texture(
                    "releste-tile-empty",
                    img,
                    egui::TextureOptions::NEAREST,
                ));
                continue;
            }
            let (w, h, rgba) = downscale_rgba(
                page.width as usize,
                page.height as usize,
                &page.rgba,
                max_side,
            );
            if w != page.width as usize || h != page.height as usize {
                warn!(
                    from = format!("{}x{}", page.width, page.height),
                    to = format!("{w}x{h}"),
                    limit = max_side,
                    "atlas page exceeds the GPU texture limit; downscaled for display"
                );
            }
            let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
            // NEAREST keeps pixel art crisp instead of blurring it.
            textures.push(ctx.load_texture(
                format!("releste-tile-page-{}", textures.len()),
                img,
                egui::TextureOptions::NEAREST,
            ));
        }
        self.tile_textures = textures;
    }

    /// Center: the Loenn-style world canvas.
    ///
    /// The whole map is laid out on one plane; the widget is a fixed viewport
    /// onto it and pan / zoom act as the world transform. Rooms draw against a
    /// checkerboard void (Loenn behavior) so a room reads as a room even where
    /// it has no tiles, and tile art uses real autotiler coordinates instead
    /// of the tileset's base cell.
    fn canvas(&mut self, ui: &mut egui::Ui, editor: &mut Editor) {
        let Some((extents, rooms)) = snapshot_world(editor, self.tiles.as_ref().map(|t| t.defs()))
        else {
            ui.centered_and_justified(|ui| ui.label(self.t("canvas.no_room")));
            return;
        };
        self.ensure_tile_textures(ui.ctx());
        self.ensure_checker_texture(ui.ctx());

        // Fit the whole map into the viewport on demand (after opening or
        // creating a map, or pressing reset).
        if self.fit_view {
            self.fit_view = false;
            let avail = ui.available_size();
            let world = EguiVec2::new(extents.width(), extents.height()).max(EguiVec2::splat(64.0));
            let zoom = (avail.x / world.x).min(avail.y / world.y).clamp(0.125, 2.0);
            self.zoom = zoom;
            self.pan = (avail - world * zoom) * 0.5;
        }

        let (response, _painter) =
            ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter().clone();
        let rect = response.rect;
        let zoom = self.zoom;
        // Screen position of the world origin (the padded extents minimum).
        let origin = rect.min + self.pan;

        // A popup (an open menu) leaves the canvas as the closest hit-test
        // widget on the popup's padding, so scroll inertia while crossing
        // menus would zoom the canvas. No canvas input while a popup is open.
        let popup_open = ui.ctx().memory(|m| m.any_popup_open());
        // Zoom at the cursor: keep the world point under the pointer fixed.
        if response.hovered() && !popup_open {
            // Plain scroll zooms (Loenn behavior); this is the primary way
            // to get close enough to paint a single tile.
            let scroll = ui.input(|i| i.raw_scroll_delta.y);
            if scroll.abs() > 0.0 {
                let factor = if scroll > 0.0 { 1.1 } else { 1.0 / 1.1 };
                let new_zoom = (zoom * factor).clamp(0.125, 8.0);
                if (new_zoom - zoom).abs() > f32::EPSILON {
                    if let Some(pos) = response.hover_pos() {
                        let world = screen_to_world(origin, extents.min, zoom, pos);
                        self.pan = (pos - rect.min)
                            - EguiVec2::new(
                                (world.0 - extents.min.0) * new_zoom,
                                (world.1 - extents.min.1) * new_zoom,
                            );
                    }
                    self.zoom = new_zoom;
                }
            }
        }
        // Pan with the middle button.
        if response.dragged_by(egui::PointerButton::Middle) {
            self.pan += response.drag_delta();
        }
        // Keep the padded world covering the viewport: no gaps at the edges,
        // and a smaller-than-viewport world stays centered. The arms are
        // mutually exclusive, so the clamp range is always valid (no panics,
        // AGENTS.md 15.8).
        {
            let vp = rect.size();
            let world = EguiVec2::new(extents.width() * self.zoom, extents.height() * self.zoom);
            self.pan = EguiVec2::new(
                if world.x <= vp.x {
                    (vp.x - world.x) * 0.5
                } else {
                    self.pan.x.clamp(vp.x - world.x, 0.0)
                },
                if world.y <= vp.y {
                    (vp.y - world.y) * 0.5
                } else {
                    self.pan.y.clamp(vp.y - world.y, 0.0)
                },
            );
        }
        let origin = rect.min + self.pan;

        // Void: an 8px checkerboard aligned to the world grid, so "nothing"
        // and "room floor" are distinguishable at any zoom.
        let cell = 8.0 * self.zoom;
        if let Some(checker) = &self.checker {
            let uv = EguiRect::from_min_max(
                Pos2::new(-self.pan.x / cell, -self.pan.y / cell),
                Pos2::new(
                    (rect.width() - self.pan.x) / cell,
                    (rect.height() - self.pan.y) / cell,
                ),
            );
            painter.image(checker.id(), rect, uv, Color32::WHITE);
        }

        // Rooms in document order; the current room details draw on top.
        for room in &rooms {
            let rmin =
                world_to_screen(origin, extents.min, self.zoom, room.bounds.0, room.bounds.1);
            let rmax = world_to_screen(
                origin,
                extents.min,
                self.zoom,
                room.bounds.0 + room.bounds.2,
                room.bounds.1 + room.bounds.3,
            );
            let rrect = EguiRect::from_min_max(rmin, rmax);
            // Room floor: darker than the void checkerboard.
            painter.rect_filled(rrect, 0.0, Color32::from_gray(12));

            // Background tiles first, dimmed so they never fight the fg art.
            for (tx, ty, value, col, row) in &room.bg {
                let tr = tile_rect(rmin, *tx as usize, *ty as usize, self.zoom);
                if !self.draw_room_tile(
                    &painter,
                    tr,
                    *value,
                    *col,
                    *row,
                    Color32::from_white_alpha(150),
                ) {
                    painter.rect_filled(tr, 0.0, placeholder_tile(*value, 0.45));
                }
            }

            // Foreground tiles with real autotiler coordinates.
            for (tx, ty, value, col, row) in &room.fg {
                let tr = tile_rect(rmin, *tx as usize, *ty as usize, self.zoom);
                if !self.draw_room_tile(&painter, tr, *value, *col, *row, Color32::WHITE) {
                    painter.rect_filled(tr, 0.0, placeholder_tile(*value, 1.0));
                }
            }

            // Triggers: orange boxes with their kind.
            for (x, y, w, h, kind) in &room.triggers {
                let tr = entity_rect(rmin, *x, *y, *w as i32, *h as i32, self.zoom);
                painter.rect_filled(tr, 0.0, Color32::from_rgba_unmultiplied(255, 150, 50, 40));
                painter.rect_stroke(
                    tr,
                    0.0,
                    Stroke::new(1.0_f32, Color32::from_rgb(255, 150, 50)),
                    egui::StrokeKind::Inside,
                );
                if room.is_current && self.zoom >= 0.75 {
                    painter.text(
                        tr.min + EguiVec2::new(2.0, 2.0),
                        egui::Align2::LEFT_TOP,
                        kind,
                        egui::FontId::proportional(10.0),
                        Color32::from_rgb(255, 200, 140),
                    );
                }
            }

            // Entities: blue boxes; the selected one stays gold. Node lines
            // and handles draw for the current room.
            for ((id, ex, ey, ew, eh, kind), nodes) in room.entities.iter().zip(&room.nodes) {
                let tr = entity_rect(rmin, *ex, *ey, *ew, *eh, self.zoom);
                let selected = editor.selection().entity() == Some(*id);
                if room.is_current {
                    let node_color = Color32::from_rgb(120, 220, 255);
                    let center = tr.center();
                    for (nx, ny) in nodes {
                        let nr = entity_rect(rmin, *nx, *ny, 3, 3, self.zoom);
                        painter
                            .line_segment([center, nr.center()], Stroke::new(1.0_f32, node_color));
                        painter.rect_filled(nr, 0.0, node_color);
                    }
                }
                let (fill, stroke_color, stroke_w) = if selected {
                    (
                        Color32::from_rgba_unmultiplied(255, 220, 80, 50),
                        Color32::from_rgb(255, 210, 60),
                        1.5_f32,
                    )
                } else {
                    (
                        Color32::from_rgba_unmultiplied(90, 160, 255, 40),
                        Color32::from_rgb(90, 160, 255),
                        1.0_f32,
                    )
                };
                painter.rect_filled(tr, 0.0, fill);
                painter.rect_stroke(
                    tr,
                    0.0,
                    Stroke::new(stroke_w, stroke_color),
                    egui::StrokeKind::Inside,
                );
                if room.is_current && self.zoom >= 0.75 {
                    painter.text(
                        tr.min + EguiVec2::new(2.0, 2.0),
                        egui::Align2::LEFT_TOP,
                        kind,
                        egui::FontId::proportional(10.0),
                        Color32::from_rgb(235, 235, 235),
                    );
                }
            }

            // Tile grid for the room being edited.
            if room.is_current && self.zoom >= 0.5 {
                for gx in 0..=room.tiles_w {
                    let px = rmin.x + gx as f32 * CANVAS_CELL_PX * self.zoom;
                    painter.line_segment(
                        [Pos2::new(px, rmin.y), Pos2::new(px, rmax.y)],
                        Stroke::new(0.5_f32, Color32::from_gray(48)),
                    );
                }
                for gy in 0..=room.tiles_h {
                    let py = rmin.y + gy as f32 * CANVAS_CELL_PX * self.zoom;
                    painter.line_segment(
                        [Pos2::new(rmin.x, py), Pos2::new(rmax.x, py)],
                        Stroke::new(0.5_f32, Color32::from_gray(48)),
                    );
                }
            }

            // Outline: the current room pops, other rooms stay quiet.
            let (outline_color, outline_w) = if room.is_current {
                (Color32::from_rgb(90, 200, 255), 2.0_f32)
            } else {
                (Color32::from_gray(120), 1.0_f32)
            };
            painter.rect_stroke(
                rrect,
                0.0,
                Stroke::new(outline_w, outline_color),
                egui::StrokeKind::Inside,
            );
            if self.zoom >= 0.5 {
                painter.text(
                    Pos2::new(rmin.x + 2.0, rmin.y - 4.0),
                    egui::Align2::LEFT_BOTTOM,
                    &room.name,
                    egui::FontId::proportional(11.0),
                    outline_color,
                );
            }
        }

        // Outline an active tile marquee selection.
        if let Selection::Tiles {
            room,
            x0,
            y0,
            x1,
            y1,
        } = editor.selection()
        {
            if let Some(room_data) = rooms.get(*room) {
                let rmin = room_screen_min(origin, extents.min, self.zoom, room_data.bounds);
                // While the selection is drag-moved the outline follows
                // the cursor as a ghost of the drop position.
                let cell_px = CANVAS_CELL_PX * self.zoom;
                let (ox, oy) = match &self.tile_drag {
                    Some(d) => (
                        (d.cur.0 as f32 - d.grab.0 as f32) * cell_px,
                        (d.cur.1 as f32 - d.grab.1 as f32) * cell_px,
                    ),
                    None => (0.0, 0.0),
                };
                let min = tile_rect(rmin, *x0 as usize, *y0 as usize, self.zoom);
                let max = tile_rect(rmin, *x1 as usize, *y1 as usize, self.zoom);
                painter.rect_stroke(
                    EguiRect::from_two_pos(
                        Pos2::new(min.min.x + ox, min.min.y + oy),
                        Pos2::new(max.min.x + ox, max.min.y + oy),
                    ),
                    0.0,
                    egui::Stroke::new(1.5_f32, Color32::from_rgb(90, 170, 255)),
                    egui::StrokeKind::Inside,
                );
            }
        }

        // Hover preview: outline the cell the next stroke would paint, so
        // painting is visible before the click commits.
        if self.tool == Tool::Tiles {
            if let Some(pos) = response.hover_pos() {
                let world = screen_to_world(origin, extents.min, self.zoom, pos);
                let current = editor.current_room();
                if let Some(room) = rooms.get(current) {
                    if room_under(&rooms, world) == Some(current) {
                        let tx = ((world.0 - room.bounds.0) / 8.0).floor();
                        let ty = ((world.1 - room.bounds.1) / 8.0).floor();
                        if tx >= 0.0
                            && ty >= 0.0
                            && tx < room.tiles_w as f32
                            && ty < room.tiles_h as f32
                        {
                            let rmin = room_screen_min(origin, extents.min, self.zoom, room.bounds);
                            let cell = tile_rect(rmin, tx as usize, ty as usize, self.zoom);
                            painter.rect_stroke(
                                cell,
                                0.0,
                                egui::Stroke::new(1.0_f32, Color32::from_rgb(255, 210, 60)),
                                egui::StrokeKind::Inside,
                            );
                        }
                    }
                }
            }
        }

        // Interaction: clicking another room switches to it, clicking an
        // entity selects it, painting only ever touches the current room and
        // never fires on the click that selected an entity.
        let pointer = response.interact_pointer_pos();
        let click = response.clicked();
        let started = response.drag_started_by(egui::PointerButton::Primary);
        let drag = response.dragged_by(egui::PointerButton::Primary);
        let mut clicked_entity = false;
        // Track the hovered tile: the paste target for clipboard operations.
        self.cursor_tile = pointer.and_then(|pos| {
            let world = screen_to_world(origin, extents.min, self.zoom, pos);
            let current = editor.current_room();
            rooms.get(current).and_then(|room| {
                let tx = ((world.0 - room.bounds.0) / 8.0).floor();
                let ty = ((world.1 - room.bounds.1) / 8.0).floor();
                if tx >= 0.0 && ty >= 0.0 && tx < room.tiles_w as f32 && ty < room.tiles_h as f32 {
                    Some((current, tx as u32, ty as u32))
                } else {
                    None
                }
            })
        });
        let alt = ui.input(|i| i.modifiers.alt);
        // Shift + primary press: the whole gesture becomes a marquee
        // selection, whatever tool is active (Loenn-style).
        if started && !popup_open && !alt && ui.input(|i| i.modifiers.shift) {
            self.shift_marquee = true;
            if let Some(pos) = pointer {
                let world = screen_to_world(origin, extents.min, self.zoom, pos);
                if let Some(idx) = room_under(&rooms, world) {
                    let room = &rooms[idx];
                    let tx = ((world.0 - room.bounds.0) / 8.0).floor().max(0.0) as u32;
                    let ty = ((world.1 - room.bounds.1) / 8.0).floor().max(0.0) as u32;
                    self.marquee = Some((idx, tx, ty));
                }
            }
        }
        // Alt + press: move the room under the pointer, or resize it when
        // the press starts on a border edge (within one tile).
        if started && !popup_open && alt && !ui.input(|i| i.modifiers.shift) {
            if let Some(pos) = pointer {
                let world = screen_to_world(origin, extents.min, self.zoom, pos);
                if let Some(idx) = room_under(&rooms, world) {
                    let (bx, by, bw, bh) = rooms[idx].bounds;
                    // The border grab band is one tile wide.
                    const ROOM_EDGE: f32 = 8.0;
                    let left = world.0 - bx <= ROOM_EDGE;
                    let right = bx + bw - world.0 <= ROOM_EDGE;
                    let top = world.1 - by <= ROOM_EDGE;
                    let bottom = by + bh - world.1 <= ROOM_EDGE;
                    let resizing = left || right || top || bottom;
                    self.room_drag = Some(RoomDrag {
                        room: idx,
                        resize: (left, top, right, bottom),
                        start: (bx, by, bw, bh),
                        grab: world,
                    });
                    if !self.stroke_open {
                        editor.begin_stroke(if resizing { "Resize room" } else { "Move room" });
                        self.stroke_open = true;
                    }
                }
            }
        }
        if !popup_open && (click || started) && !self.shift_marquee && !alt {
            if let Some(pos) = pointer {
                let world = screen_to_world(origin, extents.min, self.zoom, pos);
                match room_under(&rooms, world) {
                    Some(idx) if idx != editor.current_room() => {
                        if click {
                            if let Err(e) = editor.set_current_room(idx) {
                                warn!(error = %e, room = idx, "failed to switch room");
                            }
                        }
                    }
                    Some(idx) => {
                        let room = &rooms[idx];
                        let rmin = room_screen_min(origin, extents.min, self.zoom, room.bounds);
                        let hit = room
                            .entities
                            .iter()
                            .rev()
                            .find(|(_, ex, ey, ew, eh, _)| {
                                entity_rect(rmin, *ex, *ey, *ew, *eh, self.zoom).contains(pos)
                            })
                            .map(|(id, ..)| *id);
                        if let Some(id) = hit {
                            editor.select(Selection::Entity(id));
                            clicked_entity = true;
                            if started {
                                // Begin an entity drag: remember the grab
                                // offset so the entity keeps its grab point
                                // under the cursor while it follows.
                                if let Some((_, ex, ey, ..)) =
                                    room.entities.iter().rev().find(|(eid, ..)| *eid == id)
                                {
                                    self.dragging_entity = Some((
                                        id,
                                        ex - (world.0 - room.bounds.0),
                                        ey - (world.1 - room.bounds.1),
                                    ));
                                }
                            }
                        } else if click && self.tool == Tool::Entities {
                            // Armed placement: click drops the entity at the
                            // pointer, snapped to the tile grid (room-local
                            // pixels, 8 px per tile).
                            if let Some(kind) = self.armed_entity.clone() {
                                let x = (((world.0 - room.bounds.0) / 8.0).floor() * 8.0) as i32;
                                let y = (((world.1 - room.bounds.1) / 8.0).floor() * 8.0) as i32;
                                let id = EntityId::generate(0, &kind, 0, x, y);
                                let entity =
                                    releste_world::EntityData::new(kind, id, x as f32, y as f32);
                                match editor.add_entity(entity) {
                                    Ok(id) => {
                                        editor.select(Selection::Entity(id));
                                        self.status = self.t("status.entity_placed");
                                    }
                                    Err(e) => {
                                        self.status = self
                                            .tf("status.place_failed", &[("error", e.to_string())]);
                                    }
                                }
                                clicked_entity = true;
                            } else {
                                editor.select(Selection::None);
                            }
                        } else if self.tool == Tool::Select {
                            // Marquee: press seeds the origin, drag grows the
                            // half-open rectangle, a plain click clears.
                            let tx = ((world.0 - room.bounds.0) / 8.0).floor().max(0.0) as u32;
                            let ty = ((world.1 - room.bounds.1) / 8.0).floor().max(0.0) as u32;
                            // Pressing inside the selection grabs it for
                            // a drag-move instead of a new marquee.
                            if let Selection::Tiles {
                                room: sroom,
                                x0,
                                y0,
                                x1,
                                y1,
                            } = editor.selection()
                            {
                                if *sroom == idx && tx >= *x0 && ty >= *y0 && tx < *x1 && ty < *y1 {
                                    self.tile_drag = Some(TileDragMove {
                                        room: idx,
                                        sel: (*x0, *y0, *x1, *y1),
                                        grab: (tx, ty),
                                        cur: (tx, ty),
                                    });
                                }
                            }
                            if self.tile_drag.is_none() && started {
                                self.marquee = Some((idx, tx, ty));
                            }
                            if self.tile_drag.is_none() && click && !started {
                                self.marquee = None;
                                editor.select(Selection::None);
                            } else if let Some((mroom, mx, my)) = self.marquee {
                                if mroom == idx {
                                    let (lx, hx) = (mx.min(tx), mx.max(tx));
                                    let (ly, hy) = (my.min(ty), my.max(ty));
                                    editor.select(Selection::Tiles {
                                        room: idx,
                                        x0: lx,
                                        y0: ly,
                                        x1: hx + 1,
                                        y1: hy + 1,
                                    });
                                }
                            }
                        } else {
                            editor.select(Selection::None);
                        }
                    }
                    None => {
                        if click {
                            editor.select(Selection::None);
                        }
                    }
                }
            }
        }

        // In entity mode the secondary button cancels the armed kind instead
        // of erasing (painting is tiles-mode only).
        if self.tool == Tool::Entities && response.secondary_clicked() {
            self.armed_entity = None;
        }

        // The secondary button erases: a click erases one cell, holding it
        // erases along the drag path.
        let erase_started = response.secondary_clicked();
        let erase_drag = response.dragged_by(egui::PointerButton::Secondary);
        if !popup_open
            && !alt
            && self.tool == Tool::Tiles
            && self.dragging_entity.is_none()
            && (erase_started || erase_drag)
        {
            // The erase gesture is one undo step, separate from painting.
            if !self.stroke_open {
                editor.begin_stroke("Erase tiles");
                self.stroke_open = true;
            }
            if let Some(pos) = pointer {
                let world = screen_to_world(origin, extents.min, self.zoom, pos);
                let current = editor.current_room();
                if room_under(&rooms, world) == Some(current) {
                    let room = &rooms[current];
                    let tx = ((world.0 - room.bounds.0) / 8.0).floor();
                    let ty = ((world.1 - room.bounds.1) / 8.0).floor();
                    if tx >= 0.0
                        && ty >= 0.0
                        && tx < room.tiles_w as f32
                        && ty < room.tiles_h as f32
                    {
                        let (tx, ty) = (tx as u32, ty as u32);
                        // Same freeform line as the brush: erase along the
                        // path, not one cell per frame.
                        match self.stroke_last {
                            Some((lx, ly)) => {
                                for (x, y) in tile_line((lx, ly), (tx, ty)) {
                                    let _ = editor.erase_tile(x, y);
                                }
                            }
                            None => {
                                let _ = editor.erase_tile(tx, ty);
                            }
                        }
                        self.stroke_last = Some((tx, ty));
                    }
                } else {
                    self.stroke_last = None;
                }
            }
        }
        // Painting: primary paints the brush, in the current room only,
        // clamped to the grid (the model also clamps, but this keeps the
        // stroke tracking honest).
        if self.tool == Tool::Tiles
            && !alt
            && !clicked_entity
            && !self.shift_marquee
            && self.dragging_entity.is_none()
            && (click || drag)
        {
            // Group the whole gesture into a single undo step.
            if !self.stroke_open {
                editor.begin_stroke("Paint tiles");
                self.stroke_open = true;
            }
            if let Some(pos) = pointer {
                let world = screen_to_world(origin, extents.min, self.zoom, pos);
                let current = editor.current_room();
                if room_under(&rooms, world) == Some(current) {
                    let room = &rooms[current];
                    let tx = ((world.0 - room.bounds.0) / 8.0).floor();
                    let ty = ((world.1 - room.bounds.1) / 8.0).floor();
                    if tx >= 0.0
                        && ty >= 0.0
                        && tx < room.tiles_w as f32
                        && ty < room.tiles_h as f32
                    {
                        let (tx, ty) = (tx as u32, ty as u32);
                        // Freeform pencil: interpolate the line from the
                        // last painted cell so fast movement leaves no gaps.
                        // (A per-frame rectangle fill painted the union of
                        // every intermediate rectangle instead of the path.)
                        match self.stroke_last {
                            Some((lx, ly)) => {
                                for (x, y) in tile_line((lx, ly), (tx, ty)) {
                                    let _ = editor.paint_tile(x, y, self.brush);
                                }
                            }
                            None => {
                                let _ = editor.paint_tile(tx, ty, self.brush);
                            }
                        }
                        self.stroke_last = Some((tx, ty));
                    }
                } else {
                    // Left the room mid-stroke: re-entry starts a fresh
                    // cell instead of drawing a jump line across the map.
                    self.stroke_last = None;
                }
            }
        }
        // The marquee grows while the drag continues (the press itself only
        // seeds the origin, so drag frames must update the rectangle).
        if (self.tool == Tool::Select || self.shift_marquee) && drag && !popup_open {
            if let (Some(pos), Some((mroom, mx, my))) = (pointer, self.marquee) {
                let current = editor.current_room();
                if mroom == current {
                    if let Some(room) = rooms.get(current) {
                        let world = screen_to_world(origin, extents.min, self.zoom, pos);
                        let tx = ((world.0 - room.bounds.0) / 8.0).floor().max(0.0) as u32;
                        let ty = ((world.1 - room.bounds.1) / 8.0).floor().max(0.0) as u32;
                        let (lx, hx) = (mx.min(tx), mx.max(tx));
                        let (ly, hy) = (my.min(ty), my.max(ty));
                        editor.select(Selection::Tiles {
                            room: mroom,
                            x0: lx,
                            y0: ly,
                            x1: hx + 1,
                            y1: hy + 1,
                        });
                    }
                }
            }
        }

        // Alt-drag: apply the snapped pointer delta to the room bounds.
        if let Some(rd) = self.room_drag.as_mut() {
            if let Some(pos) = pointer.filter(|_| drag && !popup_open) {
                let world = screen_to_world(origin, extents.min, self.zoom, pos);
                let snap = |v: f32| (v / 8.0).round() * 8.0;
                let dx = snap(world.0 - rd.grab.0);
                let dy = snap(world.1 - rd.grab.1);
                let (sx, sy, sw, sh) = rd.start;
                let (l, t, r, b) = rd.resize;
                let (nx, ny, nw, nh) = if l || t || r || b {
                    let mut nx = sx;
                    let mut ny = sy;
                    let mut nw = sw;
                    let mut nh = sh;
                    if l {
                        nx = sx + dx;
                        nw = (sw - dx).max(8.0);
                    }
                    if r {
                        nw = (sw + dx).max(8.0);
                    }
                    if t {
                        ny = sy + dy;
                        nh = (sh - dy).max(8.0);
                    }
                    if b {
                        nh = (sh + dy).max(8.0);
                    }
                    (nx, ny, nw, nh)
                } else {
                    (sx + dx, sy + dy, sw, sh)
                };
                let _ = editor.set_room_bounds(
                    rd.room,
                    releste_map::RectDef {
                        x: nx,
                        y: ny,
                        w: nw,
                        h: nh,
                    },
                );
            }
        }

        // Drag-moving a selection: the anchor cell follows the cursor,
        // clamped to the room grid so the offset stays sane.
        if let Some(dragmove) = self.tile_drag.as_mut() {
            if let Some(pos) = pointer.filter(|_| drag && !popup_open) {
                if let Some(room) = rooms.get(dragmove.room) {
                    let world = screen_to_world(origin, extents.min, self.zoom, pos);
                    let tx = (((world.0 - room.bounds.0) / 8.0).floor() as i64)
                        .clamp(0, room.tiles_w as i64 - 1);
                    let ty = (((world.1 - room.bounds.1) / 8.0).floor() as i64)
                        .clamp(0, room.tiles_h as i64 - 1);
                    dragmove.cur = (tx as u32, ty as u32);
                }
            }
        }

        // Entity dragging: the grabbed entity follows the pointer with an
        // 8 px snap. The first frame that actually moves the entity records
        // one undo entry; the rest of the gesture updates silently so the
        // whole drag stays a single undo step.
        if let Some((id, ox, oy)) = self.dragging_entity {
            if drag {
                if let Some(pos) = pointer {
                    let world = screen_to_world(origin, extents.min, self.zoom, pos);
                    let current = editor.current_room();
                    if room_under(&rooms, world) == Some(current) {
                        if let Some(room) = rooms.get(current) {
                            let x = ((world.0 - room.bounds.0 + ox) / 8.0).floor() * 8.0;
                            let y = ((world.1 - room.bounds.1 + oy) / 8.0).floor() * 8.0;
                            let result = if started {
                                editor.move_entity(id, releste_math::Vec2::from_f32s(x, y))
                            } else {
                                editor.set_entity_position(id, x, y)
                            };
                            if let Err(e) = result {
                                warn!(error = %e, entity = %id, "failed to move entity");
                            }
                        }
                    }
                }
            }
        }
        if response.drag_stopped() {
            self.stroke_last = None;
            self.dragging_entity = None;
            // Alt-drag finished: report what changed (bounds already are).
            if let Some(rd) = self.room_drag.take() {
                if let Some(room) = rooms.get(rd.room) {
                    if room.bounds != rd.start {
                        self.status = self.t(
                            if rd.resize.0 || rd.resize.1 || rd.resize.2 || rd.resize.3 {
                                "status.room_resized"
                            } else {
                                "status.room_moved"
                            },
                        );
                    }
                }
            }
            // Drop a drag-moved selection: cut from the original rect and
            // paste at the offset, committed as one undo step.
            if let Some(d) = self.tile_drag.take() {
                if d.cur != d.grab {
                    editor.begin_stroke("Move tiles");
                    let clip = editor.copy_tiles(
                        d.room,
                        d.sel.0,
                        d.sel.1,
                        d.sel.2.saturating_sub(1),
                        d.sel.3.saturating_sub(1),
                    );
                    if let Some(clip) = clip {
                        let _ = editor.delete_tiles(
                            d.room,
                            d.sel.0,
                            d.sel.1,
                            d.sel.2.saturating_sub(1),
                            d.sel.3.saturating_sub(1),
                        );
                        let nx = (d.sel.0 as i64 + d.cur.0 as i64 - d.grab.0 as i64).max(0) as u32;
                        let ny = (d.sel.1 as i64 + d.cur.1 as i64 - d.grab.1 as i64).max(0) as u32;
                        let changed = editor.paste_tiles(d.room, nx, ny, &clip);
                        if changed > 0 {
                            editor.select(Selection::Tiles {
                                room: d.room,
                                x0: nx,
                                y0: ny,
                                x1: nx.saturating_add(clip.width),
                                y1: ny.saturating_add(clip.height),
                            });
                            self.status = self.t("status.moved");
                        }
                    }
                    editor.end_stroke();
                }
            }
        }
        // A click is a complete gesture even without drag movement: commit
        // it as its own undo step. The model also auto-commits an open stroke
        // on undo / redo; resetting the flag here keeps the next gesture
        // starting a fresh stroke.
        if (click && !drag) || (erase_started && !erase_drag) || response.drag_stopped() {
            if self.stroke_open {
                editor.end_stroke();
            }
            self.stroke_open = false;
            self.shift_marquee = false;
        }
    }

    /// Draws the unsaved-changes prompt shown when the window is closed
    /// while the document still has unsaved edits.
    fn close_prompt_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if !self.close_prompt {
            return;
        }
        // Resolve the strings first: the closure below borrows self mutably.
        let title = self.t("dialog.unsaved_title");
        let text = self.t("dialog.unsaved_text");
        let save = self.t("dialog.save");
        let discard = self.t("dialog.discard");
        let cancel = self.t("dialog.cancel");
        enum CloseChoice {
            Save,
            Discard,
            Cancel,
        }
        let mut choice: Option<CloseChoice> = None;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(text);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(save).clicked() {
                        choice = Some(CloseChoice::Save);
                    }
                    if ui.button(discard).clicked() {
                        choice = Some(CloseChoice::Discard);
                    }
                    if ui.button(cancel).clicked() {
                        choice = Some(CloseChoice::Cancel);
                    }
                });
            });
        match choice {
            Some(CloseChoice::Save) => {
                if editor.path().is_some() {
                    match editor.save() {
                        Ok(()) => {
                            self.status = self.t("status.saved");
                            self.close_prompt = false;
                            self.exit_approved = true;
                        }
                        Err(e) => {
                            self.status =
                                self.tf("status.save_failed", &[("error", e.to_string())]);
                        }
                    }
                } else {
                    // Untitled: route through Save-As first. The close stays
                    // disapproved; once saved the next close exits cleanly.
                    let dialog = FileDialog::new(DialogMode::SaveAs, self.map_dir.clone(), "map");
                    self.dialog = Some(dialog);
                    self.close_prompt = false;
                }
            }
            Some(CloseChoice::Discard) => {
                self.close_prompt = false;
                self.exit_approved = true;
                warn!("closing without saving; changes discarded");
            }
            Some(CloseChoice::Cancel) => self.close_prompt = false,
            None => {}
        }
    }

    /// Draws one autotiled cell from the loaded atlas.
    ///
    /// Returns false when there is no art for the id so the caller can fall
    /// back to a flat placeholder (editing must work without assets).
    fn draw_room_tile(
        &self,
        painter: &egui::Painter,
        rect: EguiRect,
        value: u16,
        col: u32,
        row: u32,
        tint: Color32,
    ) -> bool {
        let Some(tiles) = &self.tiles else {
            return false;
        };
        let Some(tr) = tiles.rect_for(value, col, row) else {
            return false;
        };
        draw_tile_rect(painter, rect, tr, &self.tile_textures, tiles.pages(), tint)
    }

    /// Builds (once) the two-cell checkerboard used for the void.
    fn ensure_checker_texture(&mut self, ctx: &egui::Context) {
        if self.checker.is_some() {
            return;
        }
        let mut rgba = vec![0u8; 2 * 2 * 4];
        for y in 0..2usize {
            for x in 0..2usize {
                let v = if (x + y) % 2 == 0 { 26u8 } else { 20u8 };
                let o = (y * 2 + x) * 4;
                rgba[o] = v;
                rgba[o + 1] = v;
                rgba[o + 2] = v;
                rgba[o + 3] = 255;
            }
        }
        let img = egui::ColorImage::from_rgba_unmultiplied([2, 2], &rgba);
        self.checker = Some(ctx.load_texture(
            "releste-canvas-checker",
            img,
            egui::TextureOptions {
                wrap_mode: egui::TextureWrapMode::Repeat,
                ..egui::TextureOptions::NEAREST
            },
        ));
    }

    /// Copies the selected tile rectangle into the clipboard.
    fn copy_selection(&mut self, editor: &Editor) {
        if let Selection::Tiles {
            room,
            x0,
            y0,
            x1,
            y1,
        } = *editor.selection()
        {
            // Selection::Tiles is half-open; the model rect is inclusive.
            let clip = editor.copy_tiles(room, x0, y0, x1.saturating_sub(1), y1.saturating_sub(1));
            if let Some(clip) = clip {
                self.tile_clipboard = Some(clip);
                self.status = self.t("status.copied");
            } else {
                self.status = self.t("status.nothing_copied");
            }
        } else {
            self.status = self.t("status.nothing_selected");
        }
    }

    /// Cuts the selected tile rectangle into the clipboard.
    fn cut_selection(&mut self, editor: &mut Editor) {
        if let Selection::Tiles {
            room,
            x0,
            y0,
            x1,
            y1,
        } = *editor.selection()
        {
            self.tile_clipboard =
                editor.cut_tiles(room, x0, y0, x1.saturating_sub(1), y1.saturating_sub(1));
            self.status = self.t("status.cut");
        } else {
            self.status = self.t("status.nothing_selected");
        }
    }

    /// Pastes the clipboard at the hovered tile (or the room origin).
    fn paste_clipboard(&mut self, editor: &mut Editor) {
        let Some(clip) = self.tile_clipboard.clone() else {
            self.status = self.t("status.nothing_copied");
            return;
        };
        let (room, x, y) = self.cursor_tile.unwrap_or((editor.current_room(), 0, 0));
        if editor.paste_tiles(room, x, y, &clip) > 0 {
            editor.select(Selection::Tiles {
                room,
                x0: x,
                y0: y,
                x1: x.saturating_add(clip.width),
                y1: y.saturating_add(clip.height),
            });
            self.status = self.t("status.pasted");
        } else {
            self.status = self.t("status.nothing_pasted");
        }
    }

    /// Deletes the selected tile rectangle.
    fn delete_selection(&mut self, editor: &mut Editor) {
        if let Selection::Tiles {
            room,
            x0,
            y0,
            x1,
            y1,
        } = *editor.selection()
        {
            let changed =
                editor.delete_tiles(room, x0, y0, x1.saturating_sub(1), y1.saturating_sub(1));
            if changed > 0 {
                self.status = self.t("status.deleted");
            } else {
                self.status = self.t("status.nothing_selected");
            }
        } else {
            self.status = self.t("status.nothing_selected");
        }
    }

    /// Handles editor keyboard shortcuts.
    ///
    /// Skipped while a text field has keyboard focus (rename editor, script
    /// console, dialogs) so typing never triggers an edit. The model commits
    /// an open stroke on undo / redo; the flag is reset here so the next
    /// gesture starts a fresh stroke.
    fn handle_shortcuts(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let shortcut = ctx.input(|i| {
            let ctrl = i.modifiers.ctrl;
            if ctrl && i.key_pressed(egui::Key::Z) && !i.modifiers.shift {
                Some(Shortcut::Undo)
            } else if ctrl
                && (i.key_pressed(egui::Key::Y)
                    || (i.key_pressed(egui::Key::Z) && i.modifiers.shift))
            {
                Some(Shortcut::Redo)
            } else if ctrl && i.key_pressed(egui::Key::C) {
                Some(Shortcut::Copy)
            } else if ctrl && i.key_pressed(egui::Key::X) {
                Some(Shortcut::Cut)
            } else if ctrl && i.key_pressed(egui::Key::V) {
                Some(Shortcut::Paste)
            } else if i.key_pressed(egui::Key::Delete) {
                Some(Shortcut::Delete)
            } else if ctrl && i.key_pressed(egui::Key::S) && i.modifiers.shift {
                Some(Shortcut::SaveAs)
            } else if ctrl && i.key_pressed(egui::Key::S) {
                Some(Shortcut::Save)
            } else if ctrl && i.key_pressed(egui::Key::O) {
                Some(Shortcut::Open)
            } else if i.key_pressed(egui::Key::Escape) {
                Some(Shortcut::Escape)
            } else if i.key_pressed(egui::Key::Num1) {
                Some(Shortcut::ToolTiles)
            } else if i.key_pressed(egui::Key::Num2) {
                Some(Shortcut::ToolEntities)
            } else if i.key_pressed(egui::Key::Num3) {
                Some(Shortcut::ToolSelect)
            } else {
                None
            }
        });
        match shortcut {
            Some(Shortcut::Undo) => {
                self.stroke_open = false;
                if let Err(e) = editor.undo() {
                    self.status = self.tf("status.undo_failed", &[("error", e.to_string())]);
                }
            }
            Some(Shortcut::Redo) => {
                self.stroke_open = false;
                if let Err(e) = editor.redo() {
                    self.status = self.tf("status.redo_failed", &[("error", e.to_string())]);
                }
            }
            Some(Shortcut::Copy) => self.copy_selection(editor),
            Some(Shortcut::Cut) => self.cut_selection(editor),
            Some(Shortcut::Paste) => self.paste_clipboard(editor),
            Some(Shortcut::Delete) => self.delete_selection(editor),
            Some(Shortcut::Save) => match editor.save() {
                Ok(()) => self.status = self.t("status.saved"),
                Err(e) => self.status = self.tf("status.save_failed", &[("error", e.to_string())]),
            },
            Some(Shortcut::SaveAs) => {
                let mut dialog = FileDialog::new(DialogMode::SaveAs, self.map_dir.clone(), "map");
                if let Some(name) = editor
                    .path()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                {
                    *dialog.filename_mut() = name.to_string();
                }
                self.dialog = Some(dialog);
            }
            Some(Shortcut::Open) => {
                self.dialog = Some(FileDialog::new(
                    DialogMode::Open,
                    self.map_dir.clone(),
                    "map",
                ));
            }
            Some(Shortcut::Escape) => {
                editor.select(Selection::None);
                self.marquee = None;
                self.tile_drag = None;
                self.room_drag = None;
            }
            Some(Shortcut::ToolTiles) => self.tool = Tool::Tiles,
            Some(Shortcut::ToolEntities) => self.tool = Tool::Entities,
            Some(Shortcut::ToolSelect) => self.tool = Tool::Select,
            None => {}
        }
    }

    /// Renders the Lua script console when it is open.
    ///
    /// The whole run is wrapped in one undo stroke, so a script that edits
    /// tiles is reverted by a single Ctrl+Z. Errors are reported in the
    /// output pane and the status bar, never swallowed.
    fn script_window(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        if !self.script_console.open {
            return;
        }
        let title = self.t("script.title");
        let run_label = self.t("script.run");
        let hint = self.t("script.hint");
        let console = &mut self.script_console;
        let mut open = true;
        let mut pending_run: Option<String> = None;
        egui::Window::new(title)
            .open(&mut open)
            .default_width(560.0)
            .default_height(420.0)
            .show(ctx, |ui| {
                let response = ui.add(
                    egui::TextEdit::multiline(&mut console.code)
                        .code_editor()
                        .desired_rows(12)
                        .desired_width(f32::INFINITY),
                );
                ui.horizontal(|ui| {
                    let ctrl_enter = response.has_focus()
                        && ctx.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl);
                    if ui.button(run_label).clicked() || ctrl_enter {
                        pending_run = Some(console.code.clone());
                    }
                    ui.weak(hint);
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &console.output {
                            ui.monospace(line);
                        }
                    });
            });
        console.open = open;
        if let Some(code) = pending_run {
            editor.begin_stroke("Run script");
            let result = crate::script::ScriptConsole::run(editor, &code);
            editor.end_stroke();
            match result {
                Ok(lines) => {
                    self.script_console.output = lines;
                    self.status = self.t("status.script_ran");
                }
                Err(err) => {
                    self.script_console.output = vec![err.clone()];
                    self.status = self.tf("status.script_failed", &[("error", err)]);
                }
            }
        }
    }

    /// Launches the `releste` game binary to playtest the current map.
    fn test_in_game(&mut self, editor: &mut Editor) {
        let map_path = if let Some(p) = editor.path().map(|p| p.to_path_buf()) {
            if editor.is_dirty() {
                if let Err(e) = editor.save() {
                    self.status = self.tf("status.save_failed", &[("error", e.to_string())]);
                    return;
                }
                self.status = self.tf("status.test_unsaved", &[("path", p.display().to_string())]);
            }
            p
        } else {
            self.status = self.t("status.test_no_path").to_string();
            return;
        };

        let assets_root = std::env::var("RELESTE_ASSETS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from("assets"));
        let game_root = if assets_root.join("maps").is_dir() {
            assets_root.clone()
        } else if assets_root
            .parent()
            .map(|p| p.join("maps").is_dir())
            .unwrap_or(false)
        {
            assets_root.parent().unwrap().to_path_buf()
        } else {
            assets_root.clone()
        };

        let game_bin = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|d| d.join("releste")))
            .filter(|p| p.is_file())
            .or_else(|| {
                let candidate = std::path::PathBuf::from("target/debug/releste");
                if candidate.is_file() {
                    Some(candidate)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| std::path::PathBuf::from("releste"));

        // The game resolves a relative MAP argument against its maps
        // directory (see Paths::map in crates/game/src/paths.rs), so hand it
        // an absolute path and the root; absolutes pass through untouched.
        let map_abs = std::path::absolute(&map_path).unwrap_or(map_path.clone());

        let mut cmd = std::process::Command::new(&game_bin);
        cmd.arg("--root").arg(&game_root).arg(&map_abs);
        cmd.current_dir(std::env::current_dir().unwrap_or_else(|_| game_root.clone()));
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());

        match cmd.spawn() {
            Ok(child) => {
                self.status = self.tf(
                    "status.test_launching",
                    &[(
                        "path",
                        format!("{} (pid {})", map_path.display(), child.id()),
                    )],
                );
                tracing::info!(game = %game_bin.display(), map = %map_path.display(), pid = child.id(), "launched game playtest");
            }
            Err(e) => {
                self.status = self.tf("status.test_failed", &[("error", e.to_string())]);
                tracing::warn!(error = %e, game = %game_bin.display(), "failed to launch game playtest");
            }
        }
    }
}

/// Draws one atlas region into `rect`. UVs are normalized against the full
/// page, so any resolution of the GPU texture keeps the same UV space (pages
/// are downscaled under the adapter limit).
fn draw_tile_rect(
    painter: &egui::Painter,
    rect: EguiRect,
    tr: crate::tiles::TileRect,
    textures: &[egui::TextureHandle],
    pages: &[crate::tiles::Page],
    tint: Color32,
) -> bool {
    let Some(tex) = textures.get(tr.page) else {
        return false;
    };
    let Some(page) = pages.get(tr.page) else {
        return false;
    };
    if page.width == 0 || page.height == 0 {
        return false;
    }
    let uv = EguiRect::from_min_max(
        Pos2::new(
            tr.x as f32 / page.width as f32,
            tr.y as f32 / page.height as f32,
        ),
        Pos2::new(
            (tr.x + tr.w) as f32 / page.width as f32,
            (tr.y + tr.h) as f32 / page.height as f32,
        ),
    );
    painter.image(tex.id(), rect, uv, tint);
    true
}

/// Preview patch size in tiles: the palette shows a small autotiled blob of
/// each tileset, the same art painting such a blob would get in a room.
const PATCH_TILES: u32 = 4;
/// Screen pixels per preview tile; integer 2x scale keeps the art crisp.
const PATCH_SCALE: f32 = 2.0;
/// Palette cell size: the patch plus a small margin.
const PALETTE_CELL: EguiVec2 = EguiVec2::new(
    PATCH_TILES as f32 * CANVAS_CELL_PX * PATCH_SCALE + 4.0,
    PATCH_TILES as f32 * CANVAS_CELL_PX * PATCH_SCALE + 4.0,
);

/// Draws a small autotiled patch of `value` into `rect`. The patch probes
/// out-of-bounds as empty, so its border picks the surface art and its
/// interior the interior art, exactly like painting this blob into an empty
/// area of a room. Returns false when the tileset has no art at all.
fn draw_palette_patch(
    painter: &egui::Painter,
    rect: EguiRect,
    tiles: &TileRenderer,
    textures: &[egui::TextureHandle],
    value: u16,
) -> bool {
    // The atlas name is irrelevant to autotiling: tile values and the mask
    // definitions alone decide which texture cell each grid position gets.
    let mut patch = releste_map::Tileset::new("preview", PATCH_TILES, PATCH_TILES);
    for y in 0..PATCH_TILES {
        for x in 0..PATCH_TILES {
            patch.set(x, y, value);
        }
    }
    let coords = releste_map::autotiler::generate_with(
        &patch,
        tiles.defs(),
        releste_map::autotiler::Oob::Empty,
    );
    let inner = rect.shrink(2.0);
    let cell = CANVAS_CELL_PX * PATCH_SCALE;
    let pages = tiles.pages();
    let mut painted = false;
    for (index, (col, row)) in coords.iter().enumerate() {
        let Some(tr) = tiles.rect_for(value, *col, *row) else {
            continue;
        };
        let tx = (index % PATCH_TILES as usize) as f32;
        let ty = (index / PATCH_TILES as usize) as f32;
        let sub = EguiRect::from_min_size(
            Pos2::new(inner.left() + tx * cell, inner.top() + ty * cell),
            EguiVec2::splat(cell),
        );
        painted |= draw_tile_rect(painter, sub, tr, textures, pages, Color32::WHITE);
    }
    painted
}

/// One clickable palette cell: a small autotiled patch of the tileset.
/// Returns the (possibly updated) brush value.
fn palette_cell(
    ui: &mut egui::Ui,
    tiles: &TileRenderer,
    textures: &[egui::TextureHandle],
    value: u16,
    brush: u16,
) -> u16 {
    let (rect, resp) = ui.allocate_exact_size(PALETTE_CELL, egui::Sense::click());
    let painter = ui.painter().clone();
    // The patch shows what painting a blob of this tileset looks like; what
    // one single stroke paints is shown by the hover preview on the canvas.
    let painted = draw_palette_patch(&painter, rect, tiles, textures, value);
    if !painted {
        painter.rect_filled(rect, 2.0, placeholder_tile(value, 0.9));
    }
    let clicked = resp.clicked();
    let hovered = resp.hovered();
    let tooltip = match tiles.texture_path(value) {
        Some(path) => format!("[{}] {}", value, path),
        None => format!("[{}] {}", value, releste_map::tile_char(value)),
    };
    resp.on_hover_text(tooltip);
    let brush = if clicked { value } else { brush };
    let stroke = if brush == value {
        egui::Stroke::new(2.0_f32, Color32::from_rgb(255, 210, 60))
    } else if hovered {
        egui::Stroke::new(1.0_f32, Color32::from_gray(180))
    } else {
        egui::Stroke::NONE
    };
    if stroke.width > 0.0 {
        painter.rect_stroke(rect, 2.0_f32, stroke, egui::StrokeKind::Inside);
    }
    brush
}

/// Iterates the integer cells of a line between two grid points (Bresenham).
///
/// A drag stroke receives one pointer sample per frame; drawing the line
/// between consecutive samples keeps a fast swipe continuous instead of
/// leaving gaps.
fn tile_line(from: (u32, u32), to: (u32, u32)) -> Vec<(u32, u32)> {
    let (mut x0, mut y0) = (from.0 as i64, from.1 as i64);
    let (x1, y1) = (to.0 as i64, to.1 as i64);
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut cells = Vec::new();
    loop {
        cells.push((x0 as u32, y0 as u32));
        if x0 == x1 && y0 == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
    cells
}

/// A tile's rectangle on screen.
fn tile_rect(origin: Pos2, x: usize, y: usize, zoom: f32) -> EguiRect {
    let s = CANVAS_CELL_PX * zoom;
    let min = Pos2::new(origin.x + x as f32 * s, origin.y + y as f32 * s);
    EguiRect::from_min_size(min, EguiVec2::splat(s))
}

/// World pixels -> tile units.
///
/// Entity positions and sizes in a map are world pixels, while the canvas grid
/// is measured in tiles. A Celeste tile is 8 source pixels, so a 56-pixel wide
/// crusher is 7 cells - not 56 cells. Skipping this conversion put every entity
/// thousands of pixels off-screen.
fn world_to_tiles(world_px: f32) -> f32 {
    world_px / crate::tiles::TILE_PX as f32
}

/// An entity's rectangle on screen (at least 1 cell visible).
fn entity_rect(origin: Pos2, x: f32, y: f32, width: i32, height: i32, zoom: f32) -> EguiRect {
    let s = CANVAS_CELL_PX * zoom;
    let tx = world_to_tiles(x);
    let ty = world_to_tiles(y);
    let w = world_to_tiles(width as f32).max(1.0);
    let h = world_to_tiles(height as f32).max(1.0);
    EguiRect::from_min_size(
        Pos2::new(origin.x + tx * s, origin.y + ty * s),
        EguiVec2::new(w * s, h * s),
    )
}

/// Active left-panel tool (Loenn-style tool tabs).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    /// Paint and erase tiles.
    Tiles,
    /// Place entities by clicking the canvas.
    Entities,
    /// Marquee-select a rectangle of tiles for clipboard operations.
    Select,
}

/// Alt-drag gesture on a room: move it, or resize it from grabbed border
/// edges (left, top, right, bottom). No edge grabbed means move.
struct RoomDrag {
    room: usize,
    resize: (bool, bool, bool, bool),
    /// Bounds at gesture start: (x, y, w, h) in world pixels.
    start: (f32, f32, f32, f32),
    /// World-space press position.
    grab: (f32, f32),
}

/// Drag-moving an existing tile selection: the original half-open rect,
/// the tile cell the grab started at, and the current anchor cell.
struct TileDragMove {
    room: usize,
    sel: (u32, u32, u32, u32),
    grab: (u32, u32),
    cur: (u32, u32),
}

/// A keyboard shortcut resolved from the current input frame.
enum Shortcut {
    Undo,
    Redo,
    Copy,
    Cut,
    Paste,
    Delete,
    Save,
    SaveAs,
    Open,
    Escape,
    ToolTiles,
    ToolEntities,
    ToolSelect,
}

/// Canvas padding around the map, in world pixels (4 tiles).
const CANVAS_PAD_PX: f32 = 32.0;

/// One room's render snapshot for this frame.
struct RoomView {
    /// Room index in the document.
    index: usize,
    name: String,
    /// (x, y, w, h) in world pixels.
    bounds: (f32, f32, f32, f32),
    is_current: bool,
    tiles_w: u32,
    tiles_h: u32,
    /// (tx, ty, value, autotile col, autotile row)
    fg: Vec<(u32, u32, u16, u32, u32)>,
    /// Same shape as \`fg\`, for the background layer.
    bg: Vec<(u32, u32, u16, u32, u32)>,
    /// (id, x, y, width px, height px, kind)
    entities: Vec<(EntityId, f32, f32, i32, i32, String)>,
    /// Node lists, one per entity (room-local world pixels).
    nodes: Vec<Vec<(f32, f32)>>,
    /// (x, y, w, h, kind)
    triggers: Vec<(f32, f32, f32, f32, String)>,
}

/// Bounding box of the whole map on the canvas (already padded).
struct WorldExtents {
    min: (f32, f32),
    max: (f32, f32),
}

impl WorldExtents {
    /// Padded world width, at least one pixel.
    fn width(&self) -> f32 {
        (self.max.0 - self.min.0).max(1.0)
    }

    /// Padded world height, at least one pixel.
    fn height(&self) -> f32 {
        (self.max.1 - self.min.1).max(1.0)
    }
}

/// Snapshots every room (with autotiler coordinates) for drawing.
///
/// The autotiler runs for both tile layers each frame; interior cells take the
/// fast all-connected path, so this stays well inside frame budget for the
/// room sizes this editor targets. Without loaded tileset definitions the
/// coordinates default to the tileset base cell and tiles still draw.
fn snapshot_world(
    editor: &Editor,
    defs: Option<&releste_map::TilesetsDoc>,
) -> Option<(WorldExtents, Vec<RoomView>)> {
    let doc = editor.document();
    if doc.rooms.is_empty() {
        return None;
    }
    let empty = releste_map::TilesetsDoc::default();
    let defs = defs.unwrap_or(&empty);
    let mut min = (f32::MAX, f32::MAX);
    let mut max = (f32::MIN, f32::MIN);
    let mut rooms = Vec::with_capacity(doc.rooms.len());
    for (index, room) in doc.rooms.iter().enumerate() {
        min.0 = min.0.min(room.bounds.x);
        min.1 = min.1.min(room.bounds.y);
        max.0 = max.0.max(room.bounds.x + room.bounds.w);
        max.1 = max.1.max(room.bounds.y + room.bounds.h);

        // Each layer iterates its own grid: bg may have different dimensions
        // than fg (a 20x12 fg grid can sit on a 53x23 bg grid), so sharing one
        // loop indexed out of bounds on real maps (crash found on
        // 0-Intro.map: bg_cols len 1219 vs fg grid 924).
        let (w, h) = (room.tiles.width, room.tiles.height);
        let fg_cols = releste_map::autotiler::generate(&room.tiles, defs);
        let mut fg = Vec::new();
        for ty in 0..h {
            for tx in 0..w {
                if let Some(v) = room.tiles.get(tx, ty).filter(|v| *v != 0) {
                    let (c, r) = fg_cols[(ty * w + tx) as usize];
                    fg.push((tx, ty, v, c, r));
                }
            }
        }
        let (bw, bh) = (room.bg.width, room.bg.height);
        let bg_cols = releste_map::autotiler::generate(&room.bg, defs);
        let mut bg = Vec::new();
        for ty in 0..bh {
            for tx in 0..bw {
                if let Some(v) = room.bg.get(tx, ty).filter(|v| *v != 0) {
                    let (c, r) = bg_cols[(ty * bw + tx) as usize];
                    bg.push((tx, ty, v, c, r));
                }
            }
        }

        let mut entities = Vec::new();
        let mut nodes = Vec::new();
        for e in &room.entities {
            entities.push((
                e.id,
                e.x,
                e.y,
                e.width.unwrap_or(8),
                e.height.unwrap_or(8),
                e.kind.clone(),
            ));
            nodes.push(e.nodes.clone());
        }
        let triggers = room
            .triggers
            .iter()
            .map(|t| (t.x, t.y, t.width, t.height, t.kind.clone()))
            .collect();

        rooms.push(RoomView {
            index,
            name: room.name.clone(),
            bounds: (room.bounds.x, room.bounds.y, room.bounds.w, room.bounds.h),
            is_current: index == editor.current_room(),
            tiles_w: w,
            tiles_h: h,
            fg,
            bg,
            entities,
            nodes,
            triggers,
        });
    }
    let extents = WorldExtents {
        min: (min.0 - CANVAS_PAD_PX, min.1 - CANVAS_PAD_PX),
        max: (max.0 + CANVAS_PAD_PX, max.1 + CANVAS_PAD_PX),
    };
    Some((extents, rooms))
}

/// The topmost room containing a world point, if any.
fn room_under(rooms: &[RoomView], world: (f32, f32)) -> Option<usize> {
    rooms.iter().rev().find_map(|r| {
        let contains = world.0 >= r.bounds.0
            && world.1 >= r.bounds.1
            && world.0 <= r.bounds.0 + r.bounds.2
            && world.1 <= r.bounds.1 + r.bounds.3;
        contains.then_some(r.index)
    })
}

/// World pixels -> screen pixels.
fn world_to_screen(origin: Pos2, map_min: (f32, f32), zoom: f32, wx: f32, wy: f32) -> Pos2 {
    Pos2::new(
        origin.x + (wx - map_min.0) * zoom,
        origin.y + (wy - map_min.1) * zoom,
    )
}

/// Screen pixels -> world pixels.
///
/// The zoom guard keeps the division defined at any zoom (AGENTS.md 15.8).
fn screen_to_world(origin: Pos2, map_min: (f32, f32), zoom: f32, p: Pos2) -> (f32, f32) {
    (
        map_min.0 + (p.x - origin.x) / zoom.max(f32::EPSILON),
        map_min.1 + (p.y - origin.y) / zoom.max(f32::EPSILON),
    )
}

/// Screen position of a room's top-left corner.
fn room_screen_min(
    origin: Pos2,
    map_min: (f32, f32),
    zoom: f32,
    bounds: (f32, f32, f32, f32),
) -> Pos2 {
    world_to_screen(origin, map_min, zoom, bounds.0, bounds.1)
}

/// Flat color used when atlas art is unavailable for a tile id.
///
/// \`dim\` scales the channels (background placeholders stay darker).
/// Parses a tileset definition key into its tile value. Keys follow the tile
/// alphabet ("1".."9", "a".."z"; "0" is the empty cell and never a def).
fn parse_tile_key(key: &str) -> Option<u16> {
    let mut chars = key.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    match c {
        '1'..='9' => Some(c as u16 - '0' as u16),
        'a'..='z' => Some(10 + c as u16 - 'a' as u16),
        'A'..='Z' => Some(36 + c as u16 - 'A' as u16),
        _ => None,
    }
}

fn placeholder_tile(value: u16, dim: f32) -> Color32 {
    let hue = (value as f32) / 62.0;
    Color32::from_rgb(
        ((60.0 + 180.0 * hue) * dim) as u8,
        ((120.0 + 80.0 * (1.0 - hue)) * dim) as u8,
        ((200.0 - 120.0 * hue) * dim) as u8,
    )
}

/// Renders a single widget. Returns whether the value changed.
///
/// `l10n` is passed in rather than read from a thread-local so this stays a
/// pure function that is easy to reason about (and to test).
fn render_widget(
    ui: &mut egui::Ui,
    name: &str,
    widget: &WidgetKind,
    value: &mut FieldValue,
    l10n: &Localizer,
) -> bool {
    match widget {
        WidgetKind::IntInput { min, max } => {
            if let FieldValue::Int(v) = value {
                ui.add(egui::DragValue::new(v).range(*min..=*max)).changed()
            } else {
                ui.label(l10n.lookup("widget.type_mismatch.int"));
                false
            }
        }
        WidgetKind::FloatInput { min, max, speed } => {
            if let FieldValue::Float(v) = value {
                ui.add(egui::DragValue::new(v).range(*min..=*max).speed(*speed))
                    .changed()
            } else {
                ui.label(l10n.lookup("widget.type_mismatch.float"));
                false
            }
        }
        WidgetKind::TextInput => {
            if let FieldValue::String(v) = value {
                ui.text_edit_singleline(v).changed()
            } else {
                ui.label(l10n.lookup("widget.type_mismatch.string"));
                false
            }
        }
        WidgetKind::Checkbox => {
            if let FieldValue::Bool(v) = value {
                ui.checkbox(v, "").changed()
            } else {
                ui.label(l10n.lookup("widget.type_mismatch.bool"));
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
                            .unwrap_or_else(|| l10n.lookup("widget.combo_empty").to_string()),
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
                ui.label(l10n.lookup("widget.type_mismatch.enum"));
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
                ui.label(l10n.lookup("widget.type_mismatch.color"));
                false
            }
        }
        WidgetKind::EntityRefPicker => {
            if let FieldValue::EntityRef(v) = value {
                ui.add(egui::DragValue::new(v).speed(1.0)).changed()
            } else {
                ui.label(l10n.lookup("widget.type_mismatch.entity_ref"));
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
                ui.label(l10n.lookup("widget.type_mismatch.vec2"));
                false
            }
        }
    }
}

/// Box-filter downscale of an RGBA8 bitmap so neither side exceeds `max_side`.
///
/// Returns the original buffer untouched when it already fits, so the common
/// case costs nothing. A source smaller than the limit is never upscaled.
///
/// This exists because `egui` panics when asked to upload a texture larger than
/// the backend allows (AGENTS.md 15.8 forbids panics); the atlas can legitimately
/// be 4096x4096 while a software adapter only supports 2048.
fn downscale_rgba(w: usize, h: usize, rgba: &[u8], max_side: usize) -> (usize, usize, Vec<u8>) {
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return (0, 0, Vec::new());
    }
    if w <= max_side && h <= max_side {
        return (w, h, rgba.to_vec());
    }

    let scale = (max_side as f64 / w.max(h) as f64).min(1.0);
    let nw = ((w as f64 * scale).floor() as usize).max(1);
    let nh = ((h as f64 * scale).floor() as usize).max(1);

    let mut out = vec![0u8; nw * nh * 4];
    for ty in 0..nh {
        let y0 = ty * h / nh;
        let y1 = ((ty + 1) * h / nh).max(y0 + 1).min(h);
        for tx in 0..nw {
            let x0 = tx * w / nw;
            let x1 = ((tx + 1) * w / nw).max(x0 + 1).min(w);
            let mut acc = [0u32; 4];
            let mut count = 0u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * w + x) * 4;
                    for c in 0..4 {
                        acc[c] += u32::from(rgba[i + c]);
                    }
                    count += 1;
                }
            }
            let o = (ty * nw + tx) * 4;
            for c in 0..4 {
                out[o + c] = (acc[c] / count.max(1)) as u8;
            }
        }
    }
    (nw, nh, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_between_two_points_is_continuous_in_x() {
        let cells = tile_line((0, 0), (5, 1));
        assert_eq!(cells.len(), 6);
        for (i, (x, _)) in cells.iter().enumerate() {
            assert_eq!(*x, i as u32);
        }
    }

    #[test]
    fn a_diagonal_line_steps_every_cell() {
        assert_eq!(
            tile_line((0, 0), (3, 3)),
            vec![(0, 0), (1, 1), (2, 2), (3, 3)],
        );
    }

    #[test]
    fn a_single_cell_line_is_that_cell() {
        assert_eq!(tile_line((2, 2), (2, 2)), vec![(2, 2)]);
    }

    #[test]
    fn palette_patch_shows_the_painted_blob_art() {
        use releste_map::{Tileset, TilesetDef, TilesetsDoc};
        let mut defs = TilesetsDoc::default();
        let def = TilesetDef {
            padding: vec![(0, 0)],
            center: vec![(1, 0)],
            ..Default::default()
        };
        defs.tilesets.insert("1".into(), def);

        // A 4x4 blob probed with empty surroundings: with no masks at all,
        // border cells fall back to the center list, while every interior
        // cell sees the outside two tiles away and picks the surface
        // (padding) art. Real defs give the border real surface masks, so
        // the palette patch reads as the painted blob.
        let mut patch = Tileset::new("preview", PATCH_TILES, PATCH_TILES);
        for y in 0..PATCH_TILES {
            for x in 0..PATCH_TILES {
                patch.set(x, y, 1);
            }
        }
        let coords = releste_map::autotiler::generate_with(
            &patch,
            &defs,
            releste_map::autotiler::Oob::Empty,
        );
        assert_eq!(coords.len(), (PATCH_TILES * PATCH_TILES) as usize);
        for (index, coord) in coords.iter().enumerate() {
            let tx = (index % PATCH_TILES as usize) as u32;
            let ty = (index / PATCH_TILES as usize) as u32;
            let border = tx == 0 || ty == 0 || tx == PATCH_TILES - 1 || ty == PATCH_TILES - 1;
            let expected = if border { (1, 0) } else { (0, 0) };
            assert_eq!(*coord, expected, "cell ({tx},{ty}) picked the wrong art");
        }

        // A bigger blob has a true interior: far enough from every edge the
        // center art appears, while cells one tile in stay on the surface.
        let mut big = Tileset::new("preview", 9, 9);
        for y in 0..9u32 {
            for x in 0..9u32 {
                big.set(x, y, 1);
            }
        }
        let coords =
            releste_map::autotiler::generate_with(&big, &defs, releste_map::autotiler::Oob::Empty);
        let at = |tx: u32, ty: u32| coords[(ty * 9 + tx) as usize];
        assert_eq!(at(4, 4), (1, 0), "deep interior uses the center art");
        assert_eq!(at(1, 4), (0, 0), "one tile in still uses the surface art");
    }

    #[test]
    fn tile_keys_parse_across_the_alphabet() {
        assert_eq!(parse_tile_key("0"), None);
        assert_eq!(parse_tile_key("1"), Some(1));
        assert_eq!(parse_tile_key("9"), Some(9));
        assert_eq!(parse_tile_key("a"), Some(10));
        assert_eq!(parse_tile_key("z"), Some(35));
        assert_eq!(parse_tile_key("A"), Some(36));
        assert_eq!(parse_tile_key("G"), Some(42));
        assert_eq!(parse_tile_key("Z"), Some(61));
        assert_eq!(parse_tile_key(""), None);
        assert_eq!(parse_tile_key("ab"), None);
        assert_eq!(parse_tile_key("-"), None);
    }

    #[test]
    fn painting_math_roundtrips_a_click_to_the_right_cell() {
        use releste_map::{RectDef, Tileset};
        let mut e = Editor::new();
        let room = releste_map::Room {
            name: "r".into(),
            bounds: RectDef {
                x: 100.0,
                y: 60.0,
                w: 32.0,
                h: 24.0,
            },
            tiles: Tileset::new("Gameplay", 4, 3),
            ..Default::default()
        };
        e.add_room(room).expect("room added");
        let (extents, rooms) = snapshot_world(&e, None).expect("rooms");

        // A click on the center of tile (2,1) at a fit-view zoom of 0.35.
        let zoom = 0.35_f32;
        let origin = Pos2::new(12.0, 8.0);
        let target = (100.0 + 2.0 * 8.0 + 4.0, 60.0 + 1.0 * 8.0 + 4.0);
        let screen = world_to_screen(origin, extents.min, zoom, target.0, target.1);
        let back = screen_to_world(origin, extents.min, zoom, screen);
        assert!((back.0 - target.0).abs() < 0.01, "x: {}", back.0);
        assert!((back.1 - target.1).abs() < 0.01, "y: {}", back.1);
        assert_eq!(room_under(&rooms, back), Some(0));

        let rv = &rooms[0];
        let tx = ((back.0 - rv.bounds.0) / 8.0).floor();
        let ty = ((back.1 - rv.bounds.1) / 8.0).floor();
        assert_eq!((tx, ty), (2.0, 1.0));
        e.paint_tile(tx as u32, ty as u32, 1)
            .expect("paint must succeed");
        assert_eq!(e.room().expect("room").tiles.get(2, 1), Some(1));
    }

    #[test]
    fn a_bitmap_that_fits_is_returned_unchanged() {
        let rgba = vec![7u8; 4 * 4 * 4];
        let (w, h, out) = downscale_rgba(4, 4, &rgba, 2048);
        assert_eq!((w, h), (4, 4));
        assert_eq!(out, rgba, "no copy should alter the pixels");
    }

    #[test]
    fn an_oversized_bitmap_is_scaled_under_the_limit() {
        // 4096x4096 is the real atlas page that crashed on a 2048 adapter.
        let rgba = vec![255u8; 4096 * 4096 * 4];
        let (w, h, out) = downscale_rgba(4096, 4096, &rgba, 2048);
        assert!(w <= 2048 && h <= 2048, "got {w}x{h}");
        assert_eq!((w, h), (2048, 2048));
        assert_eq!(out.len(), w * h * 4, "buffer must match the new size");
    }

    #[test]
    fn a_non_square_page_keeps_its_aspect_ratio() {
        let rgba = vec![128u8; 4096 * 1024 * 4];
        let (w, h, _) = downscale_rgba(4096, 1024, &rgba, 2048);
        assert_eq!(w, 2048);
        assert_eq!(h, 512);
    }

    #[test]
    fn a_degenerate_page_does_not_panic() {
        // A short buffer must be reported, never sliced out of bounds.
        assert_eq!(downscale_rgba(8, 8, &[], 2048), (0, 0, Vec::new()));
        assert_eq!(downscale_rgba(0, 8, &[], 2048), (0, 0, Vec::new()));
    }

    #[test]
    fn downscaling_preserves_average_colour() {
        // A solid block must stay that colour after the box filter.
        let rgba = [10u8, 20, 30, 255].repeat(64 * 64);
        let (w, h, out) = downscale_rgba(64, 64, &rgba, 16);
        assert_eq!((w, h), (16, 16));
        assert_eq!(&out[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn entity_rect_converts_world_pixels_to_cells() {
        // A 56x32-pixel crusher at world (256,40) is 7x4 cells at 8px/tile.
        let origin = egui::Pos2::new(0.0, 0.0);
        let r = super::entity_rect(origin, 256.0, 40.0, 56, 32, 1.0);
        let cell = super::CANVAS_CELL_PX;
        assert!((r.min.x - 32.0 * cell).abs() < 0.01, "x was {}", r.min.x);
        assert!((r.min.y - 5.0 * cell).abs() < 0.01, "y was {}", r.min.y);
        assert!((r.width() - 7.0 * cell).abs() < 0.01, "w was {}", r.width());
        assert!(
            (r.height() - 4.0 * cell).abs() < 0.01,
            "h was {}",
            r.height()
        );
    }

    #[test]
    fn a_zero_sized_entity_still_occupies_one_cell() {
        let r = super::entity_rect(egui::Pos2::new(0.0, 0.0), 0.0, 0.0, 0, 0, 1.0);
        let cell = super::CANVAS_CELL_PX;
        assert!((r.width() - cell).abs() < 0.01);
        assert!((r.height() - cell).abs() < 0.01);
    }

    #[test]
    fn snapshot_autotiles_and_extents() {
        use releste_map::{MaskEntry, RectDef, Tileset, TilesetDef, TilesetsDoc};
        let mut e = Editor::new();
        let mut room = releste_map::Room {
            name: "r".into(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: 16.0,
                h: 16.0,
            },
            tiles: Tileset::new("Gameplay", 2, 2),
            ..Default::default()
        };
        for (x, y) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
            room.tiles.set(x, y, 1);
        }
        e.add_room(room).expect("room added");

        let mut defs = TilesetsDoc::default();
        let mut def = TilesetDef {
            center: vec![(1, 0)],
            ..Default::default()
        };
        def.masks.push(MaskEntry {
            mask: [1; 9],
            tiles: Vec::new(),
        });
        defs.tilesets.insert("1".into(), def);

        let (extents, rooms) = snapshot_world(&e, Some(&defs)).expect("rooms");
        assert_eq!(rooms.len(), 1);
        assert!(rooms[0].is_current);
        // Every cell is fully surrounded (Oob::Extend makes the outside
        // solid), so all four must land on the tileset center, not (0, 0).
        assert_eq!(rooms[0].fg.len(), 4);
        for (_, _, _, c, r) in &rooms[0].fg {
            assert_eq!((*c, *r), (1, 0), "surrounded cell uses the center");
        }
        // A 16px room plus the 2x32px canvas pad.
        assert!((extents.width() - 80.0).abs() < 1e-3);
        assert!((extents.height() - 80.0).abs() < 1e-3);
    }

    #[test]
    fn screen_world_roundtrip() {
        let origin = Pos2::new(40.0, -12.0);
        let map_min = (320.0, 180.0);
        let zoom = 1.35;
        let p = world_to_screen(origin, map_min, zoom, 424.0, 200.0);
        let w = screen_to_world(origin, map_min, zoom, p);
        assert!((w.0 - 424.0).abs() < 1e-3);
        assert!((w.1 - 200.0).abs() < 1e-3);
    }

    #[test]
    fn room_under_picks_the_last_containing_room() {
        let rooms = vec![
            RoomView {
                index: 0,
                name: "a".into(),
                bounds: (0.0, 0.0, 160.0, 96.0),
                is_current: false,
                tiles_w: 20,
                tiles_h: 12,
                fg: Vec::new(),
                bg: Vec::new(),
                entities: Vec::new(),
                nodes: Vec::new(),
                triggers: Vec::new(),
            },
            RoomView {
                index: 1,
                name: "b".into(),
                bounds: (160.0, 0.0, 160.0, 96.0),
                is_current: true,
                tiles_w: 20,
                tiles_h: 12,
                fg: Vec::new(),
                bg: Vec::new(),
                entities: Vec::new(),
                nodes: Vec::new(),
                triggers: Vec::new(),
            },
        ];
        assert_eq!(room_under(&rooms, (80.0, 48.0)), Some(0));
        assert_eq!(room_under(&rooms, (200.0, 48.0)), Some(1));
        assert_eq!(room_under(&rooms, (400.0, 48.0)), None);
    }
}
