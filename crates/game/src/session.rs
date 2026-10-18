//! 一张已加载的 `.map` + 它的图集，以及「这帧该画什么」的静态部分。
//!
//! 地图是数据（AGENTS.md §4.2）：这里只把瓦片与实体翻成
//! [`SpriteDraw`]，不含任何玩法分支。

use std::cell::RefCell;
use std::collections::HashSet;

use anyhow::{bail, Context as _, Result};
use releste_map::{MapDocument, Room};
use releste_math::Vec2;
use releste_render::atlas::SpriteAtlas;
use releste_render::{GpuRenderer, Layer, SpriteDraw};
use tracing::{debug, warn};

use crate::paths::Paths;
use crate::sprites;

/// 一格瓦片的世界像素数（`lvl_-1` 宽 320 / 40 格 = 8px）。
const TILE: f32 = sprites::TILE_PX as f32;

/// 背景瓦片的色调（保持 alpha=1，纹理是预乘 alpha 的）。
const BG_TINT: [f32; 4] = [0.55, 0.6, 0.7, 1.0];
/// 前景瓦片 / 实体的色调。
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// 已加载的地图会话。
pub struct Session {
    /// 地图文档（热重载会整体替换它）。
    pub doc: MapDocument,
    /// 该地图使用的图集。
    pub atlas: SpriteAtlas,
    /// 图集名（来自 `Tileset.atlas`）。
    pub atlas_name: String,
    /// tileset 定义（content-pipeline 从 `ForegroundTiles.xml` 转换）。
    pub tilesets: releste_map::TilesetsDoc,
    /// 玩家出生点（世界坐标，= hitbox 左上角）。
    pub spawn: Vec2,
    /// 出生点所在房间。
    pub spawn_room: String,
    /// 只告警一次的 key，避免每帧刷屏。
    warned: RefCell<HashSet<String>>,
}

impl Session {
    /// 从 `paths` 解析出的地图路径加载。
    ///
    /// `tileset` 已废弃（真 tileset 按 tile id 从 `ForegroundTiles.xml`
    /// 转换的定义里来）；保留参数以兼容调用方，忽略它。
    pub fn load(map: &str, tileset: Option<&str>, paths: &Paths) -> Result<Self> {
        let _ = tileset; // 已废弃，见上
        let map_path = paths.map(map);
        let doc = releste_map::load_map(&map_path)
            .with_context(|| format!("failed to load map {}", map_path.display()))?;
        if doc.rooms.is_empty() {
            bail!("map {} has no rooms", map_path.display());
        }

        let atlas_name = doc
            .rooms
            .first()
            .map(|r| r.tiles.atlas.clone())
            .unwrap_or_else(|| "Gameplay".to_string());
        let atlas_path = paths.atlas(&atlas_name);
        let atlas = SpriteAtlas::load(&atlas_path, paths.atlas_dir()).with_context(|| {
            format!(
                "failed to load atlas {} (root {})",
                atlas_path.display(),
                paths.atlas_dir().display()
            )
        })?;

        let tilesets_path = paths.tilesets();
        let tilesets = releste_map::TilesetsDoc::from_file(&tilesets_path).with_context(|| {
            format!(
                "failed to load tileset definitions {}",
                tilesets_path.display()
            )
        })?;

        let (spawn, spawn_room) = first_spawn(&doc);
        if spawn_room.is_empty() {
            warn!("map has no `player` entity; spawning at the first room origin");
        }

        info_load(
            &doc,
            &atlas_name,
            tilesets.tilesets.len(),
            spawn,
            &spawn_room,
        );

        Ok(Session {
            doc,
            atlas_name,
            atlas,
            tilesets,
            spawn,
            spawn_room,
            warned: RefCell::default(),
        })
    }

    /// 世界坐标落在哪个房间（找不到就回落到出生房间）。
    ///
    /// `pos` 是 hitbox **左上角**；这里取它的几何中心去查房间。
    /// 中心而不是角点：角点会提前半个身位跨进相邻房间，让碰撞、
    /// 标题栏房间名和重载检查都指向错误的房间。
    pub fn room_at(&self, pos: Vec2) -> &Room {
        room_containing(&self.doc.rooms, pos)
            .or_else(|| self.doc.rooms.iter().find(|r| r.name == self.spawn_room))
            .unwrap_or(&self.doc.rooms[0])
    }

    /// 世界单元 `(cx, cy)` 是否是固体。
    ///
    /// 房间格子是房间局部坐标，这里先换算回世界格子再查表。
    pub fn solid_cell(room: &Room, cx: i32, cy: i32) -> bool {
        let ox = room.bounds.x.round() as i32;
        let oy = room.bounds.y.round() as i32;
        let lx = cx * sprites::TILE_PX - ox;
        let ly = cy * sprites::TILE_PX - oy;
        if lx < 0 || ly < 0 {
            return false;
        }
        let (tx, ty) = (
            (lx / sprites::TILE_PX) as u32,
            (ly / sprites::TILE_PX) as u32,
        );
        if tx >= room.tiles.width || ty >= room.tiles.height {
            return false;
        }
        room.tiles.get(tx, ty).unwrap_or(0) != 0
    }

    /// 画瓦片 + 实体（不含玩家，玩家每帧单独画）。
    ///
    /// 需要 GPU 是因为 `.atlas` 里没有写纹理尺寸，归一化 UV 只能问
    /// 已上传的纹理。
    pub fn build_static(&mut self, gpu: &GpuRenderer) -> Vec<SpriteDraw> {
        let mut out = Vec::new();

        for room in &self.doc.rooms {
            push_tiles(&mut out, room, &room.tiles, Layer::Tiles, WHITE, self, gpu);
            push_tiles(&mut out, room, &room.bg, Layer::BgTiles, BG_TINT, self, gpu);
            self.push_entities(&mut out, room, gpu);
        }

        debug!(
            draws = out.len(),
            atlas = %self.atlas_name,
            "static draw list rebuilt"
        );
        out
    }

    /// 玩家当前帧的 draw（贴地的底边对齐 hitbox 底边）。
    pub fn player_draw(&self, gpu: &GpuRenderer, pos: Vec2, flip_x: bool) -> Option<SpriteDraw> {
        let (group, sprite) = self.atlas.sprite(sprites::PLAYER_SPRITE)?;
        let id = sprites::group_id(&self.atlas, group);
        let (tw, th) = gpu.texture_size(id)?;
        let uv = GpuRenderer::uv_for_sprite(sprite, Vec2::from_f32s(tw as f32, th as f32));

        let w = f32::from(sprite.w);
        let h = f32::from(sprite.h);
        let top_left = Vec2::from_f32s(
            pos.x.to_f32() + (crate::player::PLAYER_W - w) * 0.5,
            pos.y.to_f32() + crate::player::PLAYER_H - h,
        );

        Some(
            SpriteDraw::new(id, top_left, Vec2::from_f32s(w, h), uv)
                .with_layer(Layer::Player.value())
                .with_color(WHITE)
                .with_flip_x(flip_x),
        )
    }

    /// 实体批次。告警去重走 `warn_once`（`RefCell`），这样整批循环只需要
    /// 不可变借用 `self`，`&Room` 与 `&mut Vec` 都能并存。
    fn push_entities(&self, out: &mut Vec<SpriteDraw>, room: &Room, gpu: &GpuRenderer) {
        for e in &room.entities {
            if e.kind == "player" {
                continue; // 玩家由仿真驱动，见 `Session::player_draw`
            }
            let Some(name) = sprites::entity_sprite(&e.kind) else {
                if self.warn_once(&format!("no-sprite:{}", e.kind)) {
                    warn!(kind = %e.kind, "no built-in sprite for entity kind; skipping it");
                }
                continue;
            };
            let Some((group, sprite)) = self.atlas.sprite(name) else {
                if self.warn_once(&format!("missing:{}", e.kind)) {
                    warn!(kind = %e.kind, sprite = name, "sprite not found in atlas; skipping it");
                }
                continue;
            };
            let id = sprites::group_id(&self.atlas, group);
            let Some((tw, th)) = gpu.texture_size(id) else {
                continue; // 纹理没上传（未转码的 .data），已经在 load_atlas 告警过
            };
            let uv = GpuRenderer::uv_for_sprite(sprite, Vec2::from_f32s(tw as f32, th as f32));
            // 实体的 `origin` 是 sprite 框架相对实体位置的偏移：
            // 框架左上角 = 房间局部 (x - origin_x, y - origin_y)。
            let pos = Vec2::from_f32s(
                room.bounds.x + e.x - e.origin_x,
                room.bounds.y + e.y - e.origin_y,
            );
            out.push(
                SpriteDraw::new(
                    id,
                    pos,
                    Vec2::from_f32s(f32::from(sprite.w), f32::from(sprite.h)),
                    uv,
                )
                .with_layer(Layer::Entities.value())
                .with_color(WHITE),
            );
        }
    }

    /// 同一条告警只发一次，首次返回 `true`。
    fn warn_once(&self, key: &str) -> bool {
        let mut warned = self.warned.borrow_mut();
        if warned.contains(key) {
            false
        } else {
            warned.insert(key.to_string());
            true
        }
    }

    /// 报告用：房间数 / 实体数 / 出生点。
    pub fn summary(&self) -> (usize, usize, String) {
        let entities: usize = self.doc.rooms.iter().map(|r| r.entities.len()).sum();
        (self.doc.rooms.len(), entities, self.spawn_room.clone())
    }
}

/// 中心点落在哪个房间（不含任何回落）。
///
/// `pos` 是 hitbox 左上角，这里取几何中心判定。它只回答"房间身份"
/// （标题栏、相机、热重载检查）；物理碰撞**不选房间** —— hitbox
/// 触及的每个房间都会被查（见 `player::Player::hit_in`），骑缝时
/// 两边的瓦片都算数，不会因为"选错房间"而卡住。
pub(crate) fn room_containing(rooms: &[Room], pos: Vec2) -> Option<&Room> {
    let centre =
        pos + Vec2::from_f32s(crate::player::PLAYER_W * 0.5, crate::player::PLAYER_H * 0.5);
    rooms.iter().find(|r| r.bounds.to_rect().contains(centre))
}

/// 离 `point` 最近的房间（点到矩形的欧氏距离，落在矩形内记 0）。
///
/// 世界级兜底用：中心掉出所有房间时把玩家夹回**最近的**房间，
/// 而不是出生房间 —— 否则地图另一头的坠落会被瞬移回出生点。
pub(crate) fn nearest_room(rooms: &[Room], point: Vec2) -> &Room {
    let mut best = 0;
    let mut best_d: Option<releste_math::Fx> = None;
    for (i, room) in rooms.iter().enumerate() {
        let r = room.bounds.to_rect();
        let dx = (r.min.x - point.x)
            .max(point.x - r.max.x)
            .max(releste_math::Fx::zero());
        let dy = (r.min.y - point.y)
            .max(point.y - r.max.y)
            .max(releste_math::Fx::zero());
        let d = dx * dx + dy * dy;
        let closer = match best_d {
            Some(bd) => d < bd,
            None => true,
        };
        if closer {
            best_d = Some(d);
            best = i;
        }
    }
    &rooms[best]
}

/// 画一层瓦片：每块瓦片按自己的 tile id 查 tileset 定义，
/// 由 autotiler 的邻接匹配给出它在 tileset 纹理里的 `(col, row)`。
///
/// 一次房间可能混多种 tileset（如 `1` = dirt、`3` = snow），所以
/// `(atlas_id, tex_size)` 是**每块瓦片**的，不是每层一份。
fn push_tiles(
    out: &mut Vec<SpriteDraw>,
    room: &Room,
    tiles: &releste_map::Tileset,
    layer: Layer,
    tint: [f32; 4],
    session: &Session,
    gpu: &GpuRenderer,
) {
    // 一次性算好每块瓦片的 (col, row)（位置哈希选变体，确定性）。
    let coords = releste_map::autotiler::generate(tiles, &session.tilesets);

    for ty in 0..tiles.height {
        for tx in 0..tiles.width {
            let value = tiles.get(tx, ty).unwrap_or(0);
            if value == 0 {
                continue; // 0 = 空
            }
            let id = releste_map::tile_char(value);
            let Some(def) = session.tilesets.def(id) else {
                if session.warn_once(&format!("no-tileset:{id}")) {
                    warn!(tile = %id, "no tileset definition; skipping this tile id");
                }
                continue;
            };
            let Some((group, sprite)) = session.atlas.sprite(&def.path) else {
                if session.warn_once(&format!("missing-tileset:{}", def.path)) {
                    warn!(
                        tile = %id,
                        path = %def.path,
                        "tileset texture not found in atlas; skipping this tile id"
                    );
                }
                continue;
            };
            let atlas_id = sprites::group_id(&session.atlas, group);
            let Some((tw, th)) = gpu.texture_size(atlas_id) else {
                continue; // 纹理没上传（未转码的 .data），load_atlas 已告警
            };
            let (col, row) = coords[(ty as usize) * (tiles.width as usize) + (tx as usize)];
            let cell = sprites::tile_cell(sprite, col, row);
            let uv = GpuRenderer::uv_for_sprite(&cell, Vec2::from_f32s(tw as f32, th as f32));

            let pos = Vec2::from_f32s(
                room.bounds.x + tx as f32 * TILE,
                room.bounds.y + ty as f32 * TILE,
            );
            out.push(
                SpriteDraw::new(atlas_id, pos, Vec2::from_f32s(TILE, TILE), uv)
                    .with_layer(layer.value())
                    .with_color(tint),
            );
        }
    }
}

/// 第一个 `player` 实体：(世界坐标, 房间名)。
fn first_spawn(doc: &MapDocument) -> (Vec2, String) {
    for room in &doc.rooms {
        for e in &room.entities {
            if e.kind == "player" {
                return (
                    Vec2::from_f32s(room.bounds.x + e.x, room.bounds.y + e.y),
                    room.name.clone(),
                );
            }
        }
    }
    let room = &doc.rooms[0];
    (Vec2::from_f32s(room.bounds.x, room.bounds.y), String::new())
}

fn info_load(doc: &MapDocument, atlas: &str, tilesets: usize, spawn: Vec2, room: &str) {
    let entities: usize = doc.rooms.iter().map(|r| r.entities.len()).sum();
    tracing::info!(
        area = %doc.area,
        rooms = doc.rooms.len(),
        entities,
        atlas = %atlas,
        tilesets,
        spawn = %format_args!("({}, {})", spawn.x.to_f32(), spawn.y.to_f32()),
        room = %room,
        "map loaded"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use releste_map::{RectDef, Tileset};
    use releste_render::atlas::{AtlasDescriptor, SpriteAtlas};
    use std::path::Path;

    /// 复刻 0-Intro 的接缝几何：`lvl_-1`（x[-368,-48]）与 `lvl_0`
    /// （x[-48,400]）贴边，y 都是 [0,184]。全部内存构造 —— nix/CI 的
    /// 构建源只含 git 跟踪文件，`assets/` 不存在，测试不能依赖它。
    fn seam_session() -> Session {
        let room = |name: &str, x: f32, w: f32| Room {
            name: name.into(),
            bounds: RectDef {
                x,
                y: 0.0,
                w,
                h: 184.0,
            },
            tiles: Tileset::new("Test", 40, 23),
            bg: Tileset::new("Test", 40, 23),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
        };
        let doc = MapDocument {
            area: "test".into(),
            rooms: vec![room("lvl_-1", -368.0, 320.0), room("lvl_0", -48.0, 448.0)],
            sync_rules: Vec::new(),
        };
        Session {
            doc,
            atlas: SpriteAtlas::new(
                AtlasDescriptor {
                    name: "Test".into(),
                    version: 0,
                    links: Vec::new(),
                    textures: Vec::new(),
                },
                Path::new("/nonexistent"),
            ),
            atlas_name: "Test".into(),
            tilesets: releste_map::TilesetsDoc::default(),
            spawn: Vec2::from_f32s(-56.0, 136.0),
            spawn_room: "lvl_-1".into(),
            warned: RefCell::default(),
        }
    }

    /// `room_at` 必须用 hitbox **中心**选房间，而不是角点。
    ///
    /// 回归：两间贴边房间里，出生点的中心在左间，但旧实现拿
    /// 角点/重复补偿后的点去查，会跨进右间 —— 标题栏房间名错、
    /// 热重载的"玩家不在任何房间"告警误报。
    #[test]
    fn room_at_uses_the_hitbox_centre() {
        let session = seam_session();

        // 出生点：中心 (-52, 140) 在 lvl_-1；右下角点正好压在接缝上。
        let spawn = Vec2::from_f32s(-56.0, 136.0);
        assert_eq!(
            session.room_at(spawn).name,
            "lvl_-1",
            "the spawn centre is inside lvl_-1, not the room across the seam"
        );

        // 中心真正进了 lvl_0（pos.x > -52）才切换房间。
        let crossed = Vec2::from_f32s(-51.0, 136.0);
        assert_eq!(
            session.room_at(crossed).name,
            "lvl_0",
            "once the centre crosses the seam the room switches"
        );

        // 中心正好压在接缝上：文件序靠前的房间赢（lvl_-1 在前）。
        let on_seam = Vec2::from_f32s(-52.0, 136.0);
        assert_eq!(
            session.room_at(on_seam).name,
            "lvl_-1",
            "a centre exactly on the shared edge resolves to the first room in file order"
        );
    }
}
