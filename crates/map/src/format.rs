//! `.map` 文件格式定义。
//!
//! 磁盘布局即字段声明顺序（bincode 不自描述，ROADMAP 附录 E.6）：
//! **只能追加到末尾，不能插队，不能删**。任何改动都是
//! [`MAP_VERSION`] 自增 + [`crate::legacy`] 里留一份冻结副本 +
//! 一条 `v_n → v_{n+1}` 迁移。
//!
//! v2 相对 v1 的追加（全部在各自结构体末尾）：
//! - [`MapDocument::style`] —— 区域级视差背景（`<Style>`，`pureCeleste.cs:112428-112449`）；
//! - [`Room::bg_decals`] / [`Room::fg_decals`] —— 贴花（`113467-113497`）；
//! - [`Room::bg_scenery`] / [`Room::fg_scenery`] —— `Scenery` 装饰层覆盖（`77244-77253`）；
//! - [`EntityData::nodes`] —— 实体节点（`introCrusher` 的终点、`wire` 的折线、`bridge` 的桥墩）。
//!
//! 同时把 `properties` / `Props.fields` 从 `HashMap` 换成 `BTreeMap`：
//! `HashMap` 按 `RandomState` 顺序遍历，同一张地图两次序列化的字节
//! 不同，内容哈希因此不可用（ROADMAP E.8 / E.9 #3），而 `HashMap` 的
//! `PartialEq` 忽略顺序，所以 §9 的 roundtrip 测试一直没发现。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use releste_math::{Rect, Vec2};
use releste_world::{EntityData, FieldValue};

/// 魔术字节：`"RELMAP\0\0"`
pub const MAP_MAGIC: &[u8; 8] = b"RELMAP\x00\x00";

/// 当前 `.map` 格式版本。
///
/// v2：区域视差背景 + 贴花 + `Scenery` 装饰层 + 实体节点 + `BTreeMap` 属性。
pub const MAP_VERSION: u32 = 2;

/// `.map` 文件的完整结构。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MapDocument {
    pub area: String,
    pub rooms: Vec<Room>,
    /// 同步规则（v1 已有，声明位置保持不变）。
    #[serde(default)]
    pub sync_rules: Vec<SyncRule>,
    /// 区域级样式：清屏色与视差背景（v2 追加）。
    #[serde(default)]
    pub style: MapStyle,
}

/// 单张地图的样式（原版 `MapData` 里来自 `<Style>` 元素）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MapStyle {
    /// 清屏色（RGB）。
    ///
    /// 原版默认 `Color.Black`，只有 `<Style color="…">` 才覆盖
    /// （`pureCeleste.cs:112310` 默认值、`112432-112435` 覆盖）。
    #[serde(default)]
    pub background_color: [u8; 3],
    /// 世界**之后**绘制的背景（原版 `<Backgrounds>`）。
    #[serde(default)]
    pub backgrounds: Vec<Backdrop>,
    /// 世界**之前**绘制的前景（原版 `<Foregrounds>`）。
    #[serde(default)]
    pub foregrounds: Vec<Backdrop>,
}

/// 一条背景 / 前景。
///
/// `kind` 是原版元素名（`"parallax"`、`"snowBg"`、`"snowFg"`…），
/// 引擎按它决定行为；表里没写出来的字段一律取原版默认值。
///
/// 默认值出处：`pureCeleste.cs:109341-109430`（`Backdrop` 字段默认）、
/// `112620-112900`（`ParseBackdrop` 逐属性覆盖）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Backdrop {
    /// 元素名（小写比较，同 `ParseBackdrop`）。
    pub kind: String,
    /// 纹理名（`parallax texture="…"`）。
    #[serde(default)]
    pub texture: String,
    /// 纹理来源图集：`"game"` / `"gui"` / `"misc"`（`112635-112646`）。
    #[serde(default)]
    pub atlas: String,
    /// 位置（`x` / `y`，默认 0）。
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    /// 滚动系数（`scrollx` / `scrolly`，默认 1 —— 即跟着相机走）。
    #[serde(default)]
    pub scroll_x: f32,
    #[serde(default)]
    pub scroll_y: f32,
    /// 自漂移速度（`speedx` / `speedy`，默认 0）。
    #[serde(default)]
    pub speed_x: f32,
    #[serde(default)]
    pub speed_y: f32,
    /// 颜色（`color`，默认白）。
    #[serde(default)]
    pub color: [u8; 4],
    /// 是否 X / Y 方向循环（`loopx` / `loopy`，原版默认**都是 true**）。
    #[serde(default)]
    pub loop_x: bool,
    #[serde(default)]
    pub loop_y: bool,
    /// 是否翻转（`flipx` / `flipy`）。
    #[serde(default)]
    pub flip_x: bool,
    #[serde(default)]
    pub flip_y: bool,
    /// 风力倍率（`wind`）。
    #[serde(default)]
    pub wind: f32,
    /// 混合模式：`"alphablend"`（默认）或 `"additive"`（`112658-112673`）。
    #[serde(default)]
    pub blend: String,
}

impl Backdrop {
    /// 原版默认色：白（`112793`）。
    pub const WHITE: [u8; 4] = [255, 255, 255, 255];

    /// 新建一条（颜色留空表示"用原版默认白"）。
    pub fn new(kind: impl Into<String>) -> Self {
        Backdrop {
            kind: kind.into(),
            texture: String::new(),
            atlas: "game".to_string(),
            x: 0.0,
            y: 0.0,
            scroll_x: 1.0,
            scroll_y: 1.0,
            speed_x: 0.0,
            speed_y: 0.0,
            color: [0, 0, 0, 0],
            loop_x: true,
            loop_y: true,
            flip_x: false,
            flip_y: false,
            wind: 0.0,
            blend: "alphablend".to_string(),
        }
    }

    /// 有效颜色：全零（"未设置"）视为原版默认白。
    pub fn effective_color(&self) -> [u8; 4] {
        if self.color == [0, 0, 0, 0] {
            Self::WHITE
        } else {
            self.color
        }
    }
}

/// 单张贴花（原版 `DecalData`，`pureCeleste.cs:113199-113206`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decal {
    /// 纹理名（相对 `decals/`，可能带 `.png` 后缀，原版会剥掉）。
    pub texture: String,
    pub x: f32,
    pub y: f32,
    /// 缩放；`scale_x == -1` 表示水平镜像（原版直接喂给 `SpriteBatch.Draw` 的 `scale`）。
    pub scale_x: f32,
    pub scale_y: f32,
}

/// 单个房间。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// 背景贴花（原版 `bgdecals`，Depth 9000，`115083-115086`）。
    #[serde(default)]
    pub bg_decals: Vec<Decal>,
    /// 前景贴花（原版 `fgdecals`，Depth -10500，`115079-115082`）。
    #[serde(default)]
    pub fg_decals: Vec<Decal>,
    /// `bgtiles` 装饰层（`Scenery` 图集，`77244-77248`）。
    #[serde(default)]
    pub bg_scenery: Tileset,
    /// `fgtiles` 装饰层（同上，`77249-77254`）。
    #[serde(default)]
    pub fg_scenery: Tileset,
}

impl Room {
    /// 房间左上角的世界坐标。
    pub fn origin(&self) -> Vec2 {
        Vec2::from_f32s(self.bounds.x, self.bounds.y)
    }
}

/// 矩形定义（f32，地图坐标）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
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
///
/// `0` 是"空"这个约定是 Releste 自己的（AGENTS.md §4.2：映射层不认识
/// 美术），转换器负责把原版的 `-1`（"不覆盖"）压成 `0`。
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
    ///
    /// `u16` 而不是 `u8`：装饰层的下标数的是整张 `tilesets/scenery`
    /// （`Tileset.this[int]` 的扁平下标，`pureCeleste.cs:15215-15226`），
    /// 原版那张图有 360 格（30×12），`5-MirrorTemple` 里就有到 352 的用法。
    /// 阻碍层（`solids`/`bg`）走的是字母表 0-9a-z，最大 35，装得下。
    #[serde(default)]
    pub tiles: Vec<u16>,
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
    pub fn get(&self, x: u32, y: u32) -> Option<u16> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.tiles
            .get((y as usize) * (self.width as usize) + (x as usize))
            .copied()
    }

    /// 设置 tile。
    pub fn set(&mut self, x: u32, y: u32, value: u16) {
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
    pub properties: BTreeMap<String, String>,
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
///
/// `BTreeMap` 而不是 `HashMap`：见本模块头部关于字节确定性的说明。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Props {
    pub fields: BTreeMap<String, FieldValue>,
}

impl Props {
    /// 从实体数据取属性。
    pub fn from_entity(entity: &EntityData) -> Self {
        Props {
            fields: entity.properties.clone(),
        }
    }
}
