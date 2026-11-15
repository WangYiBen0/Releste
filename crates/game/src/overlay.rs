//! `~` debug overlay: the player hitbox plus a text readout, drawn in screen space.
//!
//! Toggling lives in the host (`crate::app` sets [`GameService::debug_open`]); the draws
//! are produced here and appended to the batch at [`Layer::Ui`], i.e. above every other
//! layer, so nothing in the scene can cover them.
//!
//! The readout reuses two sprites that are already in the gameplay atlas, so it needs no
//! new asset and no font pipeline:
//! - `util\pixel` -- a 3x3 opaque white block, used for every solid fill and for the
//!   one-pixel edges of a hollow box.
//! - `pico8\font` -- a 30x2 sheet of 4x5 glyph cells with a fixed 4px advance and the
//!   lowercase charset `abcdefghijklmnopqrstuvwxyz0123456789~!@#4%^&*()_+-=?:.`.
//!
//! Everything is placed in **base screen pixels** (320x180, origin top-left) and converted
//! with [`Camera::screen_to_world`], which is exact for the identity zoom the game runs at;
//! the same round-trip is what makes a quad land on the pixel it was asked for.

use releste_math::{Fx, Vec2};
use releste_render::{Camera, GpuRenderer, Layer, Sprite, SpriteDraw};

use crate::service::GameService;
use crate::sprites::group_id;

/// Opaque white block used for solid fills and for box edges.
const PIXEL_SPRITE: &str = "util\\pixel";
/// Debug font sheet (30 columns x 2 rows of 4x5 cells, lowercase only).
const FONT_SPRITE: &str = "pico8\\font";
/// Width of one glyph cell, in base pixels.
const GLYPH_W: f32 = 4.0;
/// Height of one glyph cell, in base pixels.
const GLYPH_H: f32 = 5.0;
/// Glyphs per font-sheet row.
const GLYPH_COLS: usize = 30;
/// Vertical pitch between font-sheet rows: a 5px cell plus a 1px blank separator.
const ROW_PITCH: f32 = 6.0;
/// Advance per character: vanilla's `print` advances a flat 4px with no kerning.
const ADVANCE: f32 = 4.0;
/// Line advance: one glyph cell plus a blank row.
const LINE_H: f32 = 6.0;
/// Padding around the text panel, in base pixels.
const PAD: f32 = 3.0;
/// Glyph order of the font sheet, cell by cell (the pico8 charset).
const FONT_MAP: &str = "abcdefghijklmnopqrstuvwxyz0123456789~!@#4%^&*()_+-=?:.";

/// Translucent black panel background (premultiplied: rgb stays 0).
const PANEL: [f32; 4] = [0.0, 0.0, 0.0, 0.8];
/// Text and hitbox colors.
const TEXT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// A solid, collidable hitbox (vanilla draws those red).
const HITBOX: [f32; 4] = [1.0, 0.0, 0.0, 1.0];

/// Append this frame's overlay draws (hitbox + readout) to `out`.
///
/// `alpha` interpolates the player position the same way the sprite does, so the box
/// tracks the drawn body instead of lagging a tick behind it.
pub(crate) fn push_overlay(
    out: &mut Vec<SpriteDraw>,
    service: &GameService,
    gpu: &GpuRenderer,
    camera: &Camera,
    alpha: f32,
) {
    let Some((pixel_group, pixel_sprite)) = service.session.atlas.sprite(PIXEL_SPRITE) else {
        return;
    };
    let pixel_atlas = group_id(&service.session.atlas, pixel_group);
    let Some((pw, ph)) = gpu.texture_size(pixel_atlas) else {
        return;
    };
    let pixel_uv = GpuRenderer::uv_for_sprite(pixel_sprite, Vec2::from_f32s(pw as f32, ph as f32));
    let zoom = camera.zoom;

    // Player hitbox: the interpolated position, plus the ducking box's top-left offset.
    let a = Fx::from_f32(alpha.clamp(0.0, 1.0));
    let pos = service.player.prev_pos + (service.player.pos - service.player.prev_pos) * a;
    let (w, h, y_off) = service.player.box_dims();
    let world = Vec2::from_f32s(pos.x.to_f32(), pos.y.to_f32() + y_off as f32);
    // `hollow_box` takes base-screen pixels, while the hitbox is a world rectangle (vanilla
    // strokes it inside the camera matrix), so project it first.
    let top_left = camera.world_to_screen(world);
    hollow_box(
        out,
        camera,
        pixel_atlas,
        pixel_uv,
        zoom,
        top_left,
        w as f32,
        h as f32,
        HITBOX,
    );

    // Text readout at the top-left corner.
    let Some((font_group, font_sprite)) = service.session.atlas.sprite(FONT_SPRITE) else {
        return;
    };
    let font_atlas = group_id(&service.session.atlas, font_group);
    let Some((fw, fh)) = gpu.texture_size(font_atlas) else {
        return;
    };
    let tex = Vec2::from_f32s(fw as f32, fh as f32);
    let lines = readout_lines(service);
    let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as f32;
    let panel_w = PAD * 2.0 + widest * ADVANCE;
    let panel_h = PAD * 2.0 + lines.len() as f32 * LINE_H;
    screen_quad(
        out,
        camera,
        pixel_atlas,
        pixel_uv,
        zoom,
        0.0,
        0.0,
        panel_w,
        panel_h,
        PANEL,
    );
    for (row, line) in lines.iter().enumerate() {
        let y = PAD + row as f32 * LINE_H;
        for (col, ch) in line.chars().enumerate() {
            let Some(cell) = glyph_cell(ch) else {
                continue;
            };
            let uv = glyph_uv(font_sprite, cell, tex);
            let x = PAD + col as f32 * ADVANCE;
            screen_quad(
                out, camera, font_atlas, uv, zoom, x, y, GLYPH_W, GLYPH_H, TEXT,
            );
        }
    }
}

/// The overlay's text lines: room, position, velocity, and player state.
fn readout_lines(service: &GameService) -> Vec<String> {
    let p = &service.player;
    let (pos_x, pos_y) = p.pos.to_f32s();
    let (vel_x, vel_y) = p.vel.to_f32s();
    vec![
        format!("room {}", service.room_name()),
        format!("pos {pos_x:.1},{pos_y:.1}"),
        format!("vel {vel_x:.1},{vel_y:.1}"),
        format!("anim {}", p.anim_id),
        format!(
            "dash {}/{} ground {} duck {}",
            p.dashes, p.max_dashes, p.on_ground, p.ducking
        ),
        format!("stamina {:.0}", p.stamina.to_f32()),
    ]
}

/// Cell index of `ch` in the font sheet; unknown characters (including space) draw nothing.
///
/// The sheet is lowercase-only, so uppercase input folds down first -- the same way the
/// glyph map is consulted in the Pico-8 renderer.
fn glyph_cell(ch: char) -> Option<usize> {
    let lower = ch.to_ascii_lowercase();
    FONT_MAP.chars().position(|c| c == lower)
}

/// UV rectangle of one 4x5 glyph cell inside the font sheet.
fn glyph_uv(font: &Sprite, cell: usize, tex: Vec2) -> ((f32, f32), (f32, f32)) {
    let col = cell % GLYPH_COLS;
    let row = cell / GLYPH_COLS;
    let x = f32::from(font.x) + col as f32 * GLYPH_W;
    let y = f32::from(font.y) + row as f32 * ROW_PITCH;
    (
        (x / tex.x.to_f32(), y / tex.y.to_f32()),
        (
            (x + GLYPH_W) / tex.x.to_f32(),
            (y + GLYPH_H) / tex.y.to_f32(),
        ),
    )
}

/// Push a one-pixel hollow box around `rect` (vanilla `Draw.HollowRect`): four edges, and
/// the far edges are inset by one pixel so they stay inside the rectangle.
#[allow(clippy::too_many_arguments)]
fn hollow_box(
    out: &mut Vec<SpriteDraw>,
    camera: &Camera,
    atlas: u32,
    uv: ((f32, f32), (f32, f32)),
    zoom: Fx,
    rect: Vec2,
    w: f32,
    h: f32,
    color: [f32; 4],
) {
    // Vanilla truncates the box to whole pixels before stroking it.
    let x = rect.x.to_f32().floor();
    let y = rect.y.to_f32().floor();
    let w = w.max(2.0);
    let h = h.max(2.0);
    screen_quad(out, camera, atlas, uv, zoom, x, y, w, 1.0, color);
    screen_quad(out, camera, atlas, uv, zoom, x, y + h - 1.0, w, 1.0, color);
    screen_quad(out, camera, atlas, uv, zoom, x, y, 1.0, h, color);
    screen_quad(out, camera, atlas, uv, zoom, x + w - 1.0, y, 1.0, h, color);
}

/// Push one quad given in base screen pixels.
#[allow(clippy::too_many_arguments)]
fn screen_quad(
    out: &mut Vec<SpriteDraw>,
    camera: &Camera,
    atlas: u32,
    uv: ((f32, f32), (f32, f32)),
    zoom: Fx,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: [f32; 4],
) {
    let world = camera.screen_to_world(Vec2::from_f32s(x, y));
    let size = Vec2::from_f32s(w, h) / zoom;
    out.push(
        SpriteDraw::new(atlas, world, size, uv)
            .with_layer(Layer::Ui.value())
            .with_color(color),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_glyph_sheet_covers_lowercase_digits_and_punctuation() {
        assert_eq!(glyph_cell('a'), Some(0));
        assert_eq!(glyph_cell('z'), Some(25));
        assert_eq!(glyph_cell('0'), Some(26));
        assert_eq!(glyph_cell('9'), Some(35));
        assert_eq!(glyph_cell('.'), Some(53));
        // Uppercase folds down; space and anything unknown draw nothing.
        assert_eq!(glyph_cell('A'), Some(0));
        assert_eq!(glyph_cell(' '), None);
        assert_eq!(glyph_cell('\t'), None);
    }

    #[test]
    fn the_font_map_is_exactly_the_54_glyphs_the_sheet_holds() {
        assert_eq!(FONT_MAP.chars().count(), 54);
        // Every mapped glyph has its own cell, and no cell is skipped.
        let first: Vec<usize> = FONT_MAP.chars().filter_map(glyph_cell).collect();
        let last = *first.last().expect("glyphs");
        assert_eq!(last, 53);
        // 30 cells per row: the last glyph of row 0 is `9`, the first of row 1 is `~`.
        assert_eq!(glyph_cell('9'), Some(35));
        assert_eq!(glyph_cell('~'), Some(36));
    }

    #[test]
    fn glyph_uvs_step_four_pixels_per_column_and_six_per_row() {
        let font = Sprite {
            name: "pico8\\font".to_string(),
            x: 1168,
            y: 288,
            w: 119,
            h: 11,
            origin_x: 0,
            origin_y: 0,
            frame_w: 120,
            frame_h: 12,
            source: None,
        };
        let tex = Vec2::from_f32s(4096.0, 4096.0);
        let first = glyph_uv(&font, 0, tex);
        assert_eq!(first.0, (1168.0 / 4096.0, 288.0 / 4096.0));
        assert_eq!(first.1, ((1168.0 + 4.0) / 4096.0, (288.0 + 5.0) / 4096.0));
        // Second column.
        let second = glyph_uv(&font, 1, tex);
        assert_eq!(second.0, (1172.0 / 4096.0, 288.0 / 4096.0));
        // First cell of the second band: 5px cell + a 1px blank separator row.
        let wrapped = glyph_uv(&font, 30, tex);
        assert_eq!(wrapped.0, (1168.0 / 4096.0, 294.0 / 4096.0));
    }

    #[test]
    fn a_screen_quad_lands_on_the_base_pixel_it_was_asked_for() {
        // The overlay places quads in base screen pixels; the batch then projects them
        // through the world, so the round-trip must be exact for the drawn frame.
        let camera = Camera::at(Vec2::from_f32s(123.5, 45.25));
        let mut out = Vec::new();
        screen_quad(
            &mut out,
            &camera,
            7,
            ((0.0, 0.0), (1.0, 1.0)),
            camera.zoom,
            12.0,
            34.0,
            8.0,
            11.0,
            TEXT,
        );
        assert_eq!(out.len(), 1);
        let draw = out[0];
        assert_eq!(draw.atlas, 7);
        assert_eq!(draw.layer, Layer::Ui.value());
        let (sx, sy) = camera.world_to_screen(draw.position).to_f32s();
        assert!((sx - 12.0).abs() < 0.01, "x landed on {sx}");
        assert!((sy - 34.0).abs() < 0.01, "y landed on {sy}");
        let (sw, sh) = camera.world_to_screen(draw.position + draw.size).to_f32s();
        assert!((sw - 20.0).abs() < 0.01, "right edge landed on {sw}");
        assert!((sh - 45.0).abs() < 0.01, "bottom edge landed on {sh}");
    }

    #[test]
    fn a_hollow_box_draws_four_edges_inside_the_rectangle() {
        let camera = Camera::at(Vec2::ZERO);
        let mut out = Vec::new();
        hollow_box(
            &mut out,
            &camera,
            1,
            ((0.0, 0.0), (1.0, 1.0)),
            camera.zoom,
            Vec2::from_f32s(10.0, 20.0),
            8.0,
            11.0,
            HITBOX,
        );
        assert_eq!(out.len(), 4);
        let quad = |d: &SpriteDraw| {
            let (x, y) = camera.world_to_screen(d.position).to_f32s();
            (x, y, d.size.to_f32s().0, d.size.to_f32s().1)
        };
        // Top edge, bottom edge (inset by one), left edge, right edge (inset by one).
        assert_eq!(quad(&out[0]), (10.0, 20.0, 8.0, 1.0));
        assert_eq!(quad(&out[1]), (10.0, 30.0, 8.0, 1.0));
        assert_eq!(quad(&out[2]), (10.0, 20.0, 1.0, 11.0));
        assert_eq!(quad(&out[3]), (17.0, 20.0, 1.0, 11.0));
    }
}
