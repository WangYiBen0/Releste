//! `reles-render`: wgpu renderer, camera, batching.
//!
//! # Layering
//! - [`camera`]: world ↔ screen/NDC transforms (pure math, tested).
//! - [`atlas`]: loads the `.atlas` descriptors produced by content-pipeline.
//! - [`batch`]: merges draw calls by layer and texture (pure logic, tested).
//! - [`gpu`]: wgpu device / pipeline / texture upload (needs a GPU, manual checklist).

#![deny(warnings)]

pub mod atlas;
pub mod batch;
pub mod camera;
pub mod gpu;

pub use atlas::{AtlasDescriptor, AtlasError, Sprite, SpriteAtlas, TextureGroup, TextureKind};
pub use batch::{BatchError, BatchRange, SpriteBatch, SpriteDraw, SpriteVertex};
pub use camera::{Camera, BASE_HEIGHT, BASE_WIDTH};
pub use gpu::{GpuRenderer, GpuRendererConfig, RenderError};

/// Draw layer. Lower values draw first.
///
/// Values leave gaps so map authors can insert custom layers in maps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(i32)]
pub enum Layer {
    /// Distant background.
    Background = -1000,
    /// Background tiles.
    BgTiles = -500,
    /// Foreground tiles.
    Tiles = 0,
    /// Entities.
    Entities = 100,
    /// Player.
    Player = 200,
    /// Particles / effects.
    Effects = 300,
    /// Foreground decoration.
    Foreground = 500,
    /// UI / HUD.
    Ui = 900,
}

impl Layer {
    /// The layer value as used by [`SpriteDraw`].
    pub const fn value(self) -> i32 {
        self as i32
    }
}
