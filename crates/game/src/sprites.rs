//! 实体 → sprite 的查找表，以及把 tileset 图剪成 8×8 单元的规则。
//!
//! # 为什么需要这张表
//! 引擎核心不带玩法（AGENTS.md §4.1），但**渲染**一份现成的 `.map`
//! 需要知道该拿哪张图。真正的答案最终由插件通过 capability 注册
//! （ROADMAP Phase 3）；这里是 0-Intro 能跑起来的最小内置表，
//! 未命中的实体一律 `tracing::warn` + 跳过（§15.8 / §15.9）。

use releste_render::atlas::{Sprite, SpriteAtlas, TextureGroup};

/// 一格瓦片的世界像素尺寸。房间 `lvl_-1` 宽 320 / 40 格 = 8px。
pub const TILE_PX: i32 = 8;

/// 内置实体 sprite 表。
///
/// 没有条目的实体不是错误：`wire` 是线段、`introCrusher` 是
/// autotiler 生成的瓦片网格，它们由 `session` 单独成图。
pub fn entity_sprite(kind: &str) -> Option<&'static str> {
    Some(match kind {
        // `player` 由仿真每帧单独绘制，不进静态批次。
        // `introCrusher` 不在表里：它没有任何 crushblock 贴图，外观是
        // `GFX.FGAutotiler.GenerateBox('3', w/8, h/8)`（106322）。
        "lamp" => "scenery\\lamp",
        "flutterbird" => "scenery\\flutterbird\\idle00",
        // 原版 `GFX.SpriteBank.Create("bird")` 的 start 动画是 `idle`，
        // 它的 path 是 `crow`（Sprites.xml:261-263）→ 静止帧 `crow00`，
        // 不是 `Recover00`（那要挨打才播）。
        "bird" => "characters\\bird\\crow00",
        "introCar" => "scenery\\car\\body",
        "introCarWheels" => "scenery\\car\\wheels",
        _ => return None,
    })
}

/// 玩家的 sprite。
pub const PLAYER_SPRITE: &str = "characters\\player\\idle00";

/// 玩家的头发（原版画在身体后面：`PlayerHair`，发色/飘动独立于身体）。
/// 出处：`pureCeleste.cs:111066` `public const string Hair = "characters/player/hair00"`。
pub const PLAYER_HAIR_SPRITE: &str = "characters\\player\\hair00";

/// 刘海（发根那一片用的纹理）：`pureCeleste.cs:111071`
/// `bangs = GFX.Game.GetAtlasSubtextures("characters/player/bangs")`。
pub const PLAYER_BANGS_SPRITE: &str = "characters\\player\\bangs00";

/// 原版正常发色：`pureCeleste.cs` `Player.NormalHairColor = AC3232`。
/// hair00 纹理本体是白色，运行时染这个色。
/// 头发颜色状态机（原版 `Player.UpdateHair`，`pureCeleste.cs:120244-120296`）。
///
/// - `NORMAL`：有冲刺可用时的本色（118874 `NormalHairColor = AC3232`）
/// - `USED`：冲刺耗尽时的蓝色（118878 `UsedHairColor = 44B7FF`，以
///   `Lerp(Hair.Color, Used, 6*dt)` 渐入，120258-120262）
/// - `FLASH`：冲刺回满瞬间的白闪（118880 `FlashHairColor = White`，
///   `hairFlashTimer = 0.12s`，120283-120286）
/// - `TWO_DASHES`：二段冲刺库存的本色（118882 `TwoDashesHairColor =
///   ff6def`，`Dashes == 2` 时取代 NORMAL，120289）
///
/// 各状态与仿真字段（`Player.dashes` 等）的接线在 dash 实现时激活；
/// 当前无冲刺（库存 1 段且满）→ 恒为 NORMAL，与原版 Prologue 一致。
/// 「以后提供选项改颜色」：这些常量就是默认值，设置文件按状态名覆盖
/// 即可（见 `service::settings`）。
pub mod hair_color {
    pub const NORMAL: [f32; 4] = [172.0 / 255.0, 50.0 / 255.0, 50.0 / 255.0, 1.0];
    pub const USED: [f32; 4] = [68.0 / 255.0, 183.0 / 255.0, 1.0, 1.0];
    pub const FLASH: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
    pub const TWO_DASHES: [f32; 4] = [1.0, 109.0 / 255.0, 239.0 / 255.0, 1.0];
}

/// 玩家身体 sprite 的**逻辑帧原点**（32×32 帧内的底中心）。
///
/// 出处：`Graphics/Sprites.xml:5` `<Origin x="16" y="32"/>`（`<player>`）。
/// 图集里存的是裁剪后的子图，见 [`origin_in_trim`]。
pub const PLAYER_BODY_FRAME_ORIGIN: (f32, f32) = (16.0, 32.0);

/// 头发/刘海共用的**逻辑帧原点**。
///
/// 出处：`pureCeleste.cs:111182` `Vector2 origin = new Vector2(5f, 5f);`
/// —— `PlayerHair.Render` 对 `bangs[hairFrame]` 和 `hair00` 用的是
/// **同一个**原点，所以刘海自己的 XML `<Origin>` 与这里无关。
pub const HAIR_FRAME_ORIGIN: (f32, f32) = (5.0, 5.0);

/// 发片节数：`pureCeleste.cs:111321` `HairCount = 4`。
///
/// 节点 0 画刘海，1..4 画 `hair00`，**每节向尾部收窄**（`GetHairScale`）。
pub const HAIR_COUNT: usize = 4;

/// 逻辑帧原点 → **裁剪后子图内的原点**。
///
/// 图集的 `.meta` 只给出子图在逻辑帧里的位置（负值，Monocle 的
/// `DrawOffset`），不含 XML 的 `<Origin>`；而 `MTexture.Draw(position, origin)`
/// 真正传给 SpriteBatch 的是 `origin - DrawOffset`（`14544-14547`）。
/// 所以「子图内的原点」= `frame_origin + offset` = `frame_origin - trim_pos`，
/// 其中 `sprite.origin()` 存的正是 `trim_pos`（转换器取了 offset 的相反数）。
///
/// `1` 是图集 `--padding 1`：裁剪结果四周留了一圈透明，内容整体右下移了
/// 1px，原点也要跟着移（见 `sprites.rs` 里 lamp 的同一条注释）。
pub fn origin_in_trim(sprite: &Sprite, frame_origin: (f32, f32)) -> (f32, f32) {
    (
        frame_origin.0 - sprite.origin().0 + 1.0,
        frame_origin.1 - sprite.origin().1 + 1.0,
    )
}

/// 发片缩放：`pureCeleste.cs:111201-111205` `GetHairScale(index)`。
///
/// `num = 0.25 + (1 - index / HairCount) * 0.75`，越靠尾部越细：
/// index 0 → 1.0，1 → 0.8125，2 → 0.625，3 → 0.4375。
/// **index 0 的 x 缩放是 `Facing`**（刘海按朝向镜像），不是 `num`。
pub fn hair_scale(index: usize, facing: f32) -> (f32, f32) {
    let i = index as f32;
    let num = 0.25 + (1.0 - i / HAIR_COUNT as f32) * 0.75;
    (if index == 0 { facing } else { num }, num)
}

/// 一根发丝的 `HairOffset`：`Graphics/Sprites.xml:96` 的 `hair` 表。
///
/// idle 的 9 帧是 `0,-2|0,-2|0,-2|0,-2|0,-1|0,-1|0,-1|0,-1|0,-1`，
/// 即帧 0-3 为 `(0,-2)`、帧 4-8 为 `(0,-1)`；乘以 `(Facing, 1)`
/// （`pureCeleste.cs:111116`）后 x 分量仍是 0。
///
/// 我们的 idle 停在第 0 帧，所以取 `(0,-2)`。
pub const PLAYER_HAIR_OFFSET: (f32, f32) = (0.0, -2.0);

/// 路灯：原版按 `broken` 取左/右半 16×80 子图
/// （`pureCeleste.cs:106451` `GetSubtexture(broken ? 16 : 0, 0, 16, 80)`），
/// 图集里 30×77 的内容是完好/损坏两盏拼在一起的。
///
/// 原版设的绘制原点是 `(W/2, H)` = `(8, 80)`（106455），而 `MTexture.Draw`
/// 真正用的是 `origin + 图集 offset`（`14478-14485`）；图集里
/// `scenery\lamp` 的 offset 是 `(1, 3)`，所以整张图的有效原点是
/// `(8 + 1, 80 + 3) = (9, 83)`。子图 16px 宽会比原图的半幅（15px）宽 1px，
/// 这正是原版的行为，允许越界采样。
pub fn lamp_sprite(sprite: &Sprite, broken: bool) -> Sprite {
    let w = 16.min(sprite.w);
    let h = 80.min(sprite.h);
    let x = sprite.x + if broken { 16 } else { 0 };
    Sprite {
        name: sprite.name.clone(),
        x,
        y: sprite.y,
        w,
        h,
        origin_x: 8 + sprite.origin_x,
        origin_y: 80 + sprite.origin_y,
        frame_w: w,
        frame_h: h,
        source: sprite.source.clone(),
    }
}

/// 取 tileset 图里 `(col, row)` 格的 8×8 子矩形。
///
/// `col` / `row` 来自 autotiler 的邻接匹配（`releste_map::autotiler`）；
/// 越界时裁到图内，所以随便哪张 tileset 图都不会采样出图集外的像素。
pub fn tile_cell(sprite: &Sprite, col: u32, row: u32) -> Sprite {
    let w = TILE_PX as i16;
    let h = w;
    let max_x = (sprite.w - w).max(0);
    let max_y = (sprite.h - h).max(0);
    let offset_x = (col as i32 * TILE_PX).min(max_x as i32).max(0) as i16;
    let offset_y = (row as i32 * TILE_PX).min(max_y as i32).max(0) as i16;

    Sprite {
        name: sprite.name.clone(),
        x: sprite.x + offset_x,
        y: sprite.y + offset_y,
        w: w.min(sprite.w),
        h: h.min(sprite.h),
        origin_x: 0,
        origin_y: 0,
        frame_w: w.min(sprite.w),
        frame_h: h.min(sprite.h),
        source: sprite.source.clone(),
    }
}

/// 取 sprite 内任意像素子矩形（原版 `MTexture.GetSubtexture`）。
///
/// 裁到图内，永不越出图集 sprite 的范围；`origin`/`frame` 归零 —— 用它
/// 的绘制方（如 `Bridge` 的竖条）直接给左上角坐标。
pub fn sub_rect(sprite: &Sprite, x: i16, y: i16, w: i16, h: i16) -> Sprite {
    let x = x.max(0);
    let y = y.max(0);
    let w = w.min(sprite.w - x).max(0);
    let h = h.min(sprite.h - y).max(0);

    Sprite {
        name: sprite.name.clone(),
        x: sprite.x + x,
        y: sprite.y + y,
        w,
        h,
        origin_x: 0,
        origin_y: 0,
        frame_w: w,
        frame_h: h,
        source: sprite.source.clone(),
    }
}

/// 一个 sprite 在某个纹理组里的组下标（= atlas id）。
///
/// `SpriteAtlas::sprite` 只返回引用，这里按指针找回下标。
pub fn group_id(atlas: &SpriteAtlas, group: &TextureGroup) -> u32 {
    atlas
        .descriptor()
        .textures
        .iter()
        .position(|t| std::ptr::eq(t, group))
        .unwrap_or(0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sprite(w: i16, h: i16) -> Sprite {
        Sprite {
            name: "tilesets\\snow".into(),
            x: 1045,
            y: 2030,
            w,
            h,
            origin_x: 0,
            origin_y: 0,
            frame_w: w,
            frame_h: h,
            source: None,
        }
    }

    #[test]
    fn entity_table_covers_the_intro_kinds() {
        assert_eq!(entity_sprite("lamp"), Some("scenery\\lamp"));
        assert_eq!(entity_sprite("introCar"), Some("scenery\\car\\body"));
        assert_eq!(entity_sprite("wire"), None, "wire has no sprite by design");
        assert_eq!(entity_sprite("hahaha"), None);
    }

    #[test]
    fn tile_cell_clamps_inside_the_sprite() {
        let s = sprite(48, 120);
        let cap = tile_cell(&s, 0, 0);
        assert_eq!((cap.x, cap.y, cap.w, cap.h), (1045, 2030, 8, 8));

        // 一个只有 4px 高的图不能被裁出 8px，也不能跑到源矩形外面。
        let src = sprite(4, 4);
        let tiny = tile_cell(&src, 0, 2);
        assert_eq!((tiny.w, tiny.h), (4, 4));
        assert!(
            tiny.y + tiny.h <= src.y + src.h,
            "clipped cell left the source rect"
        );
        assert!(
            tiny.x + tiny.w <= src.x + src.w,
            "clipped cell left the source rect"
        );
    }

    #[test]
    fn tile_cell_col_and_row_are_multiplied_by_the_tile_size() {
        let s = sprite(48, 120);
        let fill = tile_cell(&s, 0, 2);
        assert_eq!((fill.x, fill.y), (1045, 2030 + 2 * 8));
        let right = tile_cell(&s, 1, 0);
        assert_eq!((right.x, right.y), (1045 + 8, 2030));
    }
}
