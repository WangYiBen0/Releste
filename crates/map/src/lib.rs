//! `releste-map`: `.map` binary format parsing / serialization / diff.
//!
//! - Custom binary (`bincode`), with the version number in the header.
//! - Backward compatibility goes through **explicit migration functions** (one frozen struct
//!   per historical version in [`legacy`] plus one `v_n -> v_{n+1}` step). `#[serde(default)]` does not
//!   fire under bincode and cannot replace the compatibility layer.
//! - `diff(old, new)` produces a `Vec<Patch>` for hot reload.

#![deny(warnings)]

pub mod autotiler;
mod diff;
mod format;
pub mod legacy;
mod loader;
mod tilesets;

pub use diff::{diff, Patch};
pub use format::{
    Backdrop, Decal, MapDocument, MapStyle, Props, RectDef, Room, SyncMode, SyncRule, Tileset,
    TriggerData, MAP_MAGIC, MAP_VERSION,
};
pub use loader::{load_map, load_map_bytes, save_map, to_bytes, MapError};
pub use tilesets::{tile_char, MaskEntry, TilesetDef, TilesetsDoc, TilesetsError, TILE_ALPHABET};
