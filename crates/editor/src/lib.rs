//! `releste-editor`: an egui map editor.
//!
//! # Layering
//! - [`model`]: document, selection, undo stack (**no egui dependency**, tested).
//! - [`schema_panel`]: property panel auto-generated from an entity's
//!   [`Schema`](releste_world::Schema) (form logic is pure, tested).
//! - [`ui`]: renders the model as an egui interface.
//! - [`app`]: the winit + egui-wgpu host (requires a GPU).
//! - [`l10n`]: locale catalogs, the fallback chain and placeholder
//!   substitution (**no egui dependency**, tested). Data lives in `locales/`.
//! - [`fonts`]: locates a CJK-capable system font at run time and registers it
//!   as egui's fallback, so a translated UI is not a wall of tofu boxes.
//! - [`settings`]: persisted editor settings (fallback font + UI scale) in
//!   `$XDG_CONFIG_HOME/releste/editor.toml`.
//!
//! # Manual checklist (AGENTS.md 9: pure UI code gets no unit
//! tests, but does need a checklist)
//! - [ ] A round trip through Open / Save / Save As leaves the map unchanged
//! - [ ] Undo / redo matches the world state after 50 consecutive operations
//! - [ ] Dragging the tile brush outside the bounds neither crashes nor writes
//!   out of bounds
//! - [ ] Schema panel generation stays smooth with hundreds of entities
//!   (AGENTS.md 13 item)
//! - [ ] After saving, `releste-reload` takes effect without a restart
//! - [ ] Switching the language in the menu re-renders every label immediately,
//!   with no restart and no leftover text in the old language
//! - [ ] A map opened with `--lang zh_CN` shows Chinese chrome but keeps map
//!   data (room names, entity property keys) verbatim

#![deny(warnings)]

pub mod app;
pub mod dialog;
pub mod fonts;
pub mod l10n;
pub mod model;
pub mod schema_panel;
pub mod script;
pub mod settings;
pub mod tiles;
pub mod ui;

pub use l10n::{Catalog, Locale, Localizer};
pub use model::{Editor, EditorError, Selection};
pub use schema_panel::{FormField, SchemaPanel, WidgetKind};
