//! 一张已加载的 `.map` + 它的图集，以及「这帧该画什么」的静态部分。
//!
//! 地图是数据（AGENTS.md §4.2）：这里只把瓦片与实体翻成
//! [`SpriteDraw`]，不含任何玩法分支。

use std::cell::RefCell;
use std::collections::HashSet;

use anyhow::{bail, Context as _, Result};
use releste_map::{Backdrop, MapDocument, Room};
use releste_math::Vec2;
use releste_render::atlas::SpriteAtlas;
use releste_render::{GpuRenderer, Layer, SpriteDraw};
use releste_world::FieldValue;
use tracing::{debug, warn};

use crate::paths::Paths;
use crate::sprites;

/// 一格瓦片的世界像素数（`lvl_-1` 宽 320 / 40 格 = 8px）。
const TILE: f32 = sprites::TILE_PX as f32;

/// 单个 `Backdrop` 的有效颜色：全零视为"未设置"，取原版默认白
/// （`112789` `backdrop.Color = Color.White` 起点，`112790-112796`
/// 仅当 `HasAttr("color")` 才覆盖）；否则按原版做法用 `alpha`
/// 缩放一次（`112798-112804` `backdrop.Color *= alpha`）。
fn backdrop_tint(bd: &Backdrop) -> [f32; 4] {
    let c = bd.effective_color();
    [
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
        c[3] as f32 / 255.0,
    ]
}

/// 视差平铺几何：给定一个 `Backdrop`、相机位置与贴图尺寸，算出本帧要
/// 画的每一片左上角（屏幕坐标）。纯函数，便于对照原版单测。
///
/// 逐步对照 `Parallax.Render`（`pureCeleste.cs:112163-112236`）：
/// 1. `v = (Camera.Position + CameraOffset).Floor()`（`112165`）。引擎尚无
///    `Backdrop.CameraOffset` 概念（`CameraOffset` 是 `Parallax` 独有字段、
///    默认 `Vector2.Zero`，`112129-112130`），故直接取相机位置。
/// 2. `p = (Position − v*Scroll).Floor()`（`112166`）。
/// 3. 若 `LoopX`/`LoopY`：两段 `while` 把 `p.X`/`p.Y` 折回（`112185-112206`），
///    顺序与原版一致（先 `<0` 加、后 `>0` 减）；`!Loop` 时不折。
/// 4. 双重 `for` 铺到 `320×180`（`112220-112234`）——**条件在前**、外层 X
///    内层 Y：起点越界（`p.X ≥ 320` 或 `p.Y ≥ 180`）时该轴一片都不画；
///    `!LoopY` 内层画完一片就 `break`，`!LoopX` 外层画完一列就 `break`。
///    每片 `Texture.Draw(pos, Vector2.Zero, color, 1, 0, flip)`（`112224`），
///    原点 `Vector2.Zero`（裁剪子图左上角）、缩放 `1`。
fn parallax_tiles(
    bd: &Backdrop,
    camera_pos: (f32, f32),
    tex: (f32, f32),
    screen: (f32, f32),
) -> Vec<(f32, f32)> {
    // 贴图 0 尺寸会让两段 while 死循环——钳到 ≥1（原版 `Texture.Width`
    // 恒 ≥1，引擎侧防御性钳制，不是行为差异）。
    let (tw, th) = (tex.0.max(1.0), tex.1.max(1.0));
    // 1. v = (Camera + CameraOffset).Floor()（无 CameraOffset）。
    let (cam_x, cam_y) = (camera_pos.0.floor(), camera_pos.1.floor());
    // 2. p = (Position − v*Scroll).Floor()。
    let mut px = (bd.x - cam_x * bd.scroll_x).floor();
    let mut py = (bd.y - cam_y * bd.scroll_y).floor();
    // 3. while 折回（原版两段 while 的顺序：先 <0 加，再 >0 减）。
    if bd.loop_x {
        while px < 0.0 {
            px += tw;
        }
        while px > 0.0 {
            px -= tw;
        }
    }
    if bd.loop_y {
        while py < 0.0 {
            py += th;
        }
        while py > 0.0 {
            py -= th;
        }
    }
    // 4. 双重 for（外 X 内 Y，条件在前）。
    let mut out = Vec::new();
    let mut x = px;
    while x < screen.0 {
        let mut y = py;
        while y < screen.1 {
            out.push((x, y));
            if !bd.loop_y {
                break;
            }
            y += th;
        }
        if !bd.loop_x {
            break;
        }
        x += tw;
    }
    out
}

/// 背景 / 前景视差贴图平铺（`Parallax.Render`，`pureCeleste.cs:112163-112236`）。
///
/// 几何走 [`parallax_tiles`]（纯函数、带对照单测）；这里只负责纹理查询、
/// 调色与转 [`SpriteDraw`]。
///
/// 引擎差异（记录在案，非行为偏差）：
/// - 原点 `Vector2.Zero` 落到裁剪子图 = 左上角贴齐，故直接用 `sprite.w/h` 与
///   `uv_for_sprite`，不走 [`sprites::origin_in_trim`]（那是给"有帧内原点"的
///   sprite 用的）。
/// - `blendmode="additive"` 暂不支持（wgpu 后端当前只有单一
///   `PREMULTIPLIED_ALPHA_BLENDING` 管线，见 `gpu.rs:289-291`），统一按
///   预乘 alpha 画，已知偏差；同理 `FlipY`（`SpriteDraw` 只有 `flip_x`）
///   与 `WindMultiplier` 驱动的水平漂移（`112157`）暂不生效，0-Intro 未用。
/// - `fadeIn`（`DoFadeIn`，`112153-112160`）与 `FadeX`/`FadeY`（`Fader`，
///   `109343-109378`）尚未移植：0-Intro 的 `ParseBackdrop`（`112651`
///   `DoFadeIn = bool.Parse(child.Attr("fadeIn", "false"))`）默认 `false`，
///   故 `fadeIn` 恒为 `Visible ? 1 : 0`，进场景即 `1`（无过渡），当前
///   `Service` 不追踪 `Visible`，这里恒定 `1`；`FadeX`/`FadeY` 仅在
///   `<apply fade=...>` 时才有值，0-Intro 无此写法。
/// - `Parallax.Alpha`（`112141`）字段 `ParseBackdrop` 从不写（`alpha` 属性
///   只乘进 `backdrop.Color`，`112798-112804`），默认 `1` → `Render` 里
///   `num = fadeIn*Alpha*FadeAlphaMultiplier = 1`、`color *= num` 不触发
///   （`112167-112179`），所以直接用 `effective_color()` 是等价的。
fn push_backdrops(
    out: &mut Vec<SpriteDraw>,
    backdrops: &[Backdrop],
    camera_pos: Vec2,
    layer: i32,
    session: &Session,
    gpu: &GpuRenderer,
) {
    let screen = (releste_render::BASE_WIDTH, releste_render::BASE_HEIGHT);
    for bd in backdrops {
        // 仅支持 `parallax`：其余 kind（snowBg/snowFg/mirrorFg/godrays…）
        // 是独立 `Backdrop` 子类，各自有自己的 `Render`，尚未移植。
        if !bd.kind.eq_ignore_ascii_case("parallax") {
            if session.warn_once(&format!("backdrop-kind:{}", bd.kind)) {
                warn!(
                    kind = %bd.kind,
                    "non-parallax backdrop kind is not implemented yet; skipping it"
                );
            }
            continue;
        }
        if bd.flip_y && session.warn_once(&format!("backdrop-flipy:{}", bd.texture)) {
            warn!(
                texture = %bd.texture,
                "backdrop flipy is not supported yet; drawing unflipped"
            );
        }

        let Some((group, sprite)) = session.atlas.sprite(&bd.texture) else {
            if session.warn_once(&format!("backdrop-tex:{}", bd.texture)) {
                warn!(
                    texture = %bd.texture,
                    "backdrop texture not found in atlas; skipping it"
                );
            }
            continue;
        };
        let atlas_id = sprites::group_id(&session.atlas, group);
        let Some((tw, th)) = gpu.texture_size(atlas_id) else {
            continue; // 纹理没上传（未转码的 .data），`load_atlas` 已告警
        };
        let uv = GpuRenderer::uv_for_sprite(sprite, Vec2::from_f32s(tw as f32, th as f32));
        let (w, h) = (f32::from(sprite.w), f32::from(sprite.h));
        let tint = backdrop_tint(bd);
        // 原版 `color.A <= 1` 早退（`112181-112184`）——等价于 alpha ≤ 1/255。
        if tint[3] <= 1.0 / 255.0 {
            continue;
        }

        for (x, y) in parallax_tiles(bd, camera_pos.to_f32s(), (w, h), screen) {
            out.push(
                SpriteDraw::new(atlas_id, Vec2::from_f32s(x, y), Vec2::from_f32s(w, h), uv)
                    .with_layer(layer)
                    .with_color(tint)
                    .with_flip_x(bd.flip_x),
            );
        }
    }
}

/// 前景瓦片 / 实体 / 背景瓦片的色调。
///
/// 原版所有 `TileGrid` 都用默认 `Color.White` + `Alpha = 1`
/// （`pureCeleste.cs:11234`/`11242`，全原版无一处写它），所以这里只有白。
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
    /// **背景** tileset 定义（content-pipeline 从 `BackgroundTiles.xml` 转换）。
    ///
    /// 房间的 `bg` 层必须查这张表而不是 [`Session::tilesets`]：同名 id 在
    /// 两张表里是完全不同的贴图（`'3'` = snow / bgBrickRuined）。
    pub bg_tilesets: releste_map::TilesetsDoc,
    /// 玩家出生锚点（世界坐标）—— 原版语义：hitbox **底中心**（脚）。
    /// service 用它减去 (W/2, H) 得到 hitbox 左上角，出处见
    /// [`vanilla_start`] 与 `player::PLAYER_W_PX`。
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

        // 背景砖是第二张表；缺表只告警、不拦加载（背景整层不画比进不去游戏好）。
        let bg_path = paths.bg_tilesets();
        let bg_tilesets = match releste_map::TilesetsDoc::from_file(&bg_path) {
            Ok(doc) => doc,
            Err(e) => {
                warn!(
                    path = %bg_path.display(),
                    error = %e,
                    "background tileset definitions missing; the `bg` layer will not draw"
                );
                releste_map::TilesetsDoc::default()
            }
        };

        let (spawn, spawn_room) = vanilla_start(&doc);
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
            bg_tilesets,
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

    /// 画**当前房间**的瓦片 + 实体（不含玩家，玩家每帧单独画）。
    ///
    /// 原版一次只画一个房间：跨房间连续世界里把邻室也画出来，会在
    /// 接缝上出现两边边墙贴在一起的伪影（悬空门柱、垂雪檐）。
    /// 调用方负责在房间切换时换 `room_index` 重建。
    ///
    /// 需要 GPU 是因为 `.atlas` 里没有写纹理尺寸，归一化 UV 只能问
    /// 已上传的纹理。
    pub fn build_static(&mut self, gpu: &GpuRenderer, room_index: usize) -> Vec<SpriteDraw> {
        let mut out = Vec::new();
        let room = &self.doc.rooms[room_index];
        push_tiles(
            &mut out,
            room,
            &room.tiles,
            &self.tilesets,
            Layer::Tiles,
            WHITE,
            self,
            gpu,
        );
        // 背景是**另一张**表（`BackgroundTiles.xml`），且原版不调色
        // （`TileGrid.Color` 默认 `Color.White`、`Alpha = 1`，11234/11242，
        // 全原版无一处写它）——之前那层蓝灰 tint 纯粹是自造的偏色。
        push_tiles(
            &mut out,
            room,
            &room.bg,
            &self.bg_tilesets,
            Layer::BgTiles,
            WHITE,
            self,
            gpu,
        );
        self.push_entities(&mut out, room, gpu);

        debug!(
            draws = out.len(),
            room = %room.name,
            atlas = %self.atlas_name,
            "static draw list rebuilt"
        );
        out
    }

    /// 本帧的视差背景/前景 draw（`Parallax.Render`，`pureCeleste.cs:112163-112236`）。
    ///
    /// **每帧都要重建**（不像瓦片/实体那样按房间缓存）：平铺起点依赖
    /// 相机位置（`p = (Position − Camera.Position.Floor()*Scroll).Floor()`，
    /// `112165-112166`），相机一动整张分布就变。数量很小（4 张背景图
    /// × 若干平铺），重建成本可忽略。
    ///
    /// 层：背景在所有瓦片之后画（[`Layer::Background`] = -1000 <
    /// [`Layer::BgTiles`] = -500），前景压在玩家与特效之上
    /// （[`Layer::Foreground`] = 500 > [`Layer::Effects`] = 300），
    /// 与原版 `Backdrop` 的前后层级一致。
    pub fn backdrop_draws(&self, gpu: &GpuRenderer, camera_pos: Vec2) -> Vec<SpriteDraw> {
        let mut out = Vec::new();
        push_backdrops(
            &mut out,
            &self.doc.style.backgrounds,
            camera_pos,
            Layer::Background.value(),
            self,
            gpu,
        );
        push_backdrops(
            &mut out,
            &self.doc.style.foregrounds,
            camera_pos,
            Layer::Foreground.value(),
            self,
            gpu,
        );
        out
    }

    /// 地图样式的清屏色（`<Style color="#rrggbb">`，`pureCeleste.cs:112434`）。
    pub fn clear_color(&self) -> [u8; 3] {
        self.doc.style.background_color
    }

    /// 玩家本帧的 draw：头发（4 节链 + 刘海 + 黑描边）在身体后面。
    ///
    /// 每一处都对照 `pureCeleste.cs`：
    /// - 锚点：`Player.Position` = hitbox 底中心（118856），即 `anchor`；
    /// - 发根：`Nodes[0] = Sprite.RenderPosition + (0,-9) + HairOffset*Facing`
    ///   （111116）。idle 动画未定义 HairOffset → 取默认 (0,0)
    ///   （111477 只有 yaml 提供时才赋值）；RenderPosition 即玩家位置；
    /// - 后续节：`HairCount = 4`（111321）；`AfterUpdate`（111118-111135）
    ///   的静态摆位 node[i] = node[i-1] + ((0-Facing)*step, +2)，step 在
    ///   i==1 时为 2*StepInFacingPerSegment = 1，其余 0.5（StepPerSegment
    ///   = (0,2)，111074-111076）；节点间 ≤3px 的约束不触发（|(-1,2)|≈2.24）；
    /// - 绘制（Render 111155+）：`origin = (5,5)`；node[0] 用 bangs[0]，
    ///   其余用 hair00；每片先画 4 个 ±1px 的黑描边（`Border = Black`，
    ///   111159），再按 HairCount-1..0 逆序画本色（`Color = AC3232`）。
    /// - 身体最后画（Layer::Player 高于 PlayerHair，层排序保证）。
    ///
    /// 尚未移植：发丝的运动模拟（SimulateMotion/StepApproach，随动画
    /// 系统一起）；身体四向描边（DrawPlayerSpriteOutline，idle 不开）。
    pub fn player_draws(&self, gpu: &GpuRenderer, pos: Vec2, flip_x: bool) -> Vec<SpriteDraw> {
        let facing: f32 = if flip_x { -1.0 } else { 1.0 }; // Facings.Left = -1
        let anchor = pos + Vec2::from_f32s(crate::player::PLAYER_W * 0.5, crate::player::PLAYER_H);

        // `AfterUpdate`（111114-111139）：节点 0 = RenderPosition + (0,-9)
        // + HairOffset*(Facing,1)；其后每节沿 `StepPerSegment (0,2)` 链接。
        let mut nodes = Vec::with_capacity(sprites::HAIR_COUNT);
        let hair_offset = sprites::PLAYER_HAIR_OFFSET;
        nodes.push(anchor + Vec2::from_f32s(hair_offset.0 * facing, -9.0 + hair_offset.1));
        for i in 1..sprites::HAIR_COUNT {
            // 第 1 节多探出 1px（StepInFacingPerSegment*2 只作用于第 1 节）。
            let step_x = (0.0 - facing) * if i == 1 { 1.0 } else { 0.5 };
            nodes.push(nodes[i - 1] + Vec2::from_f32s(step_x, 2.0));
        }

        let pieces: [(&str, &Vec2); sprites::HAIR_COUNT] = [
            (sprites::PLAYER_BANGS_SPRITE, &nodes[0]),
            (sprites::PLAYER_HAIR_SPRITE, &nodes[1]),
            (sprites::PLAYER_HAIR_SPRITE, &nodes[2]),
            (sprites::PLAYER_HAIR_SPRITE, &nodes[3]),
        ];
        let border = [0.0, 0.0, 0.0, 1.0];
        let outline_offsets = [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)];

        let mut out = Vec::new();
        // 先把每节的描边（4 个 ±1px）全画完，再从尾到头画本色
        // （111179-111197）—— 两趟循环，不是逐节交替。
        for (i, (name, node)) in pieces.iter().enumerate() {
            let scale = sprites::hair_scale(i, facing);
            let Some(base) = self.hair_piece(gpu, name, **node, scale, border) else {
                continue;
            };
            for (dx, dy) in outline_offsets {
                let mut d = base;
                d.position = d.position + Vec2::from_f32s(dx, dy);
                out.push(d);
            }
        }
        for (i, (name, node)) in pieces.iter().enumerate().rev() {
            let scale = sprites::hair_scale(i, facing);
            if let Some(mut d) =
                self.hair_piece(gpu, name, **node, scale, sprites::PLAYER_HAIR_COLOR)
            {
                d.layer = Layer::PlayerHair.value() + 1; // 本色压在描边之上
                out.push(d);
            }
        }
        if let Some(d) = self.body_draw(gpu, anchor, flip_x) {
            out.push(d);
        }
        out
    }

    /// 一片头发：左上角 = `node − 子图内原点 × scale`
    /// （Monocle `MTexture.Draw(position, origin, color, scale)` →
    /// `14534-14537` 把 `origin` 交给 SpriteBatch，缩放绕原点作用）。
    ///
    /// 原点必须**按每张图各算一次**：`hair00` 在 `.meta` 里裁掉了
    /// `(1,1)`、`bangs00` 裁掉了 `(1,0)`，两者共用帧原点 (5,5)
    /// （`HAIR_FRAME_ORIGIN`），落到裁剪后的子图里就不一样了。
    fn hair_piece(
        &self,
        gpu: &GpuRenderer,
        name: &str,
        node: Vec2,
        scale: (f32, f32),
        color: [f32; 4],
    ) -> Option<SpriteDraw> {
        let (group, sprite) = self.atlas.sprite(name)?;
        let id = sprites::group_id(&self.atlas, group);
        let (tw, th) = gpu.texture_size(id)?;
        let uv = GpuRenderer::uv_for_sprite(sprite, Vec2::from_f32s(tw as f32, th as f32));
        let (w, h) = (f32::from(sprite.w), f32::from(sprite.h));
        let origin = sprites::origin_in_trim(sprite, sprites::HAIR_FRAME_ORIGIN);
        Some(
            SpriteDraw::new(
                id,
                node - Vec2::from_f32s(origin.0 * scale.0, origin.1 * scale.1),
                Vec2::from_f32s(w * scale.0, h * scale.1),
                uv,
            )
            .with_layer(Layer::PlayerHair.value())
            .with_color(color),
        )
    }

    /// 身体：底中心锚在脚下（脚 = hitbox 底边 = 地面）。
    ///
    /// 左上角不是 `anchor − 裁剪图的几何中心`：`Sprites.xml:5` 的
    /// `<Origin x="16" y="32"/>` 才是原版 `Sprite.Position` 的语义，
    /// 减掉 `.meta` 的裁剪位移后落到子图内（见 [`sprites::origin_in_trim`]）。
    /// 原实现按 `w*0.5` 取，把身体往左挪了 1.5px。
    fn body_draw(&self, gpu: &GpuRenderer, anchor: Vec2, flip_x: bool) -> Option<SpriteDraw> {
        let (group, sprite) = self.atlas.sprite(sprites::PLAYER_SPRITE)?;
        let id = sprites::group_id(&self.atlas, group);
        let (tw, th) = gpu.texture_size(id)?;
        let uv = GpuRenderer::uv_for_sprite(sprite, Vec2::from_f32s(tw as f32, th as f32));
        let (w, h) = (f32::from(sprite.w), f32::from(sprite.h));
        let origin = sprites::origin_in_trim(sprite, sprites::PLAYER_BODY_FRAME_ORIGIN);
        Some(
            SpriteDraw::new(
                id,
                anchor - Vec2::from_f32s(origin.0, origin.1),
                Vec2::from_f32s(w, h),
                uv,
            )
            .with_layer(Layer::Player.value())
            .with_color(WHITE)
            .with_flip_x(flip_x),
        )
    }

    /// 实体批次。锚点按**原版语义**：
    /// - 默认（贴花类）：内容**中心**贴在实体位置（`MTexture.Draw(position)`）；
    /// - `introCar`：车身 + 轮子两张图，**底中心**贴在位置
    ///   （`pureCeleste.cs` IntroCar：`Origin = (W/2, H)`，车轮画在车身下）；
    /// - `lamp`：取左半 16×80 子图，**底中心**贴在位置（`Lamp` 同上）。
    ///
    /// 告警去重走 `warn_once`（`RefCell`），这样整批循环只需要
    /// 不可变借用 `self`，`&Room` 与 `&mut Vec` 都能并存。
    fn push_entities(&self, out: &mut Vec<SpriteDraw>, room: &Room, gpu: &GpuRenderer) {
        for e in &room.entities {
            if e.kind == "player" {
                continue; // 玩家由仿真驱动，见 `Session::player_draws`
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
            let (w, h) = (f32::from(sprite.w), f32::from(sprite.h));
            // 实体位置 = 锚点（世界坐标）。
            let anchor = Vec2::from_f32s(room.bounds.x + e.x, room.bounds.y + e.y);
            match e.kind.as_str() {
                "introCar" => {
                    // 原版是两张图：body（Depth 1）+ wheels（Depth 3），
                    // 都是**底中心**锚在位置上。
                    for (name, layer) in [
                        ("scenery\\car\\wheels", Layer::Entities.value()),
                        ("scenery\\car\\body", Layer::Entities.value() + 1),
                    ] {
                        let Some((group, part)) = self.atlas.sprite(name) else {
                            if self.warn_once(&format!("missing:{name}")) {
                                warn!(sprite = name, "sprite not found in atlas; skipping it");
                            }
                            continue;
                        };
                        let id = sprites::group_id(&self.atlas, group);
                        let Some((tw, th)) = gpu.texture_size(id) else {
                            continue;
                        };
                        let uv =
                            GpuRenderer::uv_for_sprite(part, Vec2::from_f32s(tw as f32, th as f32));
                        let (pw, ph) = (f32::from(part.w), f32::from(part.h));
                        let top_left = anchor - Vec2::from_f32s(pw * 0.5, ph);
                        out.push(
                            SpriteDraw::new(id, top_left, Vec2::from_f32s(pw, ph), uv)
                                .with_layer(layer)
                                .with_color(WHITE),
                        );
                    }
                }
                "lamp" => {
                    // 灯被打碎后画的是另一套碎玻璃子图（同一张图的右半）。
                    let broken = matches!(e.properties.get("broken"), Some(FieldValue::Bool(true)));
                    let lamp = sprites::lamp_sprite(sprite, broken);
                    let uv =
                        GpuRenderer::uv_for_sprite(&lamp, Vec2::from_f32s(tw as f32, th as f32));
                    let (lw, lh) = (f32::from(lamp.w), f32::from(lamp.h));
                    let top_left = anchor - Vec2::from_f32s(lw * 0.5, lh);
                    out.push(
                        SpriteDraw::new(id, top_left, Vec2::from_f32s(lw, lh), uv)
                            .with_layer(Layer::Entities.value())
                            .with_color(WHITE),
                    );
                }
                _ => {
                    // 贴花类：内容中心贴位置。
                    let top_left = anchor - Vec2::from_f32s(w * 0.5, h * 0.5);
                    out.push(
                        SpriteDraw::new(id, top_left, Vec2::from_f32s(w, h), uv)
                            .with_layer(Layer::Entities.value())
                            .with_color(WHITE),
                    );
                }
            }
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
    let mut best_d: Option<i64> = None;
    for (i, room) in rooms.iter().enumerate() {
        let r = room.bounds.to_rect();
        let dx = (r.min.x - point.x)
            .max(point.x - r.max.x)
            .max(releste_math::Fx::zero());
        let dy = (r.min.y - point.y)
            .max(point.y - r.max.y)
            .max(releste_math::Fx::zero());
        let d = Vec2::new(dx, dy).distance_sq(Vec2::ZERO);
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
    defs: &releste_map::TilesetsDoc,
    layer: Layer,
    tint: [f32; 4],
    session: &Session,
    gpu: &GpuRenderer,
) {
    // 一次性算好每块瓦片的 (col, row)（位置哈希选变体，确定性）。
    let coords = releste_map::autotiler::generate(tiles, defs);

    for ty in 0..tiles.height {
        for tx in 0..tiles.width {
            let value = tiles.get(tx, ty).unwrap_or(0);
            if value == 0 {
                continue; // 0 = 空
            }
            let id = releste_map::tile_char(value);
            let Some(def) = defs.def(id) else {
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

/// 原版的初始房间与出生点，三步都有出处：
///
/// 1. **初始房间**：`pureCeleste.cs:112564` `MapData.StartLevel() =>
///    GetAt(Vector2.Zero)` —— 包含**世界原点 (0,0)** 的第一个房间
///    （`GetAt` 112569 按序做 Bounds 包含判定，无命中返回 null；
///    这里回落到第一个房间并告警，属引擎健壮性差异）。
/// 2. **出生点**：`115101` `Session.RespawnPoint = DefaultSpawnPoint`，
///    `114080` `DefaultSpawnPoint => Spawns.ClosestTo((Bounds.Left, Bounds.Bottom))`
///    （`101280` `GetSpawnPoint`）—— 离房间**左下角**最近的出生点。
/// 3. **锚点语义**：`115105` `new Player(Session.RespawnPoint.Value, ...)`
///    —— 出生点是 hitbox **底中心**（脚，见 `player::PLAYER_H_PX`）。
///
/// 返回 `(锚点, 房间名)`；锚点供 service 换算成 hitbox 左上角。
fn vanilla_start(doc: &MapDocument) -> (Vec2, String) {
    let origin = Vec2::from_f32s(0.0, 0.0);
    let room = doc
        .rooms
        .iter()
        .find(|r| r.bounds.to_rect().contains(origin))
        .or_else(|| {
            warn!(
                "no room contains the world origin (pureCeleste StartLevel); using the first room"
            );
            doc.rooms.first()
        });
    let Some(room) = room else {
        // `Session::load` 已保证 rooms 非空；到这里必然有回落，仅保险。
        return (Vec2::ZERO, String::new());
    };

    // Spawns.ClosestTo((Bounds.Left, Bounds.Bottom))：出生点 = player 实体
    // 中离房间左下角最近的那个（转换器把 .bin 的 Spawns 落成 player 实体）。
    let corner = Vec2::from_f32s(room.bounds.x, room.bounds.y + room.bounds.h);
    let mut best: Option<(Vec2, i64)> = None;
    for e in &room.entities {
        if e.kind != "player" {
            continue;
        }
        let p = Vec2::from_f32s(room.bounds.x + e.x, room.bounds.y + e.y);
        let d = p.distance_sq(corner);
        if best.map(|(_, bd)| d < bd).unwrap_or(true) {
            best = Some((p, d));
        }
    }
    match best {
        Some((p, _)) => (p, room.name.clone()),
        None => {
            warn!(
                room = %room.name,
                "starting room has no spawn points; using its origin"
            );
            (
                Vec2::from_f32s(room.bounds.x, room.bounds.y),
                room.name.clone(),
            )
        }
    }
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
            ..Default::default()
        };
        let doc = MapDocument {
            area: "test".into(),
            rooms: vec![room("lvl_-1", -368.0, 320.0), room("lvl_0", -48.0, 448.0)],
            sync_rules: Vec::new(),
            ..Default::default()
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
            bg_tilesets: releste_map::TilesetsDoc::default(),
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

    /// 造一条 `parallax` backdrop。
    fn parallax_bd(x: f32, y: f32, scroll: (f32, f32), loops: (bool, bool)) -> Backdrop {
        let mut bd = Backdrop::new("parallax");
        bd.x = x;
        bd.y = y;
        bd.scroll_x = scroll.0;
        bd.scroll_y = scroll.1;
        bd.loop_x = loops.0;
        bd.loop_y = loops.1;
        bd
    }

    /// 0-Intro `bg0` 的真实参数（`x=-24 y=-64 scroll=(0.15,0.05) loopx`），
    /// 纹理 320×137（`Gameplay.atlas` 的 `bgs\00\bg0`）：
    /// 起点经 `Position − Floor(cam)*Scroll` 折算后必须覆盖整个 320 宽屏，
    /// 且 `loopy=false` 时只有一行——与 `Parallax.Render` 112185-112234 一致。
    #[test]
    fn parallax_loop_x_without_loop_y_tiles_one_row() {
        let bd = parallax_bd(-24.0, -64.0, (0.15, 0.05), (true, false));
        let tiles = parallax_tiles(&bd, (176.9, 92.2), (320.0, 137.0), (320.0, 180.0));

        // Floor(cam) = (176, 92) → px = Floor(-24 - 26.4) = -51、
        // py = Floor(-64 - 4.6) = -69；loopx 折回是原版的两段 while
        // （先 <0 加、再 >0 减，112185-112195）→ -51 原样返回。
        assert_eq!(
            tiles,
            vec![(-51.0, -69.0), (269.0, -69.0)],
            "two columns of one row: -51..269 and 269..589 cover the 320px screen"
        );
        // 覆盖性：每列 320 宽，首列尾 = -51+320 = 269 ≤ 320 屏右缘被盖住。
        for (x, y) in &tiles {
            assert!(*y < 180.0, "row must start on screen (y={y})");
            assert!(
                *x < 320.0,
                "column must start before the right edge (x={x})"
            );
        }
    }

    /// 双轴 loop（`purplesunset` 参数形态：`loopx/loopy` 都开）必须铺满
    /// 320×180，且起点折进 `[0, 宽)`/`[0, 高)`。
    #[test]
    fn parallax_double_loop_covers_the_whole_screen() {
        let bd = parallax_bd(0.0, 0.0, (0.5, 0.5), (true, true));
        let tiles = parallax_tiles(&bd, (320.0, 180.0), (160.0, 90.0), (320.0, 180.0));

        // Floor(0 − 160*0.5) = -160 → 折回 0；y 同理 → 网格 (0,0)/(160,90) 起步。
        assert_eq!(
            tiles,
            vec![(0.0, 0.0), (0.0, 90.0), (160.0, 0.0), (160.0, 90.0)]
        );
        // 每片 160×90 → 2×2 正好盖满 320×180。
        assert!(tiles.iter().all(|(x, y)| *x < 320.0 && *y < 180.0));
    }

    /// `for` 条件在前（`112220`/`112222`）：起点越界时该轴一片都不画；
    /// 不越界且双轴都不 loop 时恰好画一片。
    #[test]
    fn parallax_start_off_screen_draws_nothing() {
        let off = parallax_bd(400.0, 10.0, (1.0, 1.0), (false, false));
        assert!(
            parallax_tiles(&off, (0.0, 0.0), (320.0, 180.0), (320.0, 180.0)).is_empty(),
            "p.X = 400 >= 320: vanilla's for-condition never enters the loop"
        );

        let one = parallax_bd(10.0, 10.0, (1.0, 1.0), (false, false));
        assert_eq!(
            parallax_tiles(&one, (0.0, 0.0), (320.0, 180.0), (320.0, 180.0)),
            vec![(10.0, 10.0)],
            "!LoopX && !LoopY draws exactly one tile"
        );
    }

    /// 颜色：全零 = 原版默认白（`112789`/`effective_color`）；
    /// `alpha` 属性折进 Color（`112798-112804` `Color *= alpha`）。
    #[test]
    fn backdrop_tint_matches_parse_semantics() {
        let mut bd = parallax_bd(0.0, 0.0, (1.0, 1.0), (true, true));
        assert_eq!(
            backdrop_tint(&bd),
            [1.0; 4],
            "unset color falls back to white"
        );

        bd.color = [140, 140, 140, 140]; // `alpha=0.55` × 白，0-Intro 的 bg0
        let tint = backdrop_tint(&bd);
        for c in tint {
            assert!((c - 140.0 / 255.0).abs() < 1e-6, "tint channel {c}");
        }
    }
}
