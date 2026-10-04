//! 管线编排：遍历 `assets-src/`，转换到 `assets/`。
//!
//! 转换映射见 AGENTS.md §12。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use reles_map::{MapDocument, RectDef, Room, SyncMode, SyncRule, Tileset, TriggerData};
use reles_world::{EntityData, EntityId, FieldValue};
use tracing::{info, warn};

use crate::atlas::{read_data_header, AtlasBuild, TextureSource};
use crate::dialog::DialogDocument;
use crate::error::PipelineError;
use crate::mapbin::{BinValue, MapBin};

/// 管线配置。
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// 源目录（原版 Celeste 资源）。
    pub source: PathBuf,
    /// 输出目录。
    pub output: PathBuf,
    /// 只要转换这些分类（空 = 全部）。
    pub only: Vec<String>,
}

/// 一次运行的报告。
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
    /// 需要 Crunch 解码的纹理数（AGENTS.md §13 待办）。
    pub crunch_textures: usize,
    pub warnings: Vec<String>,
}

impl PipelineReport {
    fn warn(&mut self, msg: String) {
        warn!("{msg}");
        self.warnings.push(msg);
    }
}

/// 管线。
pub struct Pipeline {
    config: PipelineConfig,
    report: PipelineReport,
}

impl Pipeline {
    /// 新建。
    pub fn new(config: PipelineConfig) -> Self {
        Pipeline {
            config,
            report: PipelineReport::default(),
        }
    }

    /// 运行全部分类。
    pub fn run(&mut self) -> Result<PipelineReport, PipelineError> {
        std::fs::create_dir_all(&self.config.output)
            .map_err(|e| PipelineError::io(self.config.output.display(), e))?;

        if self.wants("atlas") {
            self.convert_atlases()?;
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

    /// 保证输出路径不逃逸出输出目录。
    fn safe_out(&self, rel: &Path) -> Result<PathBuf, PipelineError> {
        let joined = self.config.output.join(rel);
        // 归一化：拒绝 `..` 组件。
        for comp in rel.components() {
            if matches!(comp, std::path::Component::ParentDir) {
                return Err(PipelineError::PathEscape(rel.display().to_string()));
            }
        }
        Ok(joined)
    }

    // ---------- Atlases ----------

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
        Ok(())
    }

    fn write_atlas(&mut self, meta: &Path, build: &AtlasBuild) -> Result<(), PipelineError> {
        let stem = meta
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unnamed".into());

        let out_dir = self.safe_out(&PathBuf::from("atlas"))?;
        std::fs::create_dir_all(&out_dir).map_err(|e| PipelineError::io(out_dir.display(), e))?;

        // 描述文件（我们的 .atlas = TOML）
        let descriptor = self.atlas_descriptor(&stem, build)?;
        let out_path = out_dir.join(format!("{stem}.atlas"));
        std::fs::write(&out_path, descriptor)
            .map_err(|e| PipelineError::io(out_path.display(), e))?;

        // 复制纹理载荷
        for tex in &build.textures {
            match &tex.source {
                TextureSource::PerSpriteDir(_) => {
                    // PackerNoAtlas：每个 sprite 一个文件。
                    for sprite in &tex.sprites {
                        let src = tex.source.file_for_sprite(&sprite.name);
                        if !src.is_file() {
                            continue;
                        }
                        self.note_crunch(&src)?;
                        let rel = sprite_out_rel(&stem, &tex.name, &sprite.name);
                        self.copy_exact(&src, &rel)?;
                    }
                }
                shared => {
                    let src = shared.path();
                    if !src.is_file() {
                        continue;
                    }
                    if shared.is_crunch() {
                        self.note_crunch(src)?;
                    }
                    let rel = group_out_rel(&stem, &tex.name, shared.payload_ext());
                    self.copy_exact(src, &rel)?;
                }
            }
        }

        self.report.atlases += 1;
        self.report.sprites += build.sprite_count();
        Ok(())
    }

    /// 记录一个 Crunch 载荷并（首次）发出说明性警告。
    fn note_crunch(&mut self, path: &Path) -> Result<(), PipelineError> {
        self.report.crunch_textures += 1;
        if self.report.crunch_textures == 1 {
            let size = read_data_header(path)
                .map(|(w, h)| format!("{w}x{h}"))
                .unwrap_or_else(|_| "unknown".to_string());
            self.report.warn(format!(
                "Celeste `.data` textures are Crunch-compressed; payloads are copied \
                 but pixel decoding is not implemented (see AGENTS.md §13). \
                 First affected texture is {size}."
            ));
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
            /// 绘制原点（源文件 offset 取负，与 MTexture 一致）。
            origin_x: i16,
            origin_y: i16,
            frame_w: i16,
            frame_h: i16,
            /// 仅 `per_sprite_crunch` 用：该 sprite 自己的载荷路径。
            #[serde(skip_serializing_if = "Option::is_none")]
            source: Option<String>,
        }

        #[derive(Serialize)]
        struct TextureOut<'a> {
            name: &'a str,
            /// `png` | `crunch` | `packed_crunch` | `per_sprite_crunch`
            kind: &'static str,
            /// 组级载荷路径（`per_sprite_crunch` 时为组目录）。
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
                    // 组目录（每个 sprite 一个 .data）
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

    /// 按给定的相对路径（含扩展名）复制文件到输出目录。
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
                // 跳过 `.txt.export` 之类的中间产物：仅取直接 .txt
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

            let bytes = reles_map::to_bytes(&doc)?;
            std::fs::write(&out, bytes).map_err(|e| PipelineError::io(out.display(), e))?;

            self.report.maps += 1;
            self.report.rooms += doc.rooms.len();
        }
        Ok(())
    }

    /// 把 Celeste 地图树转换为本引擎的 [`MapDocument`]。
    fn convert_map(&mut self, map: &MapBin) -> MapDocument {
        let area = map.name.clone();
        let area_id = area_id_of(&area);
        let mut rooms = Vec::new();

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
            // 默认不同步：地图若需要同步，由作者显式声明。
            sync_rules: Vec::new(),
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

/// 目录区域 ID（稳定，用于 `EntityId` 生成）。
fn area_id_of(area: &str) -> u32 {
    let mut h = 0u32;
    for b in area.bytes() {
        h = h.wrapping_mul(31).wrapping_add(u32::from(b));
    }
    h
}

/// 组级载荷在输出目录中的相对路径。
///
/// 描述文件与复制逻辑共用，保证 `source` 字段指向真实文件。
fn group_out_rel(stem: &str, group: &str, ext: &str) -> String {
    format!("atlas/{stem}/{group}.{ext}")
}

/// `per_sprite_crunch` 单 sprite 载荷的相对路径。
fn sprite_out_rel(stem: &str, group: &str, sprite: &str) -> String {
    format!("atlas/{stem}/{group}/{sprite}.data")
}

/// Celeste tile 字母表：`0` 为空，其余为 tile 索引 + 1。
const TILE_ALPHABET: &str = "0123456789abcdefghijklmnopqrstuvwxyz";

/// 从 `<solids>` / `<bg>` 元素的 `innerText` 构造 [`Tileset`]。
fn tileset_from_element(el: &crate::mapbin::MapBinElement) -> Tileset {
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

    // 去掉行尾的空行（Celeste 的 innerText 常以若干换行结尾）。
    let rows: Vec<&str> = {
        let mut r = rows;
        while r.len() > 1 && r.last().is_some_and(|s| s.trim().is_empty()) {
            r.pop();
        }
        r
    };

    let height = rows.len() as u32;
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
    let mut tiles = vec![0u8; (width as usize) * (height as usize)];

    for (y, row) in rows.iter().enumerate() {
        for (x, ch) in row.chars().enumerate() {
            let Some(pos) = TILE_ALPHABET.find(ch) else {
                continue;
            };
            // 源格式里 '0' 是空；'1' 表示第 1 号 tile。
            tiles[y * width as usize + x] = pos as u8;
        }
    }

    Tileset {
        atlas,
        width,
        height,
        tiles,
    }
}

/// 把地图实体元素转换成本引擎的 [`EntityData`]。
fn convert_entity(
    area: &str,
    area_id: u32,
    room: &str,
    el: &crate::mapbin::MapBinElement,
) -> Option<EntityData> {
    let kind = el.name.clone();
    // 排除明显非实体的分组元素。
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

    // 稳定 ID：与运行时规则一致（area_id || room || kind || x || y）。
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

    let _ = area; // area 仅用于 area_id 计算

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
    })
}

/// 任意值 → 引擎属性值。
fn value_to_field(value: &BinValue) -> FieldValue {
    match value {
        BinValue::Bool(b) => FieldValue::Bool(*b),
        BinValue::Byte(b) => FieldValue::Int(i32::from(*b)),
        BinValue::Short(s) => FieldValue::Int(i32::from(*s)),
        BinValue::Int(i) => FieldValue::Int(*i),
        BinValue::Float(f) => {
            // 多数 Celeste 数值属性其实是整数语义的浮点。
            if f.fract() == 0.0 && f.abs() < i32::MAX as f32 {
                FieldValue::Int(*f as i32)
            } else {
                FieldValue::Float(*f)
            }
        }
        BinValue::String(s) | BinValue::RleString(s) => FieldValue::String(s.clone()),
    }
}

/// 任意值 → 字符串（触发器属性用）。
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

/// 递归收集指定扩展名的文件。
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

/// 默认同步规则（供地图作者参考的示例）。
///
/// **默认不同步**：这里返回空，同步必须由地图显式声明。
pub fn default_sync_rules() -> Vec<SyncRule> {
    Vec::new()
}

/// 参考：一个显式同步规则。
pub fn example_sync_rule(kind: &str, flag: &str) -> SyncRule {
    SyncRule {
        kind: kind.to_string(),
        mode: SyncMode::Flag,
        flag: Some(flag.to_string()),
    }
}
