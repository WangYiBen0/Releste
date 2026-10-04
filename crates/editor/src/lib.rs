//! `reles-editor`: an egui map editor.
//!
//! # Layering
//! - [`model`]: document, selection, undo stack (**no egui dependency**, tested).
//! - [`schema_panel`]: property panel auto-generated from an entity's
//!   [`Schema`](reles_world::Schema) (form logic is pure, tested).
//! - [`ui`]: renders the model as an egui interface.
//! - [`app`]: the winit + egui-wgpu host (requires a GPU).
//!
//! # Manual checklist (AGENTS.md §9: pure UI code gets no unit
//! tests, but does need a checklist)
//! - [ ] A round trip through Open / Save / Save As leaves the map unchanged
//! - [ ] Undo / redo matches the world state after 50 consecutive operations
//! - [ ] Dragging the tile brush outside the bounds neither crashes nor writes
//!   out of bounds
//! - [ ] Schema panel generation stays smooth with hundreds of entities
//!   (AGENTS.md §13 item)
//! - [ ] After saving, `reles-reload` takes effect without a restart

#![deny(warnings)]

pub mod app;
pub mod model;
pub mod schema_panel;
pub mod ui;

pub use model::{Editor, EditorError, Selection};
pub use schema_panel::{FormField, SchemaPanel, WidgetKind};
