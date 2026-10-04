//! Build script: probe for the user-provided FMOD shared library (AGENTS.md §4.7).
//!
//! **FMOD binaries are not vendored.** The library is only required when the
//! `fmod` feature is enabled; if it is missing we fail in the AGENTS.md format.
//!
//! When found, set `RELESTE_FMOD_PATH` for the runtime [`fmod_backend`] to load.

use std::env;
use std::path::{Path, PathBuf};

/// Platform-specific library filename candidates.
fn library_names() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &["fmod.dll", "fmod64.dll"]
    } else if cfg!(target_os = "macos") {
        &["libfmod.dylib", "libfmodL.dylib"]
    } else {
        &["libfmod.so", "libfmodL.so"]
    }
}

/// Platform-specific search directories.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    // Environment variables take priority.
    for var in ["RELESTE_FMOD_DIR", "FMOD_DIR"] {
        if let Ok(v) = env::var(var) {
            dirs.push(PathBuf::from(v));
        }
    }

    dirs.push(PathBuf::from("/usr/local/lib"));
    dirs.push(PathBuf::from("/usr/lib"));
    dirs.push(PathBuf::from("/opt/homebrew/lib"));

    // Dynamic linker search paths.
    for var in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "PATH"] {
        if let Ok(paths) = env::var(var) {
            let sep = if cfg!(target_os = "windows") {
                ';'
            } else {
                ':'
            };
            dirs.extend(
                paths
                    .split(sep)
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from),
            );
        }
    }

    // Next to the executable.
    if let Ok(out_dir) = env::var("OUT_DIR") {
        dirs.push(PathBuf::from(out_dir));
    }

    dirs
}

/// Search the candidate directories for the library file.
fn find_fmod() -> Option<PathBuf> {
    for dir in search_dirs() {
        for name in library_names() {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The error text specified by AGENTS.md §4.7.
const NOT_FOUND: &str = "\
error: FMOD library not found.
Download from https://www.fmod.com/download
Place libfmod.so / fmod.dll / libfmod.dylib in:
  - Linux: /usr/local/lib/ or $LD_LIBRARY_PATH
  - Windows: alongside the .exe
  - macOS: /usr/local/lib/ or DYLD_LIBRARY_PATH";

fn main() {
    println!("cargo:rerun-if-env-changed=RELESTE_FMOD_DIR");
    println!("cargo:rerun-if-env-changed=FMOD_DIR");
    println!("cargo:rerun-if-env-changed=RELESTE_FMOD_ALLOW_MISSING");
    println!("cargo:rerun-if-env-changed=RELESTE_FMOD_PATH");
    println!("cargo:rerun-if-env-changed=RELESTE_FMOD_LIB");
    println!("cargo:rerun-if-env-changed=LD_LIBRARY_PATH");
    println!("cargo:rerun-if-env-changed=DYLD_LIBRARY_PATH");

    match env::var("CARGO_FEATURE_FMOD") {
        // Feature not enabled: do not require FMOD to be present.
        Err(_) => {
            println!("cargo:rustc-cfg=reles_fmod_optional");
        }
        Ok(_) => match find_fmod() {
            Some(path) => {
                println!("cargo:rustc-env=RELESTE_FMOD_PATH={}", path.display());
                println!(
                    "cargo:warning=using user-provided FMOD at {}",
                    path.display()
                );
                ensure_exists_and_emit_rerun(&path);
            }
            None => {
                // Allow compile checks / CI in environments without FMOD.
                if env::var("RELESTE_FMOD_ALLOW_MISSING").is_ok() {
                    println!(
                        "cargo:warning=FMOD library not found; \
                         building anyway because RELESTE_FMOD_ALLOW_MISSING is set. \
                         The runtime backend will report the error on first use."
                    );
                    println!("cargo:rustc-cfg=reles_fmod_missing");
                    return;
                }

                // Fail with the clear, AGENTS.md-specified format.
                eprintln!("{NOT_FOUND}");
                std::process::exit(1);
            }
        },
    }
}

/// Make cargo re-run when the library file changes.
fn ensure_exists_and_emit_rerun(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
}
