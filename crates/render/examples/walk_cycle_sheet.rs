//! Visual cross-check: assemble the frames the walking animations use into
//! one magnified contact sheet.
//!
//! Usage (from the repository root):
//!
//! ```text
//! cargo run -p releste-render --example walk_cycle_sheet -- .devtmp/walk-cycle.png
//! ```
//!
//! Only rectangles and PNGs that actually exist in the atlas are drawn, so the
//! sheet shows exactly the frames the engine would draw. It is the quickest way
//! to judge "are these the vanilla frames, in the vanilla order?" -- but the
//! verdict still belongs to `Sprites.xml` and `references/pureCeleste.cs`,
//! not to your eyes.
//!
//! Companion tool of `.agents/skills/releste-vanilla-audit` section 6.5.

use std::path::PathBuf;

use image::{imageops, GenericImageView, RgbaImage};
use releste_render::atlas::AtlasDescriptor;

/// Magnification (pixel art, nearest neighbour).
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

    // The four walking animations plus their real frame counts (the engine's
    // `Gameplay.sprites.toml` mirrors `Sprites.xml`):
    // idle has no `frames` attribute -> enumerate all; flip hardcodes
    // `frames="0-7"`, while the atlas also holds flip08 (used by another
    // sprite), so that one is capped.
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
        assert!(!row_frames.is_empty(), "{row} has no frames at all");
        frames.push(row_frames);
    }

    let cell_w = 20 * SCALE; // vanilla player frames are up to 16px wide
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
        "{} -> {} ({}x{}) rows: {:?}",
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
