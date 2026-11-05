//! Schema: property types and values.
//!
//! Used by the editor to auto-generate panels and to serialize entity
//! properties.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Field type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FieldType {
    /// Fixed-point number (stored as Fx, the editor interacts with it as f32).
    Float,
    /// Integer.
    Int,
    /// String.
    String,
    /// Boolean.
    Bool,
    /// Enum (a finite set of strings).
    Enum(Vec<Cow<'static, str>>),
    /// Color (RGBA, shown as a color picker in the editor).
    Color,
    /// Entity reference (EntityId).
    EntityRef,
    /// Vector (Vec2).
    Vec2,
}

/// Field value.
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

/// Schema definition for an entity.
///
/// Describes which editable properties an entity has and their types.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    pub fields: Vec<SchemaField>,
}

/// A single Schema field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SchemaField {
    /// Property name (such as `"collidable"`).
    pub name: Cow<'static, str>,
    /// Property type.
    pub field_type: FieldType,
    /// Default value.
    pub default: FieldValue,
    /// Editor tooltip.
    pub tooltip: Option<Cow<'static, str>>,
}

impl Schema {
    pub fn new(fields: Vec<SchemaField>) -> Self {
        Schema { fields }
    }

    /// Find a field.
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
