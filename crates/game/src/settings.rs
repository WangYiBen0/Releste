//! 用户设置：按键绑定（原版 `Settings.Instance` 的对应物）。
//!
//! 原版把绑定存在用户目录的设置里，`SetDefaultKeyboardControls(reset)`
//! （`pureCeleste.cs:101595-101706`）在绑定缺失时回落默认：方向键移动、
//! C 跳、X 冲刺、Z/V/LeftShift 抓攀、Enter 暂停。本引擎用 `settings.toml`
//! 持久化（栈规范禁 JSON）：文件缺段/键名不认识 → 回落原版默认并告警，
//! 永不 panic（AGENTS.md §15.8/15.9）。文件位置 `<root>/settings.toml`
//! （`Paths` 解析，运行时可写 —— 后续的改键菜单直接改它）。
//!
//! 键名 = `winit::keyboard::KeyCode` 的 `Debug` 名（如 `"KeyC"`、
//! `"ArrowLeft"`），存物理键（扫描码语义），与原版 `TranslateKeys`
//! （101708）的"按物理位置绑定"一致。

use std::path::Path;

use serde::Deserialize;
use tracing::warn;
use winit::keyboard::KeyCode;

/// 一组按键绑定：动作 → 物理键列表（一个动作可绑多键，原版
/// `Binding.Add(Keys...)` 语义，101622 的 Grab 绑了三键）。
#[derive(Debug, Clone, PartialEq)]
pub struct KeyBindings {
    pub left: Vec<KeyCode>,
    pub right: Vec<KeyCode>,
    pub up: Vec<KeyCode>,
    pub down: Vec<KeyCode>,
    pub jump: Vec<KeyCode>,
    /// 冲刺（原版默认 X，`pureCeleste.cs:101632`）。
    pub dash: Vec<KeyCode>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        // 原版默认（101602-101651）。抓攀（Z/V/LeftShift，101622）与
        // 冲刺（X，101632）在 Phase 4.1 实现后加入。
        KeyBindings {
            left: vec![KeyCode::ArrowLeft],
            right: vec![KeyCode::ArrowRight],
            up: vec![KeyCode::ArrowUp],
            down: vec![KeyCode::ArrowDown],
            jump: vec![KeyCode::KeyC],
            dash: vec![KeyCode::KeyX],
        }
    }
}

/// 设置文件的可序列化形态。
#[derive(Debug, Default, Deserialize)]
struct KeysToml {
    left: Option<Vec<String>>,
    right: Option<Vec<String>>,
    up: Option<Vec<String>>,
    down: Option<Vec<String>>,
    jump: Option<Vec<String>>,
    dash: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize)]
struct SettingsToml {
    #[serde(default)]
    keys: KeysToml,
}

impl KeyBindings {
    /// 从 `<root>/settings.toml` 读绑定；文件/段落/键名缺失或非法时
    /// 逐项回落原版默认（并 `warn`，不静默）。
    pub fn load(paths_root: &Path) -> Self {
        let file = paths_root.join("settings.toml");
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(_) => {
                // 没有设置文件不是错误 —— 用原版默认（101595 的
                // `reset: false` 分支：绑定非空则不动）。
                return KeyBindings::default();
            }
        };
        let parsed: SettingsToml = match toml::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                warn!(path = %file.display(), error = %e, "settings.toml is invalid; using vanilla defaults");
                return KeyBindings::default();
            }
        };
        let k = parsed.keys;
        let map =
            |list: &Option<Vec<String>>, fallback: Vec<KeyCode>, action: &str| -> Vec<KeyCode> {
                match list {
                    None => fallback,
                    Some(names) => {
                        let keys: Vec<KeyCode> =
                            names.iter().filter_map(|n| parse_key(n)).collect();
                        if keys.is_empty() {
                            warn!(
                                action,
                                "settings.toml binds no usable keys; using vanilla default"
                            );
                            fallback
                        } else {
                            keys
                        }
                    }
                }
            };
        KeyBindings {
            left: map(&k.left, vec![KeyCode::ArrowLeft], "left"),
            right: map(&k.right, vec![KeyCode::ArrowRight], "right"),
            up: map(&k.up, vec![KeyCode::ArrowUp], "up"),
            down: map(&k.down, vec![KeyCode::ArrowDown], "down"),
            jump: map(&k.jump, vec![KeyCode::KeyC], "jump"),
            dash: map(&k.dash, vec![KeyCode::KeyX], "dash"),
        }
    }
}

/// `"ArrowLeft"` / `"KeyC"` / `"Escape"` … → `KeyCode`。
/// 用 `KeyCode` 的 `Debug` 名做键名（它与用户在设置文件里写的一致）。
fn parse_key(name: &str) -> Option<KeyCode> {
    Some(match name {
        "ArrowLeft" => KeyCode::ArrowLeft,
        "ArrowRight" => KeyCode::ArrowRight,
        "ArrowUp" => KeyCode::ArrowUp,
        "ArrowDown" => KeyCode::ArrowDown,
        "KeyA" => KeyCode::KeyA,
        "KeyC" => KeyCode::KeyC,
        "KeyD" => KeyCode::KeyD,
        "KeyS" => KeyCode::KeyS,
        "KeyV" => KeyCode::KeyV,
        "KeyW" => KeyCode::KeyW,
        "KeyX" => KeyCode::KeyX,
        "KeyZ" => KeyCode::KeyZ,
        "Space" => KeyCode::Space,
        "Enter" => KeyCode::Enter,
        "Escape" => KeyCode::Escape,
        "Tab" => KeyCode::Tab,
        "Backspace" => KeyCode::Backspace,
        "ShiftLeft" => KeyCode::ShiftLeft,
        "ShiftRight" => KeyCode::ShiftRight,
        _ => {
            warn!(key = name, "unknown key name in settings.toml; ignoring it");
            return None;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 缺文件 → 原版默认（方向键 + C）。
    #[test]
    fn missing_settings_file_falls_back_to_vanilla_defaults() {
        let root = std::env::temp_dir().join("releste-settings-missing");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let b = KeyBindings::load(&root);
        assert_eq!(b, KeyBindings::default());
    }

    /// 合法覆盖生效；坏键名告警并忽略；空列表回落默认。
    #[test]
    fn settings_file_overrides_and_falls_back_per_action() {
        let root = std::env::temp_dir().join("releste-settings-load");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("settings.toml"),
            "[keys]\njump = [\"Space\", \"KeyC\"]\nleft = [\"KeyA\"]\nright = [\"Nope\"]\n",
        )
        .unwrap();
        let b = KeyBindings::load(&root);
        assert_eq!(b.jump, vec![KeyCode::Space, KeyCode::KeyC]);
        assert_eq!(b.left, vec![KeyCode::KeyA]);
        assert_eq!(
            b.right,
            vec![KeyCode::ArrowRight],
            "bad key -> vanilla default"
        );
        assert_eq!(b.up, vec![KeyCode::ArrowUp], "missing -> vanilla default");
    }
}
