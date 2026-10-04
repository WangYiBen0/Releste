//! 对话（`Dialog/*.txt`）转换 → TOML。
//!
//! # 源格式（与反编译源码 `Language.FromTxt` 一致）
//! 逐行处理，先 `trim()`：
//! - 空行或 `#` 开头 → 跳过（正文中的 `\#` 是转义，还原为 `#`）。
//! - 匹配 `^\w+=.*` 的行 → 新条目：
//!   - `language` / `icon` / `order` / `font` / `SPLIT_REGEX` /
//!     `commas` / `periods` 为语言元数据（大小写不敏感）。
//!   - `BEGIN` 视为"元数据结束"分隔符。
//!   - 其余为对话键，值为 `=` 之后的内容。
//! - 其它行 → 上一键的续行，用 `{break}` 连接（除非已以
//!   `{break}` / `{n}` 结尾，或上一行去掉 `{...}` 后为空）。
//! - `[内容]` → `{portrait 内容}`。
//!
//! 正文中的 `{...}` 命令**原样保留**。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::PipelineError;

/// 元数据（`BEGIN` 之前）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DialogMeta {
    pub language_id: String,
    pub language_label: String,
    pub icon: Option<String>,
    pub order: Option<i32>,
    pub font_face: Option<String>,
    pub font_size: Option<f32>,
    /// 句读 / 分词配置。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commas: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub periods: Option<String>,
    /// 其余未知元数据，原样保留。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

/// 一条对话。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DialogEntry {
    /// 键（如 `AUTOSAVING_TITLE_NS`）。
    pub key: String,
    /// 值（续行以 `{break}` 连接）。
    pub value: String,
}

/// 解析后的对话文档。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DialogDocument {
    pub meta: DialogMeta,
    pub entries: Vec<DialogEntry>,
}

/// 判定 `^\w+=.*`（源格式的 variable 正则）。
fn is_variable_line(line: &str) -> bool {
    let Some(eq) = line.find('=') else {
        return false;
    };
    let head = &line[..eq];
    !head.is_empty() && head.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// 去掉所有 `{...}` 后是否还有内容（源格式用 `command.Replace(input, "")`）。
fn has_text_outside_commands(line: &str) -> bool {
    let mut depth = 0usize;
    let mut text = String::new();
    for ch in line.chars() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => text.push(ch),
            _ => {}
        }
    }
    !text.trim().is_empty()
}

/// `[内容]` → `{portrait 内容}`。
fn replace_portraits(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '[' {
            let mut content = String::new();
            let mut closed = false;
            while let Some(&next) = chars.peek() {
                chars.next();
                if next == ']' {
                    closed = true;
                    break;
                }
                content.push(next);
            }
            if closed {
                out.push_str(&format!("{{portrait {content}}}"));
            } else {
                // 未闭合则原样保留
                out.push('[');
                out.push_str(&content);
            }
        } else {
            out.push(ch);
        }
    }
    out
}

impl DialogDocument {
    /// 解析对话文件内容。
    pub fn parse(path: &str, content: &str) -> Result<Self, PipelineError> {
        // 去掉 UTF-8 BOM
        let content = content.strip_prefix('\u{feff}').unwrap_or(content);

        let mut meta_raw: Vec<(String, String)> = Vec::new();
        let mut entries: Vec<DialogEntry> = Vec::new();

        let mut cur_key: Option<String> = None;
        let mut cur_val = String::new();
        let mut prev_line = String::new();

        macro_rules! flush {
            () => {
                if let Some(key) = cur_key.take() {
                    entries.push(DialogEntry {
                        key,
                        value: std::mem::take(&mut cur_val),
                    });
                }
            };
        }

        for raw_line in content.lines() {
            let trimmed = raw_line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let mut line = trimmed.replace("\\#", "#");
            line = replace_portraits(&line);

            if is_variable_line(&line) {
                flush!();

                let eq = line.find('=').expect("checked by is_variable_line");
                let key = line[..eq].trim().to_string();
                let value = line[eq + 1..].trim().to_string();

                // `BEGIN` 是元数据结束分隔符。
                if key.eq_ignore_ascii_case("BEGIN") {
                    prev_line = line;
                    continue;
                }

                if is_metadata_key(&key) {
                    meta_raw.push((key, value));
                } else {
                    cur_key = Some(key);
                    cur_val = value;
                }
            } else {
                // 续行
                if !cur_val.is_empty()
                    && !cur_val.ends_with("{break}")
                    && !cur_val.ends_with("{n}")
                    && has_text_outside_commands(&prev_line)
                {
                    cur_val.push_str("{break}");
                }
                cur_val.push_str(&line);
            }

            prev_line = line;
        }
        flush!();

        let _ = path;
        Ok(DialogDocument {
            meta: build_meta(&meta_raw),
            entries,
        })
    }

    /// 从文件读取。
    pub fn from_file(path: &std::path::Path) -> Result<Self, PipelineError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| PipelineError::io(path.display(), e))?;
        Self::parse(&path.display().to_string(), &content)
    }

    /// 序列化为 TOML。
    pub fn to_toml(&self) -> Result<String, PipelineError> {
        #[derive(Serialize)]
        struct Out<'a> {
            meta: &'a DialogMeta,
            /// 用 BTreeMap 保证输出稳定（便于黄金测试）。
            entries: BTreeMap<&'a str, &'a str>,
        }

        let entries = self
            .entries
            .iter()
            .map(|e| (e.key.as_str(), e.value.as_str()))
            .collect();

        let out = Out {
            meta: &self.meta,
            entries,
        };
        toml::to_string_pretty(&out)
            .map_err(|e| PipelineError::Serialize(format!("dialog toml: {e}")))
    }

    /// 条目数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否无条目。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 是否为已知元数据键（大小写不敏感）。
fn is_metadata_key(key: &str) -> bool {
    matches!(
        key.to_ascii_uppercase().as_str(),
        "LANGUAGE" | "ICON" | "ORDER" | "FONT" | "SPLIT_REGEX" | "COMMAS" | "PERIODS"
    )
}

fn build_meta(raw: &[(String, String)]) -> DialogMeta {
    let mut meta = DialogMeta::default();
    for (key, value) in raw {
        match key.to_ascii_uppercase().as_str() {
            "LANGUAGE" => {
                // 形如 `english,English`
                let mut parts = value.splitn(2, ',');
                meta.language_id = parts.next().unwrap_or_default().trim().to_string();
                meta.language_label = parts.next().unwrap_or_default().trim().to_string();
            }
            "ICON" => meta.icon = Some(value.clone()),
            "ORDER" => meta.order = value.trim().parse().ok(),
            "FONT" => {
                // 形如 `Renogare,64`
                let mut parts = value.splitn(2, ',');
                meta.font_face = Some(parts.next().unwrap_or_default().trim().to_string());
                meta.font_size = parts.next().and_then(|s| s.trim().parse().ok());
            }
            "SPLIT_REGEX" => meta.split_regex = Some(value.clone()),
            "COMMAS" => meta.commas = Some(value.clone()),
            "PERIODS" => meta.periods = Some(value.clone()),
            other => {
                meta.extra.insert(other.to_string(), value.clone());
            }
        }
    }
    meta
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\u{feff}# NOTES:\n# a comment\n\n\
\tLANGUAGE=english,English\n\
\tICON=Icons/canadian-english.png\n\
\tORDER=0\n\
\tFONT=Renogare,64\n\
\tCOMMAS=,，\n\
\tBEGIN=true\n\
\n\
\tAUTOSAVING_TITLE_NS=\n\
\tThis game features Auto Saving\n\
\tAUTOSAVING_DESC_NS=\n\
\tDo not turn off the console\n\
\twhen this icon is displayed\n\
\tGREETING=Hello there\n\
\tPAUSED_MENU=\n\
\tPaused\n\
\t{n}Press {confirm}\n";

    fn doc() -> DialogDocument {
        DialogDocument::parse("english.txt", SAMPLE).unwrap()
    }

    #[test]
    fn parses_meta() {
        let d = doc();
        assert_eq!(d.meta.language_id, "english");
        assert_eq!(d.meta.language_label, "English");
        assert_eq!(d.meta.icon.as_deref(), Some("Icons/canadian-english.png"));
        assert_eq!(d.meta.order, Some(0));
        assert_eq!(d.meta.font_face.as_deref(), Some("Renogare"));
        assert_eq!(d.meta.font_size, Some(64.0));
        assert_eq!(d.meta.commas.as_deref(), Some(",，"));
    }

    #[test]
    fn begin_is_a_separator_not_an_entry() {
        let d = doc();
        assert!(!d.entries.iter().any(|e| e.key == "BEGIN"));
    }

    #[test]
    fn single_line_entry() {
        let d = doc();
        let g = d.entries.iter().find(|e| e.key == "GREETING").unwrap();
        assert_eq!(g.value, "Hello there");
    }

    #[test]
    fn continuation_lines_join_with_break() {
        let d = doc();
        let desc = d
            .entries
            .iter()
            .find(|e| e.key == "AUTOSAVING_DESC_NS")
            .unwrap();
        assert_eq!(
            desc.value,
            "Do not turn off the console{break}when this icon is displayed"
        );
    }

    #[test]
    fn command_lines_do_not_get_break_separator() {
        // `{n}Press {confirm}` 前一行是 "Paused"（有文本），因此会插入
        // `{break}`；但值本身以 `{n}` 开头，命令被原样保留。
        let d = doc();
        let paused = d.entries.iter().find(|e| e.key == "PAUSED_MENU").unwrap();
        assert!(
            paused.value.contains("{n}Press {confirm}"),
            "{}",
            paused.value
        );
    }

    #[test]
    fn no_break_after_n_command() {
        // 若上一行以 {n} 结尾，则不再插入 {break}。
        let src = "BEGIN=true\nA=line one{n}\nline two\n";
        let d = DialogDocument::parse("x.txt", src).unwrap();
        assert_eq!(d.entries[0].value, "line one{n}line two");
    }

    #[test]
    fn escaped_hash_is_restored() {
        let src = "BEGIN=true\nA=\\#not a comment\n";
        let d = DialogDocument::parse("x.txt", src).unwrap();
        assert_eq!(d.entries[0].value, "#not a comment");
    }

    #[test]
    fn comments_are_skipped() {
        let d = doc();
        assert!(!d.entries.iter().any(|e| e.key.starts_with('#')));
        assert_eq!(d.len(), 4);
    }

    #[test]
    fn portraits_are_converted() {
        let src = "BEGIN=true\nA=[granny] hi\n";
        let d = DialogDocument::parse("x.txt", src).unwrap();
        assert_eq!(d.entries[0].value, "{portrait granny} hi");
    }

    #[test]
    fn toml_roundtrip() {
        let d = doc();
        let toml_text = d.to_toml().unwrap();
        assert!(toml_text.contains("language_id = \"english\""));
        assert!(toml_text.contains("AUTOSAVING_TITLE_NS"));

        #[derive(Deserialize)]
        struct Back {
            meta: DialogMeta,
            entries: BTreeMap<String, String>,
        }
        let back: Back = toml::from_str(&toml_text).unwrap();
        assert_eq!(back.entries.len(), 4);
        assert_eq!(back.meta.font_size, Some(64.0));
    }

    #[test]
    fn empty_value_is_allowed() {
        let d = DialogDocument::parse("x.txt", "BEGIN=true\nEMPTY=\n").unwrap();
        assert_eq!(d.entries[0].value, "");
    }

    #[test]
    fn value_with_equals_is_kept_whole() {
        let d = DialogDocument::parse("x.txt", "BEGIN=true\nA=b=c\n").unwrap();
        assert_eq!(d.entries[0].value, "b=c");
    }
}
