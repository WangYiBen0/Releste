//! The ROADMAP Appendix A log format: a 7-column right-aligned level tag + a gray `module>`.
//!
//! Layout, colors and the `NO_COLOR` rules all follow ROADMAP Appendix A;
//! this module only draws one `tracing` event as that line.

use std::fmt;
use std::io::IsTerminal;

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::{FmtContext, FormatEvent};
use tracing_subscriber::registry::LookupSpan;

/// TRACE -- purple.
const C_TRACE: &str = "\x1b[35m";
/// DEBUG -- blue.
const C_DEBUG: &str = "\x1b[34m";
/// INFO -- green.
const C_INFO: &str = "\x1b[32m";
/// WARN -- yellow.
const C_WARN: &str = "\x1b[33m";
/// ERROR -- red.
const C_ERROR: &str = "\x1b[31m";
/// FATAL -- bright red (used only by the panic hook and the top level of `main`).
const C_FATAL: &str = "\x1b[91m";
/// `module>` is always bright gray (ANSI bright-white). `\e[90m` (bright black) is
/// so dim in many terminals that it is unreadable, so 97 is used instead.
const C_MODULE: &str = "\x1b[97m";
/// Reset.
const RESET: &str = "\x1b[0m";

/// Whether ANSI is enabled.
///
/// Color is applied only when all three hold: no `--no-color`, no `NO_COLOR`, and stdout is a terminal.
pub fn ansi_enabled(force_off: bool) -> bool {
    if force_off {
        return false;
    }
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

/// Level -> (tag, color).
///
/// Uses comparisons instead of a `match`: `tracing::Level` is not an enum defined by this crate,
/// avoiding exhaustiveness changes with upstream versions.
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

/// Draw a message as Appendix A's multi-line output (with a trailing `\n`).
///
/// The alignment anchor is the tag's `]`: every tag is right-aligned into 7 columns, so `]` always lands at index 6;
/// the message starts at `10 + len(module)`.
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

/// Print one `[FATAL]` from `main` / the panic hook (tracing has no FATAL level).
pub fn print_fatal(module: &str, message: &str) {
    let s = line("[FATAL]", C_FATAL, module, message, ansi_enabled(false));
    eprint!("{s}");
}

/// Assemble an event's fields into `message` text.
///
/// `message` is output as-is and the other fields are appended as `key=value`, keeping the log greppable.
///
/// The `log` bridge (libraries such as wgpu that use `log` instead of `tracing`) carries
/// `log.target` / `log.module_path` / `log.file` / `log.line`:
/// the real target is used as `module>` and the remaining three are pure noise to humans, so they are dropped.
#[derive(Default)]
struct Message {
    text: String,
    /// The real target of a `log`-bridge event (the `log.target` field).
    log_target: Option<String>,
}

impl Message {
    fn push(&mut self, name: &str, value: &str) {
        if name == "log.target" {
            // Through `record_debug` a `&str` carries quotes; strip them.
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
        // The Debug impl of `message` forwards to Display, so `format_args!` carries no quotes.
        self.push(field.name(), &format!("{value:?}"));
    }
}

/// The Appendix A formatter.
pub struct AppendixA {
    ansi: bool,
}

impl AppendixA {
    /// With `ansi` as `false` the same layout is output without escape sequences.
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

        // The metadata target of a `log`-bridge event is fixed to `log` and the real location is in
        // `log.target`; only by swapping it back can `wgpu_hal::vulkan::instance>` be seen.
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
        // `[WARN]` is 6 columns, so right-aligning it into the 7-column field -> pad 1 space in front (Appendix A).
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
