//! ROADMAP Appendix A 的日志格式：7 列右对齐的级别标签 + 灰色 `module>`。
//!
//! 布局、配色与 `NO_COLOR` 规则全部以 ROADMAP Appendix A 为准；
//! 这里只负责把一条 `tracing` 事件画成那一行。

use std::fmt;
use std::io::IsTerminal;

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent};
use tracing_subscriber::registry::LookupSpan;

/// TRACE — 紫色。
const C_TRACE: &str = "\x1b[35m";
/// DEBUG — 蓝色。
const C_DEBUG: &str = "\x1b[34m";
/// INFO — 绿色。
const C_INFO: &str = "\x1b[32m";
/// WARN — 黄色。
const C_WARN: &str = "\x1b[33m";
/// ERROR — 红色。
const C_ERROR: &str = "\x1b[31m";
/// FATAL — 亮红（仅 panic hook 与 `main` 顶层用）。
const C_FATAL: &str = "\x1b[91m";
/// `module>` 恒为亮灰（ANSI bright-white）。`\e[90m`（bright black）在
/// 不少终端里暗得看不清，改用 97。
const C_MODULE: &str = "\x1b[97m";
/// 复位。
const RESET: &str = "\x1b[0m";

/// ANSI 是否启用。
///
/// 三个条件全满足才上色：没有 `--no-color`、没有 `NO_COLOR`、stdout 是终端。
pub fn ansi_enabled(force_off: bool) -> bool {
    if force_off {
        return false;
    }
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

/// 级别 → (标签, 颜色)。
///
/// 用比较而不是 `match`：`tracing::Level` 不是本 crate 定义的枚举，
/// 避免穷举性随上游版本变化。
fn level_style(level: Level) -> (&'static str, &'static str) {
    if level == Level::TRACE {
        ("[TRACE]", C_TRACE)
    } else if level == Level::DEBUG {
        ("[DEBUG]", C_DEBUG)
    } else if level == Level::INFO {
        ("[INFO]", C_INFO)
    } else if level == Level::WARN {
        ("[WARN]", C_WARN)
    } else {
        ("[ERROR]", C_ERROR)
    }
}

/// 把一条消息画成 Appendix A 的多行输出（末尾含 `\n`）。
///
/// 对齐锚点是标签的 `]`：所有标签都右对齐进 7 列，`]` 永远落在索引 6；
/// 消息起点是 `10 + len(module)`。
pub fn line(tag: &str, colour: &str, module: &str, message: &str, ansi: bool) -> String {
    let padded = format!("{tag:>7}");
    let indent = 10 + module.len();

    let mut out = String::with_capacity(message.len() + 32);
    if ansi {
        out.push_str(colour);
        out.push_str(&padded);
        out.push_str(RESET);
    } else {
        out.push_str(&padded);
    }
    out.push(' ');
    if ansi {
        out.push_str(C_MODULE);
        out.push_str(module);
        out.push('>');
        out.push_str(RESET);
    } else {
        out.push_str(module);
        out.push('>');
    }
    out.push(' ');

    let mut lines = message.split('\n');
    if let Some(first) = lines.next() {
        push_coloured(&mut out, first, colour, ansi);
    }
    for rest in lines {
        out.push('\n');
        for _ in 0..indent {
            out.push(' ');
        }
        push_coloured(&mut out, rest, colour, ansi);
    }
    out.push('\n');
    out
}

fn push_coloured(out: &mut String, text: &str, colour: &str, ansi: bool) {
    if text.is_empty() {
        return;
    }
    if ansi {
        out.push_str(colour);
        out.push_str(text);
        out.push_str(RESET);
    } else {
        out.push_str(text);
    }
}

/// 从 `main` / panic hook 打印一条 `[FATAL]`（tracing 没有 FATAL 级别）。
pub fn print_fatal(module: &str, message: &str) {
    let s = line("[FATAL]", C_FATAL, module, message, ansi_enabled(false));
    eprint!("{s}");
}

/// 把一条事件的字段拼成 `message` 文本。
///
/// `message` 原样输出，其它字段追加成 `key=value`，保持日志可 grep。
///
/// `log` 桥接（wgpu 等用 `log` 而非 `tracing` 的库）会附带
/// `log.target` / `log.module_path` / `log.file` / `log.line`：
/// 真实 target 拿来当 `module>`，剩下三个对人是纯噪声，直接丢掉。
#[derive(Default)]
struct Message {
    text: String,
    /// `log` 桥接事件的真实 target（`log.target` 字段）。
    log_target: Option<String>,
}

impl Message {
    fn push(&mut self, name: &str, value: &str) {
        if name == "log.target" {
            // 走 `record_debug` 时 `&str` 会带引号，去掉。
            self.log_target = Some(value.trim_matches('"').to_string());
            return;
        }
        if name.starts_with("log.") {
            return; // `log.module_path` / `log.file` / `log.line`
        }
        if !self.text.is_empty() {
            self.text.push(' ');
        }
        if name == "message" {
            self.text.push_str(value);
        } else {
            self.text.push_str(name);
            self.text.push('=');
            self.text.push_str(value);
        }
    }
}

impl tracing::field::Visit for Message {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.push(field.name(), value);
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        // `message` 的 Debug 实现转发到 Display，所以 `format_args!` 不会带引号。
        self.push(field.name(), &format!("{value:?}"));
    }
}

/// Appendix A 格式化器。
pub struct AppendixA {
    ansi: bool,
}

impl AppendixA {
    /// `ansi` 为 `false` 时输出同样的排版但不带转义序列。
    pub fn new(ansi: bool) -> Self {
        AppendixA { ansi }
    }
}

impl<S, N> FormatEvent<S, N> for AppendixA
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        _ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let meta = event.metadata();
        let (tag, colour) = level_style(*meta.level());

        let mut msg = Message::default();
        event.record(&mut msg);

        // `log` 桥接事件的 metadata target 固定是 `log`，真实位置在
        // `log.target` 里；换回来才能看到 `wgpu_hal::vulkan::instance>`。
        let target = if meta.target() == "log" {
            msg.log_target.as_deref().unwrap_or(meta.target())
        } else {
            meta.target()
        };

        writer.write_str(&line(tag, colour, target, &msg.text, self.ansi))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracket_lands_on_column_six() {
        for tag in [
            "[TRACE]", "[DEBUG]", "[INFO]", "[WARN]", "[ERROR]", "[FATAL]",
        ] {
            let s = line(tag, "", "m", "x", false);
            let idx = s.find(']').expect("tag has a closing bracket");
            assert_eq!(idx, 6, "tag {tag} put `]` at {idx}");
        }
    }

    #[test]
    fn message_starts_at_ten_plus_module_len() {
        // `[WARN]` 是 6 列，右对齐进 7 列字段 → 前面补 1 个空格（Appendix A）。
        let s = line("[WARN]", "", "system", "message", false);
        assert_eq!(s, " [WARN] system> message\n");
        assert_eq!(s.find("message").unwrap(), 10 + "system".len());
    }

    #[test]
    fn continuation_indents_to_message_column() {
        let s = line("[WARN]", "", "system", "first line\nsecond line", false);
        let expected = " [WARN] system> first line\n                second line\n";
        assert_eq!(s, expected);
        assert_eq!(s.lines().nth(1).unwrap().len(), 16 + "second line".len());
    }

    #[test]
    fn padding_survives_without_ansi() {
        let s = line("[INFO]", "", "m", "x", false);
        assert_eq!(s, " [INFO] m> x\n");
    }

    #[test]
    fn ansi_wraps_tag_module_and_message() {
        let s = line("[WARN]", C_WARN, "system", "boom", true);
        assert!(s.contains(&format!("{C_WARN} [WARN]{RESET}")), "{s:?}");
        assert!(s.contains(&format!("{C_MODULE}system>{RESET}")), "{s:?}");
        assert!(s.contains(&format!("{C_WARN}boom{RESET}")), "{s:?}");
    }
}
