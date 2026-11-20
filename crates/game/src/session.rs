//! A loaded `.map` plus its atlas, and the static part of "what to draw this frame".
//!
//! Maps are data (AGENTS.md 4.2): here we only turn tiles and entities into
//! [`SpriteDraw`], with no gameplay branching at all.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashSet;

use anyhow::{bail, Context as _, Result};
use releste_map::{Backdrop, MapDocument, Room};
use releste_math::{Fx, Vec2};
use releste_render::atlas::SpriteAtlas;
use releste_render::{GpuRenderer, Layer, SpriteDraw};
use releste_world::{EntityData, FieldValue};
use tracing::{debug, warn};

use crate::paths::Paths;
use crate::sprites;

/// Normalized texture rectangle (`uv_min`, `uv_max`) of an atlas sprite.
type SpriteUv = ((f32, f32), (f32, f32));

/// World pixels per tile (320 / 40 = 8px for `lvl_-1`).
const TILE: f32 = sprites::TILE_PX as f32;

/// Draw layer for `introCrusher`: vanilla `Depth = -10501` (`references/Celeste.exe.cs.d/Celeste/IntroCrusher.cs:25`),
/// where smaller values draw in front -- ahead of SolidTiles' -10000 (`references/Celeste.exe.cs.d/Celeste/SolidTiles.cs:19`), with
/// the player (`0`) further back still, but in front of the foreground parallax layer
/// that is drawn last. The engine layer order (smaller value draws first) uses
/// `Effects(300) < this layer < Foreground(500)`.
const LAYER_INTRO_CRUSHER: i32 = Layer::Foreground.value() - 100;

/// The fallback atlas vanilla consults when a `atlas="game"` texture is absent from
/// Gameplay (`references/Celeste.exe.cs.d/Celeste/MapData.cs:345-362`): `GFX.Misc`. The content pipeline writes it as one
/// `png` group per sprite.
const MISC_ATLAS: &str = "Misc";

/// Draw layer for `wire` (`above=false`): vanilla `Depth = 2000`
/// (`references/Celeste.exe.cs.d/Celeste/Wire.cs:20`) -- drawn after the background layer and before the
/// terrain and the player (in vanilla, smaller values draw in front). The engine layer
/// order (smaller value draws first) places it between
/// `BgTiles(-500)` and `Tiles(0)`.
const LAYER_WIRE: i32 = Layer::BgTiles.value() + 100;

/// Draw layer for `wire above=true`: vanilla `Depth = -8500` (`references/Celeste.exe.cs.d/Celeste/Wire.cs:25`) -- before the
/// player and after the terrain. The engine draws terrain after the player, so the two
/// vanilla orders cannot both hold; take "before the player, before the crusher and the
/// foreground": after `Player(200)` and before `LAYER_INTRO_CRUSHER(400)` /
/// `Foreground(500)`. 0-Intro has no `above` wire (the property table has no such key).
const LAYER_WIRE_ABOVE: i32 = Layer::Player.value() + 150;

/// Draw layer for `BridgeTile`: vanilla sets no Depth -> `Entity.Depth` defaults to
/// `0` (property at `references/Celeste.exe.cs.d/Monocle/Entity.cs:30`), the same depth as the player; the engine
/// uses `Entities(100)` -- after the terrain and before the player (the planks lie in
/// front of the terrain and Madeline walks over them).
const LAYER_BRIDGE: i32 = Layer::Entities.value();

/// Draw layer for the `bgtiles` decoration overlay.
///
/// Vanilla does **not** give `bgtiles` its own depth: the loader overlays the grid
/// **into** the background tile grid (`LevelLoader`, `references/Celeste.exe.cs.d/Celeste/LevelLoader.cs:283-287`:
/// `backgroundTiles.Tiles.Overlay(GFX.SceneryTiles, tiles, ...)`), so those cells stop
/// showing the autotiled `bgDirt` tile and show the scenery tile instead, all at
/// `BackgroundTiles.Depth = 10000`. The engine keeps them as a separate pass, so this
/// must sit just **in front of** `BgTiles` -- putting it behind would let the `bg` layer
/// paint over it.
const LAYER_BG_SCENERY: i32 = Layer::BgTiles.value() + 20;

/// Draw layer for `bgdecals`: vanilla `Depth = 9000` (`references/Celeste.exe.cs.d/Celeste/Level.cs:1293-1296`).
///
/// Vanilla draws the **larger** depth first, so `BackgroundTiles` (10000) and its
/// `bgtiles` overlay are behind the background decals (9000), and both are behind
/// `SolidTiles` (-10000). Mirrored here: in front of `LAYER_BG_SCENERY`, behind
/// `Layer::Tiles(0)`.
const LAYER_BG_DECAL: i32 = Layer::BgTiles.value() + 50;

/// Draw layer for the `fgtiles` decoration overlay.
///
/// Vanilla overlays `fgtiles` **into** the solid tile grid (`LevelLoader`, `references/Celeste.exe.cs.d/Celeste/LevelLoader.cs:288-293`:
/// `solidTiles.Tiles.Overlay(GFX.SceneryTiles, tiles, ...)`), so the decorations are drawn
/// cell by cell among the solid tiles. The engine keeps them in a separate pass, so they
/// go immediately after `Tiles(0)`: in front of the terrain, behind the entities.
const LAYER_FG_SCENERY: i32 = Layer::Tiles.value() + 50;

/// Draw layer for `fgdecals`: vanilla `Depth = -10500` (`references/Celeste.exe.cs.d/Celeste/Level.cs:1289-1292`).
///
/// In vanilla the smaller depth draws in front, so the foreground decals are in front of
/// the player (`0`) and of the `wire above` (-8500), but behind the `introCrusher`
/// (-10501). Mirrored here: after `LAYER_WIRE_ABOVE(350)` and before
/// `LAYER_INTRO_CRUSHER(400)`.
const LAYER_FG_DECAL: i32 = Layer::Foreground.value() - 120;

/// The 16-segment polyline of one `Wire`: returns `(start, length, rotation radians)`
/// per segment.
///
/// Follows `Wire.Render` (`references/Celeste.exe.cs.d/Celeste/Wire.cs:32-43`): control point = midpoint of
/// the two ends sagged 24px (`references/Celeste.exe.cs.d/Celeste/Wire.cs:35`); points come from the quadratic Bezier
/// `GetPoint(i/16)` (Everest `Patches/Monocle/SimpleCurve.cs:22-28`); each segment is
/// a `Draw.Line` that uses the previous segment's end as its start. Wind sway is taken
/// as 0 (see the [`Session::push_wire`] docs).
fn wire_segments(from: (f32, f32), to: (f32, f32)) -> Vec<((f32, f32), f32, f32)> {
    let (fx, fy) = from;
    let (tx, ty) = to;
    let (cx, cy) = ((fx + tx) / 2.0, (fy + ty) / 2.0 + 24.0);
    let point = |t: f32| -> (f32, f32) {
        let u = 1.0 - t;
        (
            u * u * fx + 2.0 * u * t * cx + t * t * tx,
            u * u * fy + 2.0 * u * t * cy + t * t * ty,
        )
    };

    let mut out = Vec::with_capacity(16);
    let (mut sx, mut sy) = (fx, fy);
    for i in 1..=16 {
        let (px, py) = point(i as f32 / 16.0);
        let (dx, dy) = (px - sx, py - sy);
        let len = (dx * dx + dy * dy).sqrt();
        if len > 0.0 {
            out.push(((sx, sy), len, dy.atan2(dx)));
        }
        sx = px;
        sy = py;
    }
    out
}

/// Vanilla `Bridge`'s `tileSizes` table (`references/Celeste.exe.cs.d/Celeste/Bridge.cs:47-57`):
/// `(image x, width, height)`; `y` is always 0.
const BRIDGE_TILE_SIZES: [(i16, i16, i16); 11] = [
    (0, 16, 52),
    (16, 8, 52),
    (24, 8, 52),
    (32, 8, 52),
    (40, 8, 52),
    (48, 8, 52),
    (56, 8, 52),
    (64, 8, 52),
    (72, 8, 52),
    (80, 16, 52),
    (96, 8, 52),
];

/// The vertical strips one `BridgeTile` should draw: `(image x, image y, width,
/// height, y relative to the deck)`.
///
/// Follows the `BridgeTile` constructor (`references/Celeste.exe.cs.d/Celeste/BridgeTile.cs:8-62`): a width-16
/// end cap is cut along its height into 24 + 12 + 12 + ... (`references/Celeste.exe.cs.d/Celeste/BridgeTile.cs:26-35`), while the
/// rest is a single strip (`references/Celeste.exe.cs.d/Celeste/BridgeTile.cs:38-45`); both flatten the image y to `-8`
/// (`references/Celeste.exe.cs.d/Celeste/BridgeTile.cs:46-50`).
fn bridge_tile_strips(cell_x: i16, cell_w: i16, cell_h: i16) -> Vec<(i16, i16, i16, i16, f32)> {
    let mut out = Vec::new();
    if cell_w == 16 {
        let mut offset = 0i16;
        let mut strip = 24i16;
        while offset < cell_h {
            let h = strip.min(cell_h - offset);
            out.push((cell_x, offset, cell_w, h, offset as f32 - 8.0));
            offset += strip;
            strip = 12;
        }
    } else {
        out.push((cell_x, 0, cell_w, cell_h, -8.0));
    }
    out
}

/// The cell-by-cell layout of one `Bridge`: `(deck x, image x, width, height,
/// visible)`.
///
/// Follows `Bridge.Added` (`references/Celeste.exe.cs.d/Celeste/Bridge.cs:32-59`): starting at `Position`,
/// advance cell by cell along `width`, cycling through `tileSizes`; when `num` falls in
/// `2..=7` vanilla picks a wood-grain strip with `Calc.Random.Next(6)` (seeded by
/// `PushRandom(1)`, `references/Celeste.exe.cs.d/Celeste/Bridge.cs:49`), and those 6 candidates are **all 8px wide**, so the
/// engine just takes them by index -- the stepping matches vanilla cell for cell, and
/// only the image pattern of the 8px strips becomes a fixed sequence.
///
/// Returns **all** cells and marks with a `bool` whether each falls in the gap, because
/// vanilla "advances the cursor first, then decides whether to `level.Add`"
/// (`references/Celeste.exe.cs.d/Celeste/Bridge.cs:50-54`): the gap width still counts toward the stepping, and dropping the
/// gap cells outright would shift every plank after the gap.
fn bridge_cells(start_x: f32, width: i32, gap: (f32, f32)) -> Vec<(f32, i16, i16, i16, bool)> {
    let (gap_start, gap_end) = gap;
    let mut out = Vec::new();
    let mut x = start_x;
    let mut num = 0usize;
    while x < start_x + width as f32 {
        let (cell_x, cell_w, cell_h) = BRIDGE_TILE_SIZES[num];
        out.push((x, cell_x, cell_w, cell_h, x < gap_start || x >= gap_end));
        x += cell_w as f32;
        num = (num + 1) % BRIDGE_TILE_SIZES.len();
    }
    out
}

/// The draw flags a `parallax` backdrop resolves to: `(flip_x, flip_y, additive)`.
///
/// `Parallax.Render` (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:87-97`) derives a single `SpriteEffects` value from the
/// two booleans (`FlipX && FlipY` -> both, otherwise whichever is set), which the
/// engine expresses as two independent UV swaps; and `ParseBackdrop`
/// (`references/Celeste.exe.cs.d/Celeste/MapData.cs:383-398`) lowercases `blendmode` before comparing it with `"additive"`, so
/// the mode is matched case-insensitively here too.
fn parallax_flags(bd: &Backdrop) -> (bool, bool, bool) {
    (
        bd.flip_x,
        bd.flip_y,
        bd.blend.eq_ignore_ascii_case("additive"),
    )
}

/// The effective color of a single `Backdrop`: all-zero is treated as "unset" and
/// takes vanilla's default white (starting from `references/Celeste.exe.cs.d/Celeste/MapData.cs:514`
/// `backdrop.Color = Color.White`; `references/Celeste.exe.cs.d/Celeste/MapData.cs:515-521` only override it when
/// `HasAttr("color")`); otherwise, as vanilla does, scale it once by `alpha`
/// (`references/Celeste.exe.cs.d/Celeste/MapData.cs:523-525` `backdrop.Color *= child.AttrFloat("alpha");`).
fn backdrop_tint(bd: &Backdrop) -> [f32; 4] {
    let c = bd.effective_color();
    [
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
        c[3] as f32 / 255.0,
    ]
}

/// Parallax tiling geometry: given a `Backdrop`, the camera position and the texture
/// size, compute the top-left corner (screen coordinates) of every piece to draw this
/// frame. A pure function, so it is easy to unit-test against vanilla.
///
/// Step by step against `Parallax.Render` (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:42-115`):
/// 1. `v = (Camera.Position + CameraOffset).Floor()` (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:44`). The engine has no
///    `Backdrop.CameraOffset` concept yet (`CameraOffset` is a field unique to
///    `Parallax`, default `Vector2.Zero`, `references/Celeste.exe.cs.d/Celeste/Parallax.cs:8-9`), so it uses the camera
///    position directly.
/// 2. `p = (Position - v*Scroll).Floor()` (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:45`), where `Position` is the
///    accumulator that `Parallax.Update` advances by `Speed * dt` (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:28-30`).
/// 3. If `LoopX`/`LoopY`: two `while` loops fold `p.X`/`p.Y` back (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:64-85`), in
///    the same order as vanilla (first `<0` add, then `>0` subtract); with `!Loop`
///    nothing is folded.
/// 4. A double `for` tiles out to `320x180` (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:99-113`) -- **condition first**,
///    outer X and inner Y: when the start is off-screen (`p.X >= 320` or `p.Y >= 180`)
///    that axis draws nothing; with `!LoopY` the inner loop `break`s after one piece,
///    and with `!LoopX` the outer loop `break`s after one column. Each piece is
///    `Texture.Draw(pos, Vector2.Zero, color, 1, 0, flip)` (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:103`), with origin
///    `Vector2.Zero` (top-left of the cropped sub-image) and scale `1`.
fn parallax_tiles(
    bd: &Backdrop,
    position: (f32, f32),
    camera_pos: (f32, f32),
    tex: (f32, f32),
    screen: (f32, f32),
) -> Vec<(f32, f32)> {
    // A 0-size texture would make the two while loops spin forever -- clamp to >=1
    // (vanilla `Texture.Width` is always >=1; this is defensive clamping on the engine
    // side, not a behavior difference).
    let (tw, th) = (tex.0.max(1.0), tex.1.max(1.0));
    // 1. v = (Camera + CameraOffset).Floor() (no CameraOffset).
    let (cam_x, cam_y) = (camera_pos.0.floor(), camera_pos.1.floor());
    // 2. p = (Position - v*Scroll).Floor(). `Position` starts at the parsed `x`/`y`
    // and drifts with `Speed` every tick (`Parallax.Update`), so the caller passes the
    // accumulated value rather than reading `bd.x`/`bd.y` directly.
    let mut px = (position.0 - cam_x * bd.scroll_x).floor();
    let mut py = (position.1 - cam_y * bd.scroll_y).floor();
    // 3. while folding (vanilla's two-while order: first <0 add, then >0 subtract).
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
    // 4. Double for (outer X, inner Y, condition first).
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

/// Everything a backdrop pass needs besides the drawings and the slice itself, bundled
/// so the two calls (backgrounds, foregrounds) stay readable.
struct BackdropCtx<'a> {
    /// Per-slot `Speed * dt` drift, parallel to visibility.
    drifts: &'a [Vec2],
    camera_pos: Vec2,
    visibility: &'a [bool],
    session: &'a Session,
    gpu: &'a GpuRenderer,
}

/// Background / foreground parallax texture tiling (`Parallax.Render`,
/// `references/Celeste.exe.cs.d/Celeste/Parallax.cs:42-115`).
///
/// The geometry goes through [`parallax_tiles`] (a pure function with reference unit
/// tests); this function only does texture lookup, tinting and conversion to
/// [`SpriteDraw`].
///
/// Engine differences (on record, not behavior deviations):
/// - `Texture.Draw(pos, Vector2.Zero, ...)` passes a rotation origin of zero, which the
///   spritebatch turns into `origin - DrawOffset`; because the packer stores
///   `DrawOffset = (-origin_x, -origin_y)` for a trimmed sub-image, the piece lands at
///   `pos - (origin_x, origin_y)`. The tiling geometry itself ([`parallax_tiles`]) works
///   in frame coordinates, and this function applies that trim offset when it builds the
///   [`SpriteDraw`] (`MTexture`, `references/Celeste.exe.cs.d/Monocle/MTexture.cs:181-184`).
/// - `blendmode="additive"` selects the backend's second pipeline
///   (`SpriteDraw::additive` -> `wgpu` `src + dst`), matching `BlendState.Additive`
///   (`references/Celeste.exe.cs.d/Celeste/MapData.cs:383-398`). `FlipX`/`FlipY` map to `SpriteDraw::with_flip_x`/`...
///   with_flip_y` exactly as `Parallax.Render` derives `SpriteEffects`
///   (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:87-97`). The horizontal drift driven by `WindMultiplier` (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:31`) is
///   still unported because `Level.Wind` is not modelled yet; no map in the asset set
///   sets a non-zero `wind`.
/// - `fadeIn` (`DoFadeIn`, `references/Celeste.exe.cs.d/Celeste/Parallax.cs:32-39`) and `FadeX`/`FadeY` (`Fader`,
///   `references/Celeste.exe.cs.d/Celeste/Backdrop.cs:9-44`) are not ported yet: 0-Intro's `ParseBackdrop` (`references/Celeste.exe.cs.d/Celeste/MapData.cs:376`
///   `DoFadeIn = bool.Parse(child.Attr("fadeIn", "false"))`) defaults to `false`, so
///   `fadeIn` is always `Visible ? 1 : 0` and is `1` once in the scene (no transition);
///   the current `Service` does not track `Visible`, so it stays `1` here; `FadeX`/
///   `FadeY` only have a value with `<apply fade=...>`, which 0-Intro never uses.
/// - The `Parallax.Alpha` (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:20`) field is never written by `ParseBackdrop` (the
///   `alpha` attribute is only multiplied into `backdrop.Color`, `references/Celeste.exe.cs.d/Celeste/MapData.cs:523-529`) and
///   defaults to `1` -> in `Render`, `num = fadeIn*Alpha*FadeAlphaMultiplier = 1` and
///   `color *= num` never triggers (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:46-58`), so using `effective_color()`
///   directly is equivalent.
fn push_backdrops(
    out: &mut Vec<SpriteDraw>,
    backdrops: &[Backdrop],
    layer: i32,
    ctx: BackdropCtx<'_>,
) {
    let BackdropCtx {
        drifts,
        camera_pos,
        visibility,
        session,
        gpu,
    } = ctx;
    let screen = (releste_render::BASE_WIDTH, releste_render::BASE_HEIGHT);
    for (i, bd) in backdrops.iter().enumerate() {
        // Room-filter visibility is refreshed by [`Session::update_backdrop_visibility`]
        // at vanilla `Backdrop.Update`'s cadence; this just reads it.
        if visibility.get(i).copied() != Some(true) {
            continue;
        }
        // Animated kinds (`snowfg` / `snowbg`) are drawn by
        // [`Session::update_backdrop_effects`]'s sibling pass, not from a static image.
        if crate::backdrop::handles_kind(&bd.kind) {
            continue;
        }
        // Only `parallax` is supported: the other kinds (snowBg/snowFg/mirrorFg/
        // godrays...) are separate `Backdrop` subclasses with their own `Render`, not
        // ported yet.
        if !bd.kind.eq_ignore_ascii_case("parallax") {
            if session.warn_once(&format!("backdrop-kind:{}", bd.kind)) {
                warn!(
                    kind = %bd.kind,
                    "non-parallax backdrop kind is not implemented yet; skipping it"
                );
            }
            continue;
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
            continue; // texture not uploaded (unconverted .data); `load_atlas` already warned
        };
        let uv = GpuRenderer::uv_for_sprite(sprite, Vec2::from_f32s(tw as f32, th as f32));
        let (w, h) = (f32::from(sprite.w), f32::from(sprite.h));
        let tint = backdrop_tint(bd);
        // Vanilla's early exit for `color.A <= 1` (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:60-63`) -- equivalent to
        // alpha <= 1/255.
        if tint[3] <= 1.0 / 255.0 {
            continue;
        }

        // `Position` for this slot: parsed `x`/`y` plus the `Speed` drift accumulated
        // since the map loaded (`Parallax.Update`, `references/Celeste.exe.cs.d/Celeste/Parallax.cs:28-30`).
        let drift = drifts.get(i).copied().unwrap_or(Vec2::ZERO);
        let position = (bd.x + drift.x.to_f32(), bd.y + drift.y.to_f32());
        let (flip_x, flip_y, additive) = parallax_flags(bd);
        // Vanilla passes `Vector2.Zero` as the origin and the spritebatch turns that into
        // `origin - DrawOffset`; for a trimmed sub-image the packer stored
        // `DrawOffset = (origin_x, origin_y)`, so the piece is drawn with the crop offset
        // *added* to the tile origin, i.e. the untrimmed frame sits at the tile origin
        // (`MTexture`, `references/Celeste.exe.cs.d/Monocle/MTexture.cs:181-184`). Releste used to skip the offset entirely, which
        // drew every trimmed `bgs/00/bg*` sheet `origin_y` pixels (23/3/45/29) too high
        // and left the bottom of the viewport unpainted. Flipping mirrors the art inside
        // the same box (`SpriteDraw` swaps the UVs), so the offset does not change.
        let (origin_x, origin_y) = sprite.origin();
        for (x, y) in parallax_tiles(bd, position, camera_pos.to_f32s(), (w, h), screen) {
            out.push(
                SpriteDraw::new(
                    atlas_id,
                    Vec2::from_f32s(x + origin_x, y + origin_y),
                    Vec2::from_f32s(w, h),
                    uv,
                )
                .with_layer(layer)
                .with_color(tint)
                .with_flip_x(flip_x)
                .with_flip_y(flip_y)
                .with_additive(additive)
                // Vanilla draws backdrops with an identity view matrix
                // (`BackdropRenderer.Matrix = Matrix.Identity`), so `parallax_tiles`
                // already baked the camera in: do not project them a second time.
                .with_screen(true),
            );
        }
    }
}

/// Tint for foreground tiles / entities / background tiles.
///
/// Every vanilla `TileGrid` uses the default `Color.White` + `Alpha = 1`
/// (`references/Celeste.exe.cs.d/Monocle/TileGrid.cs:10`/`references/Celeste.exe.cs.d/Monocle/TileGrid.cs:18`; nothing in vanilla writes it), so this is white.
const WHITE: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// A loaded map session.
pub struct Session {
    /// The map document (hot reload replaces it wholesale).
    pub doc: MapDocument,
    /// The atlas this map uses.
    pub atlas: SpriteAtlas,
    /// Atlas name (from `Tileset.atlas`).
    pub atlas_name: String,
    /// Tileset definitions (converted by content-pipeline from `ForegroundTiles.xml`).
    pub tilesets: releste_map::TilesetsDoc,
    /// **Background** tileset definitions (converted by content-pipeline from
    /// `BackgroundTiles.xml`).
    ///
    /// A room's `bg` layer must query this table rather than [`Session::tilesets`]: the
    /// same id is a completely different texture in the two tables (`'3'` = snow /
    /// bgBrickRuined).
    pub bg_tilesets: releste_map::TilesetsDoc,
    /// Player spawn anchor (world coordinates) -- vanilla semantics: the hitbox
    /// **bottom center** (feet). The service subtracts (W/2, H) to get the hitbox
    /// top-left; for provenance see [`vanilla_start`] and `player::PLAYER_W_PX`.
    pub spawn: Vec2,
    /// The room the spawn point is in.
    pub spawn_room: String,
    /// Animation table (`Gameplay.sprites.toml`; missing = empty table, the player
    /// falls back to static frames).
    pub anims: crate::sprites::AnimationTable,
    /// Visibility state per backdrop (vanilla `Backdrop.Visible`, `references/Celeste.exe.cs.d/Celeste/Backdrop.cs:32` starts as
    /// `true`; `Backdrop.Update` refreshes it every frame from `IsVisible` plus the
    /// transition rules, `references/Celeste.exe.cs.d/Celeste/Backdrop.cs:134-155`). Index = backgrounds + foregrounds
    /// concatenated in order.
    backdrop_visible: RefCell<Vec<bool>>,
    /// Self-animating backdrop effects (`snowfg` / `snowbg`), parallel to
    /// [`Session::backdrop_visible`]: index = backgrounds + foregrounds concatenated in
    /// order. `None` means the slot is a static `parallax` image (or a kind that is still
    /// unimplemented), so this costs nothing per frame for normal maps.
    backdrop_effects: RefCell<Vec<Option<crate::backdrop::Effect>>>,
    /// Per-backdrop `Speed * dt` accumulation since the map loaded. Vanilla starts
    /// `Backdrop.Position` at the parsed `x`/`y` (`references/Celeste.exe.cs.d/Celeste/Backdrop.cs:32`) and `Parallax.Update` adds
    /// `Speed * dt` every tick (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:28-30`), so the render position is
    /// `(x, y) + drift`; keeping only the drift means a hot reload that rewrites `x`/`y`
    /// applies immediately instead of restarting the scroll. Index = backgrounds +
    /// foregrounds concatenated in order, parallel to [`Session::backdrop_visible`].
    backdrop_drift: RefCell<Vec<Vec2>>,
    /// Keys that warn only once, to avoid spamming every frame.
    warned: RefCell<HashSet<String>>,
    /// Shared grass-banner clock in seconds (`Banner.sineTimer`): every banner adds
    /// `Engine.DeltaTime * WindMultiplier` per tick (`references/Celeste.exe.cs.d/Celeste/Decal.cs:44-46`), and because
    /// `OnlyIfWindy` is false for the grass decals `WindMultiplier` stays at 1, so one
    /// accumulator shared by all of them is enough. Each banner's phase is seeded
    /// deterministically from its index (`Calc.Random.NextFloat()` in vanilla) and
    /// offset by this clock at draw time, so the field itself stays `&self`.
    banner_time: RefCell<Fx>,
}

/// Bangs are the hair-root texture: `bangs[Sprite.HairFrame]` (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:132, references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:142), where
/// `HairFrame` is the **metadata** frame (references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:145-155, clamped to 0..2). The resolved
/// name now lives in [`crate::skin::PlayerHairSkin`], which also carries the vanilla
/// fallback; see `skin.rs`.
impl Session {
    /// Refresh backdrop visibility every frame (vanilla `Backdrop.Update`,
    /// `references/Celeste.exe.cs.d/Celeste/Backdrop.cs:134-155`).
    ///
    /// Not transitioning: `Visible = IsVisible` ([`Session::backdrop_is_visible`], room
    /// filtering). During a transition: `InstantIn` lights up immediately and
    /// `InstantOut` goes dark immediately, while **everything else keeps the previous
    /// frame's state** -- with the defaults (InstantIn=true, InstantOut=false), entering
    /// a room where "this image is hidden" keeps the image until the transition ends,
    /// which is exactly vanilla's fade semantics for room-filtered images.
    pub fn update_backdrop_visibility(&self, room: &str, transitioning: bool) {
        let total = self.doc.style.backgrounds.len() + self.doc.style.foregrounds.len();
        let mut vis = self.backdrop_visible.borrow_mut();
        if vis.len() != total {
            // Hot reload may swap the style wholesale: new entries start at vanilla's
            // initial `true`.
            vis.resize(total, true);
        }
        for (i, bd) in self
            .doc
            .style
            .backgrounds
            .iter()
            .chain(&self.doc.style.foregrounds)
            .enumerate()
        {
            let is_visible = self.backdrop_is_visible(bd, room);
            vis[i] = if transitioning {
                if bd.instant_in && is_visible {
                    true
                } else if bd.instant_out && !is_visible {
                    false
                } else {
                    vis[i]
                }
            } else {
                is_visible
            };
        }
    }

    /// Accumulate every `parallax` backdrop's `Speed * dt` drift for this tick
    /// (vanilla `Parallax.Update`, `references/Celeste.exe.cs.d/Celeste/Parallax.cs:28-30`).
    ///
    /// A pure accumulator, so a hot reload that rewrites `<bg x=... speedx=...>` takes
    /// effect on the next frame; slots are matched positionally and the vector is only
    /// resized, never reset, when the style grows or shrinks.
    fn advance_backdrop_drift(&self, dt: Fx) {
        let total = self.doc.style.backgrounds.len() + self.doc.style.foregrounds.len();
        let mut drift = self.backdrop_drift.borrow_mut();
        if drift.len() != total {
            drift.resize(total, Vec2::ZERO);
        }
        for (slot, bd) in self
            .doc
            .style
            .backgrounds
            .iter()
            .chain(&self.doc.style.foregrounds)
            .enumerate()
        {
            drift[slot].x += Fx::from_f32(bd.speed_x) * dt;
            drift[slot].y += Fx::from_f32(bd.speed_y) * dt;
        }
    }

    /// Advance the self-animating backdrop effects by one tick (vanilla
    /// `BackdropRenderer.Update` -> `Backdrop.Update`, `references/Celeste.exe.cs.d/Celeste/Backdrop.cs:134-155` and
    /// `Snow.Update`).
    ///
    /// Runs once per simulation tick and only while the game is unpaused, matching
    /// `Scene.Update`'s `if (!Paused)` guard (`references/Celeste.exe.cs.d/Monocle/Scene.cs:75-82`). Slots whose kind
    /// changed (a hot reload rewriting a `<bg>`/`<fg>` element) are rebuilt, so a map
    /// edit can add or drop snow without a restart.
    pub fn update_backdrop_effects(&self, dt: Fx) {
        // `Parallax.Update` drifts the whole position every tick, regardless of
        // visibility (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:28-30`): `Position += Speed * Engine.DeltaTime`, then
        // `Position += WindMultiplier * Level.Wind * Engine.DeltaTime` (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:31`,
        // still unported because `Level.Wind` is not modelled).
        self.advance_backdrop_drift(dt);
        // `Banner.Update` advances its `sineTimer` by `Engine.DeltaTime` every tick
        // (`references/Celeste.exe.cs.d/Celeste/Decal.cs:44-46`); with `OnlyIfWindy = false` the wind branch is skipped, so
        // `WindMultiplier` stays at 1 and one shared clock covers every banner. Skipping
        // the tick during a freeze falls out of the caller, exactly like vanilla's
        // early-returning `Level.Update`.
        *self.banner_time.borrow_mut() += dt;
        let kinds: Vec<&str> = self
            .doc
            .style
            .backgrounds
            .iter()
            .chain(&self.doc.style.foregrounds)
            .map(|bd| bd.kind.as_str())
            .collect();
        {
            let mut effects = self.backdrop_effects.borrow_mut();
            if effects.len() != kinds.len() {
                effects.resize_with(kinds.len(), || None);
            }
            for (slot, kind) in kinds.iter().enumerate() {
                let stale = match effects[slot].as_ref() {
                    Some(effect) => !effect.same_kind(kind),
                    None => crate::backdrop::handles_kind(kind),
                };
                if stale {
                    effects[slot] = crate::backdrop::Effect::from_kind(kind, slot);
                }
            }
        }
        let visible = self.backdrop_visible.borrow();
        let mut effects = self.backdrop_effects.borrow_mut();
        for (slot, effect) in effects.iter_mut().enumerate() {
            let is_visible = visible.get(slot).copied().unwrap_or(true);
            if let Some(effect) = effect {
                effect.update(is_visible, dt);
            }
        }
    }

    /// The 1x1 opaque-white quad the animated effects are drawn with (vanilla
    /// `Draw.Pixel`, `references/Celeste.exe.cs.d/Monocle/Draw.cs:11,28-34`): the engine reuses Gameplay's
    /// `util\pixel` sprite instead of a dedicated virtual texture.
    fn pixel_quad(&self, gpu: &GpuRenderer) -> Option<(u32, SpriteUv)> {
        let (group, sprite) = self.atlas.sprite(crate::backdrop::PIXEL_SPRITE)?;
        let atlas_id = sprites::group_id(&self.atlas, group);
        let (tw, th) = gpu.texture_size(atlas_id)?;
        let uv = GpuRenderer::uv_for_sprite(sprite, Vec2::from_f32s(tw as f32, th as f32));
        Some((atlas_id, uv))
    }

    /// The room-filtering part of vanilla `Backdrop.IsVisible` (`references/Celeste.exe.cs.d/Celeste/Backdrop.cs:104-132`):
    /// `ExcludeFrom` / `OnlyIn` membership tests against the current room name
    /// (`Session.Level`). flag / dreaming / fade bands are not ported (the converter
    /// already warns about uncaptured properties).
    fn backdrop_is_visible(&self, bd: &Backdrop, room: &str) -> bool {
        if !bd.exclude_from.is_empty() && bd.exclude_from.iter().any(|n| n == room) {
            return false;
        }
        if !bd.only_in.is_empty() && !bd.only_in.iter().any(|n| n == room) {
            return false;
        }
        true
    }
    /// Load from the map path resolved through `paths`, optionally overlaid with a
    /// reskin atlas.
    ///
    /// A non-empty `skin` loads `atlas/<skin>.atlas` (produced by the content pipeline
    /// from a `references/<skin>/` mod) and appends its texture groups to this session's
    /// atlas, so `<skin>\characters\player\...` names resolve. A missing skin atlas only
    /// warns: the vanilla textures then still draw via the fallback in
    /// [`Session::player_draws`]. Use [`Session::set_skin`] to swap the skin of an
    /// already loaded session.
    pub fn load_with_skin(map: &str, skin: Option<&str>, paths: &Paths) -> Result<Self> {
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
        let (atlas, anims) = Self::atlas_and_anims(paths, &atlas_name, skin)?;

        let tilesets_path = paths.tilesets();
        let tilesets = releste_map::TilesetsDoc::from_file(&tilesets_path).with_context(|| {
            format!(
                "failed to load tileset definitions {}",
                tilesets_path.display()
            )
        })?;

        // Background bricks are a second table; a missing table only warns and does not
        // block loading (not drawing the whole background layer beats not being able to
        // enter the game).
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
            anims,
            tilesets,
            bg_tilesets,
            spawn,
            spawn_room,
            backdrop_visible: RefCell::default(),
            backdrop_effects: RefCell::default(),
            backdrop_drift: RefCell::default(),
            warned: RefCell::default(),
            banner_time: RefCell::new(Fx::zero()),
        })
    }

    /// Swaps the reskin overlay of an already loaded session, rebuilding the atlas and
    /// the animation table from disk.
    ///
    /// A rebuild rather than an in-place edit on purpose: [`SpriteAtlas::append`] has no
    /// inverse and [`crate::sprites::AnimationTable::merge_skin_into`] overwrites the
    /// `player` definition, so appending a second skin would keep the first one's groups
    /// and animation set forever. This is the hot-reload path for the `[skin]` setting
    /// (see `crate::app`); `Ok` even when the skin has no atlas, which only warns and
    /// leaves the vanilla textures in place.
    pub fn set_skin(&mut self, skin: Option<&str>, paths: &Paths) -> Result<()> {
        let (atlas, anims) = Self::atlas_and_anims(paths, &self.atlas_name, skin)?;
        self.atlas = atlas;
        self.anims = anims;
        Ok(())
    }

    /// The base atlas plus the optional skin overlay, and the matching animation table.
    ///
    /// Shared by [`Session::load_with_skin`] and [`Session::set_skin`] so both resolve
    /// skins the same way.
    fn atlas_and_anims(
        paths: &Paths,
        atlas_name: &str,
        skin: Option<&str>,
    ) -> Result<(SpriteAtlas, crate::sprites::AnimationTable)> {
        let atlas_path = paths.atlas(atlas_name);
        let atlas = SpriteAtlas::load(&atlas_path, paths.atlas_dir()).with_context(|| {
            format!(
                "failed to load atlas {} (root {})",
                atlas_path.display(),
                paths.atlas_dir().display()
            )
        })?;

        // Optional reskin layer: append the selected skin's atlas so
        // `<skin>\characters\player\...` resolves, and remember the pack's
        // `Character_ID` so its animation set can be aliased to `player` below. A skin
        // this build has no atlas for is not an error -- the player falls back to vanilla.
        // Vanilla resolves a backdrop texture declared as `atlas="game"` through Game
        // first and then through **Misc** (`references/Celeste.exe.cs.d/Celeste/MapData.cs:345-362`), which is how the
        // `purplesunset` / `mist` / `darkswamp` / `vignette` backdrops in the original
        // maps draw at all: they are not in Gameplay. Appending Misc as a second layer
        // reproduces that fallback, and it is appended BEFORE the skin so a reskin's own
        // groups still win over vanilla names.
        let mut atlas = atlas;
        let misc_path = paths.atlas(MISC_ATLAS);
        if misc_path.is_file() {
            match SpriteAtlas::load(&misc_path, paths.atlas_dir()) {
                Ok(misc) => {
                    let groups = misc.descriptor().textures.len();
                    atlas.append(misc.descriptor());
                    debug!(groups, "appended the Misc atlas as the backdrop fallback");
                }
                Err(e) => warn!(
                    path = %misc_path.display(),
                    error = %e,
                    "Misc atlas failed to load; backdrop textures outside Gameplay stay missing"
                ),
            }
        } else {
            warn!(
                path = %misc_path.display(),
                "Misc atlas is missing; run the content pipeline to convert it"
            );
        }

        let mut skin_character: Option<String> = None;
        if let Some(skin) = skin.filter(|s| !s.is_empty()) {
            let skin_path = paths.atlas(skin);
            match SpriteAtlas::load(&skin_path, paths.atlas_dir()) {
                Ok(extra) => {
                    let groups = extra.descriptor().textures.len();
                    atlas.append(extra.descriptor());
                    debug!(skin, groups, "appended skin atlas");
                }
                Err(e) => warn!(
                    skin,
                    path = %skin_path.display(),
                    error = %e,
                    "skin atlas not found; hair falls back to vanilla"
                ),
            }
            skin_character = load_skin_character(paths, skin);
        }

        let anims = match crate::sprites::AnimationTable::load(paths, atlas_name, &atlas) {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "animation table failed to parse; falling back to static frames");
                crate::sprites::AnimationTable::default()
            }
        };
        // Skin animation set: load the pack's `<skin>.sprites.toml` and alias its
        // `Character_ID` def to `player` (Skin Mod Helper's `CreateOn(Character_ID)`).
        let mut anims = anims;
        if let (Some(skin), Some(character)) = (skin.filter(|s| !s.is_empty()), skin_character) {
            match crate::sprites::AnimationTable::load(paths, skin, &atlas) {
                Ok(extra) => {
                    anims.merge_skin_into(extra, &character);
                    debug!(skin, character, "merged skin animation set");
                }
                Err(e) => warn!(skin, error = %e, "skin animation table failed to parse"),
            }
        }
        Ok((atlas, anims))
    }

    /// Test constructor: skips disk I/O and assembles directly from the document
    /// (AGENTS.md 9, "tests must build their own data"). The atlas is an empty shell;
    /// unit tests never reach texture upload.
    #[cfg(test)]
    pub(crate) fn from_doc(doc: MapDocument, atlas_name: &str) -> Self {
        let (spawn, spawn_room) = vanilla_start(&doc);
        Session {
            doc,
            atlas: SpriteAtlas::new(
                releste_render::atlas::AtlasDescriptor {
                    name: atlas_name.into(),
                    version: 0,
                    links: Vec::new(),
                    textures: Vec::new(),
                },
                std::path::Path::new("/nonexistent"),
            ),
            atlas_name: atlas_name.to_string(),
            anims: crate::sprites::AnimationTable::default(),
            tilesets: releste_map::TilesetsDoc::default(),
            bg_tilesets: releste_map::TilesetsDoc::default(),
            spawn,
            spawn_room,
            backdrop_visible: RefCell::default(),
            backdrop_effects: RefCell::default(),
            backdrop_drift: RefCell::default(),
            warned: RefCell::default(),
            banner_time: RefCell::new(Fx::zero()),
        }
    }

    /// Which room a world coordinate falls in (falls back to the spawn room if none).
    ///
    /// `pos` is the hitbox **top-left**; this queries rooms with its geometric center.
    /// Center rather than corner: a corner crosses into the adjacent room half a body
    /// early, making collision, the title-bar room name and the reload check all point
    /// at the wrong room.
    pub fn room_at(&self, pos: Vec2) -> &Room {
        room_containing(&self.doc.rooms, pos)
            .or_else(|| self.doc.rooms.iter().find(|r| r.name == self.spawn_room))
            .unwrap_or(&self.doc.rooms[0])
    }

    /// Whether world cell `(cx, cy)` is solid.
    ///
    /// Room cells are room-local coordinates, so convert back to world cells first,
    /// then query the table.
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

    /// Draw the tiles + entities of the **current room** (excluding the player, who is
    /// drawn separately each frame).
    ///
    /// Vanilla draws only one room at a time: in a continuous multi-room world, drawing
    /// neighboring rooms too produces artifacts at the seams where the side walls of
    /// both rooms touch (floating door frames, hanging snow eaves). The caller is
    /// responsible for switching `room_index` and rebuilding when the room changes.
    ///
    /// The GPU is needed because `.atlas` does not store texture dimensions, so
    /// normalized UVs can only come from an uploaded texture.
    pub fn build_static(&mut self, gpu: &GpuRenderer, room_index: usize) -> Vec<SpriteDraw> {
        let mut out = Vec::new();
        let room = &self.doc.rooms[room_index];
        // Decoration layers, in vanilla depth order (`EntityList.CompareDepth` draws the
        // larger `Depth` first): `bgtiles` (10000) is the furthest back, then `bgdecals`
        // (9000), then the solids; `fgtiles` is overlaid into the solids themselves and
        // `fgdecals` (-10500) is the front-most layer of the room.
        let tex_size = |id: u32| gpu.texture_size(id);
        push_scenery(
            &mut out,
            room,
            &room.bg_scenery,
            LAYER_BG_SCENERY,
            self,
            &tex_size,
        );
        push_decals(
            &mut out,
            room,
            &room.bg_decals,
            LAYER_BG_DECAL,
            self,
            &tex_size,
        );
        push_tiles(
            &mut out,
            room,
            &room.tiles,
            &self.tilesets,
            TileDrawCtx {
                layer: Layer::Tiles,
                tint: WHITE,
                session: self,
                gpu,
            },
        );
        // The background uses a **different** table (`BackgroundTiles.xml`) and vanilla
        // does not tint it (`TileGrid.Color` defaults to `Color.White`, `Alpha = 1`,
        // references/Celeste.exe.cs.d/Monocle/TileGrid.cs:10/references/Celeste.exe.cs.d/Monocle/TileGrid.cs:18; nothing in vanilla writes it) -- the previous blue-gray tint was a
        // home-made color cast.
        push_tiles(
            &mut out,
            room,
            &room.bg,
            &self.bg_tilesets,
            TileDrawCtx {
                layer: Layer::BgTiles,
                tint: WHITE,
                session: self,
                gpu,
            },
        );
        self.push_entities(&mut out, room, gpu);
        push_scenery(
            &mut out,
            room,
            &room.fg_scenery,
            LAYER_FG_SCENERY,
            self,
            &tex_size,
        );
        push_decals(
            &mut out,
            room,
            &room.fg_decals,
            LAYER_FG_DECAL,
            self,
            &tex_size,
        );

        debug!(
            draws = out.len(),
            room = %room.name,
            atlas = %self.atlas_name,
            "static draw list rebuilt"
        );
        out
    }

    /// This frame's parallax background/foreground draws (`Parallax.Render`,
    /// `references/Celeste.exe.cs.d/Celeste/Parallax.cs:42-115`).
    ///
    /// **Rebuilt every frame** (unlike tiles/entities, which are cached per room): the
    /// tiling origin depends on the camera position
    /// (`p = (Position - Camera.Position.Floor()*Scroll).Floor()`, `references/Celeste.exe.cs.d/Celeste/Parallax.cs:44-45`), so
    /// any camera movement changes the whole layout. The counts are tiny (4 background
    /// images x a few tiles each), so the rebuild cost is negligible.
    ///
    /// Layers: the background draws after all tiles ([`Layer::Background`] = -1000 <
    /// [`Layer::BgTiles`] = -500), and the foreground sits above the player and effects
    /// ([`Layer::Foreground`] = 500 > [`Layer::Effects`] = 300), matching the front/back
    /// layering of vanilla `Backdrop`.
    pub fn backdrop_draws(&self, gpu: &GpuRenderer, camera_pos: Vec2) -> Vec<SpriteDraw> {
        let vis = self.backdrop_visible.borrow();
        let drift = self.backdrop_drift.borrow();
        let mut out = Vec::new();
        push_backdrops(
            &mut out,
            &self.doc.style.backgrounds,
            Layer::Background.value(),
            BackdropCtx {
                drifts: &drift,
                camera_pos,
                visibility: &vis,
                session: self,
                gpu,
            },
        );
        push_backdrops(
            &mut out,
            &self.doc.style.foregrounds,
            Layer::Foreground.value(),
            BackdropCtx {
                drifts: &drift,
                camera_pos,
                visibility: &vis,
                session: self,
                gpu,
            },
        );
        // Self-animating backdrops (snow) draw one 1x1 quad per particle, layered exactly
        // where their static siblings sit: the background pass under the tiles and the
        // foreground pass over the player.
        if let Some((atlas_id, uv)) = self.pixel_quad(gpu) {
            let camera = camera_pos.to_f32s();
            let effects = self.backdrop_effects.borrow();
            let bg = self.doc.style.backgrounds.len();
            for (slot, effect) in effects.iter().enumerate() {
                let Some(effect) = effect else { continue };
                let layer = if slot < bg {
                    Layer::Background.value()
                } else {
                    Layer::Foreground.value()
                };
                effect.push_draws(&mut out, camera, atlas_id, uv, layer);
            }
        }
        out
    }

    /// This frame's grass-banner draws (`Decal.Banner`, `references/Celeste.exe.cs.d/Celeste/Decal.cs:12-68`).
    ///
    /// **Rebuilt every frame** (like [`Session::backdrop_draws`]): each 1-px slice of a
    /// banner sways on its own sine phase, so the slices cannot live in the cached
    /// static list. Vanilla builds the slices once in `MakeBanner` (`references/Celeste.exe.cs.d/Celeste/Decal.cs:372-395`)
    /// and only recomputes the sway offsets per `Render`; recomputing the whole list
    /// here produces the same quads without keeping per-decal state.
    pub fn grass_draws(
        &self,
        tex_size: &dyn Fn(u32) -> Option<(u32, u32)>,
        room_index: usize,
    ) -> Vec<SpriteDraw> {
        let mut out = Vec::new();
        let Some(room) = self.doc.rooms.get(room_index) else {
            return out;
        };
        push_grass_banners(
            &mut out,
            room,
            &room.bg_decals,
            LAYER_BG_DECAL,
            self,
            &tex_size,
            room_index,
            false,
        );
        push_grass_banners(
            &mut out,
            room,
            &room.fg_decals,
            LAYER_FG_DECAL,
            self,
            &tex_size,
            room_index,
            true,
        );
        out
    }

    /// The map style's clear color (`<Style color="#rrggbb">`, `references/Celeste.exe.cs.d/Celeste/MapData.cs:159`).
    pub fn clear_color(&self) -> [u8; 3] {
        self.doc.style.background_color
    }

    /// This frame's player draws: hair (a 4-node chain + bangs + black outline) behind
    /// the body.
    ///
    /// Every part is checked against `references/Celeste.exe.cs.d`:
    /// - Anchor: `Player.Position` = hitbox bottom center (references/Celeste.exe.cs.d/Celeste/Player.cs:605), i.e. `anchor`;
    /// - Hair root: `Nodes\[0\] = Sprite.RenderPosition + (0,-9) + HairOffset*Facing`
    ///   (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:62). The idle animation does not define HairOffset -> the default (0,0) is
    ///   used (references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:271 only assigns it when the yaml provides it); RenderPosition is the
    ///   player position;
    /// - Later nodes: `HairCount = 4` (references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:115); `AfterUpdate` (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:64-81)'s static
    ///   placement node\[i\] = node\[i-1\] + ((0-Facing)*step, +2), where step is
    ///   2*StepInFacingPerSegment = 1 for i==1 and 0.5 otherwise (StepPerSegment
    ///   = (0,2), references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:20-22); the <=3px constraint between nodes does not trigger
    ///   (|(-1,2)| ~ 2.24);
    /// - Drawing (Render references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:101+): `origin = (5,5)`; node\[0\] uses bangs\[0\], the
    ///   rest use hair00; each piece first draws 4 black outline copies at +/-1px
    ///   (`Border = Black`, references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:105), then draws the base color in reverse order from
    ///   HairCount-1..0 (`Color = AC3232`).
    /// - The body draws last (Layer::Player is above PlayerHair, guaranteed by layer
    ///   sort order).
    ///
    /// `nodes` is the simulated hair chain from [`crate::player::Player::hair_nodes`]
    /// (vanilla `PlayerHair.Nodes`, references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:34), in **world space**. The chain length is the
    /// live `PlayerSprite.HairCount` (4, or 5 while two dashes are stocked).
    ///
    /// Not ported yet: the four-direction body outline
    /// (DrawPlayerSpriteOutline, off for idle).
    // One argument per draw input; bundling them into a struct would hide the vanilla
    // body/hair/nodes mapping this function is documenting.
    #[allow(clippy::too_many_arguments)]
    pub fn player_draws(
        &self,
        tex_size: &dyn Fn(u32) -> Option<(u32, u32)>,
        pos: Vec2,
        flip_x: bool,
        scale: [f32; 2],
        hair_color: [f32; 4],
        body_sprite: Option<&str>,
        hair: sprites::HairFrame,
        nodes: &[Vec2],
        skin: crate::skin::PlayerHairSkin,
    ) -> Vec<SpriteDraw> {
        let facing: f32 = if flip_x { -1.0 } else { 1.0 }; // Facings.Left = -1
        let anchor = pos + Vec2::from_f32s(crate::player::PLAYER_W * 0.5, crate::player::PLAYER_H);
        let body = self.body_draw(tex_size, anchor, flip_x, scale, body_sprite);

        // `PlayerHair.Render` returns immediately when the frame has no hair
        // (`if (!Sprite.HasHair) return;`, references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:103). `HasHair` is per **frame**
        // (references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:157-165), taken from the `<Metadata><Frames hair=...>` table
        // (references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:246-274) -- e.g. `fallPose` is `x|x|...` and draws none.
        if !hair.has_hair || nodes.is_empty() {
            return body.into_iter().collect();
        }

        // Node 0 draws the bangs, the rest draw `hair00`; `Render` floors node 0
        // (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:126) so the head piece snaps to whole pixels instead of drifting with the
        // player's subpixel position. A reskinned player first tries its prefixed
        // `<prefix>\characters\player\...` name and falls back to the vanilla name when
        // that texture is absent, matching SkinModHelper's `GetHairTexture` override
        // (references/SkinModHelper/.../SkinModHelperModule.cs:205-222 `if
        // GFX.Game.Has(newHairPath)`).
        // Node 0 (bangs) and the tail both try the skin's name first, then the vanilla
        // name. The fallback is `PlayerHairSkin::VANILLA`'s name, not the prefixed one --
        // passing the prefixed name as its own fallback would silently drop the piece
        // when a skin atlas is absent.
        let vanilla = crate::skin::PlayerHairSkin::VANILLA;
        let resolve = |wanted: Cow<'static, str>, fallback: Cow<'static, str>| {
            if self.atlas.sprite(&wanted).is_some() {
                wanted
            } else {
                fallback
            }
        };
        let pieces: Vec<(Cow<'static, str>, Vec2)> = nodes
            .iter()
            .enumerate()
            .map(|(i, node)| {
                let name = if i == 0 {
                    resolve(skin.bangs(hair.frame), vanilla.bangs(hair.frame))
                } else {
                    resolve(skin.hair(), vanilla.hair())
                };
                (name, if i == 0 { node.floor() } else { *node })
            })
            .collect();
        let count = pieces.len();
        let border = [0.0, 0.0, 0.0, 1.0];
        let outline_offsets = [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)];

        let mut out = Vec::new();
        // First draw all outlines for every node (4 copies at +/-1px), then draw the base
        // color from tail to head (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:126-143) -- two passes, not alternating per node.
        for (i, (name, node)) in pieces.iter().enumerate() {
            let piece_scale = sprites::hair_scale(i, facing, count, scale[0]);
            let Some(base) = self.hair_piece(tex_size, name.as_ref(), *node, piece_scale, border)
            else {
                continue;
            };
            for (dx, dy) in outline_offsets {
                let mut d = base;
                d.position += Vec2::from_f32s(dx, dy);
                out.push(d);
            }
        }
        for (i, (name, node)) in pieces.iter().enumerate().rev() {
            let piece_scale = sprites::hair_scale(i, facing, count, scale[0]);
            if let Some(mut d) =
                self.hair_piece(tex_size, name.as_ref(), *node, piece_scale, hair_color)
            {
                d.layer = Layer::PlayerHair.value() + 1; // base color on top of the outline
                out.push(d);
            }
        }
        out.extend(body);
        out
    }

    /// One hair piece: top-left = `node - origin within the sub-image * scale`
    /// (Monocle `MTexture.Draw(position, origin, color, scale)` -> `references/Celeste.exe.cs.d/Monocle/MTexture.cs:186-189` hands
    /// `origin` to SpriteBatch; the scale acts around the origin).
    ///
    /// The origin must be computed **once per image**: `hair00` has `(1,1)` cropped away
    /// in `.meta` and `bangs00` has `(1,0)` cropped away, and although both share the
    /// frame origin (5,5) (`HAIR_FRAME_ORIGIN`), it lands differently inside the cropped
    /// sub-image.
    fn hair_piece(
        &self,
        tex_size: &dyn Fn(u32) -> Option<(u32, u32)>,
        name: &str,
        node: Vec2,
        scale: (f32, f32),
        color: [f32; 4],
    ) -> Option<SpriteDraw> {
        let (group, sprite) = self.atlas.sprite(name)?;
        let id = sprites::group_id(&self.atlas, group);
        let (tw, th) = tex_size(id)?;
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

    /// Body: the bottom center is anchored at the feet (feet = hitbox bottom edge = the
    /// ground).
    ///
    /// The top-left is not `anchor - the cropped image's geometric center`:
    /// `Sprites.xml:5`'s `<Origin x="16" y="32"/>` is the actual vanilla
    /// `Sprite.Position` semantics; subtracting `.meta`'s crop offset lands inside the
    /// sub-image (see [`sprites::origin_in_trim`]). The earlier implementation used
    /// `w*0.5`, which shifted the body 1.5px to the left.
    fn body_draw(
        &self,
        tex_size: &dyn Fn(u32) -> Option<(u32, u32)>,
        anchor: Vec2,
        flip_x: bool,
        scale: [f32; 2],
        body_sprite_name: Option<&str>,
    ) -> Option<SpriteDraw> {
        let (group, sprite) = self
            .atlas
            .sprite(body_sprite_name.unwrap_or(sprites::PLAYER_SPRITE))?;
        let id = sprites::group_id(&self.atlas, group);
        let (tw, th) = tex_size(id)?;
        let uv = GpuRenderer::uv_for_sprite(sprite, Vec2::from_f32s(tw as f32, th as f32));
        let (w, h) = (f32::from(sprite.w), f32::from(sprite.h));
        let origin = sprites::origin_in_trim(sprite, sprites::PLAYER_BODY_FRAME_ORIGIN);
        // Vanilla mirrors the player with a **negative `Scale.X`**, not with
        // `SpriteEffects.FlipHorizontally`: `Player.Render` multiplies the scale by `Facing`
        // around `base.Render()` and undoes it afterwards (references/Celeste.exe.cs.d/Celeste/Player.cs:1390/references/Celeste.exe.cs.d/Celeste/Player.cs:1406). FNA mirrors a
        // negative-scaled quad **about the origin** (`GenerateVertexInfo`:
        // `cornerX = -originX * destinationW`), so the destination rect itself moves.
        // Swapping only the UVs would leave the quad where the unflipped sprite was, which is
        // the same thing only when the effective origin sits at the quad's centre -- not the
        // case for a trimmed frame (`runFast00`: origin.x 9 of width 14), so the two facings
        // were not mirror images and the flipped sprite looked shifted.
        //
        // `Sprite.Scale` (the dash/jump stretch) scales the quad **about the origin**
        // (the same FNA rule above): the top-left moves by `origin * (1 - scale)` and the
        // size scales by `scale`. `Scale.X` is negative to mirror, which this renderer
        // already folds into `flip_x`, so the absolute value is used here.
        let sx = scale[0];
        let sy = scale[1];
        let offset_x = if flip_x { w - origin.0 } else { origin.0 };
        Some(
            SpriteDraw::new(
                id,
                anchor - Vec2::from_f32s(offset_x * sx, origin.1 * sy),
                Vec2::from_f32s(w * sx, h * sy),
                uv,
            )
            .with_layer(Layer::Player.value())
            .with_color(WHITE)
            .with_flip_x(flip_x),
        )
    }

    /// The entity batch. Anchors follow **vanilla semantics**:
    /// - Default (decal-like): the content **center** is placed at the entity position
    ///   (`MTexture.Draw(position)`);
    /// - `introCar`: two images, body and wheels, with the **bottom center** at the
    ///   position (`references/Celeste.exe.cs.d/Celeste/IntroCar.cs:21-22` sets the body origin;
    ///   `references/Celeste.exe.cs.d/Celeste/IntroCar.cs:37-39` sets the wheel origin);
    /// - `lamp`: takes the left half 16x80 sub-image, with the **bottom center** at the
    ///   position (`Lamp` likewise).
    ///
    /// Warning dedup goes through `warn_once` (`RefCell`), so the whole batch loop only
    /// needs an immutable borrow of `self`, letting `&Room` and `&mut Vec` coexist.
    fn push_entities(&self, out: &mut Vec<SpriteDraw>, room: &Room, gpu: &GpuRenderer) {
        for e in &room.entities {
            if e.kind == "player" {
                continue; // the player is driven by the simulation; see `Session::player_draws`
            }
            match e.kind.as_str() {
                // Entities without a texture: their look is built from geometry/autotiler,
                // drawn separately.
                "introCrusher" => {
                    self.push_intro_crusher(out, room, e, gpu);
                    continue;
                }
                "wire" => {
                    self.push_wire(out, room, e, gpu);
                    continue;
                }
                "bridge" => {
                    self.push_bridge(out, room, e, gpu);
                    continue;
                }
                // Vanilla Hahaha only draws the `Ha` laugh bubbles generated on a timer by
                // Update, and the `has` list is empty at spawn (`references/Celeste.exe.cs.d/Celeste/Hahaha.cs:19`,
                // `references/Celeste.exe.cs.d/Celeste/Hahaha.cs:87-110`): at scene t=0 it draws nothing. The engine has no such
                // simulation, so static rendering draws nothing either (there is no
                // "missing sprite", so no warning).
                "hahaha" => continue,
                _ => {}
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
                continue; // texture not uploaded (unconverted .data); load_atlas already warned
            };
            let uv = GpuRenderer::uv_for_sprite(sprite, Vec2::from_f32s(tw as f32, th as f32));
            let (w, h) = (f32::from(sprite.w), f32::from(sprite.h));
            // Entity position = anchor (world coordinates).
            let anchor = Vec2::from_f32s(room.bounds.x + e.x, room.bounds.y + e.y);
            match e.kind.as_str() {
                "introCar" => {
                    // Vanilla uses two images: body (Depth 1) + wheels (Depth 3), both with
                    // the **bottom center** anchored at the position.
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
                    // Once a lamp is broken it draws a different set of broken-glass
                    // sub-images (the right half of the same image).
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
                    // Decal-like: the content center is placed at the position.
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

    /// `IntroCrusher`: vanilla has **no crushblock/Kevin texture at all**; its look is a
    /// whole box generated by an autotiler -- `GFX.FGAutotiler.GenerateBox('3',
    /// width/8, height/8)` stuffed into the entity's own TileGrid
    /// (`references/Celeste.exe.cs.d/Celeste/IntroCrusher.cs:26`; GenerateBox itself is at `references/Celeste.exe.cs.d/Celeste/Autotiler.cs:210`: fill the whole box
    /// with the id, treat outside the box as empty per the default `Behaviour` --
    /// `CheckTile`'s `mapData == null` branch returns `EdgesExtend` = false,
    /// `references/Celeste.exe.cs.d/Celeste/Autotiler.cs:363-367`).
    ///
    /// Entity position = the box top-left (`Solid`'s `base(position, width, height)`);
    /// `Nodes[0]` is the landing point and only participates in the falling animation,
    /// so static rendering draws the start position.
    fn push_intro_crusher(
        &self,
        out: &mut Vec<SpriteDraw>,
        room: &Room,
        e: &EntityData,
        gpu: &GpuRenderer,
    ) {
        // The cell value of '3' = its index in the alphabet ("0123..." -> 3).
        let value = releste_map::TILE_ALPHABET.find('3').unwrap_or(0) as u16;
        let (cols, rows) = (
            (e.width.unwrap_or(0).max(0) / sprites::TILE_PX) as u32,
            (e.height.unwrap_or(0).max(0) / sprites::TILE_PX) as u32,
        );
        if cols == 0 || rows == 0 {
            return; // 0-cell box: vanilla likewise draws nothing
        }

        // Fill the whole box with '3'; treating out-of-bounds as empty (`Oob::Empty`) is
        // what produces edge blocks.
        let mut grid = releste_map::Tileset::new(room.tiles.atlas.clone(), cols, rows);
        for y in 0..rows {
            for x in 0..cols {
                grid.set(x, y, value);
            }
        }
        let coords = releste_map::autotiler::generate_with(
            &grid,
            &self.tilesets,
            releste_map::autotiler::Oob::Empty,
        );

        let id = releste_map::tile_char(value);
        let Some(def) = self.tilesets.def(id) else {
            if self.warn_once(&format!("no-tileset:{id}")) {
                warn!(tile = %id, "no tileset definition for introCrusher box");
            }
            return;
        };
        let Some((group, sprite)) = self.atlas.sprite(&def.path) else {
            if self.warn_once(&format!("missing-tileset:{}", def.path)) {
                warn!(tile = %id, path = %def.path, "tileset texture not in atlas; skipping introCrusher");
            }
            return;
        };
        let atlas_id = sprites::group_id(&self.atlas, group);
        let Some((tw, th)) = gpu.texture_size(atlas_id) else {
            return; // texture not uploaded (unconverted .data); load_atlas already warned
        };
        let tex = Vec2::from_f32s(tw as f32, th as f32);

        // Position = box top-left: room coordinates -> world coordinates (same conversion
        // as the tile layer).
        let anchor = Vec2::from_f32s(room.bounds.x + e.x, room.bounds.y + e.y);
        for ty in 0..rows {
            for tx in 0..cols {
                let (col, row) = coords[(ty as usize) * (cols as usize) + (tx as usize)];
                let cell = sprites::tile_cell(sprite, col, row);
                let uv = GpuRenderer::uv_for_sprite(&cell, tex);
                let pos = anchor + Vec2::from_f32s(tx as f32 * TILE, ty as f32 * TILE);
                out.push(
                    SpriteDraw::new(atlas_id, pos, Vec2::from_f32s(TILE, TILE), uv)
                        .with_layer(LAYER_INTRO_CRUSHER)
                        .with_color(WHITE),
                );
            }
        }
    }

    /// `Wire`: vanilla has no dedicated texture -- `Render` draws 16 `Draw.Line`
    /// segments along a `SimpleCurve` (quadratic Bezier), each a 1px-tall horizontal bar
    /// rotated by its angle (`references/Celeste.exe.cs.d/Celeste/Wire.cs:32-43`). Control point = the
    /// midpoint of the two ends sagged 24px (`references/Celeste.exe.cs.d/Celeste/Wire.cs:35`);
    /// `GetPoint(t) = (1-t)^2*Begin + 2(1-t)t*Control + t^2*End` (Everest
    /// `Patches/Monocle/SimpleCurve.cs:22-28`). Line color `595866` (`references/Celeste.exe.cs.d/Celeste/Wire.cs:9`); from/to
    /// = entity and node coordinates + room offset (`references/Celeste.exe.cs.d/Celeste/Wire.cs:31`).
    ///
    /// Static deviation: vanilla also adds wind sway to the control point,
    /// `sin(sine + WindSineTimer*omega) * 8 * VisualWind` (`references/Celeste.exe.cs.d/Celeste/Wire.cs:34`) -- the phase comes
    /// from a per-line random stream seeded by `new Random((int)min.X)`, and the
    /// amplitude animates with `WindSineTimer` (`references/Celeste.exe.cs.d/Celeste/Level.cs:1899-1900`; at t=0
    /// `VisualWind=0.5`). The engine's static batch has no such time or random stream,
    /// so sway is taken as 0 (max deviation of 4px per line).
    fn push_wire(&self, out: &mut Vec<SpriteDraw>, room: &Room, e: &EntityData, gpu: &GpuRenderer) {
        let Some((node_x, node_y)) = e.nodes.first().copied() else {
            if self.warn_once(&format!("wire-node:{}:{}", e.x, e.y)) {
                warn!(x = e.x, y = e.y, "wire without a node; skipping");
            }
            return;
        };
        let (fx, fy) = (room.bounds.x + e.x, room.bounds.y + e.y);
        let (tx, ty) = (room.bounds.x + node_x, room.bounds.y + node_y);

        // 1px white texel: Monocle `Draw.Line` samples a pure-white Pixel.Texture, and
        // `util/pixel` in the atlas is a 3x3 pure white (255,255,255,255).
        let Some((group, sprite)) = self.atlas.sprite("util/pixel") else {
            if self.warn_once("wire-white-pixel") {
                warn!("`util/pixel` not in atlas; skipping wires");
            }
            return;
        };
        let atlas_id = sprites::group_id(&self.atlas, group);
        let Some((tw, th)) = gpu.texture_size(atlas_id) else {
            return; // texture not uploaded yet; load_atlas already warned
        };
        let uv = GpuRenderer::uv_for_sprite(sprite, Vec2::from_f32s(tw as f32, th as f32));

        // Calc.HexToColor("595866") (references/Celeste.exe.cs.d/Celeste/Wire.cs:9).
        let color = [89.0 / 255.0, 88.0 / 255.0, 102.0 / 255.0, 1.0];
        let layer = if matches!(e.properties.get("above"), Some(FieldValue::Bool(true))) {
            LAYER_WIRE_ABOVE
        } else {
            LAYER_WIRE
        };

        // 16 segments (references/Celeste.exe.cs.d/Celeste/Wire.cs:36-42): each is a 1px-tall bar rotated from its start to
        // its end.
        for ((sx, sy), len, angle) in wire_segments((fx, fy), (tx, ty)) {
            out.push(
                SpriteDraw::new(
                    atlas_id,
                    Vec2::from_f32s(sx, sy),
                    Vec2::from_f32s(len, 1.0),
                    uv,
                )
                .with_layer(layer)
                .with_color(color)
                .with_rotation(angle),
            );
        }
    }

    /// `Bridge`: the bridge entity itself draws nothing -- in `Added` it generates a
    /// `BridgeTile` per cell along its width (`references/Celeste.exe.cs.d/Celeste/Bridge.cs:32-59`), each cell
    /// being one vertical strip of the `scenery/bridge` texture (`references/Celeste.exe.cs.d/Celeste/BridgeTile.cs:18-50`).
    ///
    /// - Gap = the room x of the two nodes plus the room's left edge (vanilla
    ///   `gapStartX += level.Bounds.Left`, `references/Celeste.exe.cs.d/Celeste/Bridge.cs:63-64`; nodes and entity share the
    ///   room coordinate system).
    /// - The planking cycles through 11 sizes (`references/Celeste.exe.cs.d/Celeste/Bridge.cs:47-57`): for index `2..=7`
    ///   vanilla takes a random wood-grain strip via `Calc.Random.Next(6)` (seeded by
    ///   `PushRandom(1)`), while the engine uses the index `num` directly -- those 6
    ///   candidates are all 8px wide, so the stepping matches vanilla cell for cell and
    ///   only the pattern of the 8px strips becomes a fixed sequence (that random stream
    ///   is not reproduced).
    /// - A 16px end cap is cut vertically into 24+12+12+..., each strip offset in Y by
    ///   `num2 - 8` (`references/Celeste.exe.cs.d/Celeste/BridgeTile.cs:26-44`), with the remainder trimmed to 52px tall.
    /// - Vanilla `Update` plays a collapse once the player has crossed the bridge
    ///   (`references/Celeste.exe.cs.d/Celeste/Bridge.cs:61+`); static rendering draws the complete bridge.
    fn push_bridge(
        &self,
        out: &mut Vec<SpriteDraw>,
        room: &Room,
        e: &EntityData,
        gpu: &GpuRenderer,
    ) {
        let Some(width) = e.width.filter(|&w| w > 0) else {
            return;
        };
        if e.nodes.len() < 2 {
            if self.warn_once(&format!("bridge-nodes:{}:{}", e.x, e.y)) {
                warn!(x = e.x, y = e.y, "bridge without two gap nodes; skipping");
            }
            return;
        }
        let gap_start = room.bounds.x + e.nodes[0].0;
        let gap_end = room.bounds.x + e.nodes[1].0;

        let Some((group, sprite)) = self.atlas.sprite("scenery/bridge") else {
            if self.warn_once("bridge-texture") {
                warn!("`scenery/bridge` not in atlas; skipping bridge");
            }
            return;
        };
        let atlas_id = sprites::group_id(&self.atlas, group);
        let Some((tw, th)) = gpu.texture_size(atlas_id) else {
            return;
        };
        let tex = Vec2::from_f32s(tw as f32, th as f32);

        let mut push_strip = |cell_x: i16, cell_y: i16, w: i16, h: i16, px: f32, py: f32| {
            let cell = sprites::sub_rect(sprite, cell_x, cell_y, w, h);
            let cell_uv = GpuRenderer::uv_for_sprite(&cell, tex);
            out.push(
                SpriteDraw::new(
                    atlas_id,
                    Vec2::from_f32s(px, py),
                    Vec2::from_f32s(w as f32, h as f32),
                    cell_uv,
                )
                .with_layer(LAYER_BRIDGE)
                .with_color(WHITE),
            );
        };

        let tile_y = room.bounds.y + e.y;
        for (x, cell_x, cell_w, cell_h, visible) in
            bridge_cells(room.bounds.x + e.x, width, (gap_start, gap_end))
        {
            if !visible {
                continue; // gap: vanilla does not level.Add this cell
            }
            for (sx, sy, sw, sh, dy) in bridge_tile_strips(cell_x, cell_w, cell_h) {
                push_strip(sx, sy, sw, sh, x, tile_y + dy);
            }
        }
    }

    /// Emit any given warning only once; returns `true` the first time.
    ///
    /// `pub(crate)` so the sibling sprite-drawing modules (particles, ...) can share the
    /// same dedup table instead of inventing their own.
    pub(crate) fn warn_once(&self, key: &str) -> bool {
        let mut warned = self.warned.borrow_mut();
        if warned.contains(key) {
            false
        } else {
            warned.insert(key.to_string());
            true
        }
    }

    /// For reporting: room count / entity count / spawn point.
    pub fn summary(&self) -> (usize, usize, String) {
        let entities: usize = self.doc.rooms.iter().map(|r| r.entities.len()).sum();
        (self.doc.rooms.len(), entities, self.spawn_room.clone())
    }
}

/// Which room the center point falls in (no fallback at all).
///
/// `pos` is the hitbox top-left; this decides using the geometric center. It only
/// answers "room identity" (title bar, camera, hot-reload checks); physical collision
/// does **not** pick a room -- every room the hitbox touches is queried (see
/// `player::Player::hit_in`), and on a seam the tiles on both sides count, so the
/// player never gets stuck because the "wrong room" was chosen.
pub(crate) fn room_containing(rooms: &[Room], pos: Vec2) -> Option<&Room> {
    let centre =
        pos + Vec2::from_f32s(crate::player::PLAYER_W * 0.5, crate::player::PLAYER_H * 0.5);
    rooms.iter().find(|r| r.bounds.to_rect().contains(centre))
}

/// The room nearest to `point` (Euclidean distance from a point to a rectangle, 0 when
/// inside the rectangle).
///
/// For the world-level fallback: when the center drops out of every room, clamp the
/// player back into the **nearest** room rather than the spawn room -- otherwise a fall
/// at the far end of the map would teleport back to the spawn point.
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

/// Draw one tile layer: each tile looks up its tileset definition by its own tile id,
/// and the autotiler's adjacency matching gives its `(col, row)` in the tileset texture.
///
/// A room may mix several tilesets (e.g. `1` = dirt, `3` = snow), so `(atlas_id,
/// tex_size)` is per **tile**, not one per layer. This bundles `push_tiles`'s ambient
/// parameters (clippy `too_many_arguments`: these five are always passed together and
/// really should be one context).
struct TileDrawCtx<'a> {
    layer: Layer,
    tint: [f32; 4],
    session: &'a Session,
    gpu: &'a GpuRenderer,
}

fn push_tiles(
    out: &mut Vec<SpriteDraw>,
    room: &Room,
    tiles: &releste_map::Tileset,
    defs: &releste_map::TilesetsDoc,
    ctx: TileDrawCtx<'_>,
) {
    // Compute every tile's (col, row) once up front (position hash picks the variant;
    // deterministic).
    let coords = releste_map::autotiler::generate(tiles, defs);

    for ty in 0..tiles.height {
        for tx in 0..tiles.width {
            let value = tiles.get(tx, ty).unwrap_or(0);
            if value == 0 {
                continue; // 0 = empty
            }
            let id = releste_map::tile_char(value);
            let Some(def) = defs.def(id) else {
                if ctx.session.warn_once(&format!("no-tileset:{id}")) {
                    warn!(tile = %id, "no tileset definition; skipping this tile id");
                }
                continue;
            };
            let Some((group, sprite)) = ctx.session.atlas.sprite(&def.path) else {
                if ctx
                    .session
                    .warn_once(&format!("missing-tileset:{}", def.path))
                {
                    warn!(
                        tile = %id,
                        path = %def.path,
                        "tileset texture not found in atlas; skipping this tile id"
                    );
                }
                continue;
            };
            let atlas_id = sprites::group_id(&ctx.session.atlas, group);
            let Some((tw, th)) = ctx.gpu.texture_size(atlas_id) else {
                continue; // texture not uploaded (unconverted .data); load_atlas already warned
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
                    .with_layer(ctx.layer.value())
                    .with_color(ctx.tint),
            );
        }
    }
}

/// The shared 8x8 scenery sheet every decoration layer (`bgtiles` / `fgtiles` / `objtiles`)
/// indexes.
///
/// Vanilla builds it once as `SceneryTiles = new Tileset(Game["tilesets/scenery"], 8, 8)`
/// (`references/Celeste.exe.cs.d/Celeste/GFX.cs:133`) and the level loader overlays it onto the tile grids
/// (`LevelLoader`, `references/Celeste.exe.cs.d/Celeste/LevelLoader.cs:283-293`). It is **not** autotiled: the CSV holds a flat tile
/// index, and `Tileset[index]` maps it to `(index % columns, index / columns)`
/// (`references/Celeste.exe.cs.d/Monocle/Tileset.cs:15-25`).
const SCENERY_SPRITE: &str = "tilesets/scenery";

/// Decode one stored scenery cell back to its flat `tilesets/scenery` index.
///
/// The converter stores `index + 1` so that `0` can mean "empty"
/// (`tools/content-pipeline/src/pipeline.rs`, `csv_tileset`), which is this inverse: a
/// stored `1` is sheet index `0`, and a stored `0` is an empty cell (`None`).
fn scenery_index(value: u16) -> Option<u32> {
    value.checked_sub(1).map(u32::from)
}

/// Draw one decoration grid (`bgtiles` / `fgtiles`) from the shared scenery sheet.
///
/// Vanilla overlays the grid onto the already-built tile grid and renders it cell by
/// cell (`TileGrid.Overlay`, `references/Celeste.exe.cs.d/Monocle/TileGrid.cs:47-58`, then `RenderAt`,
/// `:145-190`), so each cell is a plain 8x8 sprite at `room.bounds + (tx*8, ty*8)`.
/// A grid whose `is_empty()` holds is skipped without looking up the atlas.
fn push_scenery(
    out: &mut Vec<SpriteDraw>,
    room: &Room,
    grid: &releste_map::Tileset,
    layer: i32,
    session: &Session,
    tex_size: &dyn Fn(u32) -> Option<(u32, u32)>,
) {
    if grid.is_empty() {
        return;
    }
    let Some((group, sheet)) = session.atlas.sprite(SCENERY_SPRITE) else {
        if session.warn_once("missing-scenery-sheet") {
            warn!(
                sprite = SCENERY_SPRITE,
                "scenery sheet not found in atlas; skipping every decoration layer"
            );
        }
        return;
    };
    let atlas_id = sprites::group_id(&session.atlas, group);
    let Some((tw, th)) = tex_size(atlas_id) else {
        return; // texture not uploaded; load_atlas already warned
    };
    let tex = Vec2::from_f32s(tw as f32, th as f32);
    // `Tileset[index]` wraps at the sheet width (`references/Celeste.exe.cs.d/Monocle/Tileset.cs:15-25`).
    let columns = (u32::try_from(sheet.w).unwrap_or(1) / sprites::TILE_PX as u32).max(1);

    for ty in 0..grid.height {
        for tx in 0..grid.width {
            let Some(index) = scenery_index(grid.get(tx, ty).unwrap_or(0)) else {
                continue; // 0 = empty
            };
            let cell = sprites::tile_cell(sheet, index % columns, index / columns);
            if cell.w <= 0 || cell.h <= 0 {
                continue; // the index ran past the sheet: vanilla would clip it away
            }
            let uv = GpuRenderer::uv_for_sprite(&cell, tex);
            let pos = Vec2::from_f32s(
                room.bounds.x + tx as f32 * TILE,
                room.bounds.y + ty as f32 * TILE,
            );
            out.push(
                SpriteDraw::new(atlas_id, pos, Vec2::from_f32s(TILE, TILE), uv)
                    .with_layer(layer)
                    .with_color(WHITE),
            );
        }
    }
}

/// Draw one decal layer (`bgdecals` / `fgdecals`).
///
/// Vanilla renders each decal with `DrawCentered(Position, Color.White, Scale)`: the
/// **centre of the untrimmed frame** lands on `room.bounds + (decal.x, decal.y)`, and a
/// negative scale mirrors about that centre (`Decal.cs`, `DecalImage.Render`). The trim
/// offset is undone exactly like the player body does it: see
/// [`sprites::origin_in_trim`].
fn push_decals(
    out: &mut Vec<SpriteDraw>,
    room: &Room,
    decals: &[releste_map::Decal],
    layer: i32,
    session: &Session,
    tex_size: &dyn Fn(u32) -> Option<(u32, u32)>,
) {
    for decal in decals {
        if is_banner_decal(&decal.texture) {
            // `Decal.Added` (`references/Celeste.exe.cs.d/Celeste/Decal.cs:164-176`) replaces the static
            // `DecalImage` of the four `generic/grass_a..d` decals with a swaying
            // `Banner` component (`MakeBanner(2f, 2f, 1, 0.05f, easeDown: false, -2f)`),
            // so they must not join the cached static list; [`Session::grass_draws`]
            // emits them per frame instead.
            continue;
        }
        let Some((group, sprite)) = session.atlas.sprite(&decal.texture) else {
            if session.warn_once(&format!("decal-tex:{}", decal.texture)) {
                warn!(
                    texture = %decal.texture,
                    room = %room.name,
                    "decal texture not found in atlas; skipping this decal"
                );
            }
            continue;
        };
        let atlas_id = sprites::group_id(&session.atlas, group);
        let Some((tw, th)) = tex_size(atlas_id) else {
            continue;
        };
        let uv = GpuRenderer::uv_for_sprite(sprite, Vec2::from_f32s(tw as f32, th as f32));
        let (w, h) = (f32::from(sprite.w), f32::from(sprite.h));
        // `DrawCentered` mirrors about `MTexture.Center`, which is the **untrimmed frame**
        // centre (`MTexture.SetUtil`: `Center = (Width, Height) * 0.5f`, where `Width` is
        // the frame size, not the cropped rect). Using the cropped size here would anchor
        // the decal off-centre by exactly the trim offset.
        let (fw, fh) = sprite.frame_size();
        let origin = sprites::origin_in_trim(sprite, (fw * 0.5, fh * 0.5));
        let sx = decal.scale_x.abs();
        let sy = decal.scale_y.abs();
        let flip_x = decal.scale_x < 0.0;
        let offset_x = if flip_x { w - origin.0 } else { origin.0 };
        let anchor = Vec2::from_f32s(room.bounds.x + decal.x, room.bounds.y + decal.y);
        out.push(
            SpriteDraw::new(
                atlas_id,
                anchor - Vec2::from_f32s(offset_x * sx, origin.1 * sy),
                Vec2::from_f32s(w * sx, h * sy),
                uv,
            )
            .with_layer(layer)
            .with_color(WHITE)
            .with_flip_x(flip_x),
        );
    }
}

/// `Decal.Added` (`references/Celeste.exe.cs.d/Celeste/Decal.cs:164-176`) lowercases the decal name, strips the
/// `decals/` prefix and maps `generic/grass_a..d` to `MakeBanner(2f, 2f, 1, 0.05f,
/// easeDown: false, -2f)` -- those four render as swaying banners, not static images.
fn is_banner_decal(texture: &str) -> bool {
    // `Decal.Added` compares the full path after `Name.ToLower().Replace("decals/",
    // "")` (`references/Celeste.exe.cs.d/Celeste/Decal.cs:165-176`), so `scenery/grass_a` stays a static image
    // exactly like in vanilla; the trailing `.png` some sources add is ignored
    // (`format.rs` documents that vanilla strips it).
    let name = texture.to_ascii_lowercase().replace('\\', "/");
    let name = name.strip_prefix("decals/").unwrap_or(&name);
    let name = name.strip_suffix(".png").unwrap_or(name);
    matches!(
        name,
        "generic/grass_a" | "generic/grass_b" | "generic/grass_c" | "generic/grass_d"
    )
}

/// Emits one quad per visible 1-px slice of every `generic/grass_*` decal in the list.
///
/// Provenance (`Decal.MakeBanner`, `references/Celeste.exe.cs.d/Celeste/Decal.cs:372-395`, and `Banner.Render`,
/// `:52-65`): vanilla slices the decal texture with `GetSubtexture(0, i, Width, 1)` for
/// `i` in `0..Height`, where `MTexture.Height` is the **frame** height -- so the empty
/// rows the trim cut away still count in `list.Count` and lower every slice's
/// `num = 1 - i / count`. Each visible slice `i` (frame rows inside the cropped art,
/// `DrawOffset.y <= i < DrawOffset.y + h` after `GetRelativeRect`'s clamps) draws at
/// `Position + (x, 0)` with the spritebatch origin `(Width/2, Height/2 - i)` minus the
/// slice's own `DrawOffset`, and `x = sin(sineTimer * 2 + i * 0.05) * num * 2 +
/// num * -2`.
#[allow(clippy::too_many_arguments)]
fn push_grass_banners(
    out: &mut Vec<SpriteDraw>,
    room: &Room,
    decals: &[releste_map::Decal],
    layer: i32,
    session: &Session,
    tex_size: &dyn Fn(u32) -> Option<(u32, u32)>,
    room_index: usize,
    foreground: bool,
) {
    let elapsed = session.banner_time.borrow();
    for (index, decal) in decals.iter().enumerate() {
        if !is_banner_decal(&decal.texture) {
            continue;
        }
        let Some((group, sprite)) = session.atlas.sprite(&decal.texture) else {
            if session.warn_once(&format!("decal-tex:{}", decal.texture)) {
                warn!(
                    texture = %decal.texture,
                    room = %room.name,
                    "banner decal texture not found in atlas; skipping this decal"
                );
            }
            continue;
        };
        let atlas_id = sprites::group_id(&session.atlas, group);
        let Some((tw, th)) = tex_size(atlas_id) else {
            continue;
        };
        // `MTexture.GetSubtexture` clamps each slice against the parent's clip rect
        // (`references/Celeste.exe.cs.d/Monocle/MTexture.cs:130-140`); the loop bounds below reproduce those clamps with
        // the atlas numbers instead of building intermediate textures.
        let (ox, oy) = sprite.origin();
        let (fw, fh) = sprite.frame_size();
        let (x0, y0, w, h) = (
            f32::from(sprite.x),
            f32::from(sprite.y),
            f32::from(sprite.w),
            f32::from(sprite.h),
        );
        let count = fh.max(1.0);
        let sx = decal.scale_x.abs();
        let sy = decal.scale_y.abs();
        let flip_x = decal.scale_x < 0.0;
        // `sineTimer = Calc.Random.NextFloat()` per banner (`references/Celeste.exe.cs.d/Celeste/Decal.cs:21`); seeded
        // deterministically from the decal's identity instead, so replays and headless
        // screenshots stay stable.
        let phase = banner_phase(room_index, foreground, index);
        let sine_timer = *elapsed + phase;
        let anchor = Vec2::from_f32s(room.bounds.x + decal.x, room.bounds.y + decal.y);
        for i in 0..count as i32 {
            let i = i as f32;
            // `GetRelativeRect(0, i, Width, 1)`: frame row i folded into the clip rect.
            let rx = x0 - ox;
            let ry = y0 - oy + i;
            let cx = rx.max(x0).min(x0 + w);
            let cy = ry.max(y0).min(y0 + h);
            let cw = (rx + fw).min(x0 + w) - cx;
            let ch = (ry + 1.0).min(y0 + h) - cy;
            if ch <= 0.0 {
                continue;
            }
            // The slice's own `DrawOffset` (`GetSubtexture`, `references/Celeste.exe.cs.d/Monocle/MTexture.cs:141-146`) is
            // subtracted from the passed origin: `(Width/2 - max(ox, 0), Height/2 - i -
            // max(oy - i, 0))`.
            let origin_x = fw * 0.5 - ox.max(0.0);
            let origin_y = fh * 0.5 - i - (oy - i).max(0.0);
            // `num = (1 - i / list.Count) * WindMultiplier` with `WindMultiplier = 1`
            // (`references/Celeste.exe.cs.d/Celeste/Decal.cs:53-56`, `OnlyIfWindy = false`).
            let num = Fx::one() - Fx::from_ratio(i as i32, count as i32);
            let x = (sine_timer * Fx::from_int(2) + Fx::from_ratio(i as i32, 20)).sin()
                * num
                * Fx::from_int(2)
                + num * Fx::from_int(-2);
            let offset_x = if flip_x { cw - origin_x } else { origin_x };
            let uv_min = (cx / tw as f32, cy / th as f32);
            let uv_max = ((cx + cw) / tw as f32, (cy + ch) / th as f32);
            out.push(
                SpriteDraw::new(
                    atlas_id,
                    anchor - Vec2::from_f32s(offset_x * sx, origin_y * sy)
                        + Vec2::from_f32s(x.to_f32(), 0.0),
                    Vec2::from_f32s(cw * sx, ch * sy),
                    (uv_min, uv_max),
                )
                .with_layer(layer)
                .with_color(WHITE)
                .with_flip_x(flip_x),
            );
        }
    }
}

/// Deterministic banner phase in `[0, 1)` seconds, standing in for vanilla's
/// `sineTimer = Calc.Random.NextFloat()` (`references/Celeste.exe.cs.d/Celeste/Decal.cs:21`). Mixing the room index, the
/// layer and the decal index through the same constants as [`crate::backdrop`]'s
/// `SplitMix64` keeps the draw pass stateless (`&self`) while giving every banner its
/// own phase.
fn banner_phase(room_index: usize, foreground: bool, decal_index: usize) -> Fx {
    let mut z = (room_index as u64) << 1 | u64::from(foreground) | (decal_index as u64) << 32;
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    // Q16.16: the low 16 bits are exactly [0, 1) seconds.
    Fx::from_bits((z & 0xFFFF) as i32)
}

/// Vanilla's starting room and spawn point, each step with provenance:
///
/// 1. **Starting room**: `references/Celeste.exe.cs.d/Celeste/MapData.cs:289` `MapData.StartLevel() =>
///    GetAt(Vector2.Zero)` -- the first room containing the **world origin (0,0)**
///    (`GetAt`, references/Celeste.exe.cs.d/Celeste/MapData.cs:294, tests Bounds containment in order and returns null when nothing
///    matches; here it falls back to the first room and warns, an engine robustness
///    difference).
/// 2. **Spawn point**: `references/Celeste.exe.cs.d/Celeste/Level.cs:1311` `Session.RespawnPoint = DefaultSpawnPoint`,
///    `references/Celeste.exe.cs.d/Celeste/Level.cs:290` `public Vector2 DefaultSpawnPoint => GetSpawnPoint(new Vector2(Bounds.Left, Bounds.Bottom))`
///    delegates to `references/Celeste.exe.cs.d/Celeste/Session.cs:256-258` `return LevelData.Spawns.ClosestTo(from);` --
///    the spawn point closest to the room's **bottom-left corner**.
/// 3. **Anchor semantics**: `references/Celeste.exe.cs.d/Celeste/Level.cs:1315` `Player player = new Player(Session.RespawnPoint.Value, spriteMode);` --
///    the spawn point is the hitbox **bottom center** (feet, see
///    `player::PLAYER_H_PX`).
///
/// Returns `(anchor, room name)`; the service converts the anchor to a hitbox top-left.
/// Read the `Character_ID` of the first entry in `skins/<skin>.skin.toml` (written by
/// the content pipeline from `SkinModHelperConfig.yaml`). Missing file / no character
/// -> `None`, so the caller keeps the vanilla player animation set.
fn load_skin_character(paths: &Paths, skin: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Entry {
        #[serde(default)]
        character_id: Option<String>,
    }
    #[derive(serde::Deserialize)]
    struct SkinFile {
        #[serde(default)]
        entries: Vec<Entry>,
    }

    let path = paths.skins_dir().join(format!("{skin}.skin.toml"));
    let text = std::fs::read_to_string(&path).ok()?;
    let parsed: SkinFile = toml::from_str(&text).ok()?;
    parsed
        .entries
        .into_iter()
        .find_map(|e| e.character_id)
        .filter(|c| !c.is_empty())
}

fn vanilla_start(doc: &MapDocument) -> (Vec2, String) {
    let origin = Vec2::from_f32s(0.0, 0.0);
    let room = doc
        .rooms
        .iter()
        .find(|r| r.bounds.to_rect().contains(origin))
        .or_else(|| {
            warn!("no room contains the world origin (vanilla StartLevel); using the first room");
            doc.rooms.first()
        });
    let Some(room) = room else {
        // `Session::load` already guarantees rooms is non-empty; we always fall back
        // here, this is just insurance.
        return (Vec2::ZERO, String::new());
    };

    // Spawns.ClosestTo((Bounds.Left, Bounds.Bottom)): the spawn point is the player
    // entity nearest the room's bottom-left corner (the converter lowers .bin Spawns to
    // player entities).
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

    /// Reproduces 0-Intro's seam geometry: `lvl_-1` (x[-368,-48]) and `lvl_0`
    /// (x[-48,400]) are flush, both with y in [0,184]. Built entirely in memory --
    /// nix/CI build sources contain only git-tracked files, `assets/` does not exist,
    /// and tests must not depend on it.
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
            anims: crate::sprites::AnimationTable::default(),
            tilesets: releste_map::TilesetsDoc::default(),
            bg_tilesets: releste_map::TilesetsDoc::default(),
            spawn: Vec2::from_f32s(-56.0, 136.0),
            spawn_room: "lvl_-1".into(),
            backdrop_visible: RefCell::default(),
            backdrop_effects: RefCell::default(),
            backdrop_drift: RefCell::default(),
            warned: RefCell::default(),
            banner_time: RefCell::new(Fx::zero()),
        }
    }

    /// `room_at` must pick the room by the hitbox **center**, not a corner.
    ///
    /// Regression: with two flush rooms, the spawn center is in the left room, but the
    /// old implementation queried with the corner / a duplicate-compensated point and
    /// crossed into the right room -- the title-bar room name was wrong and the
    /// hot-reload "player is not in any room" warning fired spuriously.
    #[test]
    fn room_at_uses_the_hitbox_centre() {
        let session = seam_session();

        // Spawn point: its center (-52, 140) is in lvl_-1; the bottom-right corner sits
        // exactly on the seam.
        let spawn = Vec2::from_f32s(-56.0, 136.0);
        assert_eq!(
            session.room_at(spawn).name,
            "lvl_-1",
            "the spawn centre is inside lvl_-1, not the room across the seam"
        );

        // The room switches only once the center actually enters lvl_0 (pos.x > -52).
        let crossed = Vec2::from_f32s(-51.0, 136.0);
        assert_eq!(
            session.room_at(crossed).name,
            "lvl_0",
            "once the centre crosses the seam the room switches"
        );

        // Center exactly on the seam: the room earlier in file order wins (lvl_-1 is
        // first).
        let on_seam = Vec2::from_f32s(-52.0, 136.0);
        assert_eq!(
            session.room_at(on_seam).name,
            "lvl_-1",
            "a centre exactly on the shared edge resolves to the first room in file order"
        );
    }

    /// Room filtering + transition visibility semantics (vanilla `Backdrop.Update`,
    /// `references/Celeste.exe.cs.d/Celeste/Backdrop.cs:134-155`; `IsVisible`'s `OnlyIn` / `ExcludeFrom` branches,
    /// references/Celeste.exe.cs.d/Celeste/Backdrop.cs:122-131).
    #[test]
    fn backdrop_room_filter_and_transition_visibility() {
        let mut session = seam_session();
        let mut excluded = Backdrop::new("parallax");
        excluded.exclude_from = vec!["lvl_0".into()];
        let mut only = Backdrop::new("parallax");
        only.only_in = vec!["lvl_0".into()];
        session.doc.style.backgrounds = vec![excluded, only];

        // Not transitioning: follow IsVisible directly.
        session.update_backdrop_visibility("lvl_-1", false);
        let vis = session.backdrop_visible.borrow();
        assert!(vis[0], "excluded=lvl_0 must show in lvl_-1");
        assert!(!vis[1], "only_in=lvl_0 must hide in lvl_-1");
        drop(vis);

        // Transition into lvl_0: InstantIn defaults to true -> matching images become
        // visible immediately; non-matching images have no InstantOut -> they **keep**
        // the previous frame's visibility (vanilla Update does only these two checks
        // during a transition and leaves the rest untouched, references/Celeste.exe.cs.d/Celeste/Backdrop.cs:136-149).
        session.update_backdrop_visibility("lvl_0", true);
        let vis = session.backdrop_visible.borrow();
        assert!(vis[0], "without InstantOut the excluded backdrop lingers");
        assert!(vis[1], "only_in=lvl_0 must appear instantly (InstantIn)");
        drop(vis);

        // When transitioning **out** of a room where the filter matched, InstantOut
        // defaults to false -> keep it until the transition ends: the only_in image seen
        // from lvl_0 stays visible during the transition into lvl_-1.
        session.update_backdrop_visibility("lvl_0", false);
        session.update_backdrop_visibility("lvl_-1", true);
        let vis = session.backdrop_visible.borrow();
        assert!(
            vis[1],
            "without InstantOut the filtered backdrop lingers through the transition"
        );
        drop(vis);
        session.update_backdrop_visibility("lvl_-1", false);
        let vis = session.backdrop_visible.borrow();
        assert!(!vis[1], "after the transition IsVisible wins again");
    }

    /// A `snowbg` slot becomes a live effect, advances one tick per call, and follows
    /// kind changes on hot reload (vanilla `BackdropRenderer.Update` + `Snow.Update`).
    #[test]
    fn snow_backdrops_get_an_effect_and_advance_each_tick() {
        use crate::backdrop::Effect;
        let dt = Fx::from_bits(Fx::DT_ONE_SIXTIETH);

        let mut session = seam_session();
        session.doc.style.backgrounds = vec![Backdrop::new("snowbg")];
        session.doc.style.foregrounds = vec![Backdrop::new("snowfg")];
        session.update_backdrop_visibility("lvl_-1", false);
        session.update_backdrop_effects(dt);

        {
            let effects = session.backdrop_effects.borrow();
            assert_eq!(effects.len(), 2, "one slot per backdrop, bg then fg");
            let Effect::Snow(bg) = effects[0].as_ref().expect("snowbg is an effect");
            let Effect::Snow(fg) = effects[1].as_ref().expect("snowfg is an effect");
            assert!(!bg.is_foreground(), "slot 0 is the background variant");
            assert!(fg.is_foreground(), "slot 1 is the foreground variant");
        }

        let before = {
            let effects = session.backdrop_effects.borrow();
            match effects[0].as_ref() {
                Some(Effect::Snow(snow)) => snow.first_x(),
                _ => unreachable!(),
            }
        };
        session.update_backdrop_effects(dt);
        let after = {
            let effects = session.backdrop_effects.borrow();
            match effects[0].as_ref() {
                Some(Effect::Snow(snow)) => snow.first_x(),
                _ => unreachable!(),
            }
        };
        assert!(after < before, "a snow tick must drift the flakes left");

        // A hot reload that rewrites the slot to a static image drops the effect again.
        session.doc.style.backgrounds = vec![Backdrop::new("parallax")];
        session.doc.style.foregrounds = vec![Backdrop::new("snowfg")];
        session.update_backdrop_visibility("lvl_-1", false);
        session.update_backdrop_effects(dt);
        let effects = session.backdrop_effects.borrow();
        assert!(
            effects[0].is_none(),
            "a parallax slot holds no effect state"
        );
        let Effect::Snow(fg) = effects[1].as_ref().expect("snowfg survives the edit");
        assert!(fg.is_foreground());
    }

    /// Build a `parallax` backdrop.
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

    /// 0-Intro `bg0`'s real parameters (`x=-24 y=-64 scroll=(0.15,0.05) loopx`), texture
    /// 320x137 (`bgs\00\bg0` in `Gameplay.atlas`): after converting the origin through
    /// `Position - Floor(cam)*Scroll` it must cover the whole 320-wide screen, and with
    /// `loopy=false` there is only one row -- consistent with `Parallax.Render`
    /// references/Celeste.exe.cs.d/Celeste/Parallax.cs:64-113.
    #[test]
    fn parallax_loop_x_without_loop_y_tiles_one_row() {
        let bd = parallax_bd(-24.0, -64.0, (0.15, 0.05), (true, false));
        let tiles = parallax_tiles(
            &bd,
            (bd.x, bd.y),
            (176.9, 92.2),
            (320.0, 137.0),
            (320.0, 180.0),
        );

        // Floor(cam) = (176, 92) -> px = Floor(-24 - 26.4) = -51 and
        // py = Floor(-64 - 4.6) = -69; the loopx fold is vanilla's two while loops
        // (first <0 add, then >0 subtract, references/Celeste.exe.cs.d/Celeste/Parallax.cs:64-74) -> -51 is returned unchanged.
        assert_eq!(
            tiles,
            vec![(-51.0, -69.0), (269.0, -69.0)],
            "two columns of one row: -51..269 and 269..589 cover the 320px screen"
        );
        // Coverage: each column is 320 wide; the first column's end = -51+320 = 269 <=
        // 320, so the screen's right edge is covered.
        for (x, y) in &tiles {
            assert!(*y < 180.0, "row must start on screen (y={y})");
            assert!(
                *x < 320.0,
                "column must start before the right edge (x={x})"
            );
        }
    }

    /// A double-axis loop (`purplesunset`'s parameter shape: both `loopx/loopy` on) must
    /// cover 320x180, with the origin folded into `[0, width)` / `[0, height)`.
    #[test]
    fn parallax_double_loop_covers_the_whole_screen() {
        let bd = parallax_bd(0.0, 0.0, (0.5, 0.5), (true, true));
        let tiles = parallax_tiles(
            &bd,
            (bd.x, bd.y),
            (320.0, 180.0),
            (160.0, 90.0),
            (320.0, 180.0),
        );

        // Floor(0 - 160*0.5) = -160 -> folds back to 0; likewise for y -> the grid
        // starts at (0,0)/(160,90).
        assert_eq!(
            tiles,
            vec![(0.0, 0.0), (0.0, 90.0), (160.0, 0.0), (160.0, 90.0)]
        );
        // Each piece is 160x90 -> 2x2 exactly covers 320x180.
        assert!(tiles.iter().all(|(x, y)| *x < 320.0 && *y < 180.0));
    }

    /// The `for` condition comes first (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:99`/`references/Celeste.exe.cs.d/Celeste/Parallax.cs:101`): when the origin is
    /// off-screen that axis draws nothing; when it is on-screen and neither axis loops,
    /// exactly one piece is drawn.
    #[test]
    fn parallax_start_off_screen_draws_nothing() {
        let off = parallax_bd(400.0, 10.0, (1.0, 1.0), (false, false));
        assert!(
            parallax_tiles(
                &off,
                (off.x, off.y),
                (0.0, 0.0),
                (320.0, 180.0),
                (320.0, 180.0)
            )
            .is_empty(),
            "p.X = 400 >= 320: vanilla's for-condition never enters the loop"
        );

        let one = parallax_bd(10.0, 10.0, (1.0, 1.0), (false, false));
        assert_eq!(
            parallax_tiles(
                &one,
                (one.x, one.y),
                (0.0, 0.0),
                (320.0, 180.0),
                (320.0, 180.0)
            ),
            vec![(10.0, 10.0)],
            "!LoopX && !LoopY draws exactly one tile"
        );
    }

    /// `ParseBackdrop` lowercases `blendmode` and only then compares it with
    /// `"additive"` (`references/Celeste.exe.cs.d/Celeste/MapData.cs:383-398`); both flip bits travel independently
    /// (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:87-97`).
    #[test]
    fn backdrop_flags_follow_parse_backdrop() {
        let mut bd = parallax_bd(0.0, 0.0, (1.0, 1.0), (true, true));
        assert_eq!(parallax_flags(&bd), (false, false, false));

        bd.blend = "Additive".to_string();
        assert_eq!(
            parallax_flags(&bd),
            (false, false, true),
            "the blend mode is matched case-insensitively"
        );

        bd.blend = "alphablend".to_string();
        bd.flip_x = true;
        bd.flip_y = true;
        assert_eq!(
            parallax_flags(&bd),
            (true, true, false),
            "both flips survive together (vanilla's SpriteEffects.Both)"
        );
    }

    /// `Parallax.Update` drifts `Position` by `Speed * dt` every tick
    /// (`references/Celeste.exe.cs.d/Celeste/Parallax.cs:28-30`), independently of visibility, and a hot reload that rewrites
    /// `speedx` takes effect on the next tick.
    #[test]
    fn parallax_speed_drifts_the_position_each_tick() {
        let dt = Fx::from_bits(Fx::DT_ONE_SIXTIETH);

        let mut session = seam_session();
        let mut bd = parallax_bd(0.0, 0.0, (1.0, 1.0), (true, true));
        bd.speed_x = -32.0;
        session.doc.style.backgrounds = vec![bd];
        session.doc.style.foregrounds = Vec::new();

        session.update_backdrop_effects(dt);
        {
            let drift = session.backdrop_drift.borrow();
            assert_eq!(drift.len(), 1);
            // One tick of -32 px/s at 1/60 s = -32/60 px, rounded by the fixed-point
            // accumulator to within one 1/65536 step.
            let x = drift[0].x.to_f32();
            assert!(
                (x - (-32.0 / 60.0)).abs() < 0.001,
                "one tick drifts by Speed*dt, got {x}"
            );
            assert_eq!(drift[0].y.to_f32(), 0.0, "speedy is 0 here");
        }

        for _ in 0..59 {
            session.update_backdrop_effects(dt);
        }
        {
            let drift = session.backdrop_drift.borrow();
            let x = drift[0].x.to_f32();
            assert!(
                (x - -32.0).abs() < 0.01,
                "60 ticks of -32 px/s drift the backdrop by -32px, got {x}"
            );
        }

        // Hot reload rewriting the speed keeps the accumulated drift and applies the new
        // rate from the next tick on.
        let mut faster = parallax_bd(0.0, 0.0, (1.0, 1.0), (true, true));
        faster.speed_x = -64.0;
        session.doc.style.backgrounds = vec![faster];
        session.update_backdrop_effects(dt);
        let drift = session.backdrop_drift.borrow();
        let x = drift[0].x.to_f32();
        assert!(
            (x - (-32.0 - 64.0 / 60.0)).abs() < 0.02,
            "drift accumulates across the edit, got {x}"
        );
    }

    /// Color: all-zero = vanilla's default white (`references/Celeste.exe.cs.d/Celeste/MapData.cs:514`/`effective_color`); the
    /// `alpha` attribute folds into Color (`references/Celeste.exe.cs.d/Celeste/MapData.cs:523-525` `backdrop.Color *= child.AttrFloat("alpha");`).
    #[test]
    fn backdrop_tint_matches_parse_semantics() {
        let mut bd = parallax_bd(0.0, 0.0, (1.0, 1.0), (true, true));
        assert_eq!(
            backdrop_tint(&bd),
            [1.0; 4],
            "unset color falls back to white"
        );

        bd.color = [140, 140, 140, 140]; // `alpha=0.55` x white, 0-Intro's bg0
        let tint = backdrop_tint(&bd);
        for c in tint {
            assert!((c - 140.0 / 255.0).abs() < 1e-6, "tint channel {c}");
        }
    }

    /// Wire: `Wire.Render` draws segment by segment, the segments join end to end and
    /// their total length is >= the straight-line distance between the ends; the two end
    /// points must land on the curve's ends (`GetPoint(0)=Begin`, `GetPoint(1)=End`,
    /// `SimpleCurve.cs:22-28`), and the midpoint sags 12px (a Bezier sagged 24 is exactly
    /// half at t=0.5: `((24+24+0)/4)`).
    #[test]
    fn wire_segments_walk_the_bezier_end_to_end() {
        let from = (100.0f32, 50.0f32);
        let to = (200.0f32, 50.0f32);
        let segs = wire_segments(from, to);
        assert_eq!(segs.len(), 16, "vanilla draws 16 Draw.Line calls");

        let first = segs.first().unwrap();
        assert_eq!(
            (first.0 .0, first.0 .1),
            from,
            "the first segment starts at Begin"
        );

        // Segment stitching: previous segment's start + length/angle = next segment's
        // start.
        let mut cursor = from;
        for (start, len, angle) in &segs {
            assert!(
                (start.0 - cursor.0).abs() < 1e-3 && (start.1 - cursor.1).abs() < 1e-3,
                "segments must be contiguous: {start:?} vs {cursor:?}"
            );
            cursor = (start.0 + len * angle.cos(), start.1 + len * angle.sin());
        }
        assert!(
            (cursor.0 - to.0).abs() < 1e-3 && (cursor.1 - to.1).abs() < 1e-3,
            "the chain must land on End: {cursor:?}"
        );

        // Sag: the curve's lowest point (t=0.5) is 12px below the ends.
        let mid = (from.0 + to.0) / 2.0;
        let sag = segs
            .iter()
            .map(|(s, _, _)| (s.0 - mid).abs())
            .fold(f32::INFINITY, f32::min);
        let low = segs
            .iter()
            .filter(|(s, _, _)| (s.0 - mid).abs() <= sag)
            .map(|(s, _, _)| s.1)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (low - 62.0).abs() < 1.0,
            "mid-sag should be 12px below the 50px ends, got {low}"
        );
    }

    /// The deck's cell stepping must match vanilla (`references/Celeste.exe.cs.d/Celeste/Bridge.cs:47-57`): each cell is 8px
    /// or 16px and the total = `width`; cells inside the gap are skipped but the cursor
    /// still advances.
    #[test]
    fn bridge_cells_step_like_vanilla_and_skip_the_gap() {
        // 0-Intro `lvl_3`'s real data (`dump_rooms`): local position (64,128),
        // `width` 840, nodes (496,112) / (528,120) -> gap at room x 496..528.
        let start = 64.0f32;
        let width = 840i32;
        let gap = (496.0, 528.0);
        let cells = bridge_cells(start, width, gap);

        let mut x = start;
        for (cx, _, w, _, _) in &cells {
            assert!((cx - x).abs() < 1e-4, "cursor drift: {cx} vs {x}");
            x += *w as f32;
        }
        // The cursor traverses the whole width -- the gap only means no cells are
        // generated.
        assert!(
            (x - (start + width as f32)).abs() <= 16.0,
            "cursor should reach the far end, got {x}"
        );

        // All invisible cells fall in the gap, and there are visible cells on both sides
        // of it.
        for (cx, _, w, _, visible) in &cells {
            if !*visible {
                assert!(
                    *cx >= gap.0 && *cx < gap.1,
                    "invisible cell at {cx} is outside the gap"
                );
            } else {
                assert!(
                    *cx + *w as f32 <= gap.0 || *cx >= gap.1,
                    "visible cell at {cx} ({w} wide) overlaps the gap"
                );
            }
        }
        assert!(
            cells.iter().any(|(cx, .., v)| *v && *cx < gap.0),
            "left half exists"
        );
        assert!(
            cells.iter().any(|(cx, .., v)| *v && *cx >= gap.1),
            "right half exists"
        );
        assert!(
            cells.iter().any(|(.., v)| !*v),
            "the gap must skip at least one cell"
        );

        // The image x may only take values from the 11-entry size table.
        for (_, cell_x, w, h, _) in &cells {
            assert!(
                BRIDGE_TILE_SIZES
                    .iter()
                    .any(|(tx, tw, th)| tx == cell_x && tw == w && th == h),
                "cell ({cell_x},{w},{h}) is not in tileSizes"
            );
        }
    }

    /// A 16px end cap is cut vertically into 24 + 12 + 12 + ..., the rest is one strip;
    /// both flatten to y = -8 (`BridgeTile`, `references/Celeste.exe.cs.d/Celeste/BridgeTile.cs:26-50`).
    #[test]
    fn bridge_tile_strips_split_wide_ends_only() {
        let wide = bridge_tile_strips(0, 16, 52);
        assert_eq!(
            wide.iter().map(|s| s.3).collect::<Vec<_>>(),
            vec![24, 12, 12, 4],
            "52px of art is cut 24 + 12 + 12 + remainder"
        );
        assert_eq!(
            wide.iter().map(|s| (s.1, s.4)).collect::<Vec<_>>(),
            vec![(0, -8.0), (24, 16.0), (36, 28.0), (48, 40.0)],
            "each strip keeps its image y and slides `y - 8` in world space"
        );

        let narrow = bridge_tile_strips(48, 8, 52);
        assert_eq!(narrow, vec![(48, 0, 8, 52, -8.0)]);
    }

    /// A frame without hair metadata (vanilla `HasHair = false`) draws no hair, and the
    /// default offset is the vanilla zero, not the old `(0, -2)` constant.
    #[test]
    fn hair_defaults_have_no_hair_and_zero_offset() {
        let hair = sprites::parse_hair_spec("x");
        assert!(!hair.has_hair, "`x` means no hair on this frame");
        assert_eq!(hair.offset, (0, 0), "vanilla default offset is zero");
        assert_eq!(sprites::HairFrame::default().offset, (0, 0));
    }

    /// Swapping the skin rebuilds the atlas from disk instead of appending a second copy.
    ///
    /// `SpriteAtlas::append` has no inverse, so appending again on every `[skin]` edit would
    /// grow the descriptor without bound and keep the first skin's groups alive forever
    /// (which is also why `set_skin` reloads the base atlas each time).
    #[test]
    fn swapping_the_skin_rebuilds_the_atlas_instead_of_appending_it_again() {
        let root = std::env::temp_dir().join("releste-session-set-skin");
        let _ = std::fs::remove_dir_all(&root);
        let atlas_dir = root.join("assets/atlas");
        std::fs::create_dir_all(&atlas_dir).unwrap();
        std::fs::write(
            atlas_dir.join("Test.atlas"),
            r#"name = "Test"
version = 0
[[textures]]
name = "Test0"
kind = "png"
source = "atlas/Test/Test0.png"

[[textures.sprites]]
name = 'characters\player\idle00'
x = 0
y = 0
w = 8
h = 11
origin_x = 4
origin_y = 5
frame_w = 8
frame_h = 11
"#,
        )
        .unwrap();
        std::fs::write(
            atlas_dir.join("Skin.atlas"),
            r#"name = "Skin"
version = 0
[[textures]]
name = "Skin0"
kind = "png"
source = "atlas/Skin/img0.png"

[[textures.sprites]]
name = 'Skin\characters\player\hair00'
x = 0
y = 0
w = 8
h = 9
origin_x = 0
origin_y = 0
frame_w = 8
frame_h = 9
"#,
        )
        .unwrap();
        let paths = Paths::resolve(Some(&root));
        assert_eq!(paths.atlas_dir(), atlas_dir);

        let doc = MapDocument {
            area: "test".into(),
            rooms: vec![Room {
                name: "lvl_t".into(),
                bounds: RectDef {
                    x: 0.0,
                    y: 0.0,
                    w: 64.0,
                    h: 32.0,
                },
                tiles: Tileset::new("Test", 8, 4),
                bg: Tileset::new("Test", 8, 4),
                entities: Vec::new(),
                triggers: Vec::new(),
                script: None,
                ..Default::default()
            }],
            sync_rules: Vec::new(),
            ..Default::default()
        };
        let mut session = Session::from_doc(doc, "Test");

        session.set_skin(None, &paths).unwrap();
        assert_eq!(
            session.atlas.descriptor().textures.len(),
            1,
            "the vanilla session is the base atlas alone"
        );

        session.set_skin(Some("Skin"), &paths).unwrap();
        assert_eq!(session.atlas.descriptor().textures.len(), 2);
        assert!(
            session
                .atlas
                .sprite(r"Skin\characters\player\hair00")
                .is_some(),
            "the skin's hair must resolve after the swap"
        );

        session.set_skin(Some("Skin"), &paths).unwrap();
        assert_eq!(
            session.atlas.descriptor().textures.len(),
            2,
            "re-selecting the same skin must not append it a second time"
        );

        session.set_skin(None, &paths).unwrap();
        assert_eq!(session.atlas.descriptor().textures.len(), 1);
        assert!(
            session
                .atlas
                .sprite(r"Skin\characters\player\hair00")
                .is_none(),
            "dropping the skin must drop its groups as well"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Left and right facing must be mirror images about `Player.Position`.
    ///
    /// Vanilla flips by negating `Sprite.Scale.X` (references/Celeste.exe.cs.d/Celeste/Player.cs:1390/references/Celeste.exe.cs.d/Celeste/Player.cs:1406) and FNA mirrors a
    /// negative-scaled quad about its origin; flipping only the UVs (which is what
    /// `SpriteEffects.FlipHorizontally` does) leaves the quad in place and breaks the
    /// mirror for every frame whose trimmed content is not centred on the origin.
    /// A session whose atlas carries player-like frames with **off-centre** trims -- the
    /// case that makes UV-only flipping asymmetric. Built in memory: the nix/CI sources are
    /// git-tracked only, so `assets/` does not exist there.
    fn sprite_session() -> Session {
        use releste_render::atlas::{Sprite, TextureGroup, TextureKind};
        let sprite = |name: &str, w: i16, h: i16, ox: i16, oy: i16| Sprite {
            name: name.into(),
            x: 0,
            y: 0,
            w,
            h,
            origin_x: ox,
            origin_y: oy,
            frame_w: 32,
            frame_h: 32,
            source: None,
        };
        // Trim values copied from the real `Gameplay.atlas` entries (name, w, h, origin_x,
        // origin_y): none of them is centred on the 32x32 frame origin (16, 32).
        let sprites = vec![
            sprite("characters\\player\\idle00", 13, 12, 8, 20),
            sprite("characters\\player\\runFast00", 14, 12, 7, 20),
            sprite("characters\\player\\runFast05", 14, 11, 9, 21),
            sprite("characters\\player\\flip00", 14, 11, 11, 21),
            sprite("characters\\player\\jumpSlow00", 13, 16, 9, 16),
        ];
        let mut session = seam_session();
        session.atlas = SpriteAtlas::new(
            AtlasDescriptor {
                name: "Test".into(),
                version: 0,
                links: Vec::new(),
                textures: vec![TextureGroup {
                    name: "Test0".into(),
                    kind: TextureKind::Png,
                    source: "atlas/Test/Test0.png".into(),
                    width: Some(64),
                    height: Some(64),
                    sprites,
                }],
            },
            Path::new("/nonexistent"),
        );
        session
    }

    /// Left and right facing must be mirror images about `Player.Position`.
    ///
    /// Vanilla flips the player by negating `Sprite.Scale.X` (`Player.Render` multiplies the
    /// scale by `Facing` around `base.Render()`, references/Celeste.exe.cs.d/Celeste/Player.cs:1390/references/Celeste.exe.cs.d/Celeste/Player.cs:1406) and FNA
    /// mirrors a negative-scaled quad **about the origin**; swapping only the UVs (which is
    /// what `SpriteEffects.FlipHorizontally` does) leaves the quad in place, so every frame
    /// whose trimmed content is not centred on the origin came out asymmetric.
    /// The stored scenery value is `flat_index + 1`, so `0` stays "empty" and the lowest
    /// drawable index is `0` (`tilesets/scenery`'s first cell).
    ///
    /// Provenance: `tools/content-pipeline/src/pipeline.rs`, `csv_tileset` stores
    /// `value + 1`; vanilla's CSV uses `-1` for "no tile" and `Tileset[index]` indexes
    /// flat (`references/Celeste.exe.cs.d/Monocle/Tileset.cs:15-25`).
    #[test]
    fn scenery_cells_decode_to_flat_tileset_indices() {
        assert_eq!(scenery_index(0), None, "0 means empty, not index -1");
        assert_eq!(scenery_index(1), Some(0), "the first sheet cell is index 0");
        assert_eq!(scenery_index(2), Some(1));
        assert_eq!(scenery_index(353), Some(352), "5-MirrorTemple reaches 352");
    }

    /// A `bgtiles` / `fgtiles` grid draws one 8x8 sprite per non-empty cell, at the room
    /// offset, folding the flat index into `(col, row)` of the shared scenery sheet.
    ///
    /// Vanilla overlays the same grid onto the tile layer and renders `tileset[index]`
    /// per cell (`LevelLoader`, `references/Celeste.exe.cs.d/Celeste/LevelLoader.cs:283-293`; `references/Celeste.exe.cs.d/Monocle/Tileset.cs:15-25`).
    #[test]
    fn scenery_grid_draws_one_tile_per_non_empty_cell() {
        use releste_render::atlas::{Sprite, TextureGroup, TextureKind};
        // A 240x96 sheet of 8x8 cells is 30 columns x 12 rows (the real
        // `tilesets\scenery` geometry in `assets/atlas/Gameplay.atlas`).
        let sheet = Sprite {
            name: "tilesets\\scenery".into(),
            x: 0,
            y: 0,
            w: 240,
            h: 96,
            origin_x: 0,
            origin_y: 0,
            frame_w: 240,
            frame_h: 96,
            source: None,
        };
        let mut session = seam_session();
        session.atlas = SpriteAtlas::new(
            AtlasDescriptor {
                name: "Test".into(),
                version: 0,
                links: Vec::new(),
                textures: vec![TextureGroup {
                    name: "Test0".into(),
                    kind: TextureKind::Png,
                    source: "atlas/Test/Test0.png".into(),
                    width: Some(256),
                    height: Some(256),
                    sprites: vec![sheet],
                }],
            },
            Path::new("/nonexistent"),
        );
        let size = |_id: u32| Some((256u32, 256u32));

        // 3x2 grid holding indices 0, 31 (= col 1, row 1) and 60 (= col 0, row 2).
        let mut grid = Tileset {
            atlas: "Scenery".into(),
            width: 3,
            height: 2,
            tiles: vec![1, 32, 0, 0, 61, 0],
        };
        let mut room = Room {
            name: "lvl_0".into(),
            bounds: RectDef {
                x: -48.0,
                y: 0.0,
                w: 24.0,
                h: 16.0,
            },
            tiles: Tileset::default(),
            bg: Tileset::default(),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
            bg_decals: Vec::new(),
            fg_decals: Vec::new(),
            bg_scenery: Tileset::default(),
            fg_scenery: Tileset::default(),
        };
        room.fg_scenery = grid.clone();

        let mut out = Vec::new();
        push_scenery(
            &mut out,
            &room,
            &room.fg_scenery,
            LAYER_FG_SCENERY,
            &session,
            &size,
        );
        assert_eq!(out.len(), 3, "one draw per non-empty cell");
        for draw in &out {
            assert_eq!(draw.layer, LAYER_FG_SCENERY);
            assert_eq!(draw.size, Vec2::from_f32s(8.0, 8.0));
        }
        // Cell (0,0) -> index 0, i.e. the sheet's top-left cell.
        assert_eq!(out[0].position, Vec2::from_f32s(-48.0, 0.0));
        assert_eq!(out[0].uv_min, (0.0, 0.0));
        // Cell (1,0) -> index 31 -> col 1, row 1 (30 columns per sheet row).
        assert_eq!(out[1].position, Vec2::from_f32s(-40.0, 0.0));
        assert_eq!(out[1].uv_min, (8.0 / 256.0, 8.0 / 256.0));
        // Cell (1,1) -> index 60 -> col 0, row 2.
        assert_eq!(out[2].position, Vec2::from_f32s(-40.0, 8.0));
        assert_eq!(out[2].uv_min, (0.0, 16.0 / 256.0));

        // An all-zero grid is skipped without even resolving the sheet.
        grid.tiles = vec![0; 6];
        let mut empty = Vec::new();
        push_scenery(&mut empty, &room, &grid, LAYER_FG_SCENERY, &session, &size);
        assert!(empty.is_empty(), "an empty grid draws nothing");
    }

    /// Decals are **centre**-anchored on the untrimmed frame, and a negative scale
    /// mirrors them about that centre.
    ///
    /// Vanilla: `DecalImage.Render` -> `MTexture.DrawCentered(Position, White, scale)`,
    /// which passes `Center - DrawOffset` as the origin (`references/Celeste.exe.cs.d/Monocle/MTexture.cs:206-234`), and
    /// `Center` is `Width / 2, Height / 2` of the **frame**, not of the cropped rect.
    #[test]
    fn decals_are_centred_on_their_frame_and_mirror_about_the_centre() {
        use releste_render::atlas::{Sprite, TextureGroup, TextureKind};
        // A plain (non-banner) decal sprite, shaped like `decals\generic\grass_a` from
        // the real atlas: 5x5 art inside an 8x8 frame with the trim origin (2, 3).
        let sprite = Sprite {
            name: "decals\\0-prologue\\sign".into(),
            x: 0,
            y: 0,
            w: 5,
            h: 5,
            origin_x: 2,
            origin_y: 3,
            frame_w: 8,
            frame_h: 8,
            source: None,
        };
        let mut session = seam_session();
        session.atlas = SpriteAtlas::new(
            AtlasDescriptor {
                name: "Test".into(),
                version: 0,
                links: Vec::new(),
                textures: vec![TextureGroup {
                    name: "Test0".into(),
                    kind: TextureKind::Png,
                    source: "atlas/Test/Test0.png".into(),
                    width: Some(256),
                    height: Some(256),
                    sprites: vec![sprite],
                }],
            },
            Path::new("/nonexistent"),
        );
        let size = |_id: u32| Some((256u32, 256u32));
        let room = Room {
            name: "lvl_0".into(),
            bounds: RectDef {
                x: 100.0,
                y: 200.0,
                w: 320.0,
                h: 180.0,
            },
            tiles: Tileset::default(),
            bg: Tileset::default(),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
            bg_decals: Vec::new(),
            fg_decals: Vec::new(),
            bg_scenery: Tileset::default(),
            fg_scenery: Tileset::default(),
        };
        let decal = |scale_x: f32| releste_map::Decal {
            texture: "decals/0-prologue/sign".into(),
            x: 40.0,
            y: 60.0,
            scale_x,
            scale_y: 1.0,
        };

        let mut right = Vec::new();
        push_decals(
            &mut right,
            &room,
            &[decal(1.0)],
            LAYER_FG_DECAL,
            &session,
            &size,
        );
        let mut left = Vec::new();
        push_decals(
            &mut left,
            &room,
            &[decal(-1.0)],
            LAYER_FG_DECAL,
            &session,
            &size,
        );

        assert_eq!(right.len(), 1);
        assert_eq!(left.len(), 1);
        let (r, l) = (&right[0], &left[0]);
        // Frame centre (4,4) minus the trim origin (2,3) = (2,1); the quad is 5x5.
        assert_eq!(
            r.position,
            Vec2::from_f32s(100.0 + 40.0 - 2.0, 200.0 + 60.0 - 1.0)
        );
        assert_eq!(r.size, Vec2::from_f32s(5.0, 5.0));
        assert!(!r.flip_x, "an unmirrored decal does not swap UVs");
        assert_eq!(r.layer, LAYER_FG_DECAL);
        assert!(l.flip_x, "a negative scale mirrors the decal");
        assert_eq!(l.position.y, r.position.y, "mirroring keeps y");
        assert_eq!(l.size, r.size, "mirroring keeps the size");
        // Reflecting the unmirrored quad about the anchor gives the mirrored one.
        let mirrored_x = 2.0 * (100.0 + 40.0) - r.position.x.to_f32() - r.size.x.to_f32();
        assert!(
            (l.position.x.to_f32() - mirrored_x).abs() < 0.001,
            "mirrored decal x={} is not the reflection of x={} (would be {mirrored_x})",
            l.position.x.to_f32(),
            r.position.x.to_f32()
        );
    }

    /// The four `generic/grass_a..d` decals are bannered by `Decal.Added`
    /// (`references/Celeste.exe.cs.d/Celeste/Decal.cs:164-176`), so they must never join the cached static list.
    #[test]
    fn grass_decals_do_not_join_the_static_list() {
        assert!(is_banner_decal("decals/generic/grass_a"));
        assert!(is_banner_decal("decals\\generic\\grass_b"));
        assert!(is_banner_decal("GENERIC/GRASS_C"));
        assert!(is_banner_decal("generic/grass_d.png"));
        for name in [
            "decals/0-prologue/sign",
            "generic/grass",
            "generic/grass_e",
            "scenery/grass_a",
            "",
        ] {
            assert!(
                !is_banner_decal(name),
                "{name} must not be treated as a banner"
            );
        }

        let (session, room, size) = banner_fixture();
        let decal = releste_map::Decal {
            texture: "decals/generic/grass_a".into(),
            x: 40.0,
            y: 60.0,
            scale_x: 1.0,
            scale_y: 1.0,
        };
        let mut statics = Vec::new();
        push_decals(
            &mut statics,
            &room,
            &[decal],
            LAYER_FG_DECAL,
            &session,
            &size,
        );
        assert!(
            statics.is_empty(),
            "a banner decal draws nothing statically"
        );
    }

    /// Vanilla slices the banner decal into 1-px frame rows (`MakeBanner`,
    /// `references/Celeste.exe.cs.d/Celeste/Decal.cs:372-395`): the empty trim rows still count in `list.Count`, each
    /// visible slice stacks directly below the previous one, and the slice's quad is
    /// one pixel of the cropped art (`GetRelativeRect` clamps).
    #[test]
    fn grass_banners_slice_the_frame_like_vanilla() {
        let (session, room, size) = banner_fixture();
        let decal = releste_map::Decal {
            texture: "decals/generic/grass_a".into(),
            x: 40.0,
            y: 60.0,
            scale_x: 1.0,
            scale_y: 1.0,
        };
        let mut out = Vec::new();
        push_grass_banners(
            &mut out,
            &room,
            &[decal],
            LAYER_FG_DECAL,
            &session,
            &size,
            0,
            true,
        );
        // grass_a: 5x5 art with DrawOffset (2, 3) inside the 8x8 frame -> slices
        // 3..8 are visible, 5 quads.
        assert_eq!(out.len(), 5, "one quad per visible 1-px slice");
        for (k, draw) in out.iter().enumerate() {
            let i = 3.0 + k as f32;
            // Rows stack: origin_y = frame/2 - i for i >= DrawOffset.y = 3, so the
            // slice tops run anchor.y - 1, anchor.y, anchor.y + 1, ...
            assert_eq!(
                draw.position.y,
                Vec2::from_f32s(0.0, 200.0 + 60.0 - (4.0 - i)).y,
                "slice {k} must sit on frame row {i}"
            );
            assert_eq!(draw.size, Vec2::from_f32s(5.0, 1.0), "slice {k} is 5x1");
            assert_eq!(draw.layer, LAYER_FG_DECAL);
            // UV v-range covers art row k of the sprite at (5, 94) on a 256-px sheet.
            let expected_v0 = (94.0 + k as f32) / 256.0;
            assert!(
                (draw.uv_min.1 - expected_v0).abs() < 1e-6,
                "slice {k} must sample art row {k} (v0 {} vs {expected_v0})",
                draw.uv_min.1
            );
            assert!((draw.uv_max.1 - expected_v0 - 1.0 / 256.0).abs() < 1e-6);
            // Horizontal quad: origin_x = frame/2 - max(ox, 0) = 4 - 2 = 2 from the
            // slice's left edge.
            assert!(
                (draw.position.x.to_f32() - (140.0 - 2.0)).abs() < 1.5,
                "slice {k} x={} must be the anchor minus the origin plus the sway",
                draw.position.x.to_f32()
            );
        }
        assert!(!out[0].flip_x, "a positive scale does not swap UVs");
    }

    /// `Banner.Update` advances `sineTimer` by `Engine.DeltaTime` per tick
    /// (`references/Celeste.exe.cs.d/Celeste/Decal.cs:44-46`) and `Banner.Render` shifts slice `i` by
    /// `sin(sineTimer * 2 + i * 0.05) * num * 2 + num * -2` (`:53-62`): the shared
    /// clock must move the slices, and deterministically per decal.
    #[test]
    fn grass_banners_sway_with_the_shared_clock() {
        let (session, room, size) = banner_fixture();
        let decal = |x: f32| releste_map::Decal {
            texture: "decals/generic/grass_a".into(),
            x,
            y: 60.0,
            scale_x: 1.0,
            scale_y: 1.0,
        };
        let draws = |session: &Session, x: f32| {
            let mut out = Vec::new();
            push_grass_banners(
                &mut out,
                &room,
                &[decal(x)],
                LAYER_FG_DECAL,
                session,
                &size,
                0,
                true,
            );
            out
        };
        let first = draws(&session, 40.0);
        // Advancing one tick (1/60 s) re-rolls every slice's sine phase.
        *session.banner_time.borrow_mut() += Fx::from_bits(Fx::DT_ONE_SIXTIETH);
        let second = draws(&session, 40.0);
        assert_eq!(first.len(), second.len());
        let moved = first
            .iter()
            .zip(&second)
            .filter(|(a, b)| a.position != b.position)
            .count();
        assert!(moved > 0, "advancing the clock must sway the slices");
        // The same decal in another room keeps its own phase, but the phase is
        // deterministic: identical inputs reproduce identical offsets bit for bit.
        let repeat = draws(&session, 40.0);
        for (a, b) in second.iter().zip(&repeat) {
            assert_eq!(a.position, b.position, "the phase must be deterministic");
        }
        assert_ne!(
            banner_phase(0, true, 0),
            banner_phase(0, true, 1),
            "two decals must not sway in lockstep"
        );
        assert_eq!(banner_phase(3, false, 7), banner_phase(3, false, 7));
    }

    /// Builds the grass banner fixture: a session whose atlas holds the real
    /// `decals\generic\grass_a` sprite (5x5 art at (5, 94), DrawOffset (2, 3), frame
    /// 8x8) and one room anchored at (100, 200).
    fn banner_fixture() -> (Session, Room, impl Fn(u32) -> Option<(u32, u32)>) {
        use releste_render::atlas::{Sprite, TextureGroup, TextureKind};
        let sprite = Sprite {
            name: "decals\\generic\\grass_a".into(),
            x: 5,
            y: 94,
            w: 5,
            h: 5,
            origin_x: 2,
            origin_y: 3,
            frame_w: 8,
            frame_h: 8,
            source: None,
        };
        let mut session = seam_session();
        session.atlas = SpriteAtlas::new(
            AtlasDescriptor {
                name: "Test".into(),
                version: 0,
                links: Vec::new(),
                textures: vec![TextureGroup {
                    name: "Test0".into(),
                    kind: TextureKind::Png,
                    source: "atlas/Test/Test0.png".into(),
                    width: Some(256),
                    height: Some(256),
                    sprites: vec![sprite],
                }],
            },
            Path::new("/nonexistent"),
        );
        let room = Room {
            name: "lvl_0".into(),
            bounds: RectDef {
                x: 100.0,
                y: 200.0,
                w: 320.0,
                h: 180.0,
            },
            tiles: Tileset::default(),
            bg: Tileset::default(),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
            bg_decals: Vec::new(),
            fg_decals: Vec::new(),
            bg_scenery: Tileset::default(),
            fg_scenery: Tileset::default(),
        };
        (session, room, |_id: u32| Some((256u32, 256u32)))
    }

    #[test]
    fn flipped_body_is_the_mirror_of_the_unflipped_body() {
        let session = sprite_session();
        let anchor = Vec2::from_f32s(100.0, 200.0);
        let size = |_id: u32| Some((64u32, 64u32));

        for name in [
            "characters\\player\\idle00",
            "characters\\player\\runFast00",
            "characters\\player\\runFast05",
            "characters\\player\\flip00",
            "characters\\player\\jumpSlow00",
        ] {
            let right = session
                .body_draw(&size, anchor, false, [1.0, 1.0], Some(name))
                .unwrap_or_else(|| panic!("{name} missing from the fixture atlas"));
            let left = session
                .body_draw(&size, anchor, true, [1.0, 1.0], Some(name))
                .expect("the same sprite must resolve when flipped");

            // Reflecting the unflipped quad about the anchor gives the flipped quad.
            let mirrored_x =
                2.0 * anchor.x.to_f32() - right.position.x.to_f32() - right.size.x.to_f32();
            assert!(
                (left.position.x.to_f32() - mirrored_x).abs() < 0.001,
                "{name}: flipped body at x={} is not the mirror of x={} (mirror would be {mirrored_x})",
                left.position.x.to_f32(),
                right.position.x.to_f32()
            );
            assert_eq!(left.position.y, right.position.y, "{name}: y must not move");
            assert_eq!(left.size, right.size, "{name}: size must not change");
            assert!(
                left.flip_x,
                "{name}: the flipped body must still swap its UVs"
            );
        }
    }
}
