//! 视觉对照：把玩家走路相关的动画帧从真实图集里拼成一张放大对照图。
//!
//! 用法（仓库根目录）：
//!
//! ```text
//! cargo run -p releste-render --example walk_cycle_sheet -- .devtmp/walk-cycle.png
//! ```
//!
//! 只用图集里真实存在的矩形与 PNG，所以画出来的就是引擎会画的那些帧 ——
//! 用来判断"动画是不是原版那几帧/顺序对不对"最快；但结论仍要回到
//! `Sprites.xml` 与 `references/pureCeleste.cs` 的出处，别用眼睛当权威。
//!
//! 这是 `.agents/skills/releste-vanilla-audit` §6.5 的配套工具。

use std::path::PathBuf;

use image::{imageops, GenericImageView, RgbaImage};
use releste_render::atlas::AtlasDescriptor;

/// 放大倍数（像素画，用最近邻）。
const SCALE: u32 = 4;

fn main() {
    let out: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| ".devtmp/walk-cycle.png".into())
        .into();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let atlas_path = root.join("assets/atlas/Gameplay.atlas");
    let desc = AtlasDescriptor::from_file(&atlas_path).expect("Gameplay.atlas");
    let group = desc
        .textures
        .iter()
        .find(|t| t.name == "Gameplay0")
        .expect("Gameplay0 group");
    let page: RgbaImage = image::open(root.join("assets").join(&group.source))
        .expect("Gameplay0.png")
        .to_rgba8();

    // 走路相关的四条动画 + 各自的真实帧数（引擎的
    // `Gameplay.sprites.toml` 就是照 `Sprites.xml` 来的）：
    // idle 无 `frames` 属性 → 枚举全部；flip 写死 `frames="0-7"`，
    // 而图集里还存在 flip08（别的 sprite 用），所以这里要截断。
    let rows: [(&str, Option<usize>); 4] = [
        ("idle", None),
        ("runSlow", None),
        ("runFast", None),
        ("flip", Some(8)),
    ];
    let mut frames: Vec<Vec<&releste_render::atlas::Sprite>> = Vec::new();
    for (row, cap) in rows {
        let mut row_frames = Vec::new();
        for i in 0..64 {
            if cap.is_some_and(|c| i >= c) {
                break;
            }
            let name = format!("characters/player/{row}{i:02}");
            match group.sprites.iter().find(|s| {
                s.name.to_lowercase().replace('/', "\\") == name.to_lowercase().replace('/', "\\")
            }) {
                Some(s) => row_frames.push(s),
                None => break,
            }
        }
        assert!(!row_frames.is_empty(), "{row} 一帧都没有");
        frames.push(row_frames);
    }

    let cell_w = 20 * SCALE; // 原版玩家帧最宽 16px，留点余地
    let cell_h = 16 * SCALE;
    let width = cell_w * frames.iter().map(Vec::len).max().unwrap() as u32;
    let height = cell_h * frames.len() as u32;
    let mut sheet = RgbaImage::new(width, height);
    for (r, row_frames) in frames.iter().enumerate() {
        for (c, s) in row_frames.iter().enumerate() {
            let crop = page
                .view(s.x as u32, s.y as u32, s.w as u32, s.h as u32)
                .to_image();
            let big = imageops::resize(
                &crop,
                s.w as u32 * SCALE,
                s.h as u32 * SCALE,
                imageops::FilterType::Nearest,
            );
            imageops::replace(
                &mut sheet,
                &big,
                i64::from(c as u32 * cell_w + (cell_w - big.width()) / 2),
                i64::from(r as u32 * cell_h + (cell_h - big.height()) / 2),
            );
        }
    }
    sheet.save(&out).expect("write sheet");
    println!(
        "{} -> {} ({}x{}) 每行: {:?}",
        atlas_path.display(),
        out.display(),
        width,
        height,
        rows.iter()
            .zip(&frames)
            .map(|((r, _), f)| format!("{r}={}", f.len()))
            .collect::<Vec<_>>()
    );
}
