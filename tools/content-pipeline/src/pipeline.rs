//! Pipeline orchestration: walk `assets-src/` and convert into `assets/`.
//!
//! See AGENTS.md 12 for the conversion map.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use releste_map::{
    Backdrop, Decal, MapDocument, MapStyle, RectDef, Room, SyncMode, SyncRule, Tileset, TriggerData,
};
use releste_world::{EntityData, EntityId, FieldValue};
use tracing::{info, warn};

use crate::atlas::{AtlasBuild, AtlasTexture, TextureSource};
use crate::data::{self, DecodedTexture};
use crate::dialog::DialogDocument;
use crate::error::PipelineError;
use crate::mapbin::{BinValue, MapBin, MapBinElement};

/// Pipeline configuration.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Source directory (vanilla Celeste assets).
    pub source: PathBuf,
    /// Output directory.
    pub output: PathBuf,
    /// Convert only these categories (empty = all).
    pub only: Vec<String>,
}

/// Report for one run.
#[derive(Debug, Default, Clone)]
pub struct PipelineReport {
    pub atlases: usize,
    pub sprites: usize,
    pub dialogs: usize,
    pub dialog_entries: usize,
    pub maps: usize,
    pub rooms: usize,
    pub entities: usize,
    pub banks_copied: usize,
    /// Number of `.data` textures already decoded to PNG offline.
    pub decoded_textures: usize,
    /// Number of textures not yet transcoded and still copied as raw `.data` (see AGENTS.md 13).
    pub pending_textures: usize,
    /// Number of tileset definitions produced (`ForegroundTiles.xml` -> `tilesets.toml`).
    pub tilesets: usize,
    pub warnings: Vec<String>,
}

impl PipelineReport {
    fn warn(&mut self, msg: String) {
        warn!("{msg}");
        self.warnings.push(msg);
    }
}

/// Pipeline.
pub struct Pipeline {
    config: PipelineConfig,
    report: PipelineReport,
}

impl Pipeline {
    /// Create a new one.
    pub fn new(config: PipelineConfig) -> Self {
        Pipeline {
            config,
            report: PipelineReport::default(),
        }
    }

    /// Run all categories.
    pub fn run(&mut self) -> Result<PipelineReport, PipelineError> {
        std::fs::create_dir_all(&self.config.output)
            .map_err(|e| PipelineError::io(self.config.output.display(), e))?;

        if self.wants("atlas") {
            self.convert_atlases()?;
            self.convert_sprites()?;
        }
        if self.wants("tilesets") {
            self.convert_tilesets()?;
        }
        if self.wants("dialog") {
            self.convert_dialogs()?;
        }
        if self.wants("maps") {
            self.convert_maps()?;
        }
        if self.wants("audio") {
            self.copy_banks()?;
        }

        Ok(self.report.clone())
    }

    fn wants(&self, category: &str) -> bool {
        self.config.only.is_empty() || self.config.only.iter().any(|c| c == category)
    }

    /// Ensure the output path does not escape the output directory.
    fn safe_out(&self, rel: &Path) -> Result<PathBuf, PipelineError> {
        let joined = self.config.output.join(rel);
        // Normalize: reject `..` components.
        for comp in rel.components() {
            if matches!(comp, std::path::Component::ParentDir) {
                return Err(PipelineError::PathEscape(rel.display().to_string()));
            }
        }
        Ok(joined)
    }

    // ---------- Atlases ----------

    /// `Graphics/{Foreground,Background}Tiles.xml` -> `atlas/{tilesets,bg_tilesets}.toml`.
    ///
    /// The two tables **must not be merged**: the same id is a different sprite in the foreground/background table
    /// (`'1'` is dirt in the foreground and bgDirt in the background; `'3'` is snow / bgBrickRuined),
    /// and each room's `solids` and `bg` should only be looked up in their own table
    /// (vanilla has two independent `Autotiler`s, `110116-110117`).
    ///
    /// XML is the original format and is only parsed at build time (AGENTS.md 4.6).
    fn convert_tilesets(&mut self) -> Result<(), PipelineError> {
        self.convert_tileset_xml("Graphics/ForegroundTiles.xml", "atlas/tilesets.toml")?;
        self.convert_tileset_xml("Graphics/BackgroundTiles.xml", "atlas/bg_tilesets.toml")
    }

    /// One `<Tileset>` table: XML -> TOML.
    fn convert_tileset_xml(&mut self, rel_xml: &str, rel_toml: &str) -> Result<(), PipelineError> {
        let xml_path = self.config.source.join(rel_xml);
        if !xml_path.is_file() {
            self.report
                .warn(format!("tiles xml not found: {}", xml_path.display()));
            return Ok(());
        }

        let doc = crate::tilesets::from_file(&xml_path)
            .map_err(|e| PipelineError::io(xml_path.display(), std::io::Error::other(e)))?;
        let out_path = self.safe_out(Path::new(rel_toml))?;
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| PipelineError::io(parent.display(), e))?;
        }
        let text = toml::to_string_pretty(&doc)
            .map_err(|e| PipelineError::io(out_path.display(), std::io::Error::other(e)))?;
        std::fs::write(&out_path, text).map_err(|e| PipelineError::io(out_path.display(), e))?;

        self.report.tilesets += doc.tilesets.len();
        Ok(())
    }

    fn convert_atlases(&mut self) -> Result<(), PipelineError> {
        let dir = self.config.source.join("Graphics/Atlases");
        if !dir.is_dir() {
            self.report
                .warn(format!("atlas dir not found: {}", dir.display()));
            return Ok(());
        }

        let mut metas: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| PipelineError::io(dir.display(), e))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "meta"))
            .collect();
        metas.sort();

        for meta in metas {
            let build = AtlasBuild::from_file(&meta)?;
            self.write_atlas(&meta, &build)?;
        }
        if self.report.pending_textures > 0 {
            self.report.warn(format!(
                "{} per-sprite `.data` payloads were copied verbatim; only packed atlases \
                 are decoded to PNG so far (see AGENTS.md 13)",
                self.report.pending_textures
            ));
        }
        Ok(())
    }

    /// `Graphics/Sprites.xml` -> `atlas/Gameplay.sprites.toml`.
    ///
    /// Vanilla `GFX.SpriteBank = new SpriteBank(GFX.Game, ...)`: all animations
    /// live in the Game atlas (Gameplay.png, the `GFX.Load` in
    /// the startup section of `pureCeleste.cs`); this engine's Game atlas name is exactly `Gameplay`.
    /// A missing file only warns (not every image has animation data to convert).
    fn convert_sprites(&mut self) -> Result<(), PipelineError> {
        let src = self.config.source.join("Graphics/Sprites.xml");
        let text = match std::fs::read_to_string(&src) {
            Ok(t) => t,
            Err(e) => {
                self.report
                    .warn(format!("Sprites.xml not found ({}): {}", src.display(), e));
                return Ok(());
            }
        };
        let defs = crate::sprites_xml::parse_sprites(&text).map_err(|e| {
            PipelineError::io(
                src.display(),
                std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()),
            )
        })?;
        let out_dir = self.safe_out(&PathBuf::from("atlas"))?;
        std::fs::create_dir_all(&out_dir).map_err(|e| PipelineError::io(out_dir.display(), e))?;
        let out_path = out_dir.join("Gameplay.sprites.toml");
        let toml = toml::to_string_pretty(&SpritesToml { sprite: defs }).map_err(|e| {
            PipelineError::Parse {
                format: "sprites.toml",
                path: out_path.display().to_string(),
                message: e.to_string(),
            }
        })?;
        std::fs::write(&out_path, &toml).map_err(|e| PipelineError::io(out_path.display(), e))?;
        self.report.sprites += 0; // an animation table is not a sprite count; keep the counting semantics unchanged
        Ok(())
    }

    fn write_atlas(&mut self, meta: &Path, build: &AtlasBuild) -> Result<(), PipelineError> {
        let stem = meta
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unnamed".into());

        let out_dir = self.safe_out(&PathBuf::from("atlas"))?;
        std::fs::create_dir_all(&out_dir).map_err(|e| PipelineError::io(out_dir.display(), e))?;

        // Write the pixels out as PNG first, then write the descriptor: the `kind` / `source` in the descriptor must point at
        // a real engine-format file, not the conversion input.
        let mut build = build.clone();
        for tex in &mut build.textures {
            self.write_texture_payload(&stem, tex)?;
        }

        // Descriptor file (our .atlas = TOML)
        let descriptor = self.atlas_descriptor(&stem, &build)?;
        let out_path = out_dir.join(format!("{stem}.atlas"));
        std::fs::write(&out_path, descriptor)
            .map_err(|e| PipelineError::io(out_path.display(), e))?;

        self.report.atlases += 1;
        self.report.sprites += build.sprite_count();
        Ok(())
    }

    /// Write one texture group's payload in engine format and rewrite `tex.source` to the output file.
    ///
    /// After conversion every group is either a PNG copied verbatim or a PNG decoded by this tool,
    /// so [`Self::atlas_descriptor`] only needs to handle `kind = "png"`.
    fn write_texture_payload(
        &mut self,
        stem: &str,
        tex: &mut AtlasTexture,
    ) -> Result<(), PipelineError> {
        let source = tex.source.clone();
        match source {
            // Standalone PNG: copy verbatim; `kind` is already `png`.
            TextureSource::Png(src) => {
                let rel = group_out_rel(stem, &tex.name, "png");
                if src.is_file() {
                    self.copy_exact(&src, &rel)?;
                } else {
                    self.report.warn(format!(
                        "atlas `{}`: declared PNG not found at {}",
                        tex.name,
                        src.display()
                    ));
                }
                tex.source = TextureSource::Png(PathBuf::from(&rel));
            }

            // PackerNoAtlas: one `.data` per sprite.
            // These pages are independent and have no usable layout (the rectangles in `.meta` overlap;
            // they are animation frame offsets), so combining them into a single page needs paging + runtime per-sprite texture binding;
            // for now we copy verbatim and keep the original kind (AGENTS.md 13).
            TextureSource::PerSpriteDir(_) => {
                for sprite in &tex.sprites {
                    let src = source.file_for_sprite(&sprite.name);
                    if !src.is_file() {
                        continue;
                    }
                    self.report.pending_textures += 1;
                    self.copy_exact(&src, &sprite_out_rel(stem, &tex.name, &sprite.name))?;
                }
            }

            // Packer / PackerNoAtlas single page: decode to PNG.
            data_source => {
                let src = data_source.path().to_path_buf();
                let decoded = data::decode_file(&src)?;
                let rel = group_out_rel(stem, &tex.name, "png");
                write_png(&self.safe_out(Path::new(&rel))?, &decoded)?;
                self.report.decoded_textures += 1;
                tex.source = TextureSource::Png(PathBuf::from(&rel));
            }
        }
        Ok(())
    }

    fn atlas_descriptor(&self, name: &str, build: &AtlasBuild) -> Result<String, PipelineError> {
        use serde::Serialize;

        #[derive(Serialize)]
        struct SpriteOut<'a> {
            name: &'a str,
            x: i16,
            y: i16,
            w: i16,
            h: i16,
            /// Draw origin (source-file offset negated, matching MTexture).
            origin_x: i16,
            origin_y: i16,
            frame_w: i16,
            frame_h: i16,
            /// Only used by `per_sprite_crunch`: this sprite's own payload path.
            #[serde(skip_serializing_if = "Option::is_none")]
            source: Option<String>,
        }

        #[derive(Serialize)]
        struct TextureOut<'a> {
            name: &'a str,
            /// `png` | `crunch` | `packed_crunch` | `per_sprite_crunch`
            kind: &'static str,
            /// Group-level payload path (the group directory for `per_sprite_crunch`).
            source: String,
            #[serde(skip_serializing_if = "Option::is_none")]
            width: Option<i32>,
            #[serde(skip_serializing_if = "Option::is_none")]
            height: Option<i32>,
            sprites: Vec<SpriteOut<'a>>,
        }

        #[derive(Serialize)]
        struct AtlasOut<'a> {
            name: &'a str,
            version: i32,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            links: Vec<(String, String)>,
            textures: Vec<TextureOut<'a>>,
        }

        let textures = build
            .textures
            .iter()
            .map(|t| {
                let per_sprite = matches!(t.source, TextureSource::PerSpriteDir(_));
                let source = if per_sprite {
                    // Group directory (one .data per sprite)
                    format!("atlas/{name}/{}", t.name)
                } else {
                    group_out_rel(name, &t.name, t.source.payload_ext())
                };

                TextureOut {
                    name: &t.name,
                    kind: t.source.kind(),
                    source,
                    width: t.width,
                    height: t.height,
                    sprites: t
                        .sprites
                        .iter()
                        .map(|s| SpriteOut {
                            name: &s.name,
                            x: s.x,
                            y: s.y,
                            w: s.w,
                            h: s.h,
                            origin_x: -s.origin_x,
                            origin_y: -s.origin_y,
                            frame_w: s.frame_w,
                            frame_h: s.frame_h,
                            source: per_sprite.then(|| sprite_out_rel(name, &t.name, &s.name)),
                        })
                        .collect(),
                }
            })
            .collect();

        let out = AtlasOut {
            name,
            version: build.version,
            links: build.links.clone(),
            textures,
        };

        toml::to_string_pretty(&out)
            .map_err(|e| PipelineError::Serialize(format!("atlas toml: {e}")))
    }

    /// Copy a file into the output directory at the given relative path (including extension).
    fn copy_exact(&self, from: &Path, out_rel: &str) -> Result<(), PipelineError> {
        let to = self.safe_out(Path::new(out_rel))?;
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| PipelineError::io(parent.display(), e))?;
        }
        std::fs::copy(from, &to).map_err(|e| PipelineError::io(to.display(), e))?;
        Ok(())
    }

    // ---------- Dialog ----------

    fn convert_dialogs(&mut self) -> Result<(), PipelineError> {
        let dir = self.config.source.join("Dialog");
        if !dir.is_dir() {
            self.report
                .warn(format!("dialog dir not found: {}", dir.display()));
            return Ok(());
        }

        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| PipelineError::io(dir.display(), e))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "txt"))
            .filter(|p| {
                // Skip intermediate products like `.txt.export`: only take direct .txt files
                p.file_name()
                    .map(|n| !n.to_string_lossy().ends_with(".export"))
                    .unwrap_or(false)
            })
            .collect();
        files.sort();

        for file in files {
            let doc = DialogDocument::from_file(&file)?;
            let stem = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".into());

            let toml_text = doc.to_toml()?;
            let rel = PathBuf::from(format!("dialog/{stem}.toml"));
            let out = self.safe_out(&rel)?;
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| PipelineError::io(parent.display(), e))?;
            }
            std::fs::write(&out, toml_text).map_err(|e| PipelineError::io(out.display(), e))?;

            self.report.dialogs += 1;
            self.report.dialog_entries += doc.len();
        }
        Ok(())
    }

    // ---------- Maps ----------

    fn convert_maps(&mut self) -> Result<(), PipelineError> {
        let dir = self.config.source.join("Maps");
        if !dir.is_dir() {
            self.report
                .warn(format!("maps dir not found: {}", dir.display()));
            return Ok(());
        }

        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| PipelineError::io(dir.display(), e))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "bin"))
            .collect();
        files.sort();

        for file in files {
            let map = MapBin::from_file(&file)?;
            let doc = self.convert_map(&map);
            let stem = file
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".into());

            let rel = PathBuf::from(format!("maps/{stem}.map"));
            let out = self.safe_out(&rel)?;
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| PipelineError::io(parent.display(), e))?;
            }

            let bytes = releste_map::to_bytes(&doc)?;
            std::fs::write(&out, bytes).map_err(|e| PipelineError::io(out.display(), e))?;

            self.report.maps += 1;
            self.report.rooms += doc.rooms.len();
        }
        Ok(())
    }

    /// Convert the Celeste map tree into this engine's [`MapDocument`].
    fn convert_map(&mut self, map: &MapBin) -> MapDocument {
        let area = map.name.clone();
        let area_id = area_id_of(&area);
        let mut rooms = Vec::new();
        // Area-level `<Style>`: clear color + parallax backgrounds (appended in v2).
        let style = convert_style(&map.root);

        for level in map.root.children_named("levels") {
            for lv in level.children_named("level") {
                let name = lv
                    .attr("name")
                    .and_then(BinValue::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("room_{}", rooms.len()));

                let width = lv.attr("width").and_then(BinValue::as_i64).unwrap_or(0) as f32;
                let height = lv.attr("height").and_then(BinValue::as_i64).unwrap_or(0) as f32;
                let x = lv.attr("x").and_then(BinValue::as_i64).unwrap_or(0) as f32;
                let y = lv.attr("y").and_then(BinValue::as_i64).unwrap_or(0) as f32;

                let tiles = lv
                    .child("solids")
                    .map(tileset_from_element)
                    .unwrap_or_default();
                let bg = lv.child("bg").map(tileset_from_element).unwrap_or_default();

                // The decoration layer uses the room's tile size as its grid (vanilla `ReadCSVIntGrid`
                // also opens the table at `LevelData.TileBounds` width/height, 77247-77253).
                let tile_w = ((width / TILE_PX).ceil().max(1.0)) as u32;
                let tile_h = ((height / TILE_PX).ceil().max(1.0)) as u32;
                let bg_scenery = lv
                    .child("bgtiles")
                    .map(|el| csv_tileset(el, tile_w, tile_h))
                    .unwrap_or_default();
                let fg_scenery = lv
                    .child("fgtiles")
                    .map(|el| csv_tileset(el, tile_w, tile_h))
                    .unwrap_or_default();

                let bg_decals = lv.child("bgdecals").map(convert_decals).unwrap_or_default();
                let fg_decals = lv.child("fgdecals").map(convert_decals).unwrap_or_default();

                let mut entities = Vec::new();
                if let Some(group) = lv.child("entities") {
                    for ent in &group.children {
                        let Some(entity) = convert_entity(&area, area_id, &name, ent) else {
                            continue;
                        };
                        entities.push(entity);
                    }
                }

                let mut triggers = Vec::new();
                if let Some(group) = lv.child("triggers") {
                    for tr in &group.children {
                        triggers.push(TriggerData {
                            kind: tr.name.clone(),
                            x: tr.attr("x").and_then(BinValue::as_f32).unwrap_or(0.0),
                            y: tr.attr("y").and_then(BinValue::as_f32).unwrap_or(0.0),
                            width: tr.attr("width").and_then(BinValue::as_f32).unwrap_or(0.0),
                            height: tr.attr("height").and_then(BinValue::as_f32).unwrap_or(0.0),
                            properties: tr
                                .attributes
                                .iter()
                                .filter_map(|(k, v)| {
                                    if matches!(
                                        k.as_str(),
                                        "x" | "y"
                                            | "width"
                                            | "height"
                                            | "originX"
                                            | "originY"
                                            | "id"
                                    ) {
                                        None
                                    } else {
                                        Some((k.clone(), value_to_string(v)))
                                    }
                                })
                                .collect(),
                        });
                    }
                }

                self.report.entities += entities.len();

                rooms.push(Room {
                    name,
                    bounds: RectDef {
                        x,
                        y,
                        w: width,
                        h: height,
                    },
                    tiles,
                    bg,
                    entities,
                    triggers,
                    script: None,
                    bg_decals,
                    fg_decals,
                    bg_scenery,
                    fg_scenery,
                });
            }
        }

        if rooms.is_empty() {
            self.report
                .warn(format!("map `{area}` converted to 0 rooms"));
        }

        MapDocument {
            area,
            rooms,
            // Not synchronized by default: if a map needs sync, the author declares it explicitly.
            sync_rules: Vec::new(),
            style,
        }
    }

    // ---------- Audio ----------

    fn copy_banks(&mut self) -> Result<(), PipelineError> {
        let dir = self.config.source.join("FMOD/Desktop");
        if !dir.is_dir() {
            self.report
                .warn(format!("FMOD dir not found: {}", dir.display()));
            return Ok(());
        }

        let mut banks: Vec<PathBuf> = walk(&dir, "bank")?;
        banks.sort();

        for bank in banks {
            let file_name = bank
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown.bank".into());
            let rel = PathBuf::from(format!("audio/{file_name}"));
            let out = self.safe_out(&rel)?;
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| PipelineError::io(parent.display(), e))?;
            }
            std::fs::copy(&bank, &out).map_err(|e| PipelineError::io(out.display(), e))?;
            self.report.banks_copied += 1;
            info!(bank = %file_name, "copied FMOD bank");
        }
        Ok(())
    }
}

/// Directory area ID (stable, used for `EntityId` generation).
fn area_id_of(area: &str) -> u32 {
    let mut h = 0u32;
    for b in area.bytes() {
        h = h.wrapping_mul(31).wrapping_add(u32::from(b));
    }
    h
}

/// Group-level payload path relative to the output directory.
///
/// Shared by the descriptor file and the copy logic, to guarantee the `source` field points at a real file.
fn group_out_rel(stem: &str, group: &str, ext: &str) -> String {
    format!("atlas/{stem}/{group}.{ext}")
}

/// Write the decode result as a PNG (creating directories first if needed).
fn write_png(to: &Path, tex: &DecodedTexture) -> Result<(), PipelineError> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| PipelineError::io(parent.display(), e))?;
    }
    let DecodedTexture {
        width,
        height,
        rgba,
    } = tex;
    let img = image::RgbaImage::from_raw(*width, *height, rgba.clone()).ok_or_else(|| {
        PipelineError::parse(
            "data",
            to.display(),
            "RGBA buffer does not match the declared dimensions",
        )
    })?;
    img.save_with_format(to, image::ImageFormat::Png)
        .map_err(|source| PipelineError::Png {
            path: to.display().to_string(),
            source,
        })
}

/// Relative path of a `per_sprite_crunch` single-sprite payload.
fn sprite_out_rel(stem: &str, group: &str, sprite: &str) -> String {
    format!("atlas/{stem}/{group}/{sprite}.data")
}

/// Celeste tile alphabet: `0` is empty, everything else is tile index + 1.
const TILE_ALPHABET: &str = "0123456789abcdefghijklmnopqrstuvwxyz";

/// Build a [`Tileset`] from the `innerText` of a `<solids>` / `<bg>` element.
fn tileset_from_element(el: &MapBinElement) -> Tileset {
    let atlas = el
        .attr("tileset")
        .and_then(BinValue::as_str)
        .unwrap_or("Gameplay")
        .to_string();

    let text = el
        .attr("innerText")
        .and_then(BinValue::as_str)
        .unwrap_or("");
    let rows: Vec<&str> = text.split('\n').collect();

    // Drop the trailing empty lines (Celeste's innerText often ends with several newlines).
    let rows: Vec<&str> = {
        let mut r = rows;
        while r.len() > 1 && r.last().is_some_and(|s| s.trim().is_empty()) {
            r.pop();
        }
        r
    };

    let height = rows.len() as u32;
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
    let mut tiles = vec![0u16; (width as usize) * (height as usize)];

    for (y, row) in rows.iter().enumerate() {
        for (x, ch) in row.chars().enumerate() {
            let Some(pos) = TILE_ALPHABET.find(ch) else {
                continue;
            };
            // In the source format '0' is empty; '1' means tile number 1.
            tiles[y * width as usize + x] = pos as u16;
        }
    }

    Tileset {
        atlas,
        width,
        height,
        tiles,
    }
}

/// Pixel edge length of one tile (all vanilla `TileGrid`s are 8x8).
const TILE_PX: f32 = 8.0;

/// Build a decoration layer [`Tileset`] from a `bgtiles` / `fgtiles` element (a CSV integer grid).
///
/// Vanilla `Calc.ReadCSVIntGrid(csv, level.TileBounds.W, TileBounds.H)`
/// (`pureCeleste.cs:20842-20862`) first fills the whole `width x height` table with `-1`,
/// then overwrites it row by row via `Split('\n')` and within a row via `Split(',')` (dropping empty entries); missing rows, missing columns
/// and empty rows all stay `-1`. Then `TileGrid.Overlay` only draws when `tiles[i,j] >= 0`
/// (`11274-11285`), and the value is the **flat index** of `Tileset`
/// `tiles[index % columns, index / columns]` (`15215-15226`).
///
/// The engine convention is `0` = empty (see `format::Tileset`), so here `-1` is squashed to `0` and
/// the original value `v` is stored as `v + 1`; the render side recovers the index as `stored value - 1`.
///
/// `atlas` takes the element's own `tileset` attribute: the decoration layer uses `tilesets/scenery`
/// (`110118` `new Tileset(Game["tilesets/scenery"], 8, 8)`),
/// which is not the same table as `Gameplay` for `solids`/`bg`.
fn csv_tileset(el: &MapBinElement, width: u32, height: u32) -> Tileset {
    let atlas = el
        .attr("tileset")
        .and_then(BinValue::as_str)
        .unwrap_or("Scenery")
        .to_string();
    let text = el
        .attr("innerText")
        .and_then(BinValue::as_str)
        .unwrap_or("");
    let mut tiles = vec![0u16; (width as usize) * (height as usize)];
    let mut overflow = 0usize;

    for (y, row) in text.split('\n').enumerate() {
        if y >= height as usize {
            break; // vanilla: k < height && k < array2.Length
        }
        // `RemoveEmptyEntries`: empty strings are dropped and later values shift left (same as vanilla).
        for (x, cell) in row.split(',').filter(|c| !c.is_empty()).enumerate() {
            if x >= width as usize {
                break; // vanilla: l < width && l < array3.Length
            }
            let Ok(value) = cell.parse::<i32>() else {
                warn!(
                    row = y,
                    col = x,
                    cell,
                    "non-integer cell in overlay CSV; treating as empty"
                );
                continue;
            };
            if value < 0 {
                continue; // -1 = "this cell is not covered"
            }
            // u16 can hold the whole scenery table (360 cells, max index 352); if it still
            // overflows, drop it and warn -- `v + 1` must never wrap around into another table.
            let Some(encoded) = value.checked_add(1).filter(|e| *e <= i32::from(u16::MAX)) else {
                overflow += 1;
                continue;
            };
            tiles[y * width as usize + x] = encoded as u16;
        }
    }

    if overflow > 0 {
        warn!(
            atlas = %atlas,
            cells = overflow,
            "overlay tile index does not fit in u16; those cells were dropped"
        );
    }

    Tileset {
        atlas,
        width,
        height,
        tiles,
    }
}

/// Convert `<fgdecals>` / `<bgdecals>` groups into a [`Decal`] list.
///
/// Each vanilla `DecalData` only has `texture` / `x` / `y` / `scaleX` / `scaleY`
/// (`pureCeleste.cs:113199-113206`); the depth is decided by the group it is in
/// (fg `-10500` / bg `9000`, `115079-115086`).
fn convert_decals(group: &MapBinElement) -> Vec<Decal> {
    group
        .children
        .iter()
        .filter_map(|el| {
            let Some(texture) = el.attr("texture").and_then(BinValue::as_str) else {
                warn!("decal without a `texture` attribute; skipping it");
                return None;
            };
            Some(Decal {
                texture: decal_texture(texture),
                x: el.attr("x").and_then(BinValue::as_f32).unwrap_or(0.0),
                y: el.attr("y").and_then(BinValue::as_f32).unwrap_or(0.0),
                scale_x: el.attr("scaleX").and_then(BinValue::as_f32).unwrap_or(1.0),
                scale_y: el.attr("scaleY").and_then(BinValue::as_f32).unwrap_or(1.0),
            })
        })
        .collect()
}

/// Decal texture name -> atlas key (`decals/` + relative path).
///
/// Follows the vanilla `Decal` constructor (`pureCeleste.cs:88573-88582`):
/// `Path.Combine("decals", name without extension).Replace('\\', '/')`.
///
/// Vanilla then has a final `Regex.Replace(name, "\\d+$", "")` that strips trailing digits before
/// `GetAtlasSubtextures` grabs the whole frame group for animation. This engine's atlas is **exact per-frame keys**
/// with no sub-atlas groups, so trailing digits are kept (`flag00` is stored as `decals/.../flag00`);
/// stripping digits + group animation is left for when there are multi-frame decals.
fn decal_texture(raw: &str) -> String {
    let stem = match raw.rsplit_once('.') {
        // The suffix must be a real extension (no path separator), otherwise the `.` belongs to the path.
        Some((stem, ext)) if !ext.is_empty() && !ext.contains(['/', '\\']) => stem,
        _ => raw,
    };
    format!("decals/{}", stem.replace('\\', "/"))
}

/// `<Style>` -> [`MapStyle`].
///
/// Vanilla only recognizes three things: the `color` attribute -> clear color (default `Color.Black`,
/// `pureCeleste.cs:112310` / `112432-112435`), `<Backgrounds>` ->
/// `MapData.Background`, `<Foregrounds>` -> `MapData.Foreground`
/// (`112428-112449`). When `<Style>` is missing the whole thing takes the vanilla default (black background, no backgrounds).
fn convert_style(root: &MapBinElement) -> MapStyle {
    let Some(style) = root.child("Style") else {
        return MapStyle::default();
    };

    // Room name table: the wildcards in `only=` / `exclude=` are expanded at **conversion time** into concrete
    // room names (vanilla `ParseLevelsList`, `pureCeleste.cs:113019-113036`,
    // which matches each of `Levels` and collects the results). The order in which names are collected here matches
    // the levels->level loop of `convert_map`, and a missing name uses the same
    // `room_{n}` fallback, so both sides point at the same room.
    let mut room_names = Vec::new();
    for level in root.children_named("levels") {
        for lv in level.children_named("level") {
            room_names.push(
                lv.attr("name")
                    .and_then(BinValue::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("room_{}", room_names.len())),
            );
        }
    }

    let mut out = MapStyle::default();
    if let Some(hex) = style.attr("color").and_then(BinValue::as_str) {
        out.background_color = match hex_rgb(hex) {
            Some(rgb) => rgb,
            None => {
                warn!(
                    color = hex,
                    "unparsable `<Style color>`; falling back to black"
                );
                [0, 0, 0]
            }
        };
    }
    if let Some(group) = style.child("Backgrounds") {
        out.backgrounds = convert_backdrops(group, &room_names);
    }
    if let Some(group) = style.child("Foregrounds") {
        out.foregrounds = convert_backdrops(group, &room_names);
    }
    out
}

/// `<Backgrounds>` / `<Foregrounds>` -> [`Backdrop`] list.
///
/// Vanilla `CreateBackdrops` (`pureCeleste.cs:112593-112618`): when a child element is named
/// `apply`, its **children** inherit `apply`'s attributes (`ParseBackdrop(child2, child)`),
/// otherwise it is parsed as `(child, null)`.
fn convert_backdrops(group: &MapBinElement, room_names: &[String]) -> Vec<Backdrop> {
    let mut out = Vec::new();
    for child in &group.children {
        if child.name.eq_ignore_ascii_case("apply") {
            for nested in &child.children {
                out.push(parse_backdrop(nested, Some(child), room_names));
            }
        } else {
            out.push(parse_backdrop(child, None, room_names));
        }
    }
    out
}

/// One background element -> [`Backdrop`].
///
/// Aligns attribute by attribute with vanilla `ParseBackdrop` (`pureCeleste.cs:112648-112900`):
/// each attribute is "look at itself first; if absent, look at `above` (the `<apply>` parent)";
/// only if neither has it, fall back to the `Backdrop` field default (`109341-109430`).
///
/// Color goes through `backdrop.Color = HexToColor(color)` then `Color *= alpha`
/// (`112788-112803`) -- XNA's `Color * f32` multiplies **all four channels**,
/// so the stored RGBA is `[r*a, g*a, b*a, 255*a]`, which is exactly the pass-through (non-premultiplied)
/// RGBA this engine's `SpriteDraw` needs; the renderer multiplies alpha once more
/// to reproduce vanilla `SpriteBatch`'s `SourceAlpha` blending.
fn parse_backdrop(
    child: &MapBinElement,
    above: Option<&MapBinElement>,
    room_names: &[String],
) -> Backdrop {
    let kind = child.name.to_ascii_lowercase();
    let mut backdrop = Backdrop::new(&kind);

    if let Some(v) = attr_or(child, above, "texture").and_then(BinValue::as_str) {
        backdrop.texture = v.to_string();
    }
    if let Some(v) = attr_or(child, above, "atlas").and_then(BinValue::as_str) {
        // Vanilla `Attr("atlas", "game")`; compared lowercased (112628-112631).
        backdrop.atlas = v.to_ascii_lowercase();
    }

    backdrop.x = attr_or(child, above, "x")
        .and_then(BinValue::as_f32)
        .unwrap_or(0.0);
    backdrop.y = attr_or(child, above, "y")
        .and_then(BinValue::as_f32)
        .unwrap_or(0.0);
    backdrop.scroll_x = attr_or(child, above, "scrollx")
        .and_then(BinValue::as_f32)
        .unwrap_or(1.0);
    backdrop.scroll_y = attr_or(child, above, "scrolly")
        .and_then(BinValue::as_f32)
        .unwrap_or(1.0);
    backdrop.speed_x = attr_or(child, above, "speedx")
        .and_then(BinValue::as_f32)
        .unwrap_or(0.0);
    backdrop.speed_y = attr_or(child, above, "speedy")
        .and_then(BinValue::as_f32)
        .unwrap_or(0.0);
    backdrop.loop_x = attr_or(child, above, "loopx")
        .and_then(BinValue::as_bool)
        .unwrap_or(true);
    backdrop.loop_y = attr_or(child, above, "loopy")
        .and_then(BinValue::as_bool)
        .unwrap_or(true);
    backdrop.flip_x = attr_or(child, above, "flipx")
        .and_then(BinValue::as_bool)
        .unwrap_or(false);
    backdrop.flip_y = attr_or(child, above, "flipy")
        .and_then(BinValue::as_bool)
        .unwrap_or(false);
    backdrop.wind = attr_or(child, above, "wind")
        .and_then(BinValue::as_f32)
        .unwrap_or(0.0);
    backdrop.blend = attr_or(child, above, "blendmode")
        .and_then(BinValue::as_str)
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "alphablend".to_string());

    // Room filtering: `exclude=` / `only=` (112855-112873); wildcards are expanded at conversion
    // time into concrete room names (ParseLevelsList, 113019-113036).
    if let Some(list) = attr_or(child, above, "exclude").and_then(BinValue::as_str) {
        backdrop.exclude_from = expand_levels_list(list, room_names);
    }
    if let Some(list) = attr_or(child, above, "only").and_then(BinValue::as_str) {
        backdrop.only_in = expand_levels_list(list, room_names);
    }
    // Transition visibility: `instantIn=` (default true) / `instantOut=` (default false)
    // (the field defaults at 109411-109418, overridden by the attributes at 112895-112905).
    backdrop.instant_in = attr_or(child, above, "instantIn")
        .and_then(BinValue::as_bool)
        .unwrap_or(true);
    backdrop.instant_out = attr_or(child, above, "instantOut")
        .and_then(BinValue::as_bool)
        .unwrap_or(false);
    // Session attributes not yet ported: they need Session flags / dream state / gradient band width; name them
    // and warn first (must not be dropped silently, AGENTS.md 15.9).
    for attr in ["flag", "notflag", "always", "dreaming", "fadex", "fadey"] {
        if attr_or(child, above, attr).is_some() {
            warn!(attribute = attr, texture = %backdrop.texture,
                  "backdrop session attribute is not supported yet; ignoring it");
        }
    }

    // Start with `Color = White`; a `color` overrides it, then `Color *= alpha`.
    let (r, g, b) = match attr_or(child, above, "color").and_then(BinValue::as_str) {
        Some(hex) => match hex_rgb(hex) {
            Some(rgb) => (rgb[0], rgb[1], rgb[2]),
            None => {
                warn!(
                    color = hex,
                    "unparsable backdrop `color`; falling back to white"
                );
                (255, 255, 255)
            }
        },
        None => (255, 255, 255),
    };
    let alpha = attr_or(child, above, "alpha")
        .and_then(BinValue::as_f32)
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);
    backdrop.color = [
        (f32::from(r) * alpha).round() as u8,
        (f32::from(g) * alpha).round() as u8,
        (f32::from(b) * alpha).round() as u8,
        (255.0 * alpha).round() as u8,
    ];
    if backdrop.color == [0, 0, 0, 0] {
        // `Backdrop::effective_color()` treats all-zero as "never set" -> vanilla default white.
        // `alpha = 0` is truly fully transparent (vanilla `color.A <= 1` simply does not draw),
        // so use 1/255 to override the sentinel; the render side still draws nothing.
        backdrop.color = [0, 0, 0, 1];
    }

    backdrop
}

/// The room list from `only=` / `exclude=` -> a set of concrete room names.
///
/// Vanilla `ParseLevelsList` (`pureCeleste.cs:113019-113036`): comma-separated;
/// entries containing `*` are matched against `Levels` one by one with a `^` + escaped + `.*` + `$` regex and
/// collected; the rest are collected literally. Here we use an equivalent wildcard match instead of a regex
/// (after `Regex.Escape` the only character with special meaning is the `*` we substitute), and the output is sorted
/// to guarantee determinism (membership semantics match vanilla's `HashSet`).
fn expand_levels_list(list: &str, room_names: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for piece in list.split(',') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        if piece.contains('*') {
            for name in room_names {
                if wildcard_match(piece, name) && !out.contains(name) {
                    out.push(name.clone());
                }
            }
        } else if !out.iter().any(|n| n == piece) {
            out.push(piece.to_string());
        }
    }
    out.sort();
    out
}

/// `*` wildcard matching: equivalent to vanilla `^` + `Regex.Escape(p).Replace("\\*", ".*")` + `$`.
fn wildcard_match(pattern: &str, name: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !name.starts_with(first) || !name.ends_with(last) {
        return false;
    }
    if name.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &name[first.len()..name.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

/// Attribute: look at itself first, then the `<apply>` parent element (vanilla falls back this way for every attribute).
fn attr_or<'a>(
    child: &'a MapBinElement,
    above: Option<&'a MapBinElement>,
    key: &str,
) -> Option<&'a BinValue> {
    child.attr(key).or_else(|| above.and_then(|a| a.attr(key)))
}

/// `"rgb"` / `"rrggbb"` -> RGB. Corresponds to vanilla `Calc.HexToColor`.
fn hex_rgb(hex: &str) -> Option<[u8; 3]> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() == 3 {
        let bytes = hex.as_bytes();
        let half = |c: u8| {
            let d = (c as char).to_digit(16)? as u8;
            Some(d * 16 + d)
        };
        return Some([half(bytes[0])?, half(bytes[1])?, half(bytes[2])?]);
    }
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

/// Convert map entity elements into this engine's [`EntityData`].
fn convert_entity(area: &str, area_id: u32, room: &str, el: &MapBinElement) -> Option<EntityData> {
    let kind = el.name.clone();
    // Exclude the grouping elements that are clearly not entities.
    if matches!(kind.as_str(), "entities" | "triggers" | "level") {
        return None;
    }

    let x = el.attr("x").and_then(BinValue::as_f32).unwrap_or(0.0);
    let y = el.attr("y").and_then(BinValue::as_f32).unwrap_or(0.0);
    let width = el
        .attr("width")
        .and_then(BinValue::as_i64)
        .map(|v| v as i32);
    let height = el
        .attr("height")
        .and_then(BinValue::as_i64)
        .map(|v| v as i32);
    let origin_x = el.attr("originX").and_then(BinValue::as_f32).unwrap_or(0.0);
    let origin_y = el.attr("originY").and_then(BinValue::as_f32).unwrap_or(0.0);

    // Stable ID: same rule as the runtime (area_id || room || kind || x || y).
    let kind_id = area_id_of(&kind) ^ 0x9e37_79b9;
    let id = EntityId::generate(area_id, room, kind_id, x as i32, y as i32);

    let mut properties = BTreeMap::new();
    for (key, value) in &el.attributes {
        if matches!(
            key.as_str(),
            "x" | "y" | "width" | "height" | "originX" | "originY"
        ) {
            continue;
        }
        properties.insert(key.clone(), value_to_field(value));
    }

    // Nodes: vanilla attaches every `<node x= y= />` child element as a waypoint of the entity
    // (the `wire` endpoint, the `introCrusher` landing point, the `bridge` piers...).
    let mut nodes = Vec::new();
    for child in &el.children {
        if !child.name.eq_ignore_ascii_case("node") {
            continue;
        }
        let (Some(nx), Some(ny)) = (
            child.attr("x").and_then(BinValue::as_f32),
            child.attr("y").and_then(BinValue::as_f32),
        ) else {
            warn!(
                room = %room,
                kind = %kind,
                "`node` child without x/y; skipping this node"
            );
            continue;
        };
        nodes.push((nx, ny));
    }

    let _ = area; // area is only used for the area_id computation

    Some(EntityData {
        id,
        kind,
        x,
        y,
        width,
        height,
        origin_x,
        origin_y,
        properties: properties.into_iter().collect(),
        nodes,
    })
}

/// Any value -> an engine property value.
fn value_to_field(value: &BinValue) -> FieldValue {
    match value {
        BinValue::Bool(b) => FieldValue::Bool(*b),
        BinValue::Byte(b) => FieldValue::Int(i32::from(*b)),
        BinValue::Short(s) => FieldValue::Int(i32::from(*s)),
        BinValue::Int(i) => FieldValue::Int(*i),
        BinValue::Float(f) => {
            // Most Celeste numeric attributes are actually floating point with integer semantics.
            if f.fract() == 0.0 && f.abs() < i32::MAX as f32 {
                FieldValue::Int(*f as i32)
            } else {
                FieldValue::Float(*f)
            }
        }
        BinValue::String(s) | BinValue::RleString(s) => FieldValue::String(s.clone()),
    }
}

/// Any value -> a string (used for trigger properties).
fn value_to_string(value: &BinValue) -> String {
    match value {
        BinValue::Bool(b) => b.to_string(),
        BinValue::Byte(b) => b.to_string(),
        BinValue::Short(s) => s.to_string(),
        BinValue::Int(i) => i.to_string(),
        BinValue::Float(f) => f.to_string(),
        BinValue::String(s) | BinValue::RleString(s) => s.clone(),
    }
}

/// Recursively collect files with the given extension.
fn walk(dir: &Path, ext: &str) -> Result<Vec<PathBuf>, PipelineError> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let entries = std::fs::read_dir(&cur).map_err(|e| PipelineError::io(cur.display(), e))?;
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == ext) {
                out.push(path);
            }
        }
    }
    Ok(out)
}

/// Default sync rule (an example for map authors to reference).
///
/// **Not synchronized by default**: this returns empty; synchronization must be declared explicitly by the map.
pub fn default_sync_rules() -> Vec<SyncRule> {
    Vec::new()
}

/// Reference: one explicit sync rule.
pub fn example_sync_rule(kind: &str, flag: &str) -> SyncRule {
    SyncRule {
        kind: kind.to_string(),
        mode: SyncMode::Flag,
        flag: Some(flag.to_string()),
    }
}

/// Top-level structure of `Gameplay.sprites.toml`.
#[derive(serde::Serialize)]
struct SpritesToml {
    sprite: Vec<crate::sprites_xml::SpriteDef>,
}
