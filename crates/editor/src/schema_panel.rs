//! Schema-driven property panel.
//!
//! AGENTS.md §2: the editor UI **auto-generates** panels from entity Schemas.
//!
//! This module compiles a [`Schema`] into a reusable form description
//! ([`FormField`] / [`WidgetKind`]) that the `ui` module then renders.
//! Compilation and value extraction are pure and testable — which also helps
//! AGENTS.md §13's "hundreds-of-entities generation" (form descriptions are cacheable).

use std::collections::HashMap;

use reles_world::{FieldType, FieldValue, Schema, SchemaField};

/// Editor widget kind.
///
/// Contains `f32` fields, so it can only implement `PartialEq` (not `Eq`).
#[derive(Debug, Clone, PartialEq)]
pub enum WidgetKind {
    /// Integer input (drag + text).
    IntInput { min: i32, max: i32 },
    /// Float input.
    FloatInput { min: f32, max: f32, speed: f32 },
    /// Text input.
    TextInput,
    /// Checkbox.
    Checkbox,
    /// Combo box (index + option labels).
    Combo { options: Vec<String> },
    /// Color picker.
    ColorPicker,
    /// Entity reference picker.
    EntityRefPicker,
    /// 2D vector input.
    Vec2Input,
}

/// A compiled form field.
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    /// Property name.
    pub name: String,
    /// Display label.
    pub label: String,
    /// Widget kind.
    pub widget: WidgetKind,
    /// Hover tooltip.
    pub tooltip: Option<String>,
    /// Default value (used by "reset").
    pub default: FieldValue,
    /// Current value.
    pub value: FieldValue,
}

impl FormField {
    /// Whether the current value differs from the default.
    pub fn is_modified(&self) -> bool {
        self.value != self.default
    }

    /// Resets to the default value.
    pub fn reset(&mut self) {
        self.value = self.default.clone();
    }
}

/// A Schema-driven panel.
///
/// One per entity kind; cache and reuse (avoids per-frame recompiles with many entities).
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaPanel {
    /// Entity kind name (used for the title only).
    pub kind: String,
    /// Form fields.
    pub fields: Vec<FormField>,
}

/// Generates a human-readable label from a property name.
///
/// Splits words at `_` and camel-case boundaries and capitalises each word:
/// `cameraOffsetX` → `Camera Offset X`, `already_snake` → `Already Snake`.
fn humanize(name: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;

    for ch in name.chars() {
        if ch == '_' || ch == ' ' {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        // 驼峰边界：小写/数字后紧跟大写 → 切词。
        if ch.is_uppercase() && prev_lower && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        cur.push(ch);
        prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
    }
    if !cur.is_empty() {
        words.push(cur);
    }

    words
        .iter()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl SchemaPanel {
    /// 由 [`Schema`] 编译。
    pub fn compile(kind: impl Into<String>, schema: &Schema) -> Self {
        SchemaPanel {
            kind: kind.into(),
            fields: schema.fields.iter().map(compile_field).collect(),
        }
    }

    /// 由 [`Schema`] 编译，并用实体当前属性覆盖默认值。
    pub fn compile_with_values(
        kind: impl Into<String>,
        schema: &Schema,
        values: &HashMap<String, FieldValue>,
    ) -> Self {
        let mut panel = Self::compile(kind, schema);
        for field in &mut panel.fields {
            if let Some(v) = values.get(&field.name) {
                field.value = v.clone();
            }
        }
        panel
    }

    /// 按名取字段。
    pub fn field(&self, name: &str) -> Option<&FormField> {
        self.fields.iter().find(|f| f.name == name)
    }

    /// 按名取字段（可变）。
    pub fn field_mut(&mut self, name: &str) -> Option<&mut FormField> {
        self.fields.iter_mut().find(|f| f.name == name)
    }

    /// 是否有字段被改动。
    pub fn is_dirty(&self) -> bool {
        self.fields.iter().any(FormField::is_modified)
    }

    /// 全部重置。
    pub fn reset_all(&mut self) {
        for f in &mut self.fields {
            f.reset();
        }
    }

    /// 导出为属性表（仅包含 **当前值**）。
    pub fn to_properties(&self) -> HashMap<String, FieldValue> {
        self.fields
            .iter()
            .map(|f| (f.name.clone(), f.value.clone()))
            .collect()
    }

    /// 导出为属性表，但只包含与默认值不同的字段。
    ///
    /// 用于写回地图时保持 `.map` 精简。
    pub fn to_modified_properties(&self) -> HashMap<String, FieldValue> {
        self.fields
            .iter()
            .filter(|f| f.is_modified())
            .map(|f| (f.name.clone(), f.value.clone()))
            .collect()
    }
}

/// 把单个 schema 字段编译为表单字段。
fn compile_field(field: &SchemaField) -> FormField {
    let widget = match &field.field_type {
        FieldType::Int => WidgetKind::IntInput {
            min: i32::MIN,
            max: i32::MAX,
        },
        FieldType::Float => WidgetKind::FloatInput {
            min: f32::MIN,
            max: f32::MAX,
            speed: 0.1,
        },
        FieldType::String => WidgetKind::TextInput,
        FieldType::Bool => WidgetKind::Checkbox,
        FieldType::Enum(options) => WidgetKind::Combo {
            options: options.iter().map(|o| o.to_string()).collect(),
        },
        FieldType::Color => WidgetKind::ColorPicker,
        FieldType::EntityRef => WidgetKind::EntityRefPicker,
        FieldType::Vec2 => WidgetKind::Vec2Input,
    };

    FormField {
        name: field.name.to_string(),
        label: humanize(&field.name),
        widget,
        tooltip: field.tooltip.as_ref().map(|t| t.to_string()),
        default: field.default.clone(),
        value: field.default.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    fn schema() -> Schema {
        Schema::new(vec![
            SchemaField {
                name: Cow::Borrowed("width"),
                field_type: FieldType::Int,
                default: FieldValue::Int(8),
                tooltip: Some(Cow::Borrowed("tile 宽度")),
            },
            SchemaField {
                name: Cow::Borrowed("cameraOffsetX"),
                field_type: FieldType::Float,
                default: FieldValue::Float(0.0),
                tooltip: None,
            },
            SchemaField {
                name: Cow::Borrowed("dark"),
                field_type: FieldType::Bool,
                default: FieldValue::Bool(false),
                tooltip: None,
            },
            SchemaField {
                name: Cow::Borrowed("windPattern"),
                field_type: FieldType::Enum(vec![
                    Cow::Borrowed("None"),
                    Cow::Borrowed("Left"),
                    Cow::Borrowed("Right"),
                ]),
                default: FieldValue::Enum(0),
                tooltip: None,
            },
        ])
    }

    #[test]
    fn compiles_all_field_types() {
        let panel = SchemaPanel::compile("level", &schema());
        assert_eq!(panel.fields.len(), 4);
        assert!(matches!(
            panel.field("width").unwrap().widget,
            WidgetKind::IntInput { .. }
        ));
        assert!(matches!(
            panel.field("cameraOffsetX").unwrap().widget,
            WidgetKind::FloatInput { .. }
        ));
        assert!(matches!(
            panel.field("dark").unwrap().widget,
            WidgetKind::Checkbox
        ));
        match &panel.field("windPattern").unwrap().widget {
            WidgetKind::Combo { options } => assert_eq!(options.len(), 3),
            other => panic!("expected combo, got {other:?}"),
        }
    }

    #[test]
    fn labels_are_humanized() {
        let panel = SchemaPanel::compile("level", &schema());
        assert_eq!(panel.field("width").unwrap().label, "Width");
        assert_eq!(
            panel.field("cameraOffsetX").unwrap().label,
            "Camera Offset X"
        );
        assert_eq!(panel.field("windPattern").unwrap().label, "Wind Pattern");
    }

    #[test]
    fn tooltip_is_carried_over() {
        let panel = SchemaPanel::compile("level", &schema());
        assert_eq!(
            panel.field("width").unwrap().tooltip.as_deref(),
            Some("tile 宽度")
        );
    }

    #[test]
    fn fresh_panel_is_not_dirty() {
        let panel = SchemaPanel::compile("level", &schema());
        assert!(!panel.is_dirty());
        assert!(panel.to_modified_properties().is_empty());
    }

    #[test]
    fn editing_marks_dirty_and_exports_only_changes() {
        let mut panel = SchemaPanel::compile("level", &schema());
        panel.field_mut("width").unwrap().value = FieldValue::Int(16);
        assert!(panel.is_dirty());

        let modified = panel.to_modified_properties();
        assert_eq!(modified.len(), 1);
        assert_eq!(modified.get("width"), Some(&FieldValue::Int(16)));

        // 全量导出包含所有字段
        assert_eq!(panel.to_properties().len(), 4);
    }

    #[test]
    fn values_override_defaults_when_compiling() {
        let mut values = HashMap::new();
        values.insert("dark".to_string(), FieldValue::Bool(true));
        values.insert("width".to_string(), FieldValue::Int(24));

        let panel = SchemaPanel::compile_with_values("level", &schema(), &values);
        assert_eq!(panel.field("dark").unwrap().value, FieldValue::Bool(true));
        assert_eq!(panel.field("width").unwrap().value, FieldValue::Int(24));
        assert_eq!(panel.field("width").unwrap().default, FieldValue::Int(8));
        assert!(panel.is_dirty());
    }

    #[test]
    fn reset_restores_defaults() {
        let mut panel = SchemaPanel::compile("level", &schema());
        panel.field_mut("width").unwrap().value = FieldValue::Int(99);
        panel.reset_all();
        assert!(!panel.is_dirty());
        assert_eq!(panel.field("width").unwrap().value, FieldValue::Int(8));
    }

    #[test]
    fn humanize_handles_edge_cases() {
        assert_eq!(humanize("x"), "X");
        assert_eq!(humanize(""), "");
        assert_eq!(humanize("already_snake"), "Already Snake");
        assert_eq!(humanize("id"), "Id");
        assert_eq!(humanize("musicLayer1"), "Music Layer1");
    }
}
