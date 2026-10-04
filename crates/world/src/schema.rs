//! Schema：属性类型与值。
//!
//! 用于编辑器自动生成面板以及实体属性序列化。

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// 字段类型。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FieldType {
    /// 定点数（内部 Fx，编辑器用 f32 交互）。
    Float,
    /// 整数。
    Int,
    /// 字符串。
    String,
    /// 布尔。
    Bool,
    /// 枚举（有限字符串集合）。
    Enum(Vec<Cow<'static, str>>),
    /// 颜色（RGBA，编辑器中展示拾色器）。
    Color,
    /// 实体引用（EntityId）。
    EntityRef,
    /// 向量（Vec2）。
    Vec2,
}

/// 字段值。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FieldValue {
    Float(f32),
    Int(i32),
    String(String),
    Bool(bool),
    Enum(usize),
    Color([u8; 4]),
    EntityRef(u64),
    Vec2 { x: f32, y: f32 },
}

/// 实体的 Schema 定义。
///
/// 描述实体有哪些可编辑属性及类型。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    pub fields: Vec<SchemaField>,
}

/// 单个 Schema 字段。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaField {
    /// 属性名（如 `"collidable"`）。
    pub name: Cow<'static, str>,
    /// 属性类型。
    pub field_type: FieldType,
    /// 默认值。
    pub default: FieldValue,
    /// 编辑器提示。
    pub tooltip: Option<Cow<'static, str>>,
}

impl Schema {
    pub fn new(fields: Vec<SchemaField>) -> Self {
        Schema { fields }
    }

    /// 查找字段。
    pub fn find(&self, name: &str) -> Option<&SchemaField> {
        self.fields.iter().find(|f| f.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_lookup() {
        let s = Schema::new(vec![SchemaField {
            name: Cow::Borrowed("width"),
            field_type: FieldType::Int,
            default: FieldValue::Int(0),
            tooltip: None,
        }]);
        assert!(s.find("width").is_some());
        assert!(s.find("height").is_none());
    }
}
