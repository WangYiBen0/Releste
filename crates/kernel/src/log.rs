//! The ROADMAP Appendix A log format, shared by every Releste binary.
//!
//! This lives in the kernel so that `releste`, `releste-editor`,
//! `releste-net-server` and `releste-content-pipeline` cannot drift apart: a
//! user switching between tools must not see two different log dialects.
//!
//! The kernel may own this. It is presentation of the `tracing` stream, not
//! game logic, so it does not violate AGENTS.md 4.1 (the kernel must not depend
//! on business crates); it depends on `tracing`/`tracing-subscriber` only.
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
/// `module>` is dimmed (ANSI faint), matching what the editor and the other
/// binaries printed before the format was unified: `tracing_subscriber`'s own
/// default styles the target with `Style::dimmed()` (`\e[2m`), and users are
/// used to that softer grey. Bright white (`\e[97m`) was the game's choice and
/// made the scope louder than the message it introduces.
const C_MODULE: &str = "\x1b[2m";
/// Reset.
const RESET: &str = "\x1b[0m";
/// Timestamp column: dimmed so it does not compete with the level tag.
const C_DIM: &str = "\x1b[2m";

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
    line_with_time(tag, colour, module, message, ansi, None)
}

/// Like [`line`], with an optional timestamp column in front.
///
/// The timestamp is dimmed so it recedes behind the level tag, and the
/// continuation indent grows by its width so multi-line messages still align.
pub fn line_with_time(
    tag: &str,
    colour: &str,
    module: &str,
    message: &str,
    ansi: bool,
    timestamp: Option<&str>,
) -> String {
    let padded = format!("{tag:>7}");
    let time_width = timestamp.map(|t| t.len() + 1).unwrap_or(0);
    let indent = time_width + 10 + module.len();

    let mut out = String::with_capacity(message.len() + 32 + time_width);
    if let Some(t) = timestamp {
        if ansi {
            out.push_str(C_DIM);
            out.push_str(t);
            out.push_str(RESET);
        } else {
            out.push_str(t);
        }
        out.push(' ');
    }
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

/// How much of a timestamp to print before the level tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Timestamp {
    /// No timestamp (the ROADMAP Appendix A default layout).
    #[default]
    Off,
    /// Wall-clock time, HH:MM:SS.mmm.
    Time,
    /// Date and time, YYYY-MM-DD HH:MM:SS.mmm.
    DateTime,
    /// Unix epoch seconds with milliseconds.
    Unix,
}

impl Timestamp {
    /// Parses the --log-time value.
    ///
    /// Accepted: off, time, date (aliases datetime/full), unix. An unrecognised
    /// value is reported by the caller rather than silently ignored.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "false" | "0" => Some(Timestamp::Off),
            "time" | "clock" | "short" => Some(Timestamp::Time),
            "date" | "datetime" | "full" | "long" => Some(Timestamp::DateTime),
            "unix" | "epoch" | "raw" => Some(Timestamp::Unix),
            _ => None,
        }
    }

    /// Renders the timestamp for one event.
    ///
    /// Uses std::time plus a hand-rolled civil-date conversion, so no calendar
    /// crate is needed (AGENTS.md 15.1 forbids chrono/time).
    pub fn render(self) -> Option<String> {
        if self == Timestamp::Off {
            return None;
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        let secs = now.as_secs();
        let millis = now.subsec_millis();

        Some(match self {
            Timestamp::Off => return None,
            Timestamp::Unix => format!("{secs}.{millis:03}"),
            Timestamp::Time => {
                let (h, m, s) = hms(secs);
                format!("{h:02}:{m:02}:{s:02}.{millis:03}")
            }
            Timestamp::DateTime => {
                let (y, mo, d) = civil_from_days((secs / 86_400) as i64);
                let (h, m, s) = hms(secs);
                format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}.{millis:03}")
            }
        })
    }
}

/// Seconds-of-day -> (hour, minute, second), UTC.
fn hms(secs: u64) -> (u64, u64, u64) {
    let rem = secs % 86_400;
    (rem / 3_600, (rem % 3_600) / 60, rem % 60)
}

/// Days since the Unix epoch -> (year, month, day), UTC.
///
/// Howard Hinnant's civil-from-days algorithm: exact for the range we care
/// about and free of any calendar crate.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The Appendix A formatter.
pub struct AppendixA {
    ansi: bool,
    timestamp: Timestamp,
}

impl AppendixA {
    /// With ansi false the same layout is output without escape sequences.
    pub fn new(ansi: bool) -> Self {
        AppendixA {
            ansi,
            timestamp: Timestamp::Off,
        }
    }

    /// Adds a timestamp column before the level tag.
    pub fn with_timestamp(mut self, timestamp: Timestamp) -> Self {
        self.timestamp = timestamp;
        self
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

        let timestamp = self.timestamp.render();
        writer.write_str(&line_with_time(
            tag,
            colour,
            target,
            &msg.text,
            self.ansi,
            timestamp.as_deref(),
        ))
    }
}

/// Default `EnvFilter` directive for a Releste binary.
///
/// INFO for this project, WARN for everything else, so the Vulkan loader does
/// not bury the log (ROADMAP Appendix A). `EnvFilter` matches targets with
/// `starts_with`, so a bare prefix is enough and no `*` is needed.
pub const DEFAULT_FILTER: &str = "warn,releste=info,releste_=info,plugins=info";

/// Installs the shared subscriber, honouring `RUST_LOG`.
///
/// Returns the filter directive actually applied. A malformed `RUST_LOG` falls
/// back to [`DEFAULT_FILTER`] and reports the problem rather than going quiet.
pub fn init(no_color: bool) -> String {
    init_with(no_color, std::env::var("RUST_LOG").ok().as_deref())
}

/// Installs the shared subscriber with a timestamp column.
///
/// `--log-time` selects the format; an unrecognised value is reported and the
/// default (`Off`) is used, so a typo cannot silently change the layout.
pub fn init_with_timestamp(no_color: bool, timestamp: Timestamp) -> String {
    init_full(
        no_color,
        std::env::var("RUST_LOG").ok().as_deref(),
        timestamp,
    )
}

/// Installs the shared subscriber with an explicit filter directive.
pub fn init_with(no_color: bool, directive: Option<&str>) -> String {
    init_full(no_color, directive, Timestamp::Off)
}

/// Installs the shared subscriber with an explicit filter and timestamp format.
pub fn init_full(no_color: bool, directive: Option<&str>, timestamp: Timestamp) -> String {
    let requested = directive.unwrap_or(DEFAULT_FILTER).to_string();
    let filter = tracing_subscriber::EnvFilter::try_new(&requested).unwrap_or_else(|e| {
        eprintln!("warning: bad log filter ({e}); falling back to the default");
        tracing_subscriber::EnvFilter::new(DEFAULT_FILTER)
    });

    let ansi = ansi_enabled(no_color);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .event_format(AppendixA::new(ansi).with_timestamp(timestamp))
        .init();

    requested
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_shifts_the_tag_and_continuation_together() {
        let s = line_with_time("[WARN]", "", "system", "a\nb", false, Some("12:00:00.000"));
        // "12:00:00.000 " is 13 columns, so the tag now starts at 13 and
        // the message at 13 + 10 + len(module).
        assert!(s.starts_with("12:00:00.000  [WARN] system> a\n"), "{s:?}");
        let second = s.lines().nth(1).unwrap();
        let expected_indent = 13 + 10 + "system".len();
        assert_eq!(
            second.len() - "b".len(),
            expected_indent,
            "continuation must follow the shifted message column"
        );
    }

    #[test]
    fn no_timestamp_keeps_the_appendix_a_layout() {
        let plain = line("[INFO]", "", "m", "x", false);
        let explicit = line_with_time("[INFO]", "", "m", "x", false, None);
        assert_eq!(plain, explicit);
        assert_eq!(plain, " [INFO] m> x\n");
    }

    #[test]
    fn timestamp_parsing_accepts_the_documented_aliases() {
        assert_eq!(Timestamp::parse("off"), Some(Timestamp::Off));
        assert_eq!(Timestamp::parse("TIME"), Some(Timestamp::Time));
        assert_eq!(Timestamp::parse("date"), Some(Timestamp::DateTime));
        assert_eq!(Timestamp::parse("datetime"), Some(Timestamp::DateTime));
        assert_eq!(Timestamp::parse("unix"), Some(Timestamp::Unix));
        assert_eq!(Timestamp::parse("nonsense"), None);
    }

    #[test]
    fn timestamp_off_renders_nothing() {
        assert_eq!(Timestamp::Off.render(), None);
    }

    #[test]
    fn rendered_timestamps_have_the_expected_shape() {
        let t = Timestamp::Time.render().expect("time is enabled");
        // HH:MM:SS.mmm
        assert_eq!(t.len(), 12, "{t}");
        assert_eq!(t.as_bytes()[2], b':');
        assert_eq!(t.as_bytes()[5], b':');
        assert_eq!(t.as_bytes()[8], b'.');

        let d = Timestamp::DateTime.render().expect("date is enabled");
        // YYYY-MM-DD HH:MM:SS.mmm
        assert_eq!(d.len(), 23, "{d}");
        assert_eq!(d.as_bytes()[4], b'-');
        assert_eq!(d.as_bytes()[10], b' ');

        let u = Timestamp::Unix.render().expect("unix is enabled");
        assert!(u.contains('.'), "{u}");
    }

    #[test]
    fn civil_date_conversion_matches_known_dates() {
        // 1970-01-01 is day 0.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-03-01 is day 11017 (a leap-year boundary).
        assert_eq!(civil_from_days(11_017), (2000, 3, 1));
        // 2026-09-08 is day 20704; 2026-11-07 is day 20764.
        assert_eq!(civil_from_days(20_704), (2026, 9, 8));
        assert_eq!(civil_from_days(20_764), (2026, 11, 7));
    }

    #[test]
    fn hms_splits_the_day_correctly() {
        assert_eq!(hms(0), (0, 0, 0));
        assert_eq!(hms(3_661), (1, 1, 1));
        assert_eq!(hms(86_399), (23, 59, 59));
        // A full day wraps to midnight.
        assert_eq!(hms(86_400), (0, 0, 0));
    }

    #[test]
    fn scope_indicator_is_dimmed_not_bright_white() {
        // The scope must recede behind the message, matching the pre-unification
        // editor output (tracing_subscriber's Style::dimmed()).
        assert_eq!(C_MODULE, "\x1b[2m");
    }

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
