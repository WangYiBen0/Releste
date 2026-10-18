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
/// 没有条目的实体不是错误：`wire`、`jumpThru` 这类只提供几何信息，
/// 引擎侧本来就不画东西。
pub fn entity_sprite(kind: &str) -> Option<&'static str> {
    Some(match kind {
        // `player` 由仿真每帧单独绘制，不进静态批次。
        "lamp" => "scenery\\lamp",
        "flutterbird" => "scenery\\flutterbird\\idle00",
        "bird" => "characters\\bird\\Recover00",
        "introCar" => "scenery\\car\\body",
        "introCrusher" => "objects\\crushblock\\hit_down01",
        _ => return None,
    })
}

/// 玩家的 sprite。
pub const PLAYER_SPRITE: &str = "characters\\player\\idle00";

/// 玩家的头发（原版画在身体后面：`PlayerHair`，发色/飘动独立于身体）。
pub const PLAYER_HAIR_SPRITE: &str = "characters\\player\\hair00";

/// 原版正常发色：`pureCeleste.cs` `Player.NormalHairColor = AC3232`。
/// hair00 纹理本体是白色，运行时染这个色。
pub const PLAYER_HAIR_COLOR: [f32; 4] = [172.0 / 255.0, 50.0 / 255.0, 50.0 / 255.0, 1.0];

/// 路灯：原版只取**未损坏的左半** 16×80 子图
/// （`pureCeleste.cs` Lamp：`GetSubtexture(broken ? 16 : 0, 0, 16, 80)`），
/// 图集里 30×77 的内容是损坏/完好两盏拼在一起的。
pub fn lamp_sprite(sprite: &Sprite) -> Sprite {
    let w = 16.min(sprite.w);
    let h = 80.min(sprite.h);
    Sprite {
        name: sprite.name.clone(),
        x: sprite.x,
        y: sprite.y,
        w,
        h,
        origin_x: 0,
        origin_y: 0,
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
