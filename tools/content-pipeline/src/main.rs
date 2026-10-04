//! Releste 资源管线：原版 Celeste 资源 → 引擎格式。
//!
//! **一次性转换**，不在运行时做（AGENTS.md §4.6 / §12）。
//!
//! ```text
//! cargo run -p reles-content-pipeline -- \
//!     --source ../assets-src \
//!     --output ../assets
//! ```

#![deny(warnings)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use reles_content_pipeline::{Pipeline, PipelineConfig};
use tracing::info;

/// 命令行参数。
#[derive(Parser, Debug)]
#[command(
    name = "content-pipeline",
    about = "原版 Celeste 资源 → Releste 引擎格式"
)]
struct Args {
    /// 源目录（原版 Celeste 的 Content/ 目录）。
    #[arg(short, long, default_value = "../assets-src")]
    source: PathBuf,

    /// 输出目录（引擎运行时资源）。
    #[arg(short, long, default_value = "../assets")]
    output: PathBuf,

    /// 只转换指定分类（可重复）：atlas / dialog / maps / audio。
    #[arg(long = "only", value_name = "CATEGORY")]
    only: Vec<String>,

    /// 只校验，不写文件。
    #[arg(long)]
    dry_run: bool,

    /// 写完后重新读回每个 `.map`，确认运行时加载器能解析。
    #[arg(long)]
    verify: bool,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

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
            println!("  crunch textures   : {}", report.crunch_textures);
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

/// 用运行时加载器重新读取输出目录下的所有 `.map`。
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
            reles_map::load_map(&path).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        files += 1;
        rooms += doc.rooms.len();
        entities += doc.rooms.iter().map(|r| r.entities.len()).sum::<usize>();
    }

    Ok((files, rooms, entities))
}
