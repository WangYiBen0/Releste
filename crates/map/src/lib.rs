//! `reles-map`: `.map` 二进制格式解析 / 序列化 / diff。
//!
//! - 自研二进制（`bincode`），版本号在头部。
//! - 向后兼容通过显式迁移函数。
//! - `diff(old, new)` 生成 `Vec<Patch>` 用于热重载。

#![deny(warnings)]

mod diff;
mod format;
mod loader;

pub use diff::{diff, Patch};
pub use format::{
    MapDocument, Props, RectDef, Room, SyncMode, SyncRule, Tileset, TriggerData, MAP_MAGIC,
    MAP_VERSION,
};
pub use loader::{load_map, load_map_bytes, save_map, to_bytes, MapError};
