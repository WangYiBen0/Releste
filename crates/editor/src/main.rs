//! `editor` binary: map editor entry point.
//!
//! ```text
//! cargo run -p reles-editor                       # new map
//! cargo run -p reles-editor -- assets/maps/1-ForsakenCity.map
//! ```

#![deny(warnings)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use reles_editor::app::EditorApp;
use reles_editor::Editor;
use tracing::{info, warn};

/// Command-line arguments.
#[derive(Parser, Debug)]
#[command(name = "editor", about = "Releste map editor")]
struct Args {
    /// Map to open (`.map`). A new map is created if omitted.
    map: Option<PathBuf>,

    /// Only validate that the map loads, then exit (no GPU needed).
    #[arg(long)]
    check: bool,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();

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
                eprintln!("failed to open {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => Editor::new(),
    };

    if args.check {
        let doc = editor.document();
        let entities: usize = doc.rooms.iter().map(|r| r.entities.len()).sum();
        println!(
            "ok: {} ({} rooms, {} entities, {} sync rules)",
            doc.area,
            doc.rooms.len(),
            entities,
            doc.sync_rules.len()
        );
        return ExitCode::SUCCESS;
    }

    let mut app = EditorApp::new(editor);
    // Entity Schemas for the inspector's auto-generated panels.
    register_builtin_schemas(&mut app);

    match app.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("editor failed: {e}");
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
    use reles_world::{FieldType, FieldValue, Schema, SchemaField};
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
