//! Asset pipeline: vanilla Celeste assets -> engine format.
//!
//! # Core invariant (AGENTS.md 4.6)
//! **Original formats appear only at build time.** Runtime `crates/` must not contain
//! parsing code for `.bin` / `.xnb` / `.meta` -- only this tool may.
//!
//! # Conversion map (AGENTS.md 12)
//! ```text
//! assets-src/                        assets/
//!   Graphics/Atlases/    ->            atlas/*.atlas
//!   Graphics/ForegroundTiles.xml ->    atlas/tilesets.toml
//!   Maps/*.bin           ->            maps/*.map
//!   Dialog/*.txt         ->            dialog/*.toml
//!   Fonts/*.fnt          ->            fonts/*.font
//!   FMOD/Desktop/*.bank  ->            audio/*.bank (copied verbatim)
//! ```

#![deny(warnings)]

mod atlas;
mod data;
mod dialog;
mod error;
mod mapbin;
mod pipeline;
pub mod sprites_xml;
mod tilesets;
mod xml;

pub use atlas::{AtlasBuild, SpriteRegion};
pub use data::{decode, DecodedTexture};
pub use dialog::{DialogDocument, DialogEntry};
pub use error::PipelineError;
pub use mapbin::{MapBin, MapBinElement};
pub use pipeline::{
    default_sync_rules, example_sync_rule, Pipeline, PipelineConfig, PipelineReport,
};
