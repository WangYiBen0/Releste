//! 实体与组件数据。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::factory::EntityKindId;
use crate::id::EntityId;
use crate::schema::FieldValue;

/// 运行时实体。
///
/// 由 `EntityFactory::build` 构造，存储在 `World` 中。
#[derive(Debug, Clone)]
pub struct Entity {
    pub id: EntityId,
    pub kind_id: EntityKindId,
    pub properties: BTreeMap<String, FieldValue>,
}

impl Entity {
    pub fn new(id: EntityId, kind_id: EntityKindId) -> Self {
        Entity {
            id,
            kind_id,
            properties: BTreeMap::new(),
        }
    }

    /// 获取属性。
    pub fn get(&self, key: &str) -> Option<&FieldValue> {
        self.properties.get(key)
    }

    /// 设置属性。
    pub fn set(&mut self, key: impl Into<String>, value: FieldValue) {
        self.properties.insert(key.into(), value);
    }
}

/// 地图中的实体数据（在构建为 [`Entity`] 之前）。
///
/// 此结构可从 `.map` 文件反序列化。
///
/// **字段顺序就是磁盘布局**（bincode 不自描述，ROADMAP 附录 E.6）：
/// 新字段只能追加到末尾，`legacy` 模块里为每个历史版本留冻结副本。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityData {
    pub id: EntityId,
    pub kind: String,
    pub x: f32,
    pub y: f32,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub origin_x: f32,
    pub origin_y: f32,
    /// 属性表用 `BTreeMap`：`HashMap` 的遍历顺序随进程随机，
    /// 会让同一张地图序列化出不同字节（ROADMAP E.8 / E.9 #3）。
    pub properties: BTreeMap<String, FieldValue>,
    /// 实体节点（v2 追加，`node` 子元素）。
    ///
    /// 原版 `EntityData.Nodes`（`pureCeleste.cs:113135-113198`），最常用的
    /// 三个消费者：`IntroCrusher` 的落点（`106324-106327`）、`Wire` 的折线
    /// （`106674-106704`）、`Bridge` 的桥墩（`107679`）。
    ///
    /// 坐标与 `x`/`y` **同一坐标系**（房间局部、可越出房间边界）：原版
    /// 消费者写的是 `data.Nodes[0] + offset`（offset = 房间偏移，只加
    /// 一次），与 `data.Position + offset` 完全对称 —— 节点是绝对房间
    /// 坐标，**不是**相对实体位置的偏移。
    #[serde(default)]
    pub nodes: Vec<(f32, f32)>,
}

impl EntityData {
    pub fn new(kind: impl Into<String>, id: EntityId, x: f32, y: f32) -> Self {
        EntityData {
            id,
            kind: kind.into(),
            x,
            y,
            width: None,
            height: None,
            origin_x: 0.0,
            origin_y: 0.0,
            properties: BTreeMap::new(),
            nodes: Vec::new(),
        }
    }
}
