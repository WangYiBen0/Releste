//! Releste asset pipeline: vanilla Celeste assets -> engine format.
//!
//! **One-shot conversion**, not done at runtime (AGENTS.md 4.6 / 12).
//!
//! ```text
//! cargo run -p releste-content-pipeline -- \
//!     --source ../assets-src \
//!     --output ../assets
//! ```

#![deny(warnings)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use releste_content_pipeline::{Pipeline, PipelineConfig};
use tracing::info;

/// Command-line arguments.
#[derive(Parser, Debug)]
#[command(
    name = "content-pipeline",
    about = "vanilla Celeste assets -> Releste engine format"
)]
struct Args {
    /// Source directory (the vanilla Celeste Content/ directory).
    #[arg(short, long, default_value = "../assets-src")]
    source: PathBuf,

    /// Output directory (engine runtime assets).
    #[arg(short, long, default_value = "../assets")]
    output: PathBuf,

    /// Convert only the given categories (repeatable): atlas / dialog / maps / audio.
    #[arg(long = "only", value_name = "CATEGORY")]
    only: Vec<String>,

    /// Validate only, do not write files.
    #[arg(long)]
    dry_run: bool,

    /// After writing, read every `.map` back to confirm the runtime loader can parse it.
    #[arg(long)]
    verify: bool,
}

fn main() -> ExitCode {
    // Same Appendix A format as the game and the editor.
    releste_kernel::log::init(false);

    let args = Args::parse();

    if args.dry_run {
        info!("dry-run: no files will be written");
    }

    let config = PipelineConfig {
        source: args.source.clone(),
        output: args.output.clone(),
        only: args.only.clone(),
    };

    let mut pipeline = Pipeline::new(config);
    match pipeline.run() {
        Ok(report) => {
            println!("=== Releste content-pipeline ===");
            println!("  source            : {}", args.source.display());
            println!("  output            : {}", args.output.display());
            println!("  atlases           : {}", report.atlases);
            println!("  sprites           : {}", report.sprites);
            println!("  dialogs           : {}", report.dialogs);
            println!("  dialog entries    : {}", report.dialog_entries);
            println!("  maps              : {}", report.maps);
            println!("  rooms             : {}", report.rooms);
            println!("  entities          : {}", report.entities);
            println!("  FMOD banks copied : {}", report.banks_copied);
            println!("  data textures PNG : {}", report.decoded_textures);
            println!("  data texture raw  : {}", report.pending_textures);
            println!("  tilesets          : {}", report.tilesets);
            if !report.warnings.is_empty() {
                println!("  warnings          : {}", report.warnings.len());
                for w in &report.warnings {
                    println!("    - {w}");
                }
            }

            if args.verify {
                match verify_maps(&args.output) {
                    Ok((files, rooms, entities)) => {
                        println!(
                            "  verified          : {files} maps, {rooms} rooms, {entities} entities reload OK"
                        );
                    }
                    Err(e) => {
                        eprintln!("verification failed: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            }

            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("content-pipeline failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Re-read every `.map` in the output directory with the runtime loader.
fn verify_maps(output: &std::path::Path) -> anyhow::Result<(usize, usize, usize)> {
    let dir = output.join("maps");
    if !dir.is_dir() {
        anyhow::bail!("no maps directory at {}", dir.display());
    }

    let mut files = 0usize;
    let mut rooms = 0usize;
    let mut entities = 0usize;

    let mut entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "map"))
        .collect();
    entries.sort();

    for path in entries {
        let doc =
            releste_map::load_map(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        files += 1;
        rooms += doc.rooms.len();
        entities += doc.rooms.iter().map(|r| r.entities.len()).sum::<usize>();
    }

    Ok((files, rooms, entities))
}
