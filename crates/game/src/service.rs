//! Game service: fixed-step advance of the player + plugins; the render step feeds snapshots to wgpu.
//!
//! It is the only bridge between the window host and [`releste_kernel`] (AGENTS.md 4.1:
//! the kernel only knows `Service`). Maps are data, editing a map does not restart (4.3).

use std::time::Instant;

use releste_kernel::{Context, FrameId, Service, ServiceId};
use releste_math::{Fx, Vec2};
use releste_plugin_host::PluginHost;
use releste_reload::FileWatcher;
use releste_render::{Camera, GpuRenderer, SpriteBatch, SpriteDraw, BASE_HEIGHT, BASE_WIDTH};
use tracing::{debug, info, warn};

use crate::paths::Paths;
use crate::player::{Input, Player, PLAYER_H, PLAYER_H_PX, PLAYER_W, PLAYER_W_PX};
use crate::session::Session;

/// Debounce window for `FileWatcher`, ROADMAP step 10: 100ms.
const WATCH_DEBOUNCE_MS: u64 = 100;
/// Camera follow coefficient (chases 1/4 of the target each tick).
/// Vanilla camera follow coefficient (`Player.Update` 120137-120145):
/// `pos += (target - pos) * (1 - 0.01^dt)`; with dt = 1/60 the coefficient is ~0.0739,
/// taking 19/256 = 0.0742 (the closest rational within the Fx 1/256 step).
const CAMERA_LERP: Fx = Fx::from_ratio(19, 256);

/// Room transition duration 0.65s = 39 ticks (`Level.NextTransitionDuration`,
/// pureCeleste.cs:113847, Reset 115428).
const TRANSITION_TICKS: u32 = 39;

/// In-progress room transition (an explicit state version of the vanilla `Level.transition` coroutine).
#[derive(Debug, Clone, Copy)]
struct RoomTransition {
    /// Glide endpoint for the player hitbox top-left (vanilla `playerTo`).
    player_to: Vec2,
    /// Camera start point (viewport center, vanilla `cameraFrom`).
    camera_from: Vec2,
    /// Camera end point (the clamped target in the new room, vanilla `cameraTo`).
    camera_to: Vec2,
    /// Ticks elapsed (vanilla `cameraAt`, approaches 1 by dt/0.65).
    ticks: u32,
}

/// Plugin boot: scan and load every plugin under `plugins/`.
///
/// Any failure only `warn`s and the game keeps running (ROADMAP 3.6: `rm -rf plugins/`
/// must still start). A missing directory is warned about by `scan` itself.
pub fn boot_plugins(paths: &Paths) -> Option<PluginHost> {
    let mut host = PluginHost::new(paths.plugins());
    match host.scan() {
        Ok(count) => {
            info!(dir = %paths.plugins().display(), count, "plugin directory scanned");
        }
        Err(e) => {
            warn!(error = %e, "plugin scan failed; continuing without plugins");
            return None;
        }
    }

    let names: Vec<String> = host.scanned().map(str::to_string).collect();
    if names.is_empty() {
        return Some(host);
    }
    for name in names {
        match host.load(&name) {
            Ok(()) => info!(plugin = %name, "plugin loaded"),
            Err(e) => warn!(plugin = %name, error = %e, "plugin failed to load; skipping"),
        }
    }
    Some(host)
}

/// Open the `.map` watcher. Failure just means no hot reload, not a fatal error.
pub fn watch_maps(paths: &Paths) -> Option<FileWatcher> {
    match FileWatcher::new(paths.watch_dir(), WATCH_DEBOUNCE_MS) {
        Ok(w) => {
            info!(dir = %paths.watch_dir().display(), "watching maps for changes");
            Some(w)
        }
        Err(e) => {
            warn!(error = %e, "map hot-reload disabled");
            None
        }
    }
}

/// The main game service.
pub struct GameService {
    /// Current map + atlas.
    pub session: Session,
    /// Player state (hot reload must preserve it, 4.3).
    pub player: Player,
    /// Input for this tick.
    pub input: Input,
    /// Paused (Esc menu).
    pub paused: bool,
    /// Camera (at render time interpolate from `prev` -> `cur`).
    camera: Camera,
    prev_camera: Camera,
    host: Option<PluginHost>,
    watcher: Option<FileWatcher>,
    static_draws: Vec<SpriteDraw>,
    /// The **previous room** static batch kept during a transition (vanilla calls
    /// `UnloadEntities` on the old-room entities only when the transition ends, 115415/115432 -- while the
    /// screen transition is running the old room's tiles/entities must keep rendering, they must not vanish early).
    transition_statics: Option<Vec<SpriteDraw>>,
    static_dirty: bool,
    /// Room the static batch currently corresponds to (rebuild on room change -- vanilla draws only one room at a time).
    built_room: String,
    /// Current room (vanilla `Session.Level` semantics: it switches to the new room as the transition starts,
    /// and camera clamping, room filtering and EnforceBounds all follow it).
    current_room: String,
    /// In-progress room transition; while `Some`, entities are frozen and only the player glide and camera pan run
    /// (vanilla `Level.Update`'s Transitioning branch, 115659-115668).
    transition: Option<RoomTransition>,
    overflow_warned: bool,
    reloads: u64,
    /// Last reported room name (under `-v` a room switch logs a line only when this changes).
    reported_room: String,
}

impl GameService {
    /// Build the service. With `gpu` as `None` (headless) no static batch is built.
    pub fn new(
        session: Session,
        host: Option<PluginHost>,
        watcher: Option<FileWatcher>,
        gpu: Option<&GpuRenderer>,
    ) -> Self {
        // Vanilla: `new Player(Session.RespawnPoint.Value, ...)` (115105),
        // Position = spawn anchor = hitbox bottom-center; our Player stores the hitbox
        // top-left -> top-left = anchor - (W/2, H) (hitbox 8x11, offset (-4,-11),
        // 118856).
        let player = Player::new(session.spawn - Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H));
        let spawn_room = session.spawn_room.clone();
        let current_room = spawn_room.clone();
        // The initial camera placement is already clamped per vanilla (see `update_camera`).
        let anchor = player.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H);

        let (session, static_draws, built_room) = match gpu {
            Some(gpu) => {
                let mut session = session;
                let idx = session
                    .doc
                    .rooms
                    .iter()
                    .position(|r| r.name == spawn_room)
                    .unwrap_or(0);
                let draws = session.build_static(gpu, idx);
                let built = session.doc.rooms[idx].name.clone();
                (session, draws, built)
            }
            None => (session, Vec::new(), spawn_room.clone()),
        };
        // Clamp the camera into the spawn room (vanilla: the camera is always inside the room bounds).
        let camera = Camera::at(Self::vanilla_camera_center(&session, anchor, &spawn_room));

        GameService {
            session,
            player,
            input: Input::default(),
            paused: false,
            camera,
            prev_camera: camera,
            host,
            watcher,
            static_draws,
            transition_statics: None,
            static_dirty: gpu.is_none(),
            built_room,
            current_room,
            transition: None,
            overflow_warned: false,
            reloads: 0,
            reported_room: spawn_room,
        }
    }

    /// Vanilla camera center: `Player.CameraTarget` is the camera **center** of `Position + (-160,-90)`
    /// clamped into `[Left+160, Right-160] x [Top+90, Bottom-90]`
    /// (equivalent to vanilla clamping the camera top-left into `[Left, Top, Right-320, Bottom-180]`
    /// and adding half a screen back, `pureCeleste.cs:119108-119111`).
    ///
    /// The clamp uses XNA `MathHelper.Clamp(v, min, max)` per-value semantics -- when max <
    /// min (room narrower/shorter than the view), the center is **not** used: a value left of center gives min, right gives max.
    ///
    /// `anchor` is the player's **bottom-center** (vanilla Position semantics), `room_name`
    /// is the target room. The engine `Camera.position` is the viewport center.
    fn vanilla_camera_center(session: &Session, anchor: Vec2, room_name: &str) -> Vec2 {
        let Some(room) = session.doc.rooms.iter().find(|r| r.name == room_name) else {
            return anchor;
        };
        let b = &room.bounds;
        // XNA `MathHelper.Clamp`: check the lower bound first, then the upper (when max < min the value lands on a side).
        let clamp_axis = |v: Fx, lo: f32, size: f32, view: f32| -> Fx {
            let min = Fx::from_f32(lo + view * 0.5);
            let max = Fx::from_f32(lo + size - view * 0.5);
            if v < min {
                min
            } else if v > max {
                max
            } else {
                v
            }
        };
        Vec2::new(
            clamp_axis(anchor.x, b.x, b.w, BASE_WIDTH),
            clamp_axis(anchor.y, b.y, b.h, BASE_HEIGHT),
        )
    }

    /// Total hot reloads.
    pub fn reloads(&self) -> u64 {
        self.reloads
    }

    /// How many sprites are drawn in the static batch.
    pub fn static_draw_count(&self) -> usize {
        self.static_draws.len()
    }

    /// Current room name.
    pub fn room_name(&self) -> &str {
        &self.current_room
    }

    /// Number of loaded plugins.
    pub fn plugin_count(&self) -> usize {
        self.host.as_ref().map(|h| h.loaded().count()).unwrap_or(0)
    }

    /// Render step: (re)build the static batch, fill `batch`, sort.
    ///
    /// `alpha` in [0, 1] is the interpolation progress from the previous tick to this one; the player position is
    /// linearly interpolated `prev_pos` -> `pos` from it, avoiding 60Hz simulation jitter on a 144Hz screen.
    pub fn render(&mut self, gpu: &mut GpuRenderer, alpha: f32, batch: &mut SpriteBatch) {
        // The clear color comes from the map style (`<Style color=...>` -> `MapStyle::background_color`).
        gpu.set_clear_color(self.session.clear_color());

        // Vanilla draws only one room at a time: when a transition starts (`Session.Level` already switched to the new room)
        // or on hot reload, the static batch is rebuilt from the current room.
        let room_name = self.current_room.clone();
        if self.static_dirty || self.built_room != room_name {
            let idx = self
                .session
                .doc
                .rooms
                .iter()
                .position(|r| r.name == room_name)
                .unwrap_or(0);
            self.static_draws = self.session.build_static(gpu, idx);
            self.built_room = room_name;
            self.static_dirty = false;
        }

        batch.begin();
        if let Some(prev) = &self.transition_statics {
            for draw in prev {
                if batch.push(*draw).is_err() {
                    if !self.overflow_warned {
                        self.overflow_warned = true;
                        warn!(
                            capacity = gpu.vertex_capacity(),
                            "sprite batch full during transition; previous-room draws dropped"
                        );
                    }
                    break;
                }
            }
        }
        for draw in &self.static_draws {
            if batch.push(*draw).is_err() {
                if !self.overflow_warned {
                    self.overflow_warned = true;
                    warn!(
                        capacity = gpu.vertex_capacity(),
                        "sprite batch full; remaining draws are dropped"
                    );
                }
                break;
            }
        }
        // Parallax backgrounds/foregrounds depend on the camera position and are rebuilt each frame from the interpolated camera
        // (`Parallax.Render`, `pureCeleste.cs:112163-112236`).
        for draw in self.session.backdrop_draws(gpu, self.camera_pos(alpha)) {
            if batch.push(draw).is_err() && !self.overflow_warned {
                self.overflow_warned = true;
                warn!("sprite batch full; backdrop draw dropped");
            }
        }
        for draw in self.player_draws(gpu, alpha) {
            if batch.push(draw).is_err() && !self.overflow_warned {
                self.overflow_warned = true;
                warn!("sprite batch full; player draw dropped");
            }
        }
        batch.sort();
    }

    /// This frame's player draws (hair + body, position interpolated by alpha).
    fn player_draws(&self, gpu: &GpuRenderer, alpha: f32) -> Vec<SpriteDraw> {
        let a = Fx::from_f32(alpha.clamp(0.0, 1.0));
        let pos = self.player.prev_pos + (self.player.pos - self.player.prev_pos) * a;
        self.session.player_draws(
            gpu,
            pos,
            self.player.facing < 0,
            self.player.hair_color(),
            self.player
                .anim_frame_texture(&self.session.anims)
                .as_deref(),
            self.player.anim_frame,
        )
    }

    /// Interpolated camera position (viewport **center**).
    fn camera_pos(&self, alpha: f32) -> Vec2 {
        let a = Fx::from_f32(alpha.clamp(0.0, 1.0));
        self.prev_camera.position + (self.camera.position - self.prev_camera.position) * a
    }

    /// Interpolated camera (for rendering).
    pub fn render_camera(&self, alpha: f32) -> Camera {
        Camera::at(self.camera_pos(alpha))
    }

    /// Poll for `.map` changes; on success swap the document and rebuild the static batch.
    ///
    /// Player position/velocity/facing are **not reset** (AGENTS.md 4.3).
    fn poll_reload(&mut self) {
        let Some(watcher) = self.watcher.as_mut() else {
            return;
        };
        let Some(path) = watcher.poll() else {
            return;
        };

        let started = Instant::now();
        let doc = match releste_map::load_map(&path) {
            Ok(doc) => doc,
            Err(e) => {
                warn!(path = %path.display(), error = %e, "reload failed; keeping the current map");
                return;
            }
        };

        let patches = releste_map::diff(&self.session.doc, &doc);
        if patches.is_empty() {
            debug!(path = %path.display(), "reload produced no changes");
            return;
        }

        self.session.doc = doc;
        self.static_dirty = true;
        self.reloads += 1;
        info!(
            path = %path.display(),
            patches = patches.len(),
            ms = started.elapsed().as_millis() as u64,
            "map reloaded"
        );

        // `room_at` takes the hitbox top-left (it derives the center internally); here the same
        // center is used for the contains check, ensuring the "fall back + warn" decision uses the same point.
        let room = self.session.room_at(self.player.pos);
        let centre = self.player.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H * 0.5);
        if !room.bounds.to_rect().contains(centre) {
            warn!(
                room = %room.name,
                "the player is outside every room after the reload; it was left untouched (AGENTS.md 4.3)"
            );
        }
    }

    /// One simulation tick: during a transition only advance the coroutine; otherwise physics -> camera -> transition check ->
    /// backdrop visibility refresh.
    fn simulate_tick(&mut self) {
        // Vanilla `Celeste.Freeze(0.05f)` (dash startup, 122540): the whole scene freezes for
        // 0.05s = 3 ticks (`Engine.FreezeTimer`, Update does not run meanwhile).
        if self.player.freeze_ticks > 0 {
            self.player.freeze_ticks -= 1;
            return;
        }
        // Entities are frozen during a transition (vanilla `Level.Update`'s Transitioning branch,
        // 115659-115668 runs only the transition coroutine), only the player glide and camera pan run.
        if self.transition.is_some() {
            self.advance_transition();
        } else {
            self.player.update(&self.input, &self.session);
            // UpdateSprite + Sprite.Update (120068/119824: per-frame animation advance).
            self.player
                .update_visuals(&self.session.anims, &self.input, &self.session.doc.rooms);
            self.update_camera();
            // EnforceBounds: transition triggering/bounds clamping, after physics (120168).
            self.check_room_transition();
        }

        // Backdrop visibility refreshes every tick (the cadence of vanilla `Backdrop.Update`,
        // 109468-109489); room filtering uses the current room name (`Session.Level` semantics),
        // and during a transition it is kept or lit per the InstantIn/InstantOut rules.
        let transitioning = self.transition.is_some();
        let room = self.current_room.clone();
        self.session
            .update_backdrop_visibility(&room, transitioning);

        // Room identity changed: at `-v` and above log a line to debug "which room the player is actually standing in".
        if room != self.reported_room {
            debug!(
                from = %self.reported_room,
                to = %room,
                x = self.player.pos.x.to_f32(),
                y = self.player.pos.y.to_f32(),
                "room changed"
            );
            self.reported_room = room;
        }
    }

    /// Regular-tick camera follow (vanilla `Player.Update` 120133-120146):
    /// Target = the player anchor clamped inside the current room, approached exponentially.
    fn update_camera(&mut self) {
        // Player anchor = hitbox bottom-center (vanilla Position semantics).
        let anchor = self.player.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H);
        let target = Self::vanilla_camera_center(&self.session, anchor, &self.current_room);
        self.prev_camera = self.camera;
        let delta = target - self.camera.position;
        self.camera.position += delta * CAMERA_LERP;
    }

    /// Transition triggering and bounds clamping from vanilla `Level.EnforceBounds`
    /// (pureCeleste.cs:116517-116607). When the player hitbox crosses the current room bounds and the probe point lands in another
    /// room -> start a transition; otherwise clamp per vanilla (hard clamp left/right, 24px margin on top).
    ///
    /// When both sides are out of bounds at once only one triggers, per the vanilla if/else priority: left -> right -> top -> bottom.
    fn check_room_transition(&mut self) {
        let Some(room) = self
            .session
            .doc
            .rooms
            .iter()
            .find(|r| r.name == self.current_room)
        else {
            return;
        };
        let b = room.bounds;
        let (left, top, right, bottom) = (b.x, b.y, b.x + b.w, b.y + b.h);
        let pos = self.player.pos;
        let p_left = pos.x.to_f32();
        let p_top = pos.y.to_f32();
        let p_right = (pos.x + Fx::from_int(PLAYER_W_PX)).to_f32();
        let p_bottom = (pos.y + Fx::from_int(PLAYER_H_PX)).to_f32();
        let center = self.player.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H * 0.5);

        if p_left < left && p_top >= top && p_bottom < bottom {
            let probe = center + Vec2::from_f32s(-8.0, 0.0);
            match self.room_at_point(probe) {
                Some(next) => self.start_transition(next, (-1.0, 0.0)),
                None => {
                    // Vanilla clamp: player.Left = bounds.Left
                    self.player.pos.x = Fx::from_f32(left);
                    self.player.vel.x = Fx::zero();
                }
            }
        } else if p_right > right && p_top >= top && p_bottom < bottom {
            let probe = center + Vec2::from_f32s(8.0, 0.0);
            match self.room_at_point(probe) {
                Some(next) => self.start_transition(next, (1.0, 0.0)),
                None => {
                    self.player.pos.x = Fx::from_f32(right - PLAYER_W);
                    self.player.vel.x = Fx::zero();
                }
            }
        }

        // Top: the center crosses the room top edge (119560-119577); with no target room clamp to
        // 24px above the top (letting the player peek upward).
        if center.y.to_f32() < top {
            let probe = center + Vec2::from_f32s(0.0, -12.0);
            match self.room_at_point(probe) {
                Some(next) => {
                    self.player.before_up_transition();
                    self.start_transition(next, (0.0, -1.0));
                }
                None => {
                    if p_top < top - 24.0 {
                        self.player.pos.y = Fx::from_f32(top - 24.0);
                    }
                }
            }
        } else if p_bottom > bottom {
            // Bottom: transition only when nothing solid is 4px below the feet (116589-116598); vanilla dies when
            // out of bounds at the bottom without a transition (116599-116606); death is not implemented yet, so world clamping is kept.
            let probe = center + Vec2::from_f32s(0.0, 12.0);
            let grounded_below =
                self.player
                    .hits(&self.session.doc.rooms, pos.x, pos.y + Fx::from_int(4));
            match self.room_at_point(probe) {
                Some(next) if !grounded_below => {
                    self.player.before_down_transition();
                    self.start_transition(next, (0.0, 1.0));
                }
                _ => {}
            }
        }
    }

    /// The room a point is in (vanilla `MapData.GetAt`; no fallback).
    fn room_at_point(&self, point: Vec2) -> Option<String> {
        self.session
            .doc
            .rooms
            .iter()
            .find(|r| r.bounds.to_rect().contains(point))
            .map(|r| r.name.clone())
    }

    /// Start a transition (the preparation part of vanilla `Level.TransitionRoutine` 115282-115423):
    /// `Session.Level` switches to the new room immediately (the world/static batch is then rebuilt from the new room),
    /// and the player glide endpoint and camera start/end points are computed.
    fn start_transition(&mut self, next_room: String, direction: (f32, f32)) {
        let Some(next) = self
            .session
            .doc
            .rooms
            .iter()
            .find(|r| r.name == next_room)
            .cloned()
        else {
            return;
        };
        let nb = next.bounds;
        let dir = Vec2::from_f32s(direction.0, direction.1);
        // dirPad: direction * 4, for straight down it is * 12 (115345-115349).
        let dir_pad = if direction == (0.0, 1.0) {
            dir * Fx::from_int(12)
        } else {
            dir * Fx::from_int(4)
        };

        // playerTo: in the player **bottom-center** space (vanilla Position semantics).
        let (dir_x, _) = direction;
        let mut player_to = self.player.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H);
        if dir_x != 0.0 {
            // A horizontal transition first pulls the endpoint out of the floor (115351-115355).
            let nb_bottom = nb.y + nb.h;
            while player_to.y.to_f32() >= nb_bottom {
                player_to.y -= Fx::one();
            }
        }
        // Keep gliding along the direction until it lands in the new room (the padded check with dirPad,
        // `IsInBounds(Vector2, Vector2)` 116641-116657).
        let mut guard = 0u32;
        while !Self::in_bounds_padded(player_to, dir_pad, &nb) {
            player_to += dir;
            guard += 1;
            if guard > 4096 {
                warn!(
                    from = %self.current_room,
                    to = %next_room,
                    "transition target never entered the next room; clamping the glide"
                );
                break;
            }
        }

        let camera_from = self.camera.position;
        let camera_to = Self::vanilla_camera_center(&self.session, player_to, &next_room);
        info!(
            from = %self.current_room,
            to = %next_room,
            "room transition"
        );
        // Vanilla unloads the old room's entities only when the transition ends (`UnloadEntities(toRemove)`,
        // 115415; `UnloadEntities` 115432) -- the old room keeps rendering while the animation runs.
        self.transition_statics = Some(std::mem::take(&mut self.static_draws));
        self.current_room = next_room;
        // hitbox top-left = bottom-center - (W/2, H).
        let player_to = player_to - Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H);
        self.transition = Some(RoomTransition {
            player_to,
            camera_from,
            camera_to,
            ticks: 0,
        });
    }

    /// Vanilla `IsInBounds(Vector2, Vector2 dirPad)` (116641-116657):
    /// whether the room rect contains the point after inset by the positive components of `dirPad` (right/bottom open intervals).
    fn in_bounds_padded(p: Vec2, dir_pad: Vec2, b: &releste_map::RectDef) -> bool {
        let zero = Fx::zero();
        let pad_l = dir_pad.x.max(zero);
        let pad_r = (-dir_pad.x).max(zero);
        let pad_t = dir_pad.y.max(zero);
        let pad_b = (-dir_pad.y).max(zero);
        let f = Fx::from_f32;
        let (l, t, r, bot) = (f(b.x), f(b.y), f(b.x + b.w), f(b.y + b.h));
        p.x >= l + pad_l && p.y >= t + pad_t && p.x < r - pad_r && p.y < bot - pad_b
    }

    /// Advance a transition (the per-frame part of vanilla `TransitionRoutine` + `Player.TransitionTo`
    /// 120551-120566): the player glides toward the endpoint at 60 px/s (no collision, X first then Y),
    /// and the camera eases with CubeOut over 0.65s, snapping directly when > 0.9.
    ///
    /// All entities freeze during a transition (`Level.Update`'s Transitioning branch runs only
    /// the transition coroutine) and input is ignored -- matching vanilla.
    fn advance_transition(&mut self) {
        let Some(t) = self.transition.as_mut() else {
            return;
        };
        self.player.prev_pos = self.player.pos;
        // MoveTowardsX / MoveTowardsY: at most 1 px/tick per axis (60 px/s).
        let glide = |v: &mut Fx, to: Fx| {
            if *v < to {
                *v = (*v + Fx::one()).min(to);
            } else {
                *v = (*v - Fx::one()).max(to);
            }
        };
        glide(&mut self.player.pos.x, t.player_to.x);
        glide(&mut self.player.pos.y, t.player_to.y);
        let reached = self.player.pos == t.player_to;

        // cameraAt = Approach(cameraAt, 1, dt / 0.65) (115366).
        // Easing is a render-side presentation (allowed by AGENTS.md 15.7); compute it in f32 then convert back to Fx.
        t.ticks += 1;
        let at = (t.ticks as f32 / TRANSITION_TICKS as f32).min(1.0);
        let eased = 1.0 - (1.0 - at).powi(3); // Ease.CubeOut
        let eased_fx = Fx::from_f32(eased);
        let snap = eased > 0.9;
        self.prev_camera = self.camera;
        self.camera.position = if snap {
            t.camera_to
        } else {
            t.camera_from + (t.camera_to - t.camera_from) * eased_fx
        };

        if reached && t.ticks >= TRANSITION_TICKS {
            self.player.prev_pos = self.player.pos;
            self.prev_camera = self.camera;
            self.transition = None;
            // Transition ends: the old-room entities are unloaded only here (the camera is now entirely in the new room, so
            // dropping the old batch is invisible, just like vanilla UnloadEntities).
            self.transition_statics = None;
        }
    }
}

impl Service for GameService {
    fn id(&self) -> ServiceId {
        ServiceId::new("game")
    }

    fn attach(&mut self, _ctx: &mut Context) {
        info!(
            room = %self.session.spawn_room,
            plugins = self.plugin_count(),
            "game service attached"
        );
    }

    fn update(&mut self, _ctx: &mut Context, frame: FrameId, _dt: Fx) {
        self.poll_reload();

        if let Some(host) = self.host.as_mut() {
            host.tick(frame.get());
        }

        if self.paused {
            return;
        }
        self.simulate_tick();
    }

    fn dispose(&mut self) {
        if let Some(host) = self.host.as_mut() {
            host.shutdown();
            info!("plugins unloaded");
        }
        debug!(reloads = self.reloads, "game service disposed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;
    use releste_map::{RectDef, Tileset};
    use releste_world::EntityData;

    /// Self-contained test map: one room + one player spawn point.
    ///
    /// The real `assets/` is gitignored (Appendix C rule 6), and the nix/CI build
    /// sources do not have it -- tests must build their own data instead of depending on conversion output in the dev tree.
    fn fixture_doc() -> releste_map::MapDocument {
        let mut tiles = Tileset::new("Test", 8, 4);
        for x in 0..8 {
            tiles.set(x, 3, 1); // one row of floor
        }
        let player = EntityData::new("player", releste_world::EntityId::new(1), 16.0, 16.0);
        releste_map::MapDocument {
            area: "test".into(),
            rooms: vec![releste_map::Room {
                name: "lvl_t".into(),
                bounds: RectDef {
                    x: 0.0,
                    y: 0.0,
                    w: 64.0,
                    h: 32.0,
                },
                tiles,
                bg: Tileset::new("Test", 8, 4),
                entities: vec![player],
                triggers: Vec::new(),
                script: None,
                ..Default::default()
            }],
            sync_rules: Vec::new(),
            ..Default::default()
        }
    }

    /// Build a minimal resource root: `maps/<map>` + `assets/atlas/{Test.atlas,tilesets.toml}`.
    /// Atlas with zero textures, tileset with zero definitions -- `Session::load` only requires that the files parse.
    fn fixture_root() -> anyhow::Result<Paths> {
        let root = std::env::temp_dir().join("releste-game-hot-reload");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("maps"))?;
        let atlas_dir = root.join("assets/atlas");
        std::fs::create_dir_all(&atlas_dir)?;
        std::fs::write(
            atlas_dir.join("Test.atlas"),
            "name = \"Test\"\ntextures = []\n",
        )?;
        std::fs::write(atlas_dir.join("tilesets.toml"), "tileset = {}\n")?;
        releste_map::save_map(&fixture_doc(), &root.join("maps/reload-fixture.map"))?;
        Ok(Paths::resolve(Some(&root)))
    }

    /// Hot reload: change one tile -> the document is swapped, the count goes +1, and the player stays put (4.3).
    ///
    /// The map and atlas are fully synthesized (see `fixture_root`), the debounce is 100ms,
    /// so after writing we wait a while before polling.
    #[test]
    fn map_hot_reload_swaps_the_doc_and_keeps_the_player() {
        let paths = fixture_root().expect("fixture root");
        let map_path = paths.map("reload-fixture.map");

        let session = Session::load("reload-fixture.map", None, &paths)
            .expect("session from the fixture map");
        let spawn_before = session.spawn;
        let watcher = watch_maps(&paths).expect("watcher over the maps dir");
        let mut service = GameService::new(session, None, Some(watcher), None);

        // Change one tile and save -- this is the real change hot reload must pick up.
        let mut doc = releste_map::load_map(&map_path).expect("reload the fixture");
        let (room, tx, ty) = {
            let room = &mut doc.rooms[0];
            room.tiles.set(1, 1, 1);
            (room.name.clone(), 1, 1)
        };
        let before = doc.rooms[0].tiles.get(tx, ty);
        assert_eq!(before, Some(1), "the edit landed in the doc");
        releste_map::save_map(&doc, &map_path).expect("save the edited map");

        // The debounce starts from the moment the event is **seen**. Event delivery is the background thread's job
        // and may arrive late under parallel test load, so we cannot hard-code "poll twice" --
        // poll repeatedly until the deadline and pass as soon as a reload happens (exactly once).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while service.reloads() == 0 && std::time::Instant::now() < deadline {
            service.poll_reload();
            if service.reloads() == 0 {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
        service.poll_reload(); // Wrap up: if the window ripened right between two polls, absorb it once more

        assert_eq!(service.reloads(), 1, "one debounced change = one reload");
        assert_eq!(
            service.session.doc.rooms[0].tiles.get(tx, ty),
            Some(1),
            "room {room} picked up the new tile"
        );
        assert_eq!(
            service.player.pos,
            spawn_before - Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H),
            "the player was left untouched by the reload (AGENTS.md 4.3); \
             the spawn anchor converts to a hitbox top-left per pureCeleste 115105/118856"
        );

        let _ = std::fs::remove_dir_all(paths.root());
    }

    /// The minimal transition map of two edge-adjacent rooms (A x[0,320), B x[320,640), same floor height):
    /// the player walks right off A's right edge and must trigger a vanilla transition.
    fn seam_map() -> releste_map::MapDocument {
        let mut tiles = Tileset::new("Test", 40, 23);
        for x in 0..40 {
            tiles.set(x, 22, 1); // one row of floor (y 176..184)
        }
        let player = EntityData::new("player", releste_world::EntityId::new(1), 16.0, 176.0);
        let room = |name: &str, x: f32| releste_map::Room {
            name: name.into(),
            bounds: RectDef {
                x,
                y: 0.0,
                w: 320.0,
                h: 184.0,
            },
            tiles: tiles.clone(),
            bg: Tileset::new("Test", 40, 23),
            entities: vec![player.clone()],
            triggers: Vec::new(),
            script: None,
            ..Default::default()
        };
        releste_map::MapDocument {
            area: "test".into(),
            rooms: vec![room("lvl_a", 0.0), room("lvl_b", 320.0)],
            sync_rules: Vec::new(),
            ..Default::default()
        }
    }

    /// Walking off the room's right edge -> vanilla transition: the player freezes and glides into the new room, the camera pans with CubeOut
    /// for 0.65s, and afterwards the player lands on the new room's floor and the room identity has switched.
    #[test]
    fn walking_off_the_seam_starts_a_vanilla_transition() {
        let doc = seam_map();
        let session = Session::from_doc(doc, "test");
        let mut service = GameService::new(session, None, None, None);
        assert_eq!(service.current_room, "lvl_a");
        let start_x = service.player.pos.x;

        service.input.right = true;
        for _ in 0..240 {
            service.simulate_tick();
            if service.transition.is_some() {
                break;
            }
        }
        assert!(
            service.transition.is_some(),
            "the seam must start a transition"
        );
        assert_eq!(
            service.current_room, "lvl_b",
            "Session.Level switches at start"
        );
        let from = service.player.pos;

        // Input is ignored during a transition (entities frozen): pressing the opposite way does not change the glide direction.
        service.input.right = false;
        service.input.left = true;
        for _ in 0..64 {
            service.simulate_tick();
            if service.transition.is_none() {
                break;
            }
        }
        assert!(service.transition.is_none(), "the glide must finish");
        assert!(
            service.player.pos.x > from.x,
            "the player keeps gliding toward the next room despite input"
        );
        assert!(
            service.player.pos.x > start_x,
            "the player crossed the seam"
        );
        let px = service.player.pos.x.to_f32();
        assert!(
            (320.0..=328.0).contains(&px),
            "the player stops just inside lvl_b (padded by 4px), got {px}"
        );
        assert!(
            service.player.on_ground,
            "the player lands on lvl_b's floor"
        );
        assert_eq!(service.player.pos.y.to_f32(), 176.0 - PLAYER_H);
        // The camera lands on the clamped target in the new room (for the player's position in the new room).
        let anchor = service.player.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H);
        assert_eq!(
            service.camera.position,
            GameService::vanilla_camera_center(&service.session, anchor, "lvl_b")
        );
    }

    /// The camera follow coefficient must be vanilla `1 - 0.01^dt` (~0.074/tick), not the old
    /// 1/4 -- chasing too fast loses the vanilla "camera drag" feel.
    #[test]
    fn camera_follow_uses_the_vanilla_exponential_rate() {
        let doc = seam_map();
        let session = Session::from_doc(doc, "test");
        let mut service = GameService::new(session, None, None, None);
        let anchor = service.player.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H);
        let target = GameService::vanilla_camera_center(&service.session, anchor, "lvl_a");
        // Drag the camera elsewhere and run one tick: the displacement must be (target - pos) * 19/256.
        service.camera.position = Vec2::from_f32s(40.0, 90.0);
        service.update_camera();
        let expected =
            Vec2::from_f32s(40.0, 90.0) + (target - Vec2::from_f32s(40.0, 90.0)) * CAMERA_LERP;
        assert_eq!(service.camera.position, expected);
    }
}
