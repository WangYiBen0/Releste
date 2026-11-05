//! A loaded `.map` plus its atlas, and the static part of "what to draw this frame".
//!
//! Maps are data (AGENTS.md 4.2): here we only turn tiles and entities into
//! [`SpriteDraw`], with no gameplay branching at all.

use std::cell::RefCell;
use std::collections::HashSet;

use anyhow::{bail, Context as _, Result};
use releste_map::{Backdrop, MapDocument, Room};
use releste_math::Vec2;
use releste_render::atlas::SpriteAtlas;
use releste_render::{GpuRenderer, Layer, SpriteDraw};
use releste_world::{EntityData, FieldValue};
use tracing::{debug, warn};

use crate::paths::Paths;
use crate::sprites;

/// World pixels per tile (320 / 40 = 8px for `lvl_-1`).
const TILE: f32 = sprites::TILE_PX as f32;

/// Draw layer for `introCrusher`: vanilla `Depth = -10501` (`pureCeleste.cs:106324`),
/// where smaller values draw in front -- ahead of SolidTiles' -10000 (`124913`), with
/// the player (`0`) further back still, but in front of the foreground parallax layer
/// that is drawn last. The engine layer order (smaller value draws first) uses
/// `Effects(300) < this layer < Foreground(500)`.
const LAYER_INTRO_CRUSHER: i32 = Layer::Foreground.value() - 100;

/// Draw layer for `wire` (`above=false`): vanilla `Depth = 2000`
/// (`pureCeleste.cs:106687`) -- drawn after the background layer and before the
/// terrain and the player (in vanilla, smaller values draw in front). The engine layer
/// order (smaller value draws first) places it between
/// `BgTiles(-500)` and `Tiles(0)`.
const LAYER_WIRE: i32 = Layer::BgTiles.value() + 100;

/// Draw layer for `wire above=true`: vanilla `Depth = -8500` (`106692`) -- before the
/// player and after the terrain. The engine draws terrain after the player, so the two
/// vanilla orders cannot both hold; take "before the player, before the crusher and the
/// foreground": after `Player(200)` and before `LAYER_INTRO_CRUSHER(400)` /
/// `Foreground(500)`. 0-Intro has no `above` wire (the property table has no such key).
const LAYER_WIRE_ABOVE: i32 = Layer::Player.value() + 150;

/// Draw layer for `BridgeTile`: vanilla sets no Depth -> `Entity.Depth` defaults to
/// `0` (property at `pureCeleste.cs:12997`), the same depth as the player; the engine
/// uses `Entities(100)` -- after the terrain and before the player (the planks lie in
/// front of the terrain and Madeline walks over them).
const LAYER_BRIDGE: i32 = Layer::Entities.value();

/// The 16-segment polyline of one `Wire`: returns `(start, length, rotation radians)`
/// per segment.
///
/// Follows `Wire.Render` (`pureCeleste.cs:106699-106710`): control point = midpoint of
/// the two ends sagged 24px (`106702`); points come from the quadratic Bezier
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

/// Vanilla `Bridge`'s `tileSizes` table (`pureCeleste.cs:107719-107729`):
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
/// Follows the `BridgeTile` constructor (`pureCeleste.cs:107836-107890`): a width-16
/// end cap is cut along its height into 24 + 12 + 12 + ... (`107854-107863`), while the
/// rest is a single strip (`107866-107873`); both flatten the image y to `-8`
/// (`107874-107878`).
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
/// Follows `Bridge.Added` (`pureCeleste.cs:107704-107731`): starting at `Position`,
/// advance cell by cell along `width`, cycling through `tileSizes`; when `num` falls in
/// `2..=7` vanilla picks a wood-grain strip with `Calc.Random.Next(6)` (seeded by
/// `PushRandom(1)`, `107721`), and those 6 candidates are **all 8px wide**, so the
/// engine just takes them by index -- the stepping matches vanilla cell for cell, and
/// only the image pattern of the 8px strips becomes a fixed sequence.
///
/// Returns **all** cells and marks with a `bool` whether each falls in the gap, because
/// vanilla "advances the cursor first, then decides whether to `level.Add`"
/// (`107722-107726`): the gap width still counts toward the stepping, and dropping the
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

/// The effective color of a single `Backdrop`: all-zero is treated as "unset" and
/// takes vanilla's default white (starting from `112789`
/// `backdrop.Color = Color.White`; `112790-112796` only override it when
/// `HasAttr("color")`); otherwise, as vanilla does, scale it once by `alpha`
/// (`112798-112804` `backdrop.Color *= alpha`).
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
/// Step by step against `Parallax.Render` (`pureCeleste.cs:112163-112236`):
/// 1. `v = (Camera.Position + CameraOffset).Floor()` (`112165`). The engine has no
///    `Backdrop.CameraOffset` concept yet (`CameraOffset` is a field unique to
///    `Parallax`, default `Vector2.Zero`, `112129-112130`), so it uses the camera
///    position directly.
/// 2. `p = (Position - v*Scroll).Floor()` (`112166`).
/// 3. If `LoopX`/`LoopY`: two `while` loops fold `p.X`/`p.Y` back (`112185-112206`), in
///    the same order as vanilla (first `<0` add, then `>0` subtract); with `!Loop`
///    nothing is folded.
/// 4. A double `for` tiles out to `320x180` (`112220-112234`) -- **condition first**,
///    outer X and inner Y: when the start is off-screen (`p.X >= 320` or `p.Y >= 180`)
///    that axis draws nothing; with `!LoopY` the inner loop `break`s after one piece,
///    and with `!LoopX` the outer loop `break`s after one column. Each piece is
///    `Texture.Draw(pos, Vector2.Zero, color, 1, 0, flip)` (`112224`), with origin
///    `Vector2.Zero` (top-left of the cropped sub-image) and scale `1`.
fn parallax_tiles(
    bd: &Backdrop,
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
    // 2. p = (Position - v*Scroll).Floor().
    let mut px = (bd.x - cam_x * bd.scroll_x).floor();
    let mut py = (bd.y - cam_y * bd.scroll_y).floor();
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

/// Background / foreground parallax texture tiling (`Parallax.Render`,
/// `pureCeleste.cs:112163-112236`).
///
/// The geometry goes through [`parallax_tiles`] (a pure function with reference unit
/// tests); this function only does texture lookup, tinting and conversion to
/// [`SpriteDraw`].
///
/// Engine differences (on record, not behavior deviations):
/// - An origin of `Vector2.Zero` into the cropped sub-image means top-left alignment,
///   so this uses `sprite.w/h` and `uv_for_sprite` directly and does not go through
///   [`sprites::origin_in_trim`] (which is for sprites that have an in-frame origin).
/// - `blendmode="additive"` is not supported yet (the wgpu backend currently has only
///   a single `PREMULTIPLIED_ALPHA_BLENDING` pipeline, see `gpu.rs:289-291`), so
///   everything draws premultiplied-alpha; a known deviation. Likewise `FlipY`
///   (`SpriteDraw` only has `flip_x`) and the horizontal drift driven by
///   `WindMultiplier` (`112157`) do not take effect yet; 0-Intro does not use them.
/// - `fadeIn` (`DoFadeIn`, `112153-112160`) and `FadeX`/`FadeY` (`Fader`,
///   `109343-109378`) are not ported yet: 0-Intro's `ParseBackdrop` (`112651`
///   `DoFadeIn = bool.Parse(child.Attr("fadeIn", "false"))`) defaults to `false`, so
///   `fadeIn` is always `Visible ? 1 : 0` and is `1` once in the scene (no transition);
///   the current `Service` does not track `Visible`, so it stays `1` here; `FadeX`/
///   `FadeY` only have a value with `<apply fade=...>`, which 0-Intro never uses.
/// - The `Parallax.Alpha` (`112141`) field is never written by `ParseBackdrop` (the
///   `alpha` attribute is only multiplied into `backdrop.Color`, `112798-112804`) and
///   defaults to `1` -> in `Render`, `num = fadeIn*Alpha*FadeAlphaMultiplier = 1` and
///   `color *= num` never triggers (`112167-112179`), so using `effective_color()`
///   directly is equivalent.
fn push_backdrops(
    out: &mut Vec<SpriteDraw>,
    backdrops: &[Backdrop],
    camera_pos: Vec2,
    layer: i32,
    visibility: &[bool],
    session: &Session,
    gpu: &GpuRenderer,
) {
    let screen = (releste_render::BASE_WIDTH, releste_render::BASE_HEIGHT);
    for (i, bd) in backdrops.iter().enumerate() {
        // Room-filter visibility is refreshed by [`Session::update_backdrop_visibility`]
        // at vanilla `Backdrop.Update`'s cadence; this just reads it.
        if visibility.get(i).copied() != Some(true) {
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
            continue; // texture not uploaded (unconverted .data); `load_atlas` already warned
        };
        let uv = GpuRenderer::uv_for_sprite(sprite, Vec2::from_f32s(tw as f32, th as f32));
        let (w, h) = (f32::from(sprite.w), f32::from(sprite.h));
        let tint = backdrop_tint(bd);
        // Vanilla's early exit for `color.A <= 1` (`112181-112184`) -- equivalent to
        // alpha <= 1/255.
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

/// Tint for foreground tiles / entities / background tiles.
///
/// Every vanilla `TileGrid` uses the default `Color.White` + `Alpha = 1`
/// (`pureCeleste.cs:11234`/`11242`; nothing in vanilla writes it), so this is white.
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
    /// Visibility state per backdrop (vanilla `Backdrop.Visible`, `109366` starts as
    /// `true`; `Backdrop.Update` refreshes it every frame from `IsVisible` plus the
    /// transition rules, `109468-109489`). Index = backgrounds + foregrounds
    /// concatenated in order.
    backdrop_visible: RefCell<Vec<bool>>,
    /// Keys that warn only once, to avoid spamming every frame.
    warned: RefCell<HashSet<String>>,
}

/// Bangs frame name: `bangs + animation frame index mod 3` (vanilla `HairFrame` reads
/// frame metadata, `pureCeleste.cs:111351-111356`; this atlas does not store per-frame
/// metadata, so taking the animation frame index modulo 3 is equivalent: idle's single
/// frame -> bangs00, the walk loop -> three alternating frames).
fn bangs_frame(anim_frame: usize) -> &'static str {
    match anim_frame % 3 {
        0 => sprites::PLAYER_BANGS_SPRITE,
        1 => "characters\\player\\bangs01",
        _ => "characters\\player\\bangs02",
    }
}

impl Session {
    /// Refresh backdrop visibility every frame (vanilla `Backdrop.Update`,
    /// `109468-109489`).
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

    /// The room-filtering part of vanilla `Backdrop.IsVisible` (`109438-109466`):
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
    /// Load from the map path resolved through `paths`.
    ///
    /// `tileset` is deprecated (real tilesets come from the definitions converted from
    /// `ForegroundTiles.xml`, keyed by tile id); the parameter is kept for caller
    /// compatibility and ignored.
    pub fn load(map: &str, tileset: Option<&str>, paths: &Paths) -> Result<Self> {
        let _ = tileset; // deprecated, see above
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

        let anims = match crate::sprites::AnimationTable::load(paths, &atlas_name, &atlas) {
            Ok(t) => t,
            Err(e) => {
                warn!(error = %e, "animation table failed to parse; falling back to static frames");
                crate::sprites::AnimationTable::default()
            }
        };
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
            warned: RefCell::default(),
        })
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
            warned: RefCell::default(),
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
        // 11234/11242; nothing in vanilla writes it) -- the previous blue-gray tint was a
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

        debug!(
            draws = out.len(),
            room = %room.name,
            atlas = %self.atlas_name,
            "static draw list rebuilt"
        );
        out
    }

    /// This frame's parallax background/foreground draws (`Parallax.Render`,
    /// `pureCeleste.cs:112163-112236`).
    ///
    /// **Rebuilt every frame** (unlike tiles/entities, which are cached per room): the
    /// tiling origin depends on the camera position
    /// (`p = (Position - Camera.Position.Floor()*Scroll).Floor()`, `112165-112166`), so
    /// any camera movement changes the whole layout. The counts are tiny (4 background
    /// images x a few tiles each), so the rebuild cost is negligible.
    ///
    /// Layers: the background draws after all tiles ([`Layer::Background`] = -1000 <
    /// [`Layer::BgTiles`] = -500), and the foreground sits above the player and effects
    /// ([`Layer::Foreground`] = 500 > [`Layer::Effects`] = 300), matching the front/back
    /// layering of vanilla `Backdrop`.
    pub fn backdrop_draws(&self, gpu: &GpuRenderer, camera_pos: Vec2) -> Vec<SpriteDraw> {
        let vis = self.backdrop_visible.borrow();
        let mut out = Vec::new();
        push_backdrops(
            &mut out,
            &self.doc.style.backgrounds,
            camera_pos,
            Layer::Background.value(),
            &vis,
            self,
            gpu,
        );
        push_backdrops(
            &mut out,
            &self.doc.style.foregrounds,
            camera_pos,
            Layer::Foreground.value(),
            &vis,
            self,
            gpu,
        );
        out
    }

    /// The map style's clear color (`<Style color="#rrggbb">`, `pureCeleste.cs:112434`).
    pub fn clear_color(&self) -> [u8; 3] {
        self.doc.style.background_color
    }

    /// This frame's player draws: hair (a 4-node chain + bangs + black outline) behind
    /// the body.
    ///
    /// Every part is checked against `pureCeleste.cs`:
    /// - Anchor: `Player.Position` = hitbox bottom center (118856), i.e. `anchor`;
    /// - Hair root: `Nodes\[0\] = Sprite.RenderPosition + (0,-9) + HairOffset*Facing`
    ///   (111116). The idle animation does not define HairOffset -> the default (0,0) is
    ///   used (111477 only assigns it when the yaml provides it); RenderPosition is the
    ///   player position;
    /// - Later nodes: `HairCount = 4` (111321); `AfterUpdate` (111118-111135)'s static
    ///   placement node\[i\] = node\[i-1\] + ((0-Facing)*step, +2), where step is
    ///   2*StepInFacingPerSegment = 1 for i==1 and 0.5 otherwise (StepPerSegment
    ///   = (0,2), 111074-111076); the <=3px constraint between nodes does not trigger
    ///   (|(-1,2)| ~ 2.24);
    /// - Drawing (Render 111155+): `origin = (5,5)`; node\[0\] uses bangs\[0\], the
    ///   rest use hair00; each piece first draws 4 black outline copies at +/-1px
    ///   (`Border = Black`, 111159), then draws the base color in reverse order from
    ///   HairCount-1..0 (`Color = AC3232`).
    /// - The body draws last (Layer::Player is above PlayerHair, guaranteed by layer
    ///   sort order).
    ///
    /// Not ported yet: hair strand motion simulation (SimulateMotion/StepApproach,
    /// along with the animation system); the four-direction body outline
    /// (DrawPlayerSpriteOutline, off for idle).
    pub fn player_draws(
        &self,
        gpu: &GpuRenderer,
        pos: Vec2,
        flip_x: bool,
        hair_color: [f32; 4],
        body_sprite: Option<&str>,
        anim_frame: usize,
    ) -> Vec<SpriteDraw> {
        let facing: f32 = if flip_x { -1.0 } else { 1.0 }; // Facings.Left = -1
        let anchor = pos + Vec2::from_f32s(crate::player::PLAYER_W * 0.5, crate::player::PLAYER_H);

        // `AfterUpdate` (111114-111139): node 0 = RenderPosition + (0,-9)
        // + HairOffset*(Facing,1); each later node links along `StepPerSegment (0,2)`.
        let mut nodes = Vec::with_capacity(sprites::HAIR_COUNT);
        let hair_offset = sprites::PLAYER_HAIR_OFFSET;
        nodes.push(anchor + Vec2::from_f32s(hair_offset.0 * facing, -9.0 + hair_offset.1));
        for i in 1..sprites::HAIR_COUNT {
            // Node 1 sticks out 1px more (StepInFacingPerSegment*2 applies only to node 1).
            let step_x = (0.0 - facing) * if i == 1 { 1.0 } else { 0.5 };
            nodes.push(nodes[i - 1] + Vec2::from_f32s(step_x, 2.0));
        }
        // `Render` starts with `Nodes[0] = Nodes[0].Floor()` (111179): the whole hair
        // snaps to integer pixels -- without it the hair root jitters/drifts with the
        // player's subpixel position.
        nodes[0] = nodes[0].floor();

        let pieces: [(&str, &Vec2); sprites::HAIR_COUNT] = [
            (bangs_frame(anim_frame), &nodes[0]),
            (sprites::PLAYER_HAIR_SPRITE, &nodes[1]),
            (sprites::PLAYER_HAIR_SPRITE, &nodes[2]),
            (sprites::PLAYER_HAIR_SPRITE, &nodes[3]),
        ];
        let border = [0.0, 0.0, 0.0, 1.0];
        let outline_offsets = [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)];

        let mut out = Vec::new();
        // First draw all outlines for every node (4 copies at +/-1px), then draw the base
        // color from tail to head (111179-111197) -- two passes, not alternating per node.
        for (i, (name, node)) in pieces.iter().enumerate() {
            let scale = sprites::hair_scale(i, facing);
            let Some(base) = self.hair_piece(gpu, name, **node, scale, border) else {
                continue;
            };
            for (dx, dy) in outline_offsets {
                let mut d = base;
                d.position += Vec2::from_f32s(dx, dy);
                out.push(d);
            }
        }
        for (i, (name, node)) in pieces.iter().enumerate().rev() {
            let scale = sprites::hair_scale(i, facing);
            if let Some(mut d) = self.hair_piece(gpu, name, **node, scale, hair_color) {
                d.layer = Layer::PlayerHair.value() + 1; // base color on top of the outline
                out.push(d);
            }
        }
        if let Some(d) = self.body_draw(gpu, anchor, flip_x, body_sprite) {
            out.push(d);
        }
        out
    }

    /// One hair piece: top-left = `node - origin within the sub-image * scale`
    /// (Monocle `MTexture.Draw(position, origin, color, scale)` -> `14534-14537` hands
    /// `origin` to SpriteBatch; the scale acts around the origin).
    ///
    /// The origin must be computed **once per image**: `hair00` has `(1,1)` cropped away
    /// in `.meta` and `bangs00` has `(1,0)` cropped away, and although both share the
    /// frame origin (5,5) (`HAIR_FRAME_ORIGIN`), it lands differently inside the cropped
    /// sub-image.
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
        gpu: &GpuRenderer,
        anchor: Vec2,
        flip_x: bool,
        body_sprite_name: Option<&str>,
    ) -> Option<SpriteDraw> {
        let (group, sprite) = self
            .atlas
            .sprite(body_sprite_name.unwrap_or(sprites::PLAYER_SPRITE))?;
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

    /// The entity batch. Anchors follow **vanilla semantics**:
    /// - Default (decal-like): the content **center** is placed at the entity position
    ///   (`MTexture.Draw(position)`);
    /// - `introCar`: two images, body and wheels, with the **bottom center** at the
    ///   position (`pureCeleste.cs` IntroCar: `Origin = (W/2, H)`, the wheels drawn
    ///   under the body);
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
                // Update, and the `has` list is empty at spawn (`pureCeleste.cs:110944`,
                // `111012-111035`): at scene t=0 it draws nothing. The engine has no such
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
    /// (`pureCeleste.cs:106325`; GenerateBox itself is at `87832`: fill the whole box
    /// with the id, treat outside the box as empty per the default `Behaviour` --
    /// `CheckTile`'s `mapData == null` branch returns `EdgesExtend` = false,
    /// `87985-87989`).
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
    /// rotated by its angle (`pureCeleste.cs:106699-106710`). Control point = the
    /// midpoint of the two ends sagged 24px (`106702`);
    /// `GetPoint(t) = (1-t)^2*Begin + 2(1-t)t*Control + t^2*End` (Everest
    /// `Patches/Monocle/SimpleCurve.cs:22-28`). Line color `595866` (`106676`); from/to
    /// = entity and node coordinates + room offset (`106698`).
    ///
    /// Static deviation: vanilla also adds wind sway to the control point,
    /// `sin(sine + WindSineTimer*omega) * 8 * VisualWind` (`106701`) -- the phase comes
    /// from a per-line random stream seeded by `new Random((int)min.X)`, and the
    /// amplitude animates with `WindSineTimer` (`115689-115690`; at t=0
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

        // Calc.HexToColor("595866") (106676).
        let color = [89.0 / 255.0, 88.0 / 255.0, 102.0 / 255.0, 1.0];
        let layer = if matches!(e.properties.get("above"), Some(FieldValue::Bool(true))) {
            LAYER_WIRE_ABOVE
        } else {
            LAYER_WIRE
        };

        // 16 segments (106703-106709): each is a 1px-tall bar rotated from its start to
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
    /// `BridgeTile` per cell along its width (`pureCeleste.cs:107704-107731`), each cell
    /// being one vertical strip of the `scenery/bridge` texture (`107846-107878`).
    ///
    /// - Gap = the room x of the two nodes plus the room's left edge (vanilla
    ///   `gapStartX += level.Bounds.Left`, `107713-107714`; nodes and entity share the
    ///   room coordinate system).
    /// - The planking cycles through 11 sizes (`107719-107729`): for index `2..=7`
    ///   vanilla takes a random wood-grain strip via `Calc.Random.Next(6)` (seeded by
    ///   `PushRandom(1)`), while the engine uses the index `num` directly -- those 6
    ///   candidates are all 8px wide, so the stepping matches vanilla cell for cell and
    ///   only the pattern of the 8px strips becomes a fixed sequence (that random stream
    ///   is not reproduced).
    /// - A 16px end cap is cut vertically into 24+12+12+..., each strip offset in Y by
    ///   `num2 - 8` (`107854-107872`), with the remainder trimmed to 52px tall.
    /// - Vanilla `Update` plays a collapse once the player has crossed the bridge
    ///   (`107733+`); static rendering draws the complete bridge.
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
    fn warn_once(&self, key: &str) -> bool {
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

/// Vanilla's starting room and spawn point, each step with provenance:
///
/// 1. **Starting room**: `pureCeleste.cs:112564` `MapData.StartLevel() =>
///    GetAt(Vector2.Zero)` -- the first room containing the **world origin (0,0)**
///    (`GetAt`, 112569, tests Bounds containment in order and returns null when nothing
///    matches; here it falls back to the first room and warns, an engine robustness
///    difference).
/// 2. **Spawn point**: `115101` `Session.RespawnPoint = DefaultSpawnPoint`, `114080`
///    `DefaultSpawnPoint => Spawns.ClosestTo((Bounds.Left, Bounds.Bottom))` (`101280`
///    `GetSpawnPoint`) -- the spawn point closest to the room's **bottom-left corner**.
/// 3. **Anchor semantics**: `115105` `new Player(Session.RespawnPoint.Value, ...)` --
///    the spawn point is the hitbox **bottom center** (feet, see
///    `player::PLAYER_H_PX`).
///
/// Returns `(anchor, room name)`; the service converts the anchor to a hitbox top-left.
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
            warned: RefCell::default(),
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
    /// `pureCeleste.cs:109468-109489`; `IsVisible`'s `OnlyIn` / `ExcludeFrom` branches,
    /// 109456-109465).
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
        // during a transition and leaves the rest untouched, 109470-109483).
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
    /// 112185-112234.
    #[test]
    fn parallax_loop_x_without_loop_y_tiles_one_row() {
        let bd = parallax_bd(-24.0, -64.0, (0.15, 0.05), (true, false));
        let tiles = parallax_tiles(&bd, (176.9, 92.2), (320.0, 137.0), (320.0, 180.0));

        // Floor(cam) = (176, 92) -> px = Floor(-24 - 26.4) = -51 and
        // py = Floor(-64 - 4.6) = -69; the loopx fold is vanilla's two while loops
        // (first <0 add, then >0 subtract, 112185-112195) -> -51 is returned unchanged.
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
        let tiles = parallax_tiles(&bd, (320.0, 180.0), (160.0, 90.0), (320.0, 180.0));

        // Floor(0 - 160*0.5) = -160 -> folds back to 0; likewise for y -> the grid
        // starts at (0,0)/(160,90).
        assert_eq!(
            tiles,
            vec![(0.0, 0.0), (0.0, 90.0), (160.0, 0.0), (160.0, 90.0)]
        );
        // Each piece is 160x90 -> 2x2 exactly covers 320x180.
        assert!(tiles.iter().all(|(x, y)| *x < 320.0 && *y < 180.0));
    }

    /// The `for` condition comes first (`112220`/`112222`): when the origin is
    /// off-screen that axis draws nothing; when it is on-screen and neither axis loops,
    /// exactly one piece is drawn.
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

    /// Color: all-zero = vanilla's default white (`112789`/`effective_color`); the
    /// `alpha` attribute folds into Color (`112798-112804` `Color *= alpha`).
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

    /// The deck's cell stepping must match vanilla (`107719-107729`): each cell is 8px
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
    /// both flatten to y = -8 (`BridgeTile`, `107854-107878`).
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
}
