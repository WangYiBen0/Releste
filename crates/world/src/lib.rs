//! `releste-world`: ECS, entity factories, Schema, components.
//!
//! All map entities are indexed by a stable ID (`blake3` hash) and built by
//! the [`EntityFactory`] trait.
//!
//! # Core invariants
//! - Maps are data, not code: entities only reference the registry via
//!   `EntityKindId`.
//! - Stable entity ID: `blake3(area_id || room_name || kind_id || x || y)`,
//!   first 8 bytes.

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
