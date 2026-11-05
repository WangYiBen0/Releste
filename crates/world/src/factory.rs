//! EntityFactory trait, BuildCtx, Persistence.

use crate::entity::{Entity, EntityData};
use crate::schema::Schema;

/// Entity kind identifier.
///
/// Each registered entity factory has a unique `EntityKindId`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct EntityKindId(pub u32);

impl EntityKindId {
    pub const fn new(raw: u32) -> Self {
        EntityKindId(raw)
    }
}

/// Entity factory: builds a runtime [`Entity`] and declares its Schema and
/// persistence policy.
pub trait EntityFactory: Send + Sync {
    /// The entity kind identifier for this factory.
    fn id(&self) -> EntityKindId;

    /// Build a runtime entity from map data.
    fn build(&self, data: &EntityData, ctx: &mut BuildCtx) -> Entity;

    /// Return the property Schema (used by the editor to auto-generate panels).
    fn schema(&self) -> &Schema;

    /// Persistence policy for hot reload.
    fn persistence(&self) -> Persistence;
}

/// Persistence policy for an entity during hot reload.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Persistence {
    /// Rebuild on hot reload (static entities such as walls and decorations).
    Rebuild,
    /// Preserve state, only update properties (stateful entities such as
    /// pickups and doors).
    Preserve,
    /// Never unload (player, camera).
    Persistent,
}

/// Build context.
///
/// During `EntityFactory::build` it provides queries against other entities
/// and event dispatch.
pub struct BuildCtx {
    // Extended later: event bus, World read references, etc.
}

impl BuildCtx {
    pub fn new() -> Self {
        BuildCtx {}
    }
}

impl Default for BuildCtx {
    fn default() -> Self {
        Self::new()
    }
}
