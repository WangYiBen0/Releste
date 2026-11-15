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

use serde::Serialize;

use crate::error::PipelineError;
use crate::skin::SkinConfigFile;

/// What one pack produced. The caller folds this into its own report.
#[derive(Debug, Default, Clone)]
pub struct PackStats {
    /// Atlas files written (one per pack).
    pub atlases: usize,
    /// Texture groups written (one per source image).
    pub sprites: usize,
    /// Non-fatal problems (an unparsable `Sprites.xml`, ...).
    pub warnings: Vec<String>,
}

/// Write one pack into `out_root`.
///
/// `out_root` is the directory that receives the `atlas/` and `skins/` subdirectories:
/// the flat `assets/` tree for a normal pipeline run, or `resources/<namespace>/` when
/// repackaging a skin zip (ROADMAP Appendix C).
///
/// The pack becomes:
///
/// - `atlas/<pack>.atlas`: every image under `Graphics/Atlases/`, copied and named by
///   its path relative to that directory with the first (namespace) component stripped,
///   so a sprite `characters/niko/idle00` keeps exactly that name;
/// - `atlas/<pack>.sprites.toml`: animation tables from every `Graphics/**/Sprites.xml`;
/// - `skins/<pack>.skin.toml`: the engine-native descriptor (character id + hair
///   colours), so the runtime never parses YAML or XML (AGENTS.md 4.6).
pub fn write_pack(
    pack_dir: &Path,
    config: &SkinConfigFile,
    out_root: &Path,
    pack_id_override: Option<&str>,
) -> Result<PackStats, PipelineError> {
    #[derive(Serialize)]
    struct SpriteOut {
        name: String,
        x: i16,
        y: i16,
        w: i16,
        h: i16,
        origin_x: i16,
        origin_y: i16,
        frame_w: i16,
        frame_h: i16,
    }
    #[derive(Serialize)]
    struct TextureOut {
        name: String,
        kind: &'static str,
        source: String,
        sprites: Vec<SpriteOut>,
    }
    #[derive(Serialize)]
    struct AtlasOut {
        name: String,
        version: i32,
        textures: Vec<TextureOut>,
    }
    #[derive(Serialize)]
    struct SpritesToml {
        sprite: Vec<crate::sprites_xml::SpriteDef>,
    }

    let mut stats = PackStats::default();

    let pack_id = pack_id_override
        .map(|s| s.to_owned())
        .or_else(|| {
            pack_dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "skin".into());
    let atlas_dir = out_root.join("atlas");
    std::fs::create_dir_all(&atlas_dir).map_err(|e| PipelineError::io(atlas_dir.display(), e))?;

    let mut textures: Vec<TextureOut> = Vec::new();
    let mut group_index = 0usize;

    // Every atlas image the pack ships is copied and named by its path relative to
    // `Graphics/Atlases/`, so a sprite `characters/niko/idle00` keeps exactly that
    // name. This is namespace-agnostic: <Character_ID> sprites and OtherSprite sets
    // both come out in the same form.
    let atlas_root = pack_dir.join("Graphics/Atlases");
    if atlas_root.is_dir() {
        let mut pngs: Vec<PathBuf> = Vec::new();
        crate::pipeline::collect_files_with_ext(&atlas_root, "png", &mut pngs);
        pngs.sort();
        for png in pngs {
            let Ok(rel) = png.strip_prefix(&atlas_root) else {
                continue;
            };
            // `rel` is `<atlas ns>/<sprite path>.png` (e.g. `Gameplay/characters/
            // niko/idle00.png`). The namespace is a separate atlas in Celeste but this
            // engine merges the pack into one atlas whose lookups drop the namespace,
            // so strip the first component and keep `characters/niko/idle00`.
            let rel = Path::new(rel);
            let stripped: PathBuf = rel.components().skip(1).collect();
            if stripped.as_os_str().is_empty() {
                continue;
            }
            let rel_str = stripped.to_string_lossy().replace('\\', "/");
            let sprite_name = rel_str.trim_end_matches(".png").replace('/', "\\");
            let out_rel = format!("atlas/{pack_id}/img{group_index}.png");
            let out_path = out_root.join(&out_rel);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| PipelineError::io(parent.display(), e))?;
            }
            std::fs::copy(&png, &out_path).map_err(|e| PipelineError::io(out_path.display(), e))?;

            let img = image::open(&png)
                .map_err(|e| PipelineError::Png {
                    path: png.display().to_string(),
                    source: e,
                })?
                .to_rgba8();
            let (w, h) = img.dimensions();
            textures.push(TextureOut {
                name: format!("{pack_id}_{group_index}"),
                kind: "png",
                source: out_rel,
                sprites: vec![SpriteOut {
                    name: sprite_name,
                    x: 0,
                    y: 0,
                    w: w as i16,
                    h: h as i16,
                    // Full, untrimmed frames: the engine's per-sprite origin offset is
                    // `HAIR_FRAME_ORIGIN - origin`, and a pack frame has no crop, so
                    // `origin = (0,0)` keeps the maths identical to the vanilla
                    // subimages (`pureCeleste.cs` `Sprite` origin handling).
                    origin_x: 0,
                    origin_y: 0,
                    frame_w: w as i16,
                    frame_h: h as i16,
                }],
            });
            group_index += 1;
        }
    }

    let atlas = AtlasOut {
        name: pack_id.clone(),
        version: 5,
        textures,
    };
    let atlas_text = toml::to_string_pretty(&atlas)
        .map_err(|e| PipelineError::Serialize(format!("{pack_id} atlas toml: {e}")))?;
    std::fs::write(atlas_dir.join(format!("{pack_id}.atlas")), atlas_text)
        .map_err(|e| PipelineError::io(atlas_dir.display(), e))?;

    // Animation tables: the pack ships `Graphics/Sprites.xml` (the player's
    // `<Character_ID>` def) and per-namespace `Graphics/<ns>/Sprites.xml` (other
    // scene sprites). Convert them all into one `atlas/<pack>.sprites.toml` so the
    // runtime never parses XML (AGENTS.md 4.6). The player def keeps its original id
    // (`niko`); the engine aliases it to `player` at load time per `Character_ID`.
    let mut sprite_defs: Vec<crate::sprites_xml::SpriteDef> = Vec::new();
    let mut xml_files: Vec<PathBuf> = Vec::new();
    crate::pipeline::collect_files_with_ext(&pack_dir.join("Graphics"), "xml", &mut xml_files);
    xml_files.sort();
    for xml in xml_files {
        // Only `Sprites.xml` spells out animations; `Portraits.xml` and other XML
        // files use different elements.
        if xml.file_name().map(|n| n != "Sprites.xml").unwrap_or(true) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&xml) else {
            continue;
        };
        match crate::sprites_xml::parse_sprites(&text) {
            Ok(defs) => sprite_defs.extend(defs),
            Err(e) => stats.warnings.push(format!(
                "skin `{pack_id}`: could not parse {}: {e}",
                xml.display()
            )),
        }
    }
    if !sprite_defs.is_empty() {
        let sprites_toml = SpritesToml {
            sprite: sprite_defs,
        };
        let text = toml::to_string_pretty(&sprites_toml)
            .map_err(|e| PipelineError::Serialize(format!("{pack_id} sprites toml: {e}")))?;
        std::fs::write(atlas_dir.join(format!("{pack_id}.sprites.toml")), text)
            .map_err(|e| PipelineError::io(atlas_dir.display(), e))?;
    }

    // Engine-native descriptor.
    #[derive(Serialize)]
    struct SkinOut {
        pack: String,
        entries: Vec<EntryOut>,
    }
    #[derive(Serialize)]
    struct EntryOut {
        skin_name: String,
        character_id: Option<String>,
        hair_colors: Vec<HairColorOut>,
    }
    #[derive(Serialize)]
    struct HairColorOut {
        dashes: u8,
        rgb: [u8; 3],
    }

    let skins_dir = out_root.join("skins");
    std::fs::create_dir_all(&skins_dir).map_err(|e| PipelineError::io(skins_dir.display(), e))?;

    let entries = config
        .entries
        .iter()
        .map(|e| EntryOut {
            skin_name: e.skin_name.clone(),
            character_id: e.character_id.clone(),
            hair_colors: crate::skin::default_hair_colors()
                .into_iter()
                .map(|(dashes, rgb)| HairColorOut { dashes, rgb })
                .collect(),
        })
        .collect();
    let skin = SkinOut {
        pack: pack_id.clone(),
        entries,
    };
    let skin_text = toml::to_string_pretty(&skin)
        .map_err(|e| PipelineError::Serialize(format!("{pack_id} skin toml: {e}")))?;
    std::fs::write(skins_dir.join(format!("{pack_id}.skin.toml")), skin_text)
        .map_err(|e| PipelineError::io(skins_dir.display(), e))?;

    stats.atlases += 1;
    stats.sprites += group_index;
    Ok(stats)
}

/// Read `pack_dir/SkinModHelperConfig.yaml` and write the pack into `out_root`.
pub fn write_pack_dir(pack_dir: &Path, out_root: &Path) -> Result<PackStats, PipelineError> {
    let config = SkinConfigFile::from_file(&pack_dir.join("SkinModHelperConfig.yaml"))?;
    write_pack(pack_dir, &config, out_root, None)
}

/// Repackage one skin `.zip` into `resources_root/<namespace>/` (ROADMAP Appendix C).
///
/// The archive is unpacked into a temp directory under `resources_root` that is removed
/// on the way out. The namespace is the id the game selects with `--skin`, so the atlas
/// and descriptor are named after it rather than the archive's inner folder.
pub fn convert_zip(
    zip_path: &Path,
    resources_root: &Path,
    namespace: Option<&str>,
) -> Result<PathBuf, PipelineError> {
    let namespace = namespace
        .map(|s| s.to_owned())
        .unwrap_or_else(|| default_namespace(zip_path));
    let ns_root = resources_root.join(&namespace);
    std::fs::create_dir_all(&ns_root).map_err(|e| PipelineError::io(ns_root.display(), e))?;

    let work = resources_root.join(".skinpack-work");
    std::fs::create_dir_all(&work).map_err(|e| PipelineError::io(work.display(), e))?;
    let result: Result<(), PipelineError> = (|| {
        let pack_dir = extract_pack(zip_path, &work)?;
        let config = SkinConfigFile::from_file(&pack_dir.join("SkinModHelperConfig.yaml"))?;
        write_pack(&pack_dir, &config, &ns_root, Some(&namespace))?;
        Ok(())
    })();
    // Best-effort cleanup; a leftover temp dir must not fail the conversion.
    let _ = std::fs::remove_dir_all(&work);
    result?;
    Ok(ns_root)
}

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

    /// The written pack must use the namespace as the pack id, so the game's
    /// `--skin <namespace>` finds `atlas/<namespace>.atlas` and
    /// `skins/<namespace>.skin.toml` (ROADMAP Appendix C).
    #[test]
    fn write_pack_names_every_file_after_the_namespace() {
        let dir = std::env::temp_dir().join(format!("releste-writepack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pack = dir.join("inner-folder-name");
        std::fs::create_dir_all(pack.join("Graphics/Atlases/Gameplay/characters/niko")).unwrap();
        std::fs::write(pack.join("SkinModHelperConfig.yaml"), "- SkinName: N\n").unwrap();
        // One 1x1 transparent frame: `Gameplay/characters/niko/idle00.png`.
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 0]));
        img.save(pack.join("Graphics/Atlases/Gameplay/characters/niko/idle00.png"))
            .unwrap();
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let config = SkinConfigFile::from_file(&pack.join("SkinModHelperConfig.yaml")).unwrap();
        let stats = write_pack(&pack, &config, &out, Some("MySkin")).unwrap();

        assert_eq!(stats.atlases, 1);
        assert_eq!(stats.sprites, 1);
        assert!(stats.warnings.is_empty());
        assert!(out.join("atlas/MySkin.atlas").is_file());
        assert!(out.join("skins/MySkin.skin.toml").is_file());
        assert!(out.join("atlas/MySkin/img0.png").is_file());
        // The first path component (the Celeste atlas namespace) is stripped, so the
        // sprite keeps the name the engine's lookup builds.
        let atlas = std::fs::read_to_string(out.join("atlas/MySkin.atlas")).unwrap();
        // TOML renders a backslash-heavy name as a literal (single-quoted) string.
        assert!(
            atlas.contains(r"name = 'characters\niko\idle00'"),
            "{atlas}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
