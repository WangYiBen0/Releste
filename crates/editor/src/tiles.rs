//! Real tile rendering for the editor canvas.
//!
//! The canvas used to paint a hue derived from the tile value, which is useless
//! for authoring: you could not see what you were placing. This module loads the
//! same `.atlas` descriptors and `tilesets.toml` the game uses, decodes the
//! packed pages with the `image` crate, and turns a tile value into a source
//! rectangle into that page. egui then draws the sub-rectangle.
//!
//! Layering: no egui here. Everything is plain data plus an RGBA page, so the
//! mapping is unit-testable without a GPU (AGENTS.md 9).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use releste_map::TilesetsDoc;
use releste_render::{Sprite, SpriteAtlas, TextureGroup};

/// Pixel size of one tile in Celeste art.
///
/// This is 8, not 16: a Celeste tile cell is 8x8 source pixels
/// (`crates/game/src/sprites.rs:14` uses the same constant). The editor canvas
/// used 16 before tile art existed, which would have shown every tile at double
/// scale and cut the wrong cell out of the atlas.
pub const TILE_PX: i32 = 8;

/// One decoded atlas page, ready to hand to egui.
#[derive(Debug)]
pub struct Page {
    /// Page width in pixels.
    pub width: u32,
    /// Page height in pixels.
    pub height: u32,
    /// Straight RGBA8, row-major.
    pub rgba: Vec<u8>,
}

impl Page {
    /// Samples one pixel, for tests and for the brush preview.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y as usize) * (self.width as usize) + x as usize) * 4;
        self.rgba.get(i..i + 4).map(|s| [s[0], s[1], s[2], s[3]])
    }
}

/// Tile geometry inside an atlas page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileRect {
    /// Page index (an atlas may be split across several pages).
    pub page: usize,
    /// Source rectangle x in page pixels.
    pub x: u32,
    /// Source rectangle y in page pixels.
    pub y: u32,
    /// Source rectangle width in page pixels.
    pub w: u32,
    /// Source rectangle height in page pixels.
    pub h: u32,
}

/// Loads tile art for one map, or degrades to "no art" without failing.
///
/// A missing `assets/` tree is normal for a fresh checkout (it is gitignored),
/// so this warns and disables art rather than erroring: the editor must still
/// work for tile values and entity placement.
pub struct TileRenderer {
    atlas: Option<SpriteAtlas>,
    defs: TilesetsDoc,
    pages: Vec<Page>,
    /// tile id -> tileset texture path, cached from `defs`.
    paths: HashMap<char, String>,
}

impl TileRenderer {
    /// Builds a renderer for the atlas named by the map (for example `Gameplay`).
    ///
    /// `assets` is the converted asset root; `atlas/<name>.atlas` and
    /// `atlas/tilesets.toml` are read from it.
    pub fn load(assets: &Path, atlas_name: &str) -> Self {
        let atlas_dir = assets.join("atlas");

        let defs = match std::fs::read_to_string(atlas_dir.join("tilesets.toml")) {
            Ok(text) => toml::from_str::<TilesetsDoc>(&text).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "tilesets.toml is not valid; tile art disabled");
                TilesetsDoc::default()
            }),
            Err(e) => {
                tracing::warn!(
                    path = %atlas_dir.join("tilesets.toml").display(),
                    error = %e,
                    "no tilesets.toml; tile art disabled (run the content pipeline)"
                );
                TilesetsDoc::default()
            }
        };

        let atlas_path = atlas_dir.join(format!("{atlas_name}.atlas"));
        let atlas = match SpriteAtlas::load(&atlas_path, &atlas_dir) {
            Ok(a) => Some(a),
            Err(e) => {
                tracing::warn!(
                    path = %atlas_path.display(),
                    error = %e,
                    "cannot load atlas; tile art disabled"
                );
                None
            }
        };

        let pages = atlas.as_ref().map(|a| decode_pages(a)).unwrap_or_default();

        let paths = defs
            .tilesets
            .iter()
            .filter_map(|(id, def)| {
                let ch = id.chars().next()?;
                Some((ch, def.path.clone()))
            })
            .collect();

        TileRenderer {
            atlas,
            defs,
            pages,
            paths,
        }
    }

    /// Whether any art is available.
    pub fn has_art(&self) -> bool {
        self.atlas.is_some() && !self.pages.is_empty()
    }

    /// The decoded pages.
    pub fn pages(&self) -> &[Page] {
        &self.pages
    }

    /// The tileset definitions (for the palette and the autotiler).
    pub fn defs(&self) -> &TilesetsDoc {
        &self.defs
    }

    /// The atlas texture path for a tile value, if the id is known.
    pub fn texture_path(&self, value: u16) -> Option<&str> {
        self.paths
            .get(&releste_map::tile_char(value))
            .map(String::as_str)
    }

    /// The source rectangle for a tile value at autotiler coordinates `(col, row)`.
    ///
    /// `None` means "no art for this tile", which the caller draws as a flat
    /// placeholder so the user still sees that something is there.
    pub fn rect_for(&self, value: u16, col: u32, row: u32) -> Option<TileRect> {
        let atlas = self.atlas.as_ref()?;
        let path = self.texture_path(value)?;
        let (group, sprite) = atlas.sprite(path)?;
        let page = self.page_of(group, sprite)?;
        let cell = tile_cell(sprite, col, row);
        Some(TileRect {
            page,
            x: cell.x.max(0) as u32,
            y: cell.y.max(0) as u32,
            w: cell.w.max(0) as u32,
            h: cell.h.max(0) as u32,
        })
    }

    /// Which decoded page a sprite lives on.
    fn page_of(&self, group: &TextureGroup, sprite: &Sprite) -> Option<usize> {
        let atlas = self.atlas.as_ref()?;
        let descriptor = atlas.descriptor();
        descriptor
            .textures
            .iter()
            .position(|t| std::ptr::eq(t, group))
            .or_else(|| {
                descriptor.textures.iter().position(|t| {
                    t.name == group.name && t.sprites.iter().any(|s| s.name == sprite.name)
                })
            })
    }
}

/// Cuts a `TILE_PX` cell out of a tileset sprite, clamped to the sprite bounds.
///
/// Mirrors the game's `sprites::tile_cell` so the editor shows exactly what the
/// game will draw.
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

/// Decodes every page of an atlas into RGBA8.
fn decode_pages(atlas: &SpriteAtlas) -> Vec<Page> {
    let mut out = Vec::new();
    for group in &atlas.descriptor().textures {
        if !group.kind.is_decodable() {
            out.push(empty_page());
            continue;
        }
        let path = atlas.group_payload_path(group);
        match decode_png(&path) {
            Ok(page) => out.push(page),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot decode atlas page");
                out.push(empty_page());
            }
        }
    }
    out
}

/// A page that failed to decode, kept so page indices stay aligned.
fn empty_page() -> Page {
    Page {
        width: 0,
        height: 0,
        rgba: Vec::new(),
    }
}

/// Decodes one PNG file into straight RGBA8.
fn decode_png(path: &PathBuf) -> Result<Page, String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    Ok(Page {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sprite(x: i16, y: i16, w: i16, h: i16) -> Sprite {
        Sprite {
            name: "tilesets/dirt".into(),
            x,
            y,
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
    fn tile_cell_picks_the_expected_cell() {
        let s = sprite(100, 200, 48, 120);
        let c = tile_cell(&s, 0, 0);
        assert_eq!((c.x, c.y, c.w, c.h), (100, 200, 8, 8));

        let c = tile_cell(&s, 2, 1);
        assert_eq!((c.x, c.y), (100 + 16, 200 + 8));
    }

    #[test]
    fn tile_cell_clamps_out_of_range_to_the_last_full_cell() {
        // A 48x120 sprite holds 6x15 full 8px cells; cell 99 clamps to the last.
        let s = sprite(0, 0, 48, 120);
        let c = tile_cell(&s, 99, 99);
        assert_eq!((c.x, c.y), (40, 112), "must clamp, not run off the sprite");
        assert_eq!((c.w, c.h), (8, 8));
    }

    #[test]
    fn tile_cell_handles_a_sprite_smaller_than_one_cell() {
        let s = sprite(5, 5, 4, 4);
        let c = tile_cell(&s, 1, 1);
        assert_eq!((c.x, c.y), (5, 5));
        assert_eq!((c.w, c.h), (4, 4), "never exceed the sprite");
    }

    #[test]
    fn page_pixel_is_bounds_checked() {
        let page = Page {
            width: 2,
            height: 2,
            rgba: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
        };
        assert_eq!(page.pixel(0, 0), Some([1, 2, 3, 4]));
        assert_eq!(page.pixel(1, 1), Some([13, 14, 15, 16]));
        assert_eq!(page.pixel(2, 0), None);
        assert_eq!(page.pixel(0, 2), None);
    }

    #[test]
    fn missing_assets_degrade_to_no_art_instead_of_failing() {
        let r = TileRenderer::load(Path::new("/nonexistent/releste/assets"), "Gameplay");
        assert!(!r.has_art());
        assert!(r.pages().is_empty());
        assert_eq!(r.rect_for(1, 0, 0), None);
    }
}
