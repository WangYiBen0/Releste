//! 资源管线：原版 Celeste 资源 → 引擎格式。
//!
//! # 核心不变量（AGENTS.md §4.6）
//! **原版格式只在构建期出现。** 运行时 `crates/` 里不能出现
//! `.bin` / `.xnb` / `.meta` 的解析代码——只有本工具可以。
//!
//! # 转换映射（AGENTS.md §12）
//! ```text
//! assets-src/                   assets/
//!   Graphics/Atlases/    →        atlas/*.atlas
//!   Maps/*.bin           →        maps/*.map
//!   Dialog/*.txt         →        dialog/*.toml
//!   Fonts/*.fnt          →        fonts/*.font
//!   FMOD/Desktop/*.bank  →        audio/*.bank（原样复制）
//! ```

#![deny(warnings)]

mod atlas;
mod dialog;
mod error;
mod mapbin;
mod pipeline;

pub use atlas::{AtlasBuild, SpriteRegion};
pub use dialog::{DialogDocument, DialogEntry};
pub use error::PipelineError;
pub use mapbin::{MapBin, MapBinElement};
pub use pipeline::{
    default_sync_rules, example_sync_rule, Pipeline, PipelineConfig, PipelineReport,
};
