//! `editor` binary: map editor entry point.
//!
//! ```text
//! cargo run -p releste-editor                       # new map
//! cargo run -p releste-editor -- assets/maps/1-ForsakenCity.map
//! ```

#![deny(warnings)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use releste_editor::app::EditorApp;
use releste_editor::{l10n, Editor};
use tracing::{info, warn};

// Command-line arguments.
//
// NOTE: clap's `about` and help strings are deliberately NOT localised. clap
// renders `--help` before the locale can be known, because the locale itself
// comes from `--lang`, which clap is still parsing. The runtime UI - the
// surface a translator actually cares about - is fully localised; the CLI
// keeps stable English text so error messages and scripts stay greppable.
// See docs/i18n.md section 6 for the rationale.
#[derive(Parser, Debug)]
#[command(name = "editor", about = "Releste map editor")]
struct Args {
    /// Map to open (`.map`). A new map is created if omitted.
    map: Option<PathBuf>,

    /// Only validate that the map loads, then exit (no GPU needed).
    #[arg(long)]
    check: bool,

    /// Interface language, for example `en_US` or `zh_CN`.
    ///
    /// Defaults to `$LANG` (and friends), then to `en_US`. See docs/i18n.md 6.
    #[arg(long, value_name = "TAG")]
    lang: Option<String>,

    /// Disable ANSI colours in log output.
    #[arg(long)]
    no_color: bool,

    /// Log timestamp format: off (default), time, date, or unix.
    ///
    /// `time` is HH:MM:SS.mmm, `date` is YYYY-MM-DD HH:MM:SS.mmm, `unix` is
    /// epoch seconds. The ROADMAP Appendix A layout has no timestamp, so `off`
    /// is the default; set it when you need to correlate logs with other tools.
    #[arg(long, value_name = "FORMAT", default_value = "off")]
    log_time: String,
}

fn main() -> ExitCode {
    // Same Appendix A format as the game: releste-kernel::log is the single
    // implementation, so every binary logs identically (ROADMAP 1.1).
    let args = Args::parse();
    let timestamp = match releste_kernel::log::Timestamp::parse(&args.log_time) {
        Some(t) => t,
        None => {
            eprintln!(
                "warning: unknown --log-time {:?}; using off (accepted: off, time, date, unix)",
                args.log_time
            );
            releste_kernel::log::Timestamp::Off
        }
    };
    releste_kernel::log::init_with_timestamp(args.no_color, timestamp);

    // Resolve the language first: it also localises the messages below.
    let locale = l10n::locale_from_env(args.lang.as_deref());
    let root = l10n::find_catalog_root().unwrap_or_else(|| PathBuf::from("locales"));
    let l10n = l10n::Localizer::load(&root, &locale);
    if l10n.missing_keys().is_empty() {
        info!(locale = %locale, root = %root.display(), "language loaded");
    } else {
        // AGENTS.md 15.9: never swallow it quietly.
        warn!(
            locale = %locale,
            missing = l10n.missing_keys().len(),
            "catalog is incomplete; falling back to en_US for those keys"
        );
    }

    let editor = match &args.map {
        Some(path) => match Editor::open(path) {
            Ok(e) => {
                info!(
                    path = %path.display(),
                    rooms = e.document().rooms.len(),
                    "map opened"
                );
                e
            }
            Err(e) => {
                eprintln!(
                    "{}",
                    l10n.format(
                        "app.error.open_failed",
                        &[
                            ("path", path.display().to_string()),
                            ("error", e.to_string()),
                        ],
                    )
                );
                return ExitCode::FAILURE;
            }
        },
        None => Editor::new(),
    };

    if args.check {
        let doc = editor.document();
        let entities: usize = doc.rooms.iter().map(|r| r.entities.len()).sum();
        println!(
            "{}",
            l10n.format(
                "app.check.summary",
                &[
                    ("area", doc.area.clone()),
                    ("rooms", doc.rooms.len().to_string()),
                    ("entities", entities.to_string()),
                    ("sync_rules", doc.sync_rules.len().to_string()),
                ],
            )
        );
        return ExitCode::SUCCESS;
    }

    let mut app = EditorApp::new_with_locale(editor, locale, root);
    // Tile art lives under `assets/` (ROADMAP Appendix C); without this the
    // canvas can only draw placeholders. Missing art is a warning, not an
    // error: a map must still open when the asset tree is absent.
    let assets_root = std::env::var_os("RELESTE_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("assets"));
    app.set_assets_root(assets_root);
    // Entity Schemas for the inspector's auto-generated panels.
    register_builtin_schemas(&mut app);

    match app.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!(
                "{}",
                l10n.format("app.error.run_failed", &[("error", e.to_string())])
            );
            if cfg!(target_os = "linux") {
                warn!("tip: ensure a GPU/display is available (Wayland/X11)");
            }
            ExitCode::FAILURE
        }
    }
}

/// Registers Schemas for the built-in entity kinds.
///
/// In a real project these come from `world`'s `EntityFactory::schema()`;
/// a minimal set is provided here to exercise the
/// "Schema auto-generates panels" pipeline.
fn register_builtin_schemas(app: &mut EditorApp) {
    use releste_world::{FieldType, FieldValue, Schema, SchemaField};
    use std::borrow::Cow;

    let player = Schema::new(vec![
        SchemaField {
            name: Cow::Borrowed("facing"),
            field_type: FieldType::Enum(vec![Cow::Borrowed("Left"), Cow::Borrowed("Right")]),
            default: FieldValue::Enum(1),
            tooltip: Some(Cow::Borrowed("initial facing")),
        },
        SchemaField {
            name: Cow::Borrowed("invincible"),
            field_type: FieldType::Bool,
            default: FieldValue::Bool(false),
            tooltip: Some(Cow::Borrowed("whether invincible during cutscenes")),
        },
    ]);
    app.register_schema("player", &player);

    let door = Schema::new(vec![
        SchemaField {
            name: Cow::Borrowed("open"),
            field_type: FieldType::Bool,
            default: FieldValue::Bool(false),
            tooltip: Some(Cow::Borrowed("network sync channel: door_open")),
        },
        SchemaField {
            name: Cow::Borrowed("side"),
            field_type: FieldType::Enum(vec![
                Cow::Borrowed("Left"),
                Cow::Borrowed("Right"),
                Cow::Borrowed("Up"),
                Cow::Borrowed("Down"),
            ]),
            default: FieldValue::Enum(0),
            tooltip: None,
        },
    ]);
    app.register_schema("door", &door);
}
