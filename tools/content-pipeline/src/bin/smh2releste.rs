//! `smh2releste`: repackage an Everest Skin Mod Helper skin into a Releste resource pack.
//!
//! Skins ship as either a `.zip` archive or an extracted mod directory, and both are
//! accepted. The output is the distribution layout of ROADMAP Appendix C, so the game
//! picks it up with no extra flags beyond `--skin <namespace>`:
//!
//! ```text
//! resources/<namespace>/
//!   atlas/<namespace>.atlas         # texture groups + sprite rects
//!   atlas/<namespace>.sprites.toml  # animation tables
//!   atlas/<namespace>/imgN.png      # the copied frames
//!   skins/<namespace>.skin.toml     # engine-native skin descriptor
//! ```
//!
//! Every converter follows this shape: one small binary per input format, each a thin
//! CLI over a reusable function in `releste_content_pipeline`. Nothing about original
//! formats lives in the runtime crates (AGENTS.md 4.6).
//!
//! ```text
//! cargo run -p releste-content-pipeline --bin smh2releste -- \
//!     path/to/Skin.zip --resources-root resources --namespace MySkin
//! ```

#![deny(warnings)]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use releste_content_pipeline::skin::SkinConfigFile;
use releste_content_pipeline::skinpack;
use tracing::info;

/// Command-line arguments.
#[derive(Parser, Debug)]
#[command(
    name = "smh2releste",
    about = "Everest Skin Mod Helper skin -> Releste resource pack"
)]
struct Args {
    /// Skin to convert: a `.zip` archive or an extracted mod directory.
    input: PathBuf,

    /// Directory that holds (or will hold) `resources/`.
    #[arg(long, value_name = "DIR", default_value = "resources")]
    resources_root: PathBuf,

    /// Namespace directory to write under, which is also the id `--skin` selects.
    /// Defaults to the archive stem / directory name.
    #[arg(long, value_name = "NS")]
    namespace: Option<String>,
}

fn main() -> ExitCode {
    releste_kernel::log::init(false);
    let args = Args::parse();

    let namespace = args.namespace.clone().unwrap_or_else(|| {
        if args.input.is_dir() {
            args.input
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "skin".to_owned())
        } else {
            skinpack::default_namespace(&args.input)
        }
    });

    let result = if args.input.is_dir() {
        convert_dir(&args.input, &args.resources_root, &namespace)
    } else {
        skinpack::convert_zip(&args.input, &args.resources_root, Some(&namespace))
    };

    match result {
        Ok(ns_root) => {
            info!("converted {}", args.input.display());
            println!("=== smh2releste ===");
            println!("  input     : {}", args.input.display());
            println!("  namespace : {namespace}");
            println!("  output    : {}", ns_root.display());
            println!();
            println!("Play with it:  releste --skin {namespace}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("smh2releste: conversion failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Convert an already-extracted mod directory. The archive path only has to unpack
/// first; everything after that is shared with [`skinpack::convert_zip`].
fn convert_dir(
    pack_dir: &std::path::Path,
    resources_root: &std::path::Path,
    namespace: &str,
) -> Result<PathBuf, releste_content_pipeline::PipelineError> {
    let config = SkinConfigFile::from_file(&pack_dir.join("SkinModHelperConfig.yaml"))?;
    let ns_root = resources_root.join(namespace);
    skinpack::write_pack(pack_dir, &config, &ns_root, Some(namespace))?;
    Ok(ns_root)
}
