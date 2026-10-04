//! `reles-world`: ECS、实体工厂、Schema、组件。
//!
//! 所有地图实体通过稳定 ID（`blake3` 散列）索引，
//! 由 [`EntityFactory`] trait 构建。
//!
//! # 核心不变量
//! - 地图是数据，不是代码：实体仅通过 `EntityKindId` 引用注册表。
//! - 稳定实体 ID：`blake3(area_id || room_name || kind_id || x || y)` 前 8 字节。

#![deny(warnings)]

mod entity;
mod factory;
mod id;
mod schema;
mod world;

pub use entity::{Entity, EntityData};
pub use factory::{BuildCtx, EntityFactory, EntityKindId, Persistence};
pub use id::EntityId;
pub use schema::{FieldType, FieldValue, Schema, SchemaField};
pub use world::World;
