//! 实体与组件数据。

use std::collections::HashMap;

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
    pub properties: HashMap<String, FieldValue>,
}

impl Entity {
    pub fn new(id: EntityId, kind_id: EntityKindId) -> Self {
        Entity {
            id,
            kind_id,
            properties: HashMap::new(),
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
    pub properties: HashMap<String, FieldValue>,
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
            properties: HashMap::new(),
        }
    }
}
