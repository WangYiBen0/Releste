//! Repackage an Everest skin `.zip` into a Releste resource pack.
//!
//! `releste-content-pipeline --skin-pack <Skin.zip> --resources-root <dir>` uses this:
//! the archive is extracted, its `SkinModHelperConfig.yaml` is read, and the pack is
//! written in the distribution layout of ROADMAP Appendix C:
//!
//! ```text
//! resources/<namespace>/
//!   atlas/<pack>.atlas         # texture groups + sprite rects
//!   atlas/<pack>.sprites.toml  # animation tables
//!   atlas/<pack>/imgN.png      # the copied frames
//!   skins/<pack>.skin.toml     # engine-native skin descriptor
//! ```
//!
//! That is the same on-disk shape the pipeline already produces for `assets/`, so the
//! runtime needs no new loader: `Paths` resolves `resources/<namespace>/atlas`, and the
//! skin descriptor sits in `resources/<namespace>/skins`.

use std::path::{Path, PathBuf};

use crate::error::PipelineError;

/// Extract `zip_path` into a fresh directory under `work_root` and return the pack root
/// (the directory that actually holds `SkinModHelperConfig.yaml`).
///
/// A pack may nest its manifest (some archives wrap everything in a folder), so this
/// searches one or two levels deep. The caller owns `work_root` and may delete it.
pub fn extract_pack(zip_path: &Path, work_root: &Path) -> Result<PathBuf, PipelineError> {
    let stem = zip_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "skin".to_owned());
    let dest = work_root.join(sanitize(&stem));
    std::fs::create_dir_all(&dest).map_err(|e| PipelineError::io(dest.display(), e))?;

    let file =
        std::fs::File::open(zip_path).map_err(|e| PipelineError::io(zip_path.display(), e))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| PipelineError::parse("zip", zip_path.display(), e.to_string()))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| PipelineError::parse("zip", zip_path.display(), e.to_string()))?;
        // `enclosed_name` rejects absolute paths and `..` traversal.
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out_path = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|e| PipelineError::io(out_path.display(), e))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| PipelineError::io(parent.display(), e))?;
        }
        let mut out = std::fs::File::create(&out_path)
            .map_err(|e| PipelineError::io(out_path.display(), e))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| PipelineError::io(out_path.display(), e))?;
    }

    find_pack_root(&dest).ok_or_else(|| {
        PipelineError::parse(
            "SkinModHelperConfig.yaml",
            zip_path.display(),
            "the archive contains no SkinModHelperConfig.yaml",
        )
    })
}

/// Depth-first search for the directory holding `SkinModHelperConfig.yaml`.
fn find_pack_root(root: &Path) -> Option<PathBuf> {
    if root.join("SkinModHelperConfig.yaml").is_file() {
        return Some(root.to_path_buf());
    }
    let entries = std::fs::read_dir(root).ok()?;
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        if let Some(found) = find_pack_root(&dir) {
            return Some(found);
        }
    }
    None
}

/// The namespace to write under when the caller does not supply one: the zip stem with
/// characters that would break a path segment replaced by `_`.
pub fn default_namespace(zip_path: &Path) -> String {
    let stem = zip_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "skin".to_owned());
    sanitize(&stem)
}

fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "skin".to_owned()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_namespace_sanitises_the_stem() {
        assert_eq!(
            default_namespace(Path::new("/x/Niko_-_Celeste_Skin-Helper.zip")),
            "Niko_-_Celeste_Skin-Helper"
        );
        assert_eq!(default_namespace(Path::new("/x/a b:c.zip")), "a_b_c");
    }

    #[test]
    fn extract_pack_finds_a_nested_manifest() {
        // Build a tiny zip in-memory: `wrap/skin/SkinModHelperConfig.yaml`.
        let dir = std::env::temp_dir().join(format!("releste-skinpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let zip_path = dir.join("pack.zip");
        {
            let file = std::fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let opts = zip::write::SimpleFileOptions::default();
            zip.start_file("wrap/skin/SkinModHelperConfig.yaml", opts)
                .unwrap();
            use std::io::Write as _;
            zip.write_all(b"- SkinName: x\n").unwrap();
            zip.finish().unwrap();
        }
        let work = dir.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let root = extract_pack(&zip_path, &work).unwrap();
        assert!(root.join("SkinModHelperConfig.yaml").is_file());
        assert!(root.ends_with("wrap/skin"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
