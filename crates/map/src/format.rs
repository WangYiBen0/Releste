//! `.map` 文件格式定义。

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use reles_math::{Rect, Vec2};
use reles_world::{EntityData, FieldValue};

/// 魔术字节：`"RELMAP\0\0"`
pub const MAP_MAGIC: &[u8; 8] = b"RELMAP\x00\x00";

/// 当前 `.map` 格式版本。
pub const MAP_VERSION: u32 = 1;

/// `.map` 文件的完整结构。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapDocument {
    pub area: String,
    pub rooms: Vec<Room>,
    #[serde(default)]
    pub sync_rules: Vec<SyncRule>,
}

/// 单个房间。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    pub name: String,
    pub bounds: RectDef,
    pub tiles: Tileset,
    #[serde(default)]
    pub bg: Tileset,
    #[serde(default)]
    pub entities: Vec<EntityData>,
    #[serde(default)]
    pub triggers: Vec<TriggerData>,
    #[serde(default)]
    pub script: Option<PathBuf>,
}

/// 矩形定义（f32，地图坐标）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RectDef {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectDef {
    pub fn to_rect(self) -> Rect {
        Rect::new(
            Vec2::from_f32s(self.x, self.y),
            Vec2::from_f32s(self.x + self.w, self.y + self.h),
        )
    }
}

/// Tile 集定义。
///
/// 行优先的 tile 索引网格；`0` 表示空。索引的语义由 `atlas`
/// 决定（引擎在渲染层解释，映射层不理解美术）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Tileset {
    /// 纹理图集名称（如 `"Gameplay"`）。
    #[serde(default)]
    pub atlas: String,
    /// 网格宽度（tile 数）。
    #[serde(default)]
    pub width: u32,
    /// 网格高度（tile 数）。
    #[serde(default)]
    pub height: u32,
    /// 行优先 tile 索引，长度应为 `width * height`。
    #[serde(default)]
    pub tiles: Vec<u8>,
}

impl Tileset {
    /// 新建网格。
    pub fn new(atlas: impl Into<String>, width: u32, height: u32) -> Self {
        Tileset {
            atlas: atlas.into(),
            width,
            height,
            tiles: vec![0; (width as usize) * (height as usize)],
        }
    }

    /// 取 tile。
    pub fn get(&self, x: u32, y: u32) -> Option<u8> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.tiles
            .get((y as usize) * (self.width as usize) + (x as usize))
            .copied()
    }

    /// 设置 tile。
    pub fn set(&mut self, x: u32, y: u32, value: u8) {
        if x < self.width && y < self.height {
            let idx = (y as usize) * (self.width as usize) + (x as usize);
            if let Some(slot) = self.tiles.get_mut(idx) {
                *slot = value;
            }
        }
    }

    /// 是否有任意非空 tile。
    pub fn is_empty(&self) -> bool {
        self.tiles.iter().all(|t| *t == 0)
    }
}

/// 触发器数据。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TriggerData {
    pub kind: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub properties: HashMap<String, String>,
}

/// 同步规则。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SyncRule {
    pub kind: String,
    pub mode: SyncMode,
    #[serde(default)]
    pub flag: Option<String>,
}

/// 同步模式。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SyncMode {
    Flag,
    Full,
    Event,
}

/// 实体属性集合（用于 diff）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Props {
    pub fields: HashMap<String, FieldValue>,
}

impl Props {
    pub fn from_entity(entity: &EntityData) -> Self {
        Props {
            fields: entity.properties.clone(),
        }
    }
}
