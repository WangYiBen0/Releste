//! `releste-map`: `.map` 二进制格式解析 / 序列化 / diff。
//!
//! - 自研二进制（`bincode`），版本号在头部。
//! - 向后兼容通过**显式迁移函数**（[`legacy`] 里每个历史版本一份冻结
//!   结构体 + 一条 `v_n → v_{n+1}`）。`#[serde(default)]` 在 bincode 下
//!   不生效，不能当作兼容层的替代品。
//! - `diff(old, new)` 生成 `Vec<Patch>` 用于热重载。

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
