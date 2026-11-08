//! Player simulation: an 8x8 AABB against the tile grid, solved with separated axes.
//!
//! All state is [`Fx`] (AGENTS.md 15.7), and the time step is always
//! [`FIXED_DT`](releste_math::FIXED_DT).
//!
//! Relationship to vanilla (`references/pureCeleste.cs`, behavioral reference):
//! - **Aligned**: run acceleration (1000/400/0.65), gravity 900, fall-speed cap
//!   160/240 easing (the `maxFall` state), apex half gravity (40 threshold),
//!   coyote / jump buffer / release cut (landed in Phase 4.1).
//! - **Continuous-world semantics** (vanilla uses screen transitions; here it is
//!   continuous walking in one world): collision queries cover **every** room the
//!   hitbox touches (on a seam both sides are checked, so the head never drives into
//!   the solid interior of the neighboring room); wall and floor landings snap to
//!   **world cell edges**, independent of the room origin. When the center falls out
//!   of every room, the nearest room clamps it back.
//! - **Jump is aligned**: `Jump()`'s -105 + `JumpHBoost` (120704-120705) + varJump
//!   clamping (122042-122048) + 0.08s buffer (110376) / 0.1s coyote (119832-119839);
//!   see the provenance comments on each constant.
//! - **Engine-invented (no vanilla counterpart; deviation declared)**: collision in
//!   a continuous multi-room world (`Player::hit_in`'s seam double check), the
//!   world-level fallback clamp (`Player::clamp_to_world`) and the fixed 1/2px
//!   advance step (`STEP`). Vanilla is **one tile grid per map** (77231-77233) plus a
//!   single room boundary, so collision is naturally map-wide; the engine stores tiles
//!   per room and can only check seams explicitly. Each function's docs note this.
//! - **Implemented states**: 0 Normal (119397), 1 Climb with stamina (119398),
//!   2 Dash with eight-way/refill/cooldown (119399); `Player::step` dispatches on
//!   `state`.
//! - **Not yet implemented states** (vanilla `StateMachine.SetCallbacks`
//!   119397-119411): 3 Swim, 4 Boost, 5 RedDash, 6 HitSquash, 7 Launch, 8 Pickup,
//!   9 DreamDash, 10 SummitLaunch, 11 Dummy, and the 12-15 intro coroutines; these
//!   `state` values currently all fall back to Normal. ROADMAP Phase 4.1. The hair
//!   color USED/FLASH/TWO_DASHES states are already prepared in `sprites::hair_color`
//!   and activate as dash lands.

use releste_map::Room;
use releste_math::{Fx, Vec2};

use crate::session::{nearest_room, room_containing, Session};

/// Hitbox width (pixels, integer, for room collision).
///
/// Vanilla: `pureCeleste.cs:118856`
/// `normalHitbox = new Hitbox(8f, 11f, -4f, -11f)` -- 8 wide, 11 tall; `Position` is
/// the hitbox **bottom center** (the anchor under the feet).
pub const PLAYER_W_PX: i32 = 8;
/// Hitbox height (pixels, integer). Provenance in [`PLAYER_W_PX`].
pub const PLAYER_H_PX: i32 = 11;
/// Hitbox width (f32 view, for rendering).
pub const PLAYER_W: f32 = PLAYER_W_PX as f32;
/// Hitbox height (f32 view, for rendering).
pub const PLAYER_H: f32 = PLAYER_H_PX as f32;

/// Horizontal run speed 90 px/s (1.5 px/tick at 60Hz). Vanilla `MaxRun = 90f`
/// (pureCeleste.cs:118386 `MaxRun = 90f`).
const RUN: Fx = Fx::from_ratio(3, 2);
/// Speed threshold for the idle / edge branch: 25 px/s (5/12 px/tick), vanilla
/// `Math.Abs(Speed.X) <= 25f && moveX == 0`
/// (pureCeleste.cs:120402 `Math.Abs(Speed.X) <= 25f && moveX == 0`).
///
/// **This is not the same threshold as [`RUN`]**: the runSlow/runFast split is
/// 45 px/s (pureCeleste.cs:120449 `Math.Abs(Speed.X) < 45f`); 25 px/s only decides
/// "have we finished moving". Mixing the two selects the wrong frame while
/// decelerating (this engine previously used 45).
const IDLE_SPEED: Fx = Fx::from_ratio(5, 12);
/// Horizontal acceleration 1000 px/s^2 (~0.278 px/tick^2) -- starting from rest does
/// not snap to top speed. Vanilla `RunAccel = 1000f`
/// (pureCeleste.cs:118388 `RunAccel = 1000f`).
const RUN_ACCEL: Fx = Fx::from_ratio(5, 18);
/// Overspeed reduction 400 px/s^2: used only for **same-direction overspeed**
/// (|vx| > 90 and the same sign as the input); easing to a stop with no input and
/// braking in the opposite direction both use the full 1000 rate
/// (pureCeleste.cs:121965-121975). Definition: `RunReduce = 400f`
/// (pureCeleste.cs:118390 `RunReduce = 400f`).
const RUN_REDUCE: Fx = Fx::from_ratio(1, 9);
/// Air control factor 0.65 -- air acceleration is a bit more than half of ground.
/// Vanilla `AirMult = 0.65f` (pureCeleste.cs:118392 `AirMult = 0.65f`).
const AIR_MULT: Fx = Fx::from_ratio(13, 20);
/// Gravity 900 px/s^2 (0.25 px/tick^2).
/// Vanilla `Gravity = 900f` (pureCeleste.cs:118378 `Gravity = 900f`).
const GRAVITY: Fx = Fx::from_ratio(1, 4);
/// Apex half gravity: while rising or falling very slowly and holding jump, gravity
/// is halved (450 px/s^2). Vanilla multiplies gravity by 0.5
/// (pureCeleste.cs:122031 `? 0.5f : 1f`).
const HALF_GRAVITY: Fx = Fx::from_ratio(1, 8);
/// Normal fall-speed cap 160 px/s (8/3 px/tick).
///
/// **Reference value**: `MaxFall = 160f` (pureCeleste.cs:118376 `MaxFall = 160f`).
/// It is an **eased state** (see [`Player::max_fall`]): holding "down" ramps the cap
/// toward [`FAST_MAX_FALL`], releasing eases it back -- not a hard cut.
const MAX_FALL: Fx = Fx::from_ratio(8, 3);
/// Fast-fall cap 240 px/s (4 px/tick). Reference `FastMaxFall = 240f`
/// (pureCeleste.cs:118382 `FastMaxFall = 240f`).
const FAST_MAX_FALL: Fx = Fx::from_int(4);
/// Easing rate of the fall-speed cap: 300 px/s^2 (1/12 px/tick^2). Reference
/// `FastMaxAccel = 300f` (pureCeleste.cs:118384 `FastMaxAccel = 300f`).
const FAST_MAX_ACCEL: Fx = Fx::from_ratio(1, 12);
/// Apex half-gravity threshold 40 px/s (2/3 px/tick). Reference
/// `HalfGravThreshold = 40f` (pureCeleste.cs:118380 `HalfGravThreshold = 40f`).
const HALF_GRAV_THRESHOLD: Fx = Fx::from_ratio(2, 3);
/// Jump vertical speed -105 px/s (-7/4 px/tick). `Player.Jump`:
/// `Speed.Y = -105f` (pureCeleste.cs:120705).
const JUMP_SPEED: Fx = Fx::from_ratio(-7, 4);
/// Jump horizontal boost 40 px/s (2/3 px/tick): `Speed.X += 40f * moveX`
/// (pureCeleste.cs:120704). Jumping while running briefly overspeeds, then gets
/// pulled back at the 400 rate.
const JUMP_H_BOOST: Fx = Fx::from_ratio(2, 3);
/// varJump duration 0.2s = 12 ticks (pureCeleste.cs:120698).
///
/// While jump is held, vertical speed is clamped back to the jump value
/// (122042-122048) -- gravity is fully cancelled, giving a full jump of
/// ~12x1.75 + 105^2/1800 ~ 27px (3.4 cells); releasing zeroes the timer and only
/// gravity remains, which is the variable jump height. There is **no** "cut speed at
/// release" -- the height is reduced purely by the clamp.
const VAR_JUMP_TICKS: u32 = 12;
/// Advance step for collision solving: 1/2 px.
///
/// **Engine-invented (deviation declared)**: vanilla `Actor.MoveH/MoveV` advances
/// pixel by pixel on an integer-pixel grid through `Collider`, with no concept of a
/// "subpixel step". Here, so that a 240 px/s (4 px/tick) dash still queries collision
/// cell by cell, it advances in fixed 1/2px steps -- behaviorally equivalent to
/// pixel-by-pixel, at the cost of a few extra queries per tick.
const STEP: Fx = Fx::from_ratio(1, 2);
/// Dash speed 240 px/s (4 px/tick). `DashCoroutine`: `speed = value * 240f`
/// (pureCeleste.cs:122745).
const DASH_SPEED: Fx = Fx::from_int(4);
/// Dash end speed 160 px/s (8/3 px/tick): after 0.15s `Speed = DashDir * 160f`
/// (122824, when `DashDir.Y <= 0`).
const DASH_END_SPEED: Fx = Fx::from_ratio(8, 3);
/// Dash duration 0.15s = 9 ticks (`yield return 0.15f`, 122817).
const DASH_TICKS: u16 = 9;
/// Dash cooldown 0.2s = 12 ticks (`DashBegin`: `dashCooldownTimer = 0.2f`, 122541).
const DASH_COOLDOWN_TICKS: u16 = 12;
/// Cooldown after a ground refill during which dashing is blocked: 0.1s = 6 ticks
/// (122545; vanilla only writes it and never reads it, ported anyway to keep the
/// field complete).
const DASH_REFILL_COOLDOWN_TICKS: u16 = 6;
/// Dash attack window 0.3s = 18 ticks (`dashAttackTimer = 0.3f`, 122554).
const DASH_ATTACK_TICKS: u16 = 18;
/// Wall jump horizontal speed 130 px/s (13/6 px/tick, `WallJump`: `Speed.X = 130f*dir`,
/// pureCeleste.cs:120836).
const WALLJUMP_X: Fx = Fx::from_ratio(13, 6);
/// Wall jump probe distance 5px: `CollideCheck<Solid>(Position + UnitX*dir*5)`
/// (`WallJumpCheck`, the `num = 5` normal branch at 120779-120804).
const WALLJUMP_PROBE: Fx = Fx::from_int(5);
/// Duration of the forced moveX after a wall jump: 0.16s = 10 ticks (120832
/// `forceMoveXTimer = 0.16f`; the anti-re-grab window while a direction is held).
const FORCE_MOVE_X_TICKS: u16 = 10;
/// Super wall jump horizontal speed 170 px/s (17/6 px/tick, `SuperWallJump`:
/// `Speed.X = 170f*dir`, pureCeleste.cs:120877).
///
/// Note: the value was once mis-written as 240 (4 px/tick) -- the section comment was
/// fixed but the constant was not. `super_wall_jump_pins_vanilla_speeds` now pins
/// these two values.
const SUPER_WALLJUMP_X: Fx = Fx::from_ratio(17, 6);
/// Super wall jump vertical speed -160 px/s (-8/3 px/tick, `varJumpSpeed =
/// (Speed.Y = -160f)`, pureCeleste.cs:120878).
const SUPER_WALLJUMP_Y: Fx = Fx::from_ratio(-8, 3);
/// Super jump (jumping during a ground horizontal dash) horizontal speed 260 px/s
/// (13/3 px/tick, `SuperJump`: `Speed.X = 260f*Facing`, 120748).
const SUPER_JUMP_X: Fx = Fx::from_ratio(13, 3);
/// Hair flash duration 0.12s = 7 ticks (`UpdateHair`: `hairFlashTimer = 0.12f`,
/// pureCeleste.cs:120271).
const HAIR_FLASH_TICKS: u8 = 7;
/// Hair color ease-in rate 6/s (`Lerp(Hair.Color, Used, 6f*dt)`, 120258-120262; the
/// per-tick lerp factor = 6/60 = 0.1).
/// Max climb stamina 110 (`ClimbMaxStamina`, pureCeleste.cs:118504; full at spawn,
/// 118680 `Stamina = 110f`).
const CLIMB_MAX_STAMINA: Fx = Fx::from_ratio(110, 1);
/// Upward climb stamina drain: vanilla's literal `45.454544f`/s (122266-122268,
/// draining 110 in ~2.42s) converted per tick by /60.
const CLIMB_UP_DRAIN_PER_TICK: Fx = Fx::from_ratio(45454544, 60000000);
/// Stamina drain for holding still 10/s (122278-122280).
const CLIMB_IDLE_DRAIN_PER_TICK: Fx = Fx::from_ratio(1, 6);
/// Stamina drain for a climb jump (without leaving the ground) 27.5 (120904-120906).
const CLIMBJUMP_DRAIN: Fx = Fx::from_ratio(55, 2);
/// Upward climb speed -45 px/s (-3/4 px/tick, 122220-122222).
const CLIMB_UP_SPEED: Fx = Fx::from_ratio(-3, 4);
/// Downward climb speed 80 px/s (4/3 px/tick, 122228-122230).
const CLIMB_DOWN_SPEED: Fx = Fx::from_ratio(4, 3);
/// Slip speed when holding without input 30 px/s (1/2 px/tick, `num = 30f` at
/// 122246-122248).
const CLIMB_SLIP_SPEED: Fx = Fx::from_ratio(1, 2);
/// Hop off the top of a climbed wall: horizontal 100 px/s (5/3 px/tick, `ClimbHop`
/// 122368), vertical clamped to <= -120 px/s (-2 px/tick, 122372).
const CLIMB_HOP_X: Fx = Fx::from_ratio(5, 3);
const CLIMB_HOP_Y: Fx = Fx::from_int(-2);
/// Duck hitbox: 8x6, offset (-4,-6) (`duckHitbox`, pureCeleste.cs:118858; its bottom
/// edge aligns with the normal hitbox -> the engine's top-left y offset = 11-6 = 5).
const DUCK_H: i32 = 6;
const DUCK_Y_OFF: i32 = 5;
/// Ground deceleration while ducking 500 px/s^2 (122047-122049:
/// `Approach(Speed.X, 0, 500*dt)`; per tick^2, 500/3600 = 5/36 px/tick^2).
const DUCK_DECEL: Fx = Fx::from_ratio(5, 36);
/// Wall slide speed eased from 160 to 20 px/s (122240: `Lerp(160, 20,
/// wallSlideTimer/1.2)`).
const WALLSLIDE_FAST: Fx = Fx::from_ratio(8, 3);
const WALLSLIDE_SLOW: Fx = Fx::from_ratio(1, 3);
/// Wall slide timer maximum 1.2s = 72 ticks (`wallSlideTimer = 1.2f`, 118760).
const WALLSLIDE_TICKS: u16 = 72;
/// Climb start stagger 0.1s = 6 ticks (`climbNoMoveTimer = 0.1f`, 122146).
const CLIMB_NO_MOVE_TICKS: u16 = 6;
/// Push window toward the wall after a climb jump: 0.2s = 12 ticks (`ClimbJump`:
/// `wallBoostTimer = 0.2f`, 120910-120913).
const WALL_BOOST_TICKS: u16 = 12;
const HAIR_LERP_PER_TICK: f32 = 0.1;

/// Jump input buffer: 0.08s = 5 ticks (including the press frame).
///
/// Vanilla `VirtualButton`'s `BufferTime`: Jump = 0.08f (pureCeleste.cs:110376); it
/// fills on press, decays every frame, and `Pressed` is true while the count > 0
/// (16715-16840) -- a jump pressed within 4 ticks before landing still fires on
/// landing.
const JUMP_BUFFER_FRAMES: u32 = 5;
/// Coyote time: 0.1s (`jumpGraceTimer`, pureCeleste.cs:119832-119839 -- refilled to
/// 0.1 every grounded frame, decayed every frame once airborne, and a jump is allowed
/// while > 0).
///
/// The engine decrements at the end of the tick (physics has already used the previous
/// tick's grounded state), and 6 counts give exactly 5 airborne ticks in which a jump
/// can fire, equivalent frame for frame to vanilla.
const COYOTE_FRAMES: u32 = 6;

/// Button state for one frame (fed in by the window host).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Input {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
    pub jump: bool,
    /// Dash button (vanilla `Input.Dash`, default X, 101632). Buffering matches jump
    /// (`VirtualButton.BufferTime = 0.08f`, 110376).
    pub dash: bool,
    /// Grab button (vanilla `Input.Grab`, default Z/V/LeftShift, 101622).
    pub grab: bool,
}

impl Input {
    /// Horizontal axis: -1 / 0 / +1 (pressing both left and right does not move).
    pub fn axis_x(self) -> Fx {
        match (self.left, self.right) {
            (true, false) => -Fx::one(),
            (false, true) => Fx::one(),
            _ => Fx::zero(),
        }
    }

    /// Union of two input states: a key is true if it is true in either.
    ///
    /// Overlapping input segments in scripts merge by this rule (`devtools::input_at`)
    /// -- pressing left and right together is allowed and `axis_x` returns 0 (vanilla
    /// `Axis` semantics, 16705-16714).
    pub fn merge(self, other: Input) -> Input {
        Input {
            left: self.left || other.left,
            right: self.right || other.right,
            up: self.up || other.up,
            down: self.down || other.down,
            jump: self.jump || other.jump,
            dash: self.dash || other.dash,
            grab: self.grab || other.grab,
        }
    }

    /// Vertical axis: -1 (up) / 0 / +1 (down). Vanilla `Input.MoveY.Value`
    /// (`Axis(bool, bool, bothValue)`, 16705-16714: pressing both returns 0).
    pub fn axis_y(self) -> Fx {
        match (self.up, self.down) {
            (true, false) => -Fx::one(),
            (false, true) => Fx::one(),
            _ => Fx::zero(),
        }
    }
}

/// The player's current state.
#[derive(Debug, Clone)]
pub struct Player {
    /// Hitbox top-left (world coordinates).
    pub pos: Vec2,
    /// Previous tick's position, used for linear interpolation when rendering.
    pub prev_pos: Vec2,
    /// Velocity (px/tick).
    pub vel: Vec2,
    /// Whether the player is standing on a solid.
    pub on_ground: bool,
    /// Facing: 1 right, -1 left.
    pub facing: i32,
    /// Current fall-speed cap (px/tick): eased between [`MAX_FALL`] and
    /// [`FAST_MAX_FALL`] at [`FAST_MAX_ACCEL`] -- reference pureCeleste's `maxFall`
    /// local state.
    max_fall: Fx,
    /// Previous tick's jump button (edge detection: press / release).
    prev_jump: bool,
    /// Jump buffer countdown: 0 = none. Set full on jump press, cleared after takeoff.
    jump_buffer: u32,
    /// Coyote countdown: 0 = none. Reset while grounded, decayed once airborne.
    coyote: u32,
    /// varJump countdown (ticks): while > 0 and jump is held, vertical speed is
    /// clamped back to the jump value (vanilla `varJumpTimer`,
    /// pureCeleste.cs:122042-122048).
    var_jump: u32,
    /// Remaining ticks of forced varJump clamping (vanilla `AutoJump` +
    /// `AutoJumpTimer`, 120590-120594: auto-jump on an up transition, clamping for
    /// 0.2s regardless of input).
    auto_jump: u32,
    /// Current dash count (vanilla `Player.Dashes`, constructed as MaxDashes around
    /// 118860).
    pub dashes: u8,
    /// Dash inventory maximum (vanilla `Player.MaxDashes`; the Prologue
    /// `Inventory.Prologue` has no second dash -> 1, 119421).
    pub max_dashes: u8,
    /// Previous tick's dash count (vanilla `lastDashes`, used for the hair flash
    /// check, 120295).
    last_dashes: u8,
    /// Remaining hair flash time (vanilla `hairFlashTimer`, 120271).
    hair_flash: u8,
    /// The **current** hair color (the carrier for `UpdateHair`'s per-frame lerp,
    /// 120258-120262).
    hair_color_current: [f32; 4],
    /// State machine (vanilla `StateMachine.State`, numbered as vanilla: 0 = Normal,
    /// 2 = Dash, 119397-119409; 1/3/8 etc. not implemented).
    state: u8,
    /// Dash direction (vanilla `DashDir`, an eight-way unit vector with components
    /// +/-1/0/+/-0.707).
    dash_dir: Vec2,
    /// Dash coroutine phase: 0 = entry tick (`yield return null`, speed not yet set),
    /// 1..=9 = the 0.15s wait, ending on tick 10 (`DashCoroutine` 122731-122831).
    dash_phase: u16,
    /// Snapshot of speed at the jump/dash instant (vanilla `beforeDashSpeed`, 122559).
    before_dash_speed: Vec2,
    /// Dash cooldown (vanilla `dashCooldownTimer`, 0.2s, 122541).
    dash_cooldown: u16,
    /// Dash refill cooldown (vanilla `dashRefillCooldownTimer`, 0.1s, 122545).
    dash_refill_cooldown: u16,
    /// Dash attack window (vanilla `dashAttackTimer`, 0.3s, 122554).
    dash_attack: u16,
    /// Target speed for varJump clamping (vanilla `varJumpSpeed` field: `Jump()` sets
    /// -105 (120698), `WallJump` -105 (120837), `BeforeUpTransition` -105 (120588),
    /// `Rebound` -120; the clamp is `min(Speed.Y, varJumpSpeed)`, 122134).
    var_jump_speed: Fx,
    /// Forced horizontal input (vanilla `forceMoveX` + `forceMoveXTimer`,
    /// 119883-119889: moveX is overridden for 0.16s after a wall jump, preventing an
    /// immediate re-grab).
    force_move_x: i32,
    force_move_x_ticks: u16,
    /// Horizontal input in effect this tick (`moveX`: input or the forceMoveX
    /// override, 119883-119891).
    move_x: Fx,
    /// Previous tick's dash button (buffer edge).
    prev_dash: bool,
    /// Dash button buffer (`Input.Dash`'s VirtualButton.BufferTime = 0.08s, 110376).
    dash_buffer: u16,
    /// Freeze request at dash start (`Celeste.Freeze(0.05f)`, 122540; consumed by the
    /// service).
    pub freeze_ticks: u8,
    /// Animation state (vanilla `Sprite`: current animation id, frame, timer, playing).
    pub anim_id: String,
    pub anim_frame: usize,
    anim_timer: f32,
    anim_playing: bool,
    /// The id most recently passed to `play` (vanilla `Sprite.LastAnimationID`,
    /// pureCeleste.cs:10181, written at 10491-10492). `PlayerSprite.Running`
    /// (111375-111388) and `UpdateSprite`'s run guard (120447) both read it -- "is it
    /// running" is **not** the same as "does the current animation have a goto"
    /// (`idle`/`edge`/`walk` all carry `<Loop>`).
    last_anim_id: String,
    /// Fast jump/fall flag (`fastJump`, 120437/120469).
    fast_jump: bool,
    /// Stamina (`Stamina`, 110 at spawn per 118680; drained by climbing, refilled on
    /// landing 119818-119823).
    pub stamina: Fx,
    /// Ducking (`Ducking`; hitbox 8x6, 118858).
    pub ducking: bool,
    /// Climb start stagger (`climbNoMoveTimer`, 0.1s, 122146).
    climb_no_move_ticks: u16,
    /// Previous tick's climb move direction (`lastClimbMove`, 122251).
    last_climb_move: i32,
    /// Wall slide direction/timer (`wallSlideDir`/`wallSlideTimer`, 118758-118760).
    wall_slide_dir: i32,
    wall_slide_ticks: u16,
    /// Climb jump boost (`wallBoostDir`/`wallBoostTimer`, 119119-119820).
    wall_boost_dir: i32,
    wall_boost_ticks: u16,
}

/// Fx version of `Math.Sign(before.X) == Math.Sign(speed.X)` (122770).
fn vel_x_same_dir(before: &Vec2, other_x: Fx) -> bool {
    before.x != Fx::zero() && other_x != Fx::zero() && before.x.signum() == other_x.signum()
}

/// Per-channel linear interpolation (`Color.Lerp(a, b, amount)`; amount is this tick's
/// factor).
fn lerp_color(current: &mut [f32; 4], target: [f32; 4], t: f32) {
    for (c, t_target) in current.iter_mut().zip(target.iter()) {
        *c += (t_target - *c) * t;
    }
}

impl Player {
    /// Spawn the player at `pos` (world coordinates, hitbox top-left).
    pub fn new(pos: Vec2) -> Self {
        Player {
            pos,
            prev_pos: pos,
            vel: Vec2::ZERO,
            on_ground: false,
            facing: 1,
            max_fall: MAX_FALL,
            prev_jump: false,
            jump_buffer: 0,
            coyote: 0,
            var_jump: 0,
            auto_jump: 0,
            dashes: 1,
            max_dashes: 1,
            last_dashes: 1,
            hair_flash: 0,
            hair_color_current: crate::sprites::hair_color::NORMAL,
            state: 0,
            dash_dir: Vec2::ZERO,
            dash_phase: 0,
            before_dash_speed: Vec2::ZERO,
            dash_cooldown: 0,
            dash_refill_cooldown: 0,
            dash_attack: 0,
            var_jump_speed: JUMP_SPEED,
            force_move_x: 0,
            force_move_x_ticks: 0,
            move_x: Fx::zero(),
            prev_dash: false,
            dash_buffer: 0,
            freeze_ticks: 0,
            anim_id: String::new(),
            anim_frame: 0,
            anim_timer: 0.0,
            anim_playing: false,
            last_anim_id: String::new(),
            fast_jump: false,
            stamina: CLIMB_MAX_STAMINA,
            ducking: false,
            climb_no_move_ticks: 0,
            last_climb_move: 0,
            wall_slide_dir: 0,
            wall_slide_ticks: WALLSLIDE_TICKS,
            wall_boost_dir: 0,
            wall_boost_ticks: 0,
        }
    }

    /// Advance one fixed tick. Room identity (title bar / camera / hot-reload checks)
    /// is decided by [`Session::room_at`]; physics does not pick a room -- see
    /// [`Player::hit_in`].
    pub fn update(&mut self, input: &Input, session: &Session) {
        self.step(input, &session.doc.rooms);
    }

    /// A serializable state snapshot for one tick (for `--trace` / `--script`).
    ///
    /// `tick` is supplied by the caller (1-based, after which `kernel.tick()`), and
    /// `room` is that tick's room name. Units are in the field names: position in px,
    /// velocity in px/tick, with px/s conversions alongside for line-by-line comparison
    /// with vanilla.
    pub fn snapshot<'a>(&'a self, tick: u64, room: &'a str) -> crate::devtools::Snapshot<'a> {
        crate::devtools::Snapshot {
            tick,
            room,
            x: self.pos.x.to_f32(),
            y: self.pos.y.to_f32(),
            vx: self.vel.x.to_f32(),
            vy: self.vel.y.to_f32(),
            vx_ps: self.vel.x.to_f32() * 60.0,
            vy_ps: self.vel.y.to_f32() * 60.0,
            on_ground: self.on_ground,
            facing: self.facing,
            state: self.state,
            dashes: self.dashes,
            stamina: self.stamina.to_f32(),
            ducking: self.ducking,
            anim: &self.anim_id,
            anim_frame: self.anim_frame,
            last_anim: &self.last_anim_id,
            move_x: self.move_x.to_f32(),
            var_jump: self.var_jump,
            jump_buffer: self.jump_buffer,
            coyote: self.coyote,
        }
    }

    /// One simulation step: the shared timer head -> state machine (Normal/Dash) ->
    /// world-level fallback.
    ///
    /// Mirrors the structure of vanilla `Player.Update` (pureCeleste.cs:119700-120168):
    /// timers/refill/facing (119827-119921) -> `base.Update()` into the state machine
    /// (registered 119397-119409: 0 = NormalUpdate, 2 = DashUpdate) -> MoveH/MoveV
    /// (120049-120056) -> EnforceBounds is handled by the service.
    ///
    /// `rooms` is passed explicitly so unit tests can build room geometry directly
    /// without spinning up a whole Session.
    pub(crate) fn step(&mut self, input: &Input, rooms: &[Room]) {
        self.update_head(input);
        match self.state {
            1 => self.climb_update(input, rooms),
            2 => self.dash_update(input, rooms),
            _ => self.normal_update(input, rooms),
        }
        self.clamp_to_world(rooms);
        self.update_hair_color();
    }

    /// Animation advance + state mapping (vanilla calls `UpdateSprite` after movement,
    /// 120068; `Sprite.Update` runs every frame via `base.Update`).
    pub fn update_visuals(
        &mut self,
        table: &crate::sprites::AnimationTable,
        input: &Input,
        rooms: &[Room],
    ) {
        // Never played an animation -> the start animation (the sprite element's
        // `start`, validated at 10189).
        if self.anim_id.is_empty() {
            if let Some(def) = table.sprites.get("player") {
                let start = def.start.clone();
                self.play(table, &start);
            }
        }
        self.update_animation(table);
        self.update_sprite(table, input, rooms);
    }

    /// The shared timers and refill at the head of vanilla `Player.Update`
    /// (119827-119921).
    fn update_head(&mut self, input: &Input) {
        // dashAttackTimer -= dt (119827-119830).
        self.dash_attack = self.dash_attack.saturating_sub(1);
        // Ground refill (119855-119863): the state 3 (swim) branch does not apply;
        // standing on a solid (`on_ground` means there is solid under the feet,
        // equivalent to `CollideCheck<Solid>(Position + UnitY)`) -> RefillDash
        // (121082-121090: writes only when not already full).
        if self.on_ground && self.dashes < self.max_dashes {
            self.dashes = self.max_dashes;
        }
        // jumpGraceTimer (119830-119840) -- already handled by the coyote counter at the
        // end of physics (semantically equivalent; see the COYOTE_FRAMES docs).
        // varJumpTimer / AutoJumpTimer decay (119864-119877).
        if self.var_jump > 0 {
            self.var_jump -= 1;
        }
        if self.auto_jump > 0 {
            self.auto_jump -= 1;
        }
        // dashCooldownTimer / dashRefillCooldownTimer (119843-119848).
        self.dash_cooldown = self.dash_cooldown.saturating_sub(1);
        self.dash_refill_cooldown = self.dash_refill_cooldown.saturating_sub(1);
        // Climb jump boost (119811-119820): push toward wallBoostDir -> horizontal speed
        // 130 + stamina +27.5, once.
        if self.wall_boost_ticks > 0 {
            self.wall_boost_ticks -= 1;
            if self.move_x == Fx::from_int(self.wall_boost_dir) {
                self.vel.x = WALLJUMP_X * self.move_x;
                self.stamina += CLIMBJUMP_DRAIN;
                self.wall_boost_ticks = 0;
            }
        }
        // Wall slide timer (119808-119812): the direction is cleared at the end of each
        // tick and the timer decays; NormalUpdate re-arms it while its condition holds.
        if self.wall_slide_dir != 0 {
            self.wall_slide_ticks = self.wall_slide_ticks.saturating_sub(1);
            self.wall_slide_dir = 0;
        }
        // Refill stamina on landing + reset the wall slide (119818-119823).
        if self.on_ground && self.state != 1 {
            self.auto_jump = 0;
            self.stamina = CLIMB_MAX_STAMINA;
            self.wall_slide_ticks = WALLSLIDE_TICKS;
        }
        // moveX: the forceMoveX override window (119883-119891), otherwise read input.
        let move_x = if self.force_move_x_ticks > 0 {
            self.force_move_x_ticks -= 1;
            Fx::from_int(self.force_move_x)
        } else {
            input.axis_x()
        };
        self.move_x = move_x;
        // Facing follows input (119908-119917).
        if move_x != Fx::zero() {
            self.facing = move_x.signum();
        }
    }

    /// Hair color: the non-gradient part of vanilla `Player.UpdateHair`
    /// (pureCeleste.cs:120244-120296) -- USED when the dash is spent, FLASH at the
    /// instant of refill (previous tick's count differs, or the white flash is still
    /// running), otherwise `Dashes == 2 ? TWO_DASHES : NORMAL`.
    ///
    /// `used` ease-in (`Lerp(Hair.Color, Used, 6*dt)`, 120258-120262) and
    /// `hairFlashTimer = 0.12s` (120271) need per-frame color state; when dash lands,
    /// this function is upgraded to a version that carries the current color. For now
    /// it returns a discrete state, and the Prologue (one full dash in inventory) is
    /// always NORMAL -- consistent with vanilla.
    pub fn hair_color(&self) -> [f32; 4] {
        // The current per-frame lerp value (`UpdateHair` writes it into `Hair.Color`,
        // read directly by rendering).
        self.hair_color_current
    }

    /// Adjustment before an up transition (vanilla `BeforeUpTransition`,
    /// pureCeleste.cs:120590-120598): zero horizontal, auto-jump upward (-105 + 0.2s
    /// of forced varJump clamping), landing in the room above.
    pub fn before_up_transition(&mut self) {
        self.vel.x = Fx::zero();
        self.vel.y = JUMP_SPEED;
        self.var_jump = VAR_JUMP_TICKS;
        self.auto_jump = VAR_JUMP_TICKS;
    }

    /// Adjustment before a down transition (vanilla `BeforeDownTransition`,
    /// pureCeleste.cs:120572-120583): cut any upward speed, clear varJump.
    pub fn before_down_transition(&mut self) {
        self.vel.y = self.vel.y.max(Fx::zero());
        self.var_jump = 0;
        self.auto_jump = 0;
    }

    /// Whether a world-coordinate point is inside a solid tile of any touched room
    /// (Monocle `Scene.CollideCheck<Solid>(point)` point semantics; used by
    /// `SlipCheck`).
    fn solid_at(rooms: &[Room], x: Fx, y: Fx) -> bool {
        let (cx, cy) = (cell(x), cell(y));
        for room in rooms {
            let (ox, oy) = (room.bounds.x.round() as i32, room.bounds.y.round() as i32);
            let cols = (room.bounds.w.round() as i32).div_euclid(8);
            let rows = (room.bounds.h.round() as i32).div_euclid(8);
            let (gx0, gy0) = (ox.div_euclid(8), oy.div_euclid(8));
            if cx < gx0 || cy < gy0 || cx >= gx0 + cols || cy >= gy0 + rows {
                continue;
            }
            if Session::solid_cell(room, cx, cy) {
                return true;
            }
        }
        false
    }

    /// Vanilla `CanUnDuck` (119281-119291): switching back to the normal hitbox must be
    /// collision-free.
    fn can_unduck(&self, rooms: &[Room]) -> bool {
        !self.ducking || !self.hits(rooms, self.pos.x, self.pos.y)
    }

    /// Vanilla `CanUnDuckAt(position)`: the normal hitbox shifted to x is
    /// collision-free.
    fn can_unduck_at(&self, rooms: &[Room], x: Fx) -> bool {
        !self.hits(rooms, x, self.pos.y)
    }

    /// Vanilla `SlipCheck(addY)` (122395-122407): no solid 4px below the facing-side
    /// top corner of the hitbox and none 4px above it -> the wall has ended.
    fn slip_check(&self, rooms: &[Room], add_y: Fx) -> bool {
        let top = self.pos.y + Fx::from_int(self.box_dims().2 - PLAYER_H_PX);
        let (px, py) = if self.facing > 0 {
            (
                self.pos.x + Fx::from_int(PLAYER_W_PX),
                top + Fx::from_int(4) + add_y,
            )
        } else {
            (self.pos.x - Fx::one(), top + Fx::from_int(4) + add_y)
        };
        if !Self::solid_at(rooms, px, py) {
            return !Self::solid_at(rooms, px, py - Fx::from_int(4) + add_y);
        }
        false
    }

    /// The normal branch of vanilla `ClimbCheck(dir, yAdd)` (121815-121822): probe 2px
    /// toward the wall for a solid (no ClimbBlocker entity -> EdgeCheck is always
    /// false). `ClimbBoundsCheck` (121799-121806) uses room bounds.
    fn climb_check(&self, rooms: &[Room], dir: i32, y_add: i32) -> bool {
        let Some(room) = crate::session::room_containing(rooms, self.pos) else {
            return false;
        };
        let b = &room.bounds;
        let left_ok = (self.pos.x.to_f32() + ((dir * 2) as f32)) >= b.x;
        let right_ok = self.pos.x.to_f32() + PLAYER_W + ((dir * 2) as f32) < b.x + b.w;
        if !(left_ok && right_ok) {
            return false;
        }
        self.hits(
            rooms,
            self.pos.x + Fx::from_int(dir * 2),
            self.pos.y + Fx::from_int(y_add),
        )
    }

    /// Enter climbing (vanilla `StateMachine.State = 1` + `ClimbBegin`,
    /// pureCeleste.cs:122140-122164): zero horizontal speed, vertical speed x0.2, snap
    /// 2px to the wall.
    fn enter_climb(&mut self, rooms: &[Room]) {
        self.state = 1;
        self.auto_jump = 0;
        self.vel.x = Fx::zero();
        self.vel.y *= Fx::from_ratio(1, 5); // Speed.Y *= 0.2f (122146)
        self.wall_slide_ticks = WALLSLIDE_TICKS;
        self.climb_no_move_ticks = CLIMB_NO_MOVE_TICKS;
        self.last_climb_move = 0;
        for _ in 0..2 {
            if self.hits(rooms, self.pos.x + Fx::from_int(self.facing), self.pos.y) {
                break;
            }
            self.pos.x += Fx::from_int(self.facing); // 122153-122160 snap to the wall
        }
    }

    /// Vanilla `ClimbJump()` (pureCeleste.cs:120902-120928): deduct 27.5 stamina when
    /// not grounded; a normal Jump; with no horizontal input, grant a 0.2s boost window
    /// (120910-120913).
    fn climb_jump(&mut self) {
        if !self.on_ground {
            self.stamina -= CLIMBJUMP_DRAIN;
        }
        self.jump(Fx::zero()); // Jump(particles: false, playSfx: false)
        if self.move_x == Fx::zero() {
            self.wall_boost_dir = -self.facing;
            self.wall_boost_ticks = WALL_BOOST_TICKS;
        }
    }

    /// Vanilla `ClimbHop()` (pureCeleste.cs:122380-122402, no moving-platform branch):
    /// hop away from the wall at 100 px/s, clamp vertical to <= -120 px/s, clear
    /// forceMoveX.
    fn climb_hop(&mut self) {
        self.vel.x = CLIMB_HOP_X * Fx::from_int(self.facing);
        self.vel.y = self.vel.y.min(CLIMB_HOP_Y);
        self.force_move_x = 0;
        self.force_move_x_ticks = 12; // 0.2s (122377)
        self.fast_jump = false;
    }

    /// Vanilla `Jump()` (pureCeleste.cs:120694-120716): vertical -105, horizontal
    /// +40*moveX (JumpHBoost), varJump filled to 0.2s, varJumpSpeed saved, coyote and
    /// buffer zeroed. (`LiftBoost` is 0 on static ground and is omitted.)
    fn jump(&mut self, move_x: Fx) {
        self.jump_buffer = 0;
        self.coyote = 0;
        self.var_jump = VAR_JUMP_TICKS;
        self.auto_jump = 0;
        self.dash_attack = 0;
        self.vel.x += move_x * JUMP_H_BOOST;
        self.vel.y = JUMP_SPEED;
        self.var_jump_speed = JUMP_SPEED;
        self.on_ground = false;
    }

    /// The normal branch of vanilla `WallJumpCheck(dir)` (pureCeleste.cs:120779-120804)
    /// (no spike/star-fly corrections, `num = 5`): probe 5px toward the wall for a
    /// solid. `ClimbBoundsCheck` / `ClimbBlocker.EdgeCheck` (the 120779 preamble) are
    /// always false/true in this engine, which has no climb-boundary entities (deviation
    /// declared: see the module docs).
    fn wall_jump_check(&self, rooms: &[Room], dir: i32) -> bool {
        let dx = Fx::from_int(dir) * WALLJUMP_PROBE;
        self.hits(rooms, self.pos.x + dx, self.pos.y)
    }

    /// Vanilla `WallJump(dir)` (pureCeleste.cs:120806-120860): horizontal 130*dir,
    /// vertical -105, varJump filled, `forceMoveX = dir` for 0.16s (120832-120838),
    /// consumes the jump buffer, zeroes coyote.
    fn wall_jump(&mut self, rooms: &[Room], dir: i32) {
        self.jump_buffer = 0;
        self.coyote = 0;
        self.var_jump = VAR_JUMP_TICKS;
        self.auto_jump = 0;
        self.dash_attack = 0;
        if self.move_x != Fx::zero() {
            // 120828-120838: force moveX while a direction is held, preventing an
            // immediate re-grab.
            self.force_move_x = dir;
            self.force_move_x_ticks = FORCE_MOVE_X_TICKS;
        }
        self.vel.x = WALLJUMP_X * Fx::from_int(dir);
        self.vel.y = JUMP_SPEED;
        self.var_jump_speed = JUMP_SPEED;
        self.on_ground = false;
        let _ = rooms;
    }

    /// Vanilla `StartDash()` (pureCeleste.cs:122469-122479) + `DashBegin`
    /// (122534-122565): consume one dash, enter State 2, record `beforeDashSpeed`, zero
    /// speed (no movement this tick), start the cooldown and attack window.
    fn start_dash(&mut self, input: &Input, rooms: &[Room]) {
        self.dashes = self.dashes.saturating_sub(1);
        self.dash_buffer = 0; // Input.Dash.ConsumeBuffer() (122474)
        self.state = 2;
        self.dash_phase = 0;
        self.dash_cooldown = DASH_COOLDOWN_TICKS;
        self.dash_refill_cooldown = DASH_REFILL_COOLDOWN_TICKS;
        self.dash_attack = DASH_ATTACK_TICKS;
        self.before_dash_speed = self.vel;
        self.vel = Vec2::ZERO;
        self.dash_dir = Vec2::ZERO;
        // 122566-122573: an airborne ducked dash stands up; a grounded down-push dash
        // crouches.
        if !self.on_ground && self.ducking && self.can_unduck(rooms) {
            self.ducking = false;
        } else if !self.ducking && input.down {
            self.ducking = true;
        }
        // Note: vanilla DashBegin does **not** change onGround -- a ground dash stays
        // grounded, and the next tick's head refill (119855-119863) works as usual.
        // DashBegin's `Celeste.Freeze(0.05f)` (122540): 0.05s = 3 ticks of global
        // freeze, consumed by the service (equivalent to skipping 3 simulation ticks in
        // the engine's frame rhythm).
        self.freeze_ticks = 3;
    }

    /// Vanilla `GetAimVector(Facing)` (Celeste.Input, pureCeleste.cs:110554-110600, no
    /// assist-mode branch): the direction keys compose an eight-way aim; angles within
    /// +/-pi/8 snap to an orthogonal axis; with no input -> the facing direction.
    fn aim_dir(&self, input: &Input) -> Vec2 {
        let (mx, my): (f32, f32) = (
            input.axis_x().to_f32(),
            match (input.up, input.down) {
                (true, false) => -1.0,
                (false, true) => 1.0,
                _ => 0.0,
            },
        );
        if mx == 0.0 && my == 0.0 {
            return Vec2::from_f32s(self.facing as f32, 0.0);
        }
        // Angle snapping: vanilla leaves pi/8 below each of 0/pi/+/-pi/2 (orthogonal)
        // or pi/8 - 0.05 (diagonal preferred); the equivalent implementation takes the
        // orthogonal axis when the |x| to |y| ratio >= tan(pi/8), otherwise the diagonal.
        let ax = mx.abs();
        let ay = my.abs();
        if ax >= 2.414 * ay {
            Vec2::from_f32s(mx.signum(), 0.0)
        } else if ay >= 2.414 * ax {
            Vec2::from_f32s(0.0, my.signum())
        } else {
            // Diagonal: `Math.Sign(value).SafeNormalize()` (110596).
            let inv = std::f32::consts::FRAC_1_SQRT_2;
            Vec2::from_f32s(mx.signum() * inv, my.signum() * inv)
        }
    }

    /// State 2: an explicit-phase version of `DashUpdate` (pureCeleste.cs:122598-122727)
    /// + `DashCoroutine` (122731-122831).
    ///
    /// One tick's order matches vanilla `StateMachine.Update` (12185-12201): first
    /// `DashUpdate` (the jump branch, 122627-122676), then advance the coroutine, and
    /// finally the shared MoveH/MoveV (120049-120056) -- after a wall jump / super jump
    /// rewrites the speed, the frame moves at the new speed. Phase 0 runs the
    /// coroutine's first segment (122756-122806, after `yield return null`: speed = aim
    /// x 240, DashDir, facing), phases 1..=9 wait 0.15s, and tick 10 runs the last
    /// segment (122817-122830). There is no gravity/running during a dash (`DashUpdate`
    /// does not contain them).
    fn dash_update(&mut self, input: &Input, rooms: &[Room]) {
        self.prev_pos = self.pos;

        // ---- DashUpdate (122598-122727) ----
        // The jump branch during a dash (122627-122676). A horizontal dash
        // (|DashDir.Y| < 0.1) while grounded -> SuperJump (120740); a vertical dash
        // (|DashDir.X| <= 0.2) -> SuperWallJump (120865); otherwise -> WallJump
        // (ClimbJump needs a grab, not implemented, skipped -- deviation declared).
        let jump_pressed = input.jump && !self.prev_jump;
        self.prev_jump = input.jump;
        if jump_pressed {
            self.jump_buffer = JUMP_BUFFER_FRAMES;
        }
        if self.dash_dir != Vec2::ZERO && self.jump_buffer > 0 {
            let horizontal_dash = self.dash_dir.y.abs() < Fx::from_ratio(1, 10);
            let vertical_dash = self.dash_dir.x.abs() <= Fx::from_ratio(1, 5);
            if horizontal_dash && self.on_ground {
                // 122632-122641 -> `SuperJump` (120740-120778).
                self.super_jump(rooms);
            } else if vertical_dash {
                if self.wall_jump_check(rooms, 1) {
                    self.super_wall_jump(-1);
                } else if self.wall_jump_check(rooms, -1) {
                    self.super_wall_jump(1);
                }
            } else if self.wall_jump_check(rooms, 1) {
                self.wall_jump(rooms, -1);
                self.state = 0;
            } else if self.wall_jump_check(rooms, -1) {
                self.wall_jump(rooms, 1);
                self.state = 0;
            }
        }

        // ---- DashCoroutine (122731-122831) ----
        if self.state == 2 {
            if self.dash_phase == 0 {
                // Coroutine first segment (122756-122806).
                let aim = self.aim_dir(input);
                let mut speed = aim * DASH_SPEED;
                // Horizontal overspeed retention (122770-122774): keep the original value
                // when the horizontal speed before the dash was faster and in the same
                // direction (the wall-speed semantics of `beforeDashSpeed`).
                if vel_x_same_dir(&self.before_dash_speed, speed.x)
                    && self.before_dash_speed.x.abs() > speed.x.abs()
                {
                    speed.x = self.before_dash_speed.x;
                }
                self.vel = speed;
                self.dash_dir = aim;
                if aim.x != Fx::zero() {
                    self.facing = aim.x.signum(); // 122779-122782
                }
                // The grounded down-diagonal dash converts to a duck slide on the same
                // tick, but through the collision handler: `Speed = aim * 240` is applied
                // here, `MoveV` then hits the floor and `OnCollideV`
                // (121577-121582) performs the conversion. It is implemented once, in
                // `slide_y`, so the airborne landing path gets it too.
                self.dash_phase = 1;
            } else {
                self.dash_phase += 1;
                if self.dash_phase > DASH_TICKS {
                    // Last segment (122817-122830).
                    self.auto_jump = 0; // AutoJumpTimer = 0f (122820)
                    if self.dash_dir.y <= Fx::zero() {
                        self.vel = self.dash_dir * DASH_END_SPEED;
                        if self.vel.y < Fx::zero() {
                            // 122826-122829: `if (Speed.Y < 0f) Speed.Y *= 0.75f`.
                            self.vel.y *= Fx::from_ratio(3, 4);
                        }
                    }
                    self.state = 0;
                }
            }
        }

        // ---- MoveH / MoveV (120049-120056) ----
        self.slide_x(rooms);
        self.on_ground = false;
        self.slide_y(rooms);
        if !self.on_ground {
            self.on_ground = self.hits(rooms, self.pos.x, self.pos.y + Fx::one());
        }
    }

    /// Vanilla `SuperJump()` (pureCeleste.cs:120738-120777, no super-slide/super-glide
    /// branch): horizontal 260*Facing, vertical -105, varJump filled, back to State 0.
    fn super_jump(&mut self, rooms: &[Room]) {
        self.jump_buffer = 0;
        self.coyote = 0;
        self.var_jump = VAR_JUMP_TICKS;
        self.auto_jump = 0;
        self.dash_attack = 0;
        self.vel.x = SUPER_JUMP_X * Fx::from_int(self.facing);
        self.vel.y = JUMP_SPEED;
        self.var_jump_speed = JUMP_SPEED;
        // 120753-120760: ducked super jump -- horizontal speed x1.25, vertical speed x0.5.
        if self.ducking {
            self.ducking = false;
            self.vel.x *= Fx::from_ratio(5, 4);
            self.vel.y *= Fx::from_ratio(1, 2);
        }
        self.on_ground = false;
        self.state = 0;
        let _ = rooms;
    }

    /// Vanilla `SuperWallJump(dir)` (pureCeleste.cs:120865-120900): horizontal 170*dir,
    /// vertical -160 (120877-120880, with varJumpSpeed synced), varJumpTimer = 0.25s
    /// (120870), **AutoJump = false** (120871), no forceMoveX.
    fn super_wall_jump(&mut self, dir: i32) {
        self.jump_buffer = 0;
        self.coyote = 0;
        self.var_jump = 15; // varJumpTimer = 0.25f (120870)
        self.auto_jump = 0; // AutoJump = false (120871)
        self.dash_attack = 0;
        self.vel.x = SUPER_WALLJUMP_X * Fx::from_int(dir);
        self.vel.y = SUPER_WALLJUMP_Y;
        self.var_jump_speed = SUPER_WALLJUMP_Y;
        self.on_ground = false;
        self.state = 0;
    }

    /// State 1: `ClimbUpdate` (pureCeleste.cs:122184-122362). Order: stagger decay ->
    /// stamina refill while grounded -> jump branch (reverse wall jump / ClimbJump) ->
    /// dash -> release grab on landing -> leave wall (apex ClimbHop / let-go) -> vertical
    /// climb (up -45, down 80, slip 30 when SlipCheck at the top) -> stamina drain (up
    /// 45.45/s, still 10/s) -> let go when exhausted. WallBooster / sprite sweat effect
    /// are not ported (declared).
    fn climb_update(&mut self, input: &Input, rooms: &[Room]) {
        self.prev_pos = self.pos;
        self.climb_no_move_ticks = self.climb_no_move_ticks.saturating_sub(1);
        if self.on_ground {
            self.stamina = CLIMB_MAX_STAMINA; // 122188-122191
        }
        // Jump branch (122193-122205): push the other way -> wall jump; otherwise a
        // climb jump.
        if self.jump_buffer > 0 && self.can_unduck(rooms) {
            if self.move_x == Fx::from_int(-self.facing) {
                self.wall_jump(rooms, -self.facing);
            } else {
                self.climb_jump();
            }
            self.state = 0;
            return;
        }
        // Dash (122206-122209).
        if self.dash_buffer > 0 && self.dash_cooldown == 0 && self.dashes > 0 {
            self.start_dash(input, rooms);
            return;
        }
        // Grab released (122211-122216).
        if !input.grab {
            self.state = 0;
            return;
        }
        // Past the top of the wall (122218-122231): while rising -> ClimbHop; otherwise
        // let go.
        if !self.hits(rooms, self.pos.x + Fx::from_int(self.facing), self.pos.y) {
            if self.vel.y < Fx::zero() {
                self.climb_hop(); // 122226-122230 (wallBoosting is always false)
            }
            self.state = 0;
            return;
        }
        // WallBooster is always null (there are no Core dream-wall entities) -> go
        // straight to the climb branch.
        {
            let mut num = Fx::zero();
            let mut flag = false;
            if self.climb_no_move_ticks == 0 {
                // the ClimbBlocker branch at 122224-122240 has no entity, always false.
                if input.axis_y() == -Fx::one() {
                    num = CLIMB_UP_SPEED; // -45 px/s (122220-122222)
                    if self.hits(rooms, self.pos.x, self.pos.y - Fx::one())
                        || (self.climb_hop_blocked(rooms) && self.slip_check(rooms, -Fx::one()))
                    {
                        if self.vel.y < Fx::zero() {
                            self.vel.y = Fx::zero();
                        }
                        num = Fx::zero();
                        flag = true;
                    } else if self.slip_check(rooms, Fx::zero()) {
                        self.climb_hop(); // 122232-122236: hop off the top of the wall
                        self.state = 0;
                        return;
                    }
                } else if input.axis_y() == Fx::one() {
                    num = CLIMB_DOWN_SPEED; // 80 px/s (122228-122230)
                    if self.on_ground {
                        if self.vel.y > Fx::zero() {
                            self.vel.y = Fx::zero();
                        }
                        num = Fx::zero();
                    }
                } else {
                    flag = true; // 122243-122245: no vertical input
                }
            } else {
                flag = true; // no movement during the stagger
            }
            self.last_climb_move = num.signum(); // 122251
            if flag && self.slip_check(rooms, Fx::zero()) {
                num = CLIMB_SLIP_SPEED; // 122252-122255: 30 slow descent while hanging at the top
            }
            // 122257: `Approach(Speed.Y, num, 900*dt)`.
            approach(&mut self.vel.y, num, GRAVITY);
        }
        // 122260-122264: no longer pushing down and the wall has ended below the front ->
        // stop.
        if input.axis_y() != Fx::one()
            && self.vel.y > Fx::zero()
            && !self.hits(
                rooms,
                self.pos.x + Fx::from_int(self.facing),
                self.pos.y + Fx::one(),
            )
        {
            self.vel.y = Fx::zero();
        }
        // Stamina (122266-122300): up 45.454544/s, still 10/s; let go when exhausted.
        if self.climb_no_move_ticks == 0 {
            if self.last_climb_move == -1 {
                self.stamina -= CLIMB_UP_DRAIN_PER_TICK;
            } else if self.last_climb_move == 0 {
                self.stamina -= CLIMB_IDLE_DRAIN_PER_TICK;
            }
        }
        if self.stamina <= Fx::zero() {
            self.state = 0; // 122302-122306
            return;
        }
        self.slide_x(rooms);
        self.on_ground = false;
        self.slide_y(rooms);
        if !self.on_ground {
            self.on_ground = self.hits(rooms, self.pos.x, self.pos.y + Fx::one());
        }
    }

    /// The reachable part of vanilla `ClimbHopBlockedCheck()` (122409-122424): a solid
    /// 6px above the head -> the hop is blocked (no Follower/LedgeBlocker entities).
    fn climb_hop_blocked(&self, rooms: &[Room]) -> bool {
        self.hits(rooms, self.pos.x, self.pos.y - Fx::from_int(6))
    }

    /// Vanilla `UpdateHair` (pureCeleste.cs:120247-120298): dash spent -> ease into USED
    /// at 6/s; at the instant of refill (lastDashes != Dashes) -> FLASH +
    /// `hairFlashTimer` 0.12s; stay FLASH during it; otherwise pick TWO_DASHES/NORMAL by
    /// the dash count. State 19 (star-fly) and `OverrideHairColor` are not implemented.
    fn update_hair_color(&mut self) {
        if self.dashes == 0 && self.dashes < self.max_dashes {
            lerp_color(
                &mut self.hair_color_current,
                crate::sprites::hair_color::USED,
                HAIR_LERP_PER_TICK,
            );
        } else if self.last_dashes != self.dashes {
            self.hair_color_current = crate::sprites::hair_color::FLASH;
            self.hair_flash = HAIR_FLASH_TICKS;
        } else if self.hair_flash > 0 {
            self.hair_flash -= 1;
        } else if self.dashes == 2 {
            self.hair_color_current = crate::sprites::hair_color::TWO_DASHES;
        } else {
            self.hair_color_current = crate::sprites::hair_color::NORMAL;
        }
        self.last_dashes = self.dashes; // 120295
    }

    /// Vanilla `Sprite.Play(id, restart = false)` (pureCeleste.cs:10483-10503): the same
    /// id is a no-op (pureCeleste.cs:10485 `(CurrentAnimationID != id) | restart`); on a
    /// switch, `LastAnimationID` records the **new** id (pureCeleste.cs:10491-10492;
    /// `(CurrentAnimationID = id)` evaluates to `id`), `Animating = Delay > 0f`
    /// (pureCeleste.cs:10494 `Animating = currentAnimation.Delay > 0f`), and the timer
    /// and frame reset to zero. A missing table / unknown id is still a no-op (rendering
    /// falls back to static frames).
    pub fn play(&mut self, table: &crate::sprites::AnimationTable, id: &str) {
        if self.anim_id == id {
            return;
        }
        let Some(def) = table.sprites.get("player") else {
            return;
        };
        // Dictionary keys are case-insensitive (Monocle `OrdinalIgnoreCase`, 10206; the
        // XML has `climbup` while `UpdateSprite` plays `climbUp`); but CurrentAnimationID
        // stores the **argument** (120274 behavior), so the no-op comparison uses the
        // string as-is.
        let Some(anim) = def.anims.get(&id.to_ascii_lowercase()) else {
            return;
        };
        // `string lastAnimationID = (CurrentAnimationID = id); LastAnimationID =
        // lastAnimationID;` (10491-10492): a C# assignment expression evaluates to the
        // **new** id, so `LastAnimationID` ends up holding the animation just played, not
        // the previous one. `Running` (111375-111388) reads it as "the most recently
        // played animation is a run*". Recording the old id instead leaves `Running` true
        // after a stop -> idle, so the `!Sprite.Running` guard (120447) never fires again
        // and the character slides at full speed holding the idle pose.
        self.last_anim_id = id.to_string(); // pureCeleste.cs:10491-10492
        self.anim_id = id.to_string();
        self.anim_frame = 0;
        self.anim_timer = 0.0;
        self.anim_playing = anim.delay > 0.0; // 10493 `Animating = Delay > 0f`
    }

    /// Vanilla `Sprite.Update` (10170-10308): advance the timer by the frame interval,
    /// loop back to 0 for looping animations, and stop at the last frame for non-looping
    /// ones.
    pub fn update_animation(&mut self, table: &crate::sprites::AnimationTable) {
        let Some(def) = table.sprites.get("player") else {
            return;
        };
        let Some(anim) = def.anims.get(&self.anim_id.to_ascii_lowercase()) else {
            return;
        };
        if !self.anim_playing || anim.delay <= 0.0 {
            return;
        }
        // 10234-10243: `animationTimer += DeltaTime` (Rate = 1); only advance a frame
        // when `|timer| >= Delay`, then subtract Delay from the timer.
        self.anim_timer += 1.0 / 60.0;
        if self.anim_timer < anim.delay {
            return;
        }
        self.anim_timer -= anim.delay;
        self.anim_frame += 1;
        if self.anim_frame >= anim.frames.len() {
            // 10278-10305.
            match &anim.goto {
                Some(next) => {
                    // Goto branch (pureCeleste.cs:10260-10282):
                    // `CurrentAnimationID = currentAnimation.Goto.Choose()` (10262), and it
                    // writes the **new** id into `LastAnimationID`
                    // (pureCeleste.cs:10267 `LastAnimationID = CurrentAnimationID` --
                    // **different** from `play`'s "record the old id" rule); the frame
                    // resets to 0 (10275) and the timer and `Animating` are untouched.
                    let key = next.to_ascii_lowercase();
                    if !table
                        .sprites
                        .get("player")
                        .is_some_and(|d| d.anims.contains_key(&key))
                    {
                        return;
                    }
                    self.anim_frame = 0;
                    self.anim_id = next.clone();
                    self.last_anim_id = next.clone();
                }
                None => {
                    // pureCeleste.cs:10283-10302: `Animating = false`, frame stays at the
                    // last frame, `CurrentAnimationID` cleared (texture not reset).
                    self.anim_frame = anim.frames.len() - 1;
                    self.anim_playing = false;
                    self.anim_id.clear();
                }
            }
        }
    }

    /// The atlas texture name for the current frame (missing table/frame -> `None`;
    /// rendering falls back to static frames).
    pub fn anim_frame_texture(&self, table: &crate::sprites::AnimationTable) -> Option<String> {
        let def = table.sprites.get("player")?;
        let anim = def.anims.get(&self.anim_id.to_ascii_lowercase())?;
        anim.frames.get(self.anim_frame).cloned()
    }

    /// Per-frame hair metadata for the current animation frame (vanilla
    /// `PlayerSprite.HasHair` / `HairOffset` / `HairFrame`, pureCeleste.cs:111351-111371),
    /// which `PlayerHair.Render` reads. A missing animation or frame yields the vanilla
    /// defaults (no hair) rather than a guessed offset.
    pub fn anim_frame_hair(
        &self,
        table: &crate::sprites::AnimationTable,
    ) -> crate::sprites::HairFrame {
        let Some(def) = table.sprites.get("player") else {
            return crate::sprites::HairFrame::default();
        };
        let Some(anim) = def.anims.get(&self.anim_id.to_ascii_lowercase()) else {
            return crate::sprites::HairFrame::default();
        };
        anim.hair_at(self.anim_frame)
    }

    /// Vanilla `PlayerSprite.Running` (pureCeleste.cs:111375-111388): the **previous**
    /// animation (`LastAnimationID`) starts with `run`, or is exactly `flip`.
    ///
    /// It is **not** "the current animation has a goto" -- `idle`/`edge`/`walk` all
    /// carry `<Loop>`, and treating it that way misreads "already running" as true, so
    /// starting from idle would never play `runSlow` / `runFast` and the character would
    /// merely **slide** (walking with no leg animation).
    fn running(&self) -> bool {
        self.last_anim_id == "flip" || self.last_anim_id.starts_with("run")
    }

    /// The reachable branches of vanilla `Player.UpdateSprite`
    /// (pureCeleste.cs:120300-120508) (no held item / wind / climb / dream / space /
    /// duck -- deviation declared in the module docs): dash attack window -> skid/dash;
    /// grounded -> push/edge/edgeBack/lookUp/idle/skid/flip/runSlow/runFast; airborne ->
    /// jumpSlow/jumpFast/fallSlow/fallFast.
    pub fn update_sprite(
        &mut self,
        table: &crate::sprites::AnimationTable,
        input: &Input,
        rooms: &[Room],
    ) {
        let dash_attacking = self.dash_attack > 0;
        if dash_attacking {
            if self.on_ground
                && self.dash_dir.y == Fx::zero()
                && self.vel.x != Fx::zero()
                && self.move_x == Fx::from_int(-self.vel.x.signum())
            {
                self.play(table, "skid");
            } else {
                self.play(table, "dash");
            }
        } else if self.state == 1 {
            // 120365-120383: climb animations (lastClimbMove / dangling / look back).
            if self.last_climb_move < 0 {
                self.play(table, "climbUp"); // XML id `climbup`; case-insensitive
            } else if self.last_climb_move > 0 {
                self.play(table, "wallslide");
            } else if !self.hits(
                rooms,
                self.pos.x + Fx::from_int(self.facing * 6),
                self.pos.y,
            ) {
                self.play(table, "dangling"); // 120371-120373: probe at (Facing, 6)
            } else if self.move_x == Fx::from_int(-self.facing) {
                self.play(table, "climbLookBackStart");
            } else {
                self.play(table, "wallslide");
            }
        } else if self.ducking && self.state == 0 {
            self.play(table, "duck"); // 120408-120412
        } else if self.state == 0 {
            if self.on_ground {
                self.fast_jump = false;
                // pureCeleste.cs:120398-120400: push against a wall (`Holding == null &&
                // moveX != 0 && CollideCheck<Solid>(Position + UnitX * moveX)`; vanilla also
                // has `ClimbBlocker.EdgeCheck`, but this engine has no ClimbBlocker yet).
                if self.move_x != Fx::zero()
                    && self.hits(rooms, self.pos.x + self.move_x, self.pos.y)
                {
                    self.play(table, "push");
                } else if self.vel.x.abs() <= IDLE_SPEED && self.move_x == Fx::zero() {
                    // pureCeleste.cs:120402 `Math.Abs(Speed.X) <= 25f && moveX == 0`.
                    let front = self.hits(
                        rooms,
                        self.pos.x + Fx::from_int(self.facing),
                        self.pos.y + Fx::from_int(2),
                    ) || self.hits(
                        rooms,
                        self.pos.x + Fx::from_int(self.facing * 4),
                        self.pos.y + Fx::from_int(2),
                    );
                    let back = self.hits(
                        rooms,
                        self.pos.x - Fx::from_int(self.facing),
                        self.pos.y + Fx::from_int(2),
                    ) || self.hits(
                        rooms,
                        self.pos.x - Fx::from_int(self.facing * 4),
                        self.pos.y + Fx::from_int(2),
                    );
                    if !front {
                        self.play(table, "edge"); // pureCeleste.cs:120408-120410
                    } else if !back {
                        self.play(table, "edgeBack"); // pureCeleste.cs:120412-120414
                    } else if input.up {
                        // pureCeleste.cs:120418 `Sprite.LastAnimationID != "lookUp"` --
                        // vanilla replays only then (lookUp does not loop, and after it ends
                        // CurrentAnimationID is cleared; replaying unconditionally would stay
                        // on frame 0 forever).
                        if self.last_anim_id != "lookUp" {
                            self.play(table, "lookUp");
                        }
                    } else if !self.anim_id.is_empty() && !self.anim_id.contains("idle") {
                        // pureCeleste.cs:120423 `Sprite.CurrentAnimationID.Contains("idle")`
                        // -- the full condition is: `CurrentAnimationID != null &&
                        // (!Contains("idle") || (== "idle_carry" && Holding == null))`
                        // (120423-120425).
                        self.play(table, "idle");
                    }
                } else if self.move_x != Fx::zero() && self.vel.x.signum() == -self.move_x.signum()
                {
                    // pureCeleste.cs:120432-120440: opposite to the input and still sliding
                    // (`Math.Sign(Speed.X) == -moveX && moveX != 0`, 120432).
                    if self.vel.x.abs() > RUN {
                        self.play(table, "skid");
                    } else if self.anim_id != "skid" {
                        self.play(table, "flip");
                    }
                } else if !self.running() {
                    // pureCeleste.cs:120447 `!Sprite.Running` -- the full condition also has
                    // `CurrentAnimationID == "runWind" || (== "runSlow_carry" &&
                    // Holding == null)`; the latter two are wind / held item, not implemented
                    // in this engine (120447-120455).
                    if self.vel.x.abs() < Fx::from_ratio(3, 4) {
                        self.play(table, "runSlow");
                    } else {
                        self.play(table, "runFast");
                    }
                }
            } else if self.vel.y < Fx::zero() {
                if self.fast_jump || self.vel.x.abs() > Fx::from_ratio(3, 2) {
                    self.fast_jump = true;
                    self.play(table, "jumpFast");
                } else {
                    self.play(table, "jumpSlow");
                }
            } else if self.fast_jump || self.vel.y >= Fx::from_ratio(8, 3) {
                self.fast_jump = true;
                // pureCeleste.cs:120486 `Sprite.LastAnimationID != "fallFast"`.
                if self.last_anim_id != "fallFast" {
                    self.play(table, "fallFast");
                }
            } else {
                self.play(table, "fallSlow");
            }
        }
    }

    /// State 0 (NormalUpdate, pureCeleste.cs:121824-122101).
    ///
    /// The within-tick order mirrors vanilla: grab/duck (not implemented, see the
    /// deviation declaration) -> dash trigger (121841-121843 `if (CanDash)`) -> running
    /// (121965-121975) -> maxFall (121977-121987) -> gravity (122027-122040, airborne
    /// only) -> varJump clamp (122042-122048) -> jump decision (122053-122100, including
    /// the wall jump) -> movement solve -> coyote/buffer decay.
    fn normal_update(&mut self, input: &Input, rooms: &[Room]) {
        self.prev_pos = self.pos;

        // Wall grab (the GrabCheck branch at 121832-121864, no pickups): probe when the
        // horizontal speed is not away from the wall and vertical speed >= 0; when it is
        // 1-2px short, vanilla lifts the player with MoveVExact. `ClimbTrigger` is a
        // write-only field in vanilla (118714/119688/121267, with no consumer), skipped.
        // No grab while ducking (121832 `!Ducking`).
        if input.grab && !self.ducking && self.vel.y >= Fx::zero() {
            let behind = self.vel.x.signum() == -self.facing && self.vel.x != Fx::zero();
            if !behind {
                if self.climb_check(rooms, self.facing, 0) {
                    self.ducking = false;
                    self.enter_climb(rooms);
                    return;
                }
                for i in 1..=2i32 {
                    // 121854-121862: when 1-2px short, lift, then grab.
                    if !self.hits(rooms, self.pos.x, self.pos.y - Fx::from_int(i))
                        && self.climb_check(rooms, self.facing, -i)
                    {
                        self.pos.y -= Fx::from_int(i);
                        self.ducking = false;
                        self.enter_climb(rooms);
                        return;
                    }
                }
            }
        }

        // Duck enter/exit (121996-122008): press down while grounded to duck; standing up
        // needs CanUnDuck, and when blocked with zero horizontal speed, vanilla probes a
        // 4px crawl position (122001).
        if self.ducking {
            if self.on_ground && input.axis_y() != Fx::one() {
                if self.can_unduck(rooms) {
                    self.ducking = false;
                } else if self.vel.x == Fx::zero() {
                    for i in (1..=4i32).rev() {
                        let px = Fx::from_int(i);
                        if self.can_unduck_at(rooms, self.pos.x + px) {
                            self.pos.x += Fx::from_ratio(5, 6); // MoveH(50*dt) (122004)
                            break;
                        }
                        if self.can_unduck_at(rooms, self.pos.x - px) {
                            self.pos.x -= Fx::from_ratio(5, 6);
                            break;
                        }
                    }
                }
            }
        } else if self.on_ground && input.axis_y() == Fx::one() && self.vel.y >= Fx::zero() {
            self.ducking = true; // 122004-122008
        }

        // Dash trigger (121841-121843 + `CanDash` 119325-119340): the dash button was
        // pressed (within the buffer window), the cooldown has ended, and dashes remain.
        let dash_pressed = input.dash && !self.prev_dash;
        self.prev_dash = input.dash;
        if dash_pressed {
            self.dash_buffer = JUMP_BUFFER_FRAMES as u16; // VirtualButton.BufferTime, likewise 0.08s (110376)
        }
        if self.dash_buffer > 0 && self.dash_cooldown == 0 && self.dashes > 0 {
            self.start_dash(input, rooms); // StartDash (122469-122479) -> State 2
            return; // speed is 0 this tick (DashBegin zeroes it); go straight to movement
        }

        // Horizontal: vanilla's two branches (121965-121975) -- same-direction overspeed
        // (|vx| > 90 and the same sign as the input) uses the 400 reduction; everything
        // else (no input, opposite braking, starting) uses the full 1000 rate. In the air
        // both are multiplied by 0.65.
        let move_x = self.move_x;
        let mult = if self.on_ground { Fx::one() } else { AIR_MULT };
        let target = move_x * RUN;
        let overspeed = move_x != Fx::zero()
            && self.vel.x.abs() > RUN
            && self.vel.x.signum() == move_x.signum();
        let rate = if overspeed {
            RUN_REDUCE * mult
        } else {
            RUN_ACCEL * mult
        };
        approach(&mut self.vel.x, target, rate);
        // Facing follows input, not speed (119908-119917): holding into a wall leaves
        // speed at 0, so facing must still point at the wall for the jump boost direction
        // to be right.
        if move_x != Fx::zero() {
            self.facing = move_x.signum();
        }
        // Grounded duck braking (122047-122049): `Approach(Speed.X, 0, 500*dt)`.
        if self.ducking && self.on_ground {
            approach(&mut self.vel.x, Fx::zero(), DUCK_DECEL);
        }

        // The fall-speed cap is an **eased state**: holding "down" while already
        // accelerating downward -> the cap ramps toward fast fall; otherwise it eases back
        // to normal fall speed. Rate 300 px/s^2. Reference pureCeleste:
        // `maxFall = Approach(maxFall, 240, 300*dt)`.
        if input.down && self.vel.y >= MAX_FALL {
            approach(&mut self.max_fall, FAST_MAX_FALL, FAST_MAX_ACCEL);
        } else {
            approach(&mut self.max_fall, MAX_FALL, FAST_MAX_ACCEL);
        }

        // Gravity: applied only in the air (122040). Near the apex (|vy| < 40 px/s) while
        // holding jump (or AutoJump) -> gravity is halved (num7 at 122031).
        // Wall slide (122105-122139): pushing into the wall (or holding grab with no
        // horizontal input) while falling -> the target speed eases from 160 to 20
        // (`Lerp(160, 20, wallSlideTimer/1.2)`, WALLSLIDE_FAST/SLOW). The wall slide
        // direction is re-armed by the condition every tick.
        if !self.on_ground {
            let mut target = self.max_fall;
            let wall_ahead = self.hits(rooms, self.pos.x + Fx::from_int(self.facing), self.pos.y);
            if (self.move_x == Fx::from_int(self.facing)
                || (self.move_x == Fx::zero() && input.grab))
                && input.axis_y() != Fx::one()
                && self.vel.y >= Fx::zero()
                && self.wall_slide_ticks > 0
                && wall_ahead
            {
                self.wall_slide_dir = self.facing;
            }
            if self.wall_slide_dir != 0 {
                let t = self.wall_slide_ticks as f32 / WALLSLIDE_TICKS as f32;
                let lerp = WALLSLIDE_FAST + (WALLSLIDE_SLOW - WALLSLIDE_FAST) * Fx::from_f32(t);
                target = lerp;
            }
            let half = (input.jump || self.auto_jump > 0) && self.vel.y.abs() < HALF_GRAV_THRESHOLD;
            let rate = if half { HALF_GRAVITY } else { GRAVITY };
            approach(&mut self.vel.y, target, rate);
        }

        // varJump clamp (122042-122048): while the timer is running and jump is held ->
        // vertical speed is clamped back to the jump value (gravity cancelled); on
        // release -> the timer is zeroed. This is "variable jump height", with no extra
        // release speed cut.
        if self.var_jump > 0 {
            if input.jump || self.auto_jump > 0 {
                // 122134: `Speed.Y = Math.Min(Speed.Y, varJumpSpeed)`.
                self.vel.y = self.vel.y.min(self.var_jump_speed);
            } else {
                self.var_jump = 0;
            }
        }

        // Jump input edge: on press -> fill the buffer (VirtualButton.BufferTime, 110376;
        // buffer semantics 16824-16840).
        let jump_pressed = input.jump && !self.prev_jump;
        self.prev_jump = input.jump;
        if jump_pressed {
            self.jump_buffer = JUMP_BUFFER_FRAMES;
        }

        // Jump decision (122090-122101): inside the buffer window -> grounded/coyote ->
        // `Jump()` (120694-120716: vertical -105, horizontal +40*moveX (JumpHBoost),
        // varJump filled to 0.2s); airborne -> wall jump (122057-122100,
        // `WallJumpCheck` 120779-120804 -> `WallJump` 120806-120860).
        if self.jump_buffer > 0 {
            if self.on_ground || self.coyote > 0 {
                self.jump(move_x);
            } else if self.wall_jump_check(rooms, 1) {
                self.wall_jump(rooms, -1);
            } else if self.wall_jump_check(rooms, -1) {
                self.wall_jump(rooms, 1);
            }
        }

        self.slide_x(rooms);
        self.on_ground = false;
        self.slide_y(rooms);
        if !self.on_ground {
            self.on_ground = self.hits(rooms, self.pos.x, self.pos.y + Fx::one());
        }
        // Stand up in the air (120058-120060): falling with nothing overhead -> cancel
        // ducking.
        if !self.on_ground && self.vel.y > Fx::zero() && self.coyote == 0 && self.can_unduck(rooms)
        {
            self.ducking = false;
        }
        // Coyote time + jump buffer expiry: refresh coyote while grounded; once airborne
        // both decay. The buffer can only fire on "the frame after landing" (the grounded
        // state is updated at the end of the tick), so the grounded frame does not decay
        // it, avoiding eating the landing margin.
        if self.on_ground {
            self.coyote = COYOTE_FRAMES;
        } else {
            if self.coyote > 0 {
                self.coyote -= 1;
            }
            self.jump_buffer = self.jump_buffer.saturating_sub(1);
        }
    }

    /// Whether the hitbox intersects a solid in **any** touched room; `Some` returns the
    /// blocking room.
    ///
    /// When the hitbox straddles a room seam (<=8px) both rooms must be checked -- with
    /// only the room containing the center, the head drives into the neighboring room's
    /// solid tile first and it is only noticed once the center crosses the seam, leaving
    /// the player stuck in the wall (0-Intro's `lvl_0` ceiling opening sits directly
    /// above `lvl_0b`'s solid floor, and that is exactly how it traps the player).
    ///
    /// Rooms convert world cells back to local cells using their own origin; a room whose
    /// rectangle does not intersect the hitbox's cell range is skipped (flush contact
    /// does not count as intersecting).
    /// Current hitbox size: while ducking 8x6 with the top-left moved down 5px (its
    /// bottom edge aligns with the normal 8x11; `duckHitbox = new Hitbox(8f, 6f, -4f,
    /// -6f)`, 118858).
    fn box_dims(&self) -> (i32, i32, i32) {
        if self.ducking {
            (PLAYER_W_PX, DUCK_H, DUCK_Y_OFF)
        } else {
            (PLAYER_W_PX, PLAYER_H_PX, 0)
        }
    }

    /// Whether the **current** hitbox intersects a solid at `(x, y)`. `y` is the normal
    /// hitbox's top-left y; while ducking the probe box is moved down 5px (bottom edge
    /// aligned).
    fn hit_in<'r>(&self, rooms: &'r [Room], x: Fx, y: Fx) -> Option<&'r Room> {
        let (w, h, y_off) = self.box_dims();
        let y = y + Fx::from_int(y_off);
        let right = x + Fx::from_int(w) - Fx::from_bits(1);
        let bottom = y + Fx::from_int(h) - Fx::from_bits(1);
        let (cx0, cx1) = (cell(x), cell(right));
        let (cy0, cy1) = (cell(y), cell(bottom));
        for room in rooms {
            let (ox, oy) = (room.bounds.x.round() as i32, room.bounds.y.round() as i32);
            let cols = (room.bounds.w.round() as i32).div_euclid(8);
            let rows = (room.bounds.h.round() as i32).div_euclid(8);
            let (gx0, gy0) = (ox.div_euclid(8), oy.div_euclid(8));
            let lx0 = cx0.max(gx0);
            let ly0 = cy0.max(gy0);
            let lx1 = cx1.min(gx0 + cols - 1);
            let ly1 = cy1.min(gy0 + rows - 1);
            if lx0 > lx1 || ly0 > ly1 {
                continue; // this room is not in the hitbox's cell range at all
            }
            for cy in ly0..=ly1 {
                for cx in lx0..=lx1 {
                    if Session::solid_cell(room, cx, cy) {
                        return Some(room);
                    }
                }
            }
        }
        None
    }

    /// Whether the current hitbox intersects a solid (the boolean view of
    /// [`Player::hit_in`]).
    pub(crate) fn hits(&self, rooms: &[Room], x: Fx, y: Fx) -> bool {
        self.hit_in(rooms, x, y).is_some()
    }

    /// Advance along X, snapping to the outside edge of that column on hitting a solid.
    fn slide_x(&mut self, rooms: &[Room]) {
        let mut dx = self.vel.x;
        while dx != Fx::zero() {
            let step = clamp_step(dx);
            let nx = self.pos.x + step;
            if self.hits(rooms, nx, self.pos.y) {
                self.pos.x = edge_x(nx, step);
                self.vel.x = Fx::zero();
                return;
            }
            self.pos.x = nx;
            dx -= step;
        }
    }

    /// Advance along Y, snapping to the outside edge of that row on hitting a solid and
    /// recording the landing.
    fn slide_y(&mut self, rooms: &[Room]) {
        let mut dy = self.vel.y;
        while dy != Fx::zero() {
            let step = clamp_step(dy);
            let ny = self.pos.y + step;
            if self.hits(rooms, self.pos.x, ny) {
                self.pos.y = edge_y(ny, step, self.box_dims().2);
                if step > Fx::zero() {
                    self.on_ground = true;
                    // Vanilla `OnCollideV` (121577-121582): a **down-diagonal dash that
                    // lands** becomes a horizontal one -- `DashDir.Y = 0`, `Speed.Y = 0`,
                    // `Speed.X *= 1.2`, `Ducking = true`. This is what makes the hyper
                    // reachable from the air as well as from the ground: the dash-state
                    // jump branch only SuperJumps for `|DashDir.Y| < 0.1`
                    // (122632-122641), and a down-diagonal dash never satisfies that
                    // until it lands. Regressing this made the hyper unmatchable
                    // (a plain `Jump()` at 169.7 + 40 instead of 325 hspeed).
                    if self.dash_dir.x != Fx::zero()
                        && self.dash_dir.y > Fx::zero()
                        && self.vel.y > Fx::zero()
                    {
                        self.dash_dir =
                            Vec2::new(Fx::from_int(self.dash_dir.x.signum()), Fx::zero());
                        self.vel.y = Fx::zero();
                        self.vel.x *= Fx::from_ratio(6, 5);
                        self.ducking = true;
                    }
                }
                self.vel.y = Fx::zero();
                return;
            }
            self.pos.y = ny;
            dy -= step;
        }
    }

    /// World-level fallback: clamp only when the center is not in **any** room.
    ///
    /// Previously it unconditionally clamped the player into the rectangle of the "room
    /// containing the center" -- at a flush room edge the center had just crossed the
    /// seam, and the clamp yanked the player into the neighboring room (~4px teleport).
    /// Now walking along a seam is continuous; only when the player truly falls out of
    /// the world (a gap between rooms, a hot reload shrinking a room) is it caught by the
    /// **nearest** room, and it is never yanked to the spawn room.
    fn clamp_to_world(&mut self, rooms: &[Room]) {
        if room_containing(rooms, self.pos).is_some() {
            return;
        }
        let centre = self.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H * 0.5);
        let b = &nearest_room(rooms, centre).bounds;
        let min_x = Fx::from_f32(b.x);
        let max_x = Fx::from_f32(b.x + b.w - PLAYER_W);
        let min_y = Fx::from_f32(b.y);
        let max_y = Fx::from_f32(b.y + b.h - PLAYER_H);

        if self.pos.x < min_x {
            self.pos.x = min_x;
            self.vel.x = Fx::zero();
        } else if self.pos.x > max_x {
            self.pos.x = max_x;
            self.vel.x = Fx::zero();
        }
        if self.pos.y < min_y {
            self.pos.y = min_y;
            self.vel.y = Fx::zero();
        } else if self.pos.y > max_y {
            self.pos.y = max_y;
            self.vel.y = Fx::zero();
            self.on_ground = true;
        }
    }
}

/// Clamp one step to `+/-STEP`, without exceeding the remaining displacement.
fn clamp_step(d: Fx) -> Fx {
    if d > Fx::zero() {
        d.min(STEP)
    } else {
        d.max(-STEP)
    }
}

/// Push `v` toward `target` at at most `rate` per tick, never overshooting.
///
/// The core of the acceleration curve: `v` moves toward `target` but at most `rate` per
/// step, so starting is gradual; conversely it also brakes when `target` becomes
/// reverse.
fn approach(v: &mut Fx, target: Fx, rate: Fx) {
    let diff = target - *v;
    if diff.abs() <= rate {
        *v = target;
    } else {
        *v += Fx::from_int(diff.signum()) * rate;
    }
}

/// Snap the hitbox to the outside edge of the column it just entered on hitting a wall.
///
/// `cell()` yields **world** cell indices, so the snap edge is a world coordinate too --
/// with no room origin mixed in. The old implementation treated world cell indices as
/// room-local indices and then added the origin back: hitting a wall in `lvl_-1`
/// (ox=-368) bounced it 368px west, landing in `lvl_2`/`lvl_3` (oy=-88) bounced it 88px
/// up -- any room with a non-zero origin teleported hugely on touching a wall.
///
/// Subtract straight to the cell boundary rather than stopping at half a pixel --
/// otherwise the 1/2px step leaves the character forever a hair short of the ground.
fn edge_x(nx: Fx, step: Fx) -> Fx {
    if step > Fx::zero() {
        let right = nx + Fx::from_int(PLAYER_W_PX) - Fx::from_bits(1);
        Fx::from_int(cell(right) * 8 - PLAYER_W_PX)
    } else {
        Fx::from_int(cell(nx) * 8 + 8)
    }
}

/// Snap the hitbox to the outside edge of the row it just entered (a world cell edge) on
/// hitting the ground or a ceiling. `y_off` is the duck probe box's downward shift: the
/// bottom edge is unchanged (5+6 = 11), but the top edge must snap per the box top
/// (ny + y_off), otherwise a ducked jump passes into the ceiling cell.
fn edge_y(ny: Fx, step: Fx, y_off: i32) -> Fx {
    if step > Fx::zero() {
        let bottom = ny + Fx::from_int(y_off + PLAYER_H_PX) - Fx::from_bits(1);
        Fx::from_int(cell(bottom) * 8 - PLAYER_H_PX)
    } else {
        let top = ny + Fx::from_int(y_off);
        Fx::from_int(cell(top) * 8 + 8) - Fx::from_int(y_off)
    }
}

/// World coordinate -> world cell index (arithmetic shift right = floor, correct for
/// negatives too).
fn cell(v: Fx) -> i32 {
    (v.bits() >> 16).div_euclid(8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use releste_map::{RectDef, Tileset};

    /// A 4x4 cell room whose last row is the floor.
    fn floor_room() -> Room {
        let mut tiles = Tileset::new("Gameplay", 4, 4);
        for x in 0..4 {
            tiles.set(x, 3, 1);
        }
        Room {
            name: "t".into(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: 32.0,
                h: 32.0,
            },
            tiles,
            bg: Tileset::new("Gameplay", 4, 4),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
            ..Default::default()
        }
    }

    #[test]
    fn axis_x_ignores_simultaneous_directions() {
        let mut i = Input {
            left: true,
            ..Input::default()
        };
        assert_eq!(i.axis_x(), -Fx::one());
        i.right = true;
        assert_eq!(i.axis_x(), Fx::zero());
    }

    #[test]
    fn player_lands_on_the_floor() {
        let room = floor_room();
        let mut p = Player::new(Vec2::from_f32s(8.0, 0.0));
        for _ in 0..120 {
            p.step(&Input::default(), std::slice::from_ref(&room));
        }
        assert!(p.on_ground, "player never landed; pos = {:?}", p.pos);
        // The floor is row 3 (y = 24..32) and the hitbox is 11 tall (118856) -> the top
        // edge stops at y = 13.
        assert_eq!(p.pos.y.to_f32(), 13.0, "hitbox must snap onto the floor");
        assert_eq!(p.vel.y, Fx::zero());
    }

    #[test]
    fn player_walks_into_a_wall_and_stops() {
        let mut room = floor_room();
        // Raise a wall in column 2 (x = 16..24)
        for y in 0..3 {
            room.tiles.set(2, y, 1);
        }
        let mut p = Player::new(Vec2::from_f32s(0.0, 16.0));
        let i = Input {
            right: true,
            ..Input::default()
        };
        for _ in 0..60 {
            p.step(&i, std::slice::from_ref(&room));
        }
        assert_eq!(p.pos.x.to_f32(), 8.0, "must stop with the hitbox flush");
        assert_eq!(p.vel.x, Fx::zero());
    }

    #[test]
    fn player_never_leaves_the_room() {
        let room = floor_room();
        let mut p = Player::new(Vec2::from_f32s(400.0, 400.0));
        for _ in 0..60 {
            p.step(&Input::default(), std::slice::from_ref(&room));
        }
        assert!(p.pos.x.to_f32() <= 32.0 - PLAYER_W);
        assert!(p.pos.y.to_f32() <= 32.0 - PLAYER_H);
    }

    #[test]
    fn jump_lifts_the_player() {
        let room = floor_room();
        // Standing on the row-3 floor (top = 24) -> the hitbox top edge is y = 13 (11
        // tall, 118856).
        let mut p = Player::new(Vec2::from_f32s(8.0, 13.0));
        p.on_ground = true;
        let i = Input {
            jump: true,
            ..Input::default()
        };
        let start = p.pos.y;
        p.step(&i, std::slice::from_ref(&room));
        assert!(p.pos.y < start, "jump should move the player up");
        assert!(!p.on_ground);
    }

    #[test]
    fn cell_uses_floor_division() {
        assert_eq!(cell(Fx::from_int(0)), 0);
        assert_eq!(cell(Fx::from_int(8)), 1);
        assert_eq!(cell(Fx::from_int(-1)), -1);
        assert_eq!(cell(Fx::from_int(-8)), -1);
        assert_eq!(cell(Fx::from_int(-9)), -2);
    }

    /// An 8x8 cell room whose floor is only the right half (columns 4..8), with an empty
    /// abyss on the left.
    ///
    /// Walking left this way lets the coyote test happen exactly at the instant of
    /// "leaving support", with no solid on the left to catch the player.
    fn ledge_room() -> Room {
        let mut tiles = Tileset::new("Gameplay", 8, 8);
        for x in 4..8 {
            tiles.set(x, 3, 1);
        }
        Room {
            name: "t".into(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: 64.0,
                h: 64.0,
            },
            tiles,
            bg: Tileset::new("Gameplay", 8, 8),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
            ..Default::default()
        }
    }

    /// An 8-cell-wide, 16-cell-tall room with a full floor row at row 15 (y = 120..128).
    ///
    /// The height matters: a full jump rises about 30px, and a short room's clamp would
    /// truncate the height and distort the variable-jump-height comparison.
    fn tall_room() -> Room {
        let mut tiles = Tileset::new("Gameplay", 8, 16);
        for x in 0..8 {
            tiles.set(x, 15, 1);
        }
        Room {
            name: "t".into(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: 64.0,
                h: 128.0,
            },
            tiles,
            bg: Tileset::new("Gameplay", 8, 16),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
            ..Default::default()
        }
    }

    /// A 32-cell-wide, 16-cell-tall room with a full floor row at row 15 (y = 120..128).
    ///
    /// The width matters: the run-speed / braking tests need to reach acceleration
    /// convergence without hitting the right wall.
    fn wide_room() -> Room {
        let mut tiles = Tileset::new("Gameplay", 32, 16);
        for x in 0..32 {
            tiles.set(x, 15, 1);
        }
        Room {
            name: "t".into(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: 256.0,
                h: 128.0,
            },
            tiles,
            bg: Tileset::new("Gameplay", 32, 16),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
            ..Default::default()
        }
    }

    /// Place a player on a solid floor and run until it settles (`x` must land on a floor
    /// column).
    fn settled(room: &Room, x: f32) -> Player {
        let mut p = Player::new(Vec2::from_f32s(x, 8.0));
        for _ in 0..60 {
            p.step(&Input::default(), std::slice::from_ref(room));
        }
        assert!(p.on_ground, "fixture: player must settle on the floor");
        p
    }

    /// Walk left from the right half of the ledge room's floor and return the frame that
    /// just left support.
    fn walk_off_ledge(room: &Room) -> Player {
        let mut p = settled(room, 40.0);
        let walk = Input {
            left: true,
            ..Input::default()
        };
        for _ in 0..120 {
            p.step(&walk, std::slice::from_ref(room));
            if !p.on_ground {
                return p;
            }
        }
        panic!("fixture: player never stepped off the ledge");
    }

    #[test]
    fn coyote_time_allows_jump_just_after_stepping_off() {
        let room = ledge_room();
        let mut p = walk_off_ledge(&room);
        assert!(!p.on_ground, "player should be airborne right now");
        // Already airborne, but the coyote window is still open -- a jump this frame must
        // still fire.
        let jump = Input {
            left: true,
            jump: true,
            ..Input::default()
        };
        let start_y = p.pos.y;
        p.step(&jump, std::slice::from_ref(&room));
        assert!(
            p.pos.y < start_y,
            "coyote time must still allow a jump just after leaving the ground"
        );
    }

    #[test]
    fn coyote_time_expires_when_waiting_too_long() {
        let room = ledge_room();
        let mut p = walk_off_ledge(&room);
        let walk = Input {
            left: true,
            ..Input::default()
        };
        // Already airborne for 1 frame; wait out the whole window -- coyote must reach
        // zero after that.
        for _ in 0..COYOTE_FRAMES + 2 {
            p.step(&walk, std::slice::from_ref(&room));
        }
        assert!(!p.on_ground);
        let jump = Input {
            left: true,
            jump: true,
            ..Input::default()
        };
        let start_y = p.pos.y;
        p.step(&jump, std::slice::from_ref(&room));
        assert!(
            p.pos.y >= start_y,
            "after coyote expires a mid-air jump must not fire"
        );
    }

    #[test]
    fn jump_buffering_fires_on_landing() {
        let room = tall_room();
        // Falling from the air, press and hold jump on the **frame before landing** -- the
        // buffer must carry that input to the ground and jump right at landing (rather
        // than being discarded).
        let mut p = Player::new(Vec2::from_f32s(32.0, 90.0));
        let idle = Input::default();
        let buffered = Input {
            jump: true,
            ..Input::default()
        };
        // Floor top = 120, hitbox height 8 -> pos.y = 112 once settled.
        let floor_top = Fx::from_int(120);
        let mut pressed = false;
        let mut fired = false;
        for _ in 0..40 {
            let bottom = p.pos.y + Fx::from_int(PLAYER_H_PX);
            if bottom >= floor_top - Fx::from_int(5) {
                pressed = true;
            }
            let input = if pressed { buffered } else { idle };
            p.step(&input, std::slice::from_ref(&room));
            if p.vel.y < Fx::zero() {
                fired = true;
                break;
            }
        }
        assert!(
            fired,
            "a jump pressed just before landing must fire on landing"
        );
    }

    #[test]
    fn holding_jump_gives_more_height_than_releasing_early() {
        let room = tall_room();
        let hold = Input {
            jump: true,
            ..Input::default()
        };
        let idle = Input::default();
        let mut held = settled(&room, 32.0);
        let mut released = settled(&room, 32.0);

        // Same jump: held keeps jump held throughout, released lets go the next frame.
        held.step(&hold, std::slice::from_ref(&room));
        released.step(&hold, std::slice::from_ref(&room));

        let mut held_peak = held.pos.y;
        let mut released_peak = released.pos.y;
        for _ in 0..90 {
            held.step(&hold, std::slice::from_ref(&room));
            released.step(&idle, std::slice::from_ref(&room));
            if held.pos.y < held_peak {
                held_peak = held.pos.y;
            }
            if released.pos.y < released_peak {
                released_peak = released.pos.y;
            }
        }
        assert!(
            held_peak < released_peak,
            "holding jump must jump higher: held peak {} vs released peak {}",
            held_peak.to_f32(),
            released_peak.to_f32()
        );
    }

    #[test]
    fn releasing_jump_mid_rise_resumes_gravity_without_a_cut() {
        // Vanilla's release only stops the varJump clamp (122153-122155), with **no**
        // extra speed cut: gravity continues from the current speed (122123). The second
        // tick after jumping must be exactly jump speed + one frame of gravity, and the
        // clamp must not pull it back.
        let room = tall_room();
        let mut p = settled(&room, 32.0);
        let jump = Input {
            jump: true,
            ..Input::default()
        };
        p.step(&jump, std::slice::from_ref(&room));
        assert_eq!(p.vel.y, JUMP_SPEED, "the jump tick pins the rise speed");
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert_eq!(
            p.vel.y,
            JUMP_SPEED + GRAVITY,
            "release must not cut the rise; gravity alone applies"
        );
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert_eq!(
            p.vel.y,
            JUMP_SPEED + GRAVITY + GRAVITY,
            "the pin must stay off after release"
        );
    }

    #[test]
    fn run_accelerates_instead_of_snapping_to_top_speed() {
        let room = wide_room();
        let mut p = settled(&room, 32.0);
        let walk = Input {
            right: true,
            ..Input::default()
        };
        // The first frame cannot already be at top speed -- that is exactly what "instant
        // assignment" is meant to forbid.
        p.step(&walk, std::slice::from_ref(&room));
        assert_eq!(
            p.vel.x, RUN_ACCEL,
            "starting from rest must use the acceleration rate, not the slower reduce rate"
        );
        assert!(
            p.vel.x < RUN,
            "the first tick must not snap to top speed: {}",
            p.vel.x
        );
        // After several frames it converges to top speed and stops growing.
        for _ in 0..60 {
            p.step(&walk, std::slice::from_ref(&room));
        }
        assert_eq!(p.vel.x, RUN, "must settle at exactly the run speed");
    }

    #[test]
    fn releasing_the_direction_decelerates_instead_of_stopping_dead() {
        let room = wide_room();
        let mut p = settled(&room, 8.0);
        let walk = Input {
            right: true,
            ..Input::default()
        };
        for _ in 0..60 {
            p.step(&walk, std::slice::from_ref(&room));
        }
        assert_eq!(p.vel.x, RUN);
        let idle = Input::default();
        p.step(&idle, std::slice::from_ref(&room));
        assert!(
            p.vel.x > Fx::zero() && p.vel.x < RUN,
            "letting go must bleed speed off, not zero it: {}",
            p.vel.x
        );
        for _ in 0..60 {
            p.step(&idle, std::slice::from_ref(&room));
        }
        assert_eq!(p.vel.x, Fx::zero(), "must come to rest");
    }

    #[test]
    fn air_control_is_weaker_than_ground_control() {
        let room = wide_room();
        let walk = Input {
            right: true,
            ..Input::default()
        };
        // Same instant, same initial speed (0); the only difference is grounded or not.
        let mut ground = settled(&room, 32.0);
        ground.vel.x = Fx::zero();
        let mut air = Player::new(Vec2::from_f32s(32.0, 8.0));
        air.vel.x = Fx::zero();
        air.on_ground = false;

        ground.step(&walk, std::slice::from_ref(&room));
        air.step(&walk, std::slice::from_ref(&room));

        assert!(ground.on_ground, "fixture: first player stays grounded");
        assert!(!air.on_ground, "fixture: second player is airborne");
        assert_eq!(
            ground.vel.x, RUN_ACCEL,
            "grounded player accelerates at the full rate"
        );
        assert_eq!(
            air.vel.x,
            RUN_ACCEL * AIR_MULT,
            "airborne player accelerates at the discounted rate"
        );
        assert!(
            air.vel.x < ground.vel.x,
            "air control must be weaker: air {} vs ground {}",
            air.vel.x,
            ground.vel.x
        );
    }

    #[test]
    fn airborne_player_can_still_be_steered() {
        let room = wide_room();
        let mut p = Player::new(Vec2::from_f32s(32.0, 8.0));
        let walk = Input {
            right: true,
            ..Input::default()
        };
        for _ in 0..5 {
            p.step(&walk, std::slice::from_ref(&room));
        }
        assert!(!p.on_ground, "fixture: must still be falling");
        assert!(
            p.vel.x > Fx::zero(),
            "air control must be non-zero, not locked out"
        );
    }

    /// An 8x8 cell room; `floor` decides whether row 7 gets a floor, and `x` is the room
    /// origin.
    ///
    /// Same shape as `ledge_room`, but with an adjustable origin -- for pairing two rooms
    /// flush or with a gap between them.
    fn seam_room(x: f32, floor: bool) -> Room {
        let mut tiles = Tileset::new("Test", 8, 8);
        if floor {
            for col in 0..8 {
                tiles.set(col, 7, 1);
            }
        }
        Room {
            name: "t".into(),
            bounds: RectDef {
                x,
                y: 0.0,
                w: 64.0,
                h: 64.0,
            },
            tiles,
            bg: Tileset::new("Test", 8, 8),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
            ..Default::default()
        }
    }

    /// Two flush rooms (A x[0,64], B x[64,128], same floor height): walking right across
    /// the seam, the per-tick displacement must not exceed a normal step.
    ///
    /// Regression: the old implementation unconditionally clamped the player into the
    /// rectangle of the "room containing the center" every tick, so the moment the center
    /// crossed the seam it yanked the player into the neighboring room -- a ~4px teleport.
    /// (The tick count is tuned for the player stopping mid-B, not reaching B's far edge.)
    #[test]
    fn walking_across_a_seam_does_not_teleport() {
        let rooms = [seam_room(0.0, true), seam_room(64.0, true)];
        // Standing on A's floor (row 7, top y=56 -> hitbox top edge y=48).
        let mut p = Player::new(Vec2::from_f32s(40.0, 48.0));
        let walk = Input {
            right: true,
            ..Input::default()
        };
        let mut max_delta = Fx::zero();
        for _ in 0..30 {
            let before = p.pos;
            p.step(&walk, &rooms);
            let d = (p.pos.x - before.x).abs();
            if d > max_delta {
                max_delta = d;
            }
        }
        assert!(
            max_delta <= RUN,
            "walking across the seam must stay continuous, got a {max_delta:?} px jump in one tick"
        );
        // And the player really did walk into B (center past the seam: pos.x > 60).
        assert!(
            p.pos.x.to_f32() > 60.0,
            "the player should end up past the seam, at {:?}",
            p.pos.x
        );
    }

    /// A gap between two rooms (A x[0,64], B x[128,192]): when falling out of every room,
    /// the player is caught by the **nearest** room rather than yanked to the spawn room
    /// (the old implementation's fallback target).
    #[test]
    fn falling_into_a_gap_is_caught_by_the_nearest_room() {
        // There is no room over the gap; B does not take part (A is nearer). B gets a floor
        // but must not be chosen.
        let rooms = [seam_room(0.0, true), seam_room(128.0, true)];
        // Drop the player straight into the gap (center 74 is outside every room): within
        // one tick the nearest room A must clamp it back to its right edge (64 - 8 = 56) --
        // that is the fallback.
        let mut p = Player::new(Vec2::from_f32s(70.0, 48.0));
        p.step(&Input::default(), &rooms);
        assert_eq!(
            p.pos.x.to_f32(),
            56.0,
            "the gap must be guarded by the nearest room's right edge"
        );
        // Land on A's floor (the clamp only fixes x; landing relies on subsequent gravity).
        for _ in 0..120 {
            if p.on_ground {
                break;
            }
            p.step(&Input::default(), &rooms);
        }
        assert!(
            p.on_ground,
            "the guard stands the player on the room's bottom edge"
        );
        // It must never be yanked to B (x >= 120).
        assert!(p.pos.x.to_f32() < 100.0, "must not teleport across the map");
    }

    /// Dash: 240 px/s (4 px/tick) for 0.15s (9 ticks), then 160 px/s (8/3 px/tick) at the
    /// end; the count drops by 1; refill on landing; 0.2s cooldown.
    #[test]
    fn dash_matches_vanilla_speeds_and_timing() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        let dash = Input {
            right: true,
            dash: true,
            ..Input::default()
        };
        let x0 = p.pos.x;
        // tick 1: StartDash + DashBegin -- zero speed, consume a dash, request the freeze.
        p.step(&dash, std::slice::from_ref(&room));
        assert_eq!(p.state, 2, "dash enters vanilla state 2");
        assert_eq!(p.vel, Vec2::ZERO, "DashBegin zeroes Speed (122560)");
        assert_eq!(p.dashes, 0, "StartDash consumes one dash (122471)");
        assert_eq!(
            p.freeze_ticks, 3,
            "Celeste.Freeze(0.05f) = 3 ticks (122540)"
        );
        // tick 2: coroutine first segment -> speed = 4 px/tick.
        p.step(&dash, std::slice::from_ref(&room));
        assert_eq!(p.vel.x, DASH_SPEED, "Speed = aim * 240 px/s (122745)");
        assert_eq!(p.dash_dir.x, Fx::one(), "DashDir snaps to the aim axis");
        let after_first = p.pos.x;
        // tick 3..10: constant-speed dash.
        for _ in 0..8 {
            p.step(&dash, std::slice::from_ref(&room));
            assert_eq!(p.vel.x, DASH_SPEED);
        }
        assert_eq!(
            (p.pos.x - after_first).to_f32(),
            32.0,
            "8 ticks at 4 px/tick"
        );
        // tick 11: coroutine ends -> last segment at 8/3 px/tick, back to State 0.
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.state, 0, "the coroutine returns to Normal (122830)");
        assert_eq!(p.vel.x, DASH_END_SPEED, "Speed = DashDir * 160 (122824)");
        let _ = x0;
    }

    /// Dash count: exhausting it with an air dash -> hair eases toward USED; refill on
    /// landing -> white flash, then back to NORMAL after 0.12s.
    #[test]
    fn dash_refill_and_hair_states_follow_update_hair() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        // Refilled by the ground immediately after the dash (the ground branch at
        // 119855-119863 fires in the head tick).
        let dash = Input {
            right: true,
            dash: true,
            ..Input::default()
        };
        p.step(&dash, std::slice::from_ref(&room));
        assert_eq!(p.dashes, 0);
        // Head refill after landing (119855-119863): the tick in which the dash speed
        // takes effect is still grounded (ground horizontal dash), and the next tick the
        // head refills.
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert_eq!(
            p.dashes, 1,
            "RefillDash refills on the ground (119855-119863)"
        );
        assert_eq!(
            p.hair_color(),
            crate::sprites::hair_color::FLASH,
            "lastDashes != Dashes flashes white (120268-120271)"
        );
        // Dash 0.2s cooldown (122541): it starts on tick 1 and the coroutine ends on tick
        // 10 (phase 10 > DASH_TICKS), so the cooldown only reaches zero on tick 12.
        // Coroutine phases: tick 2 = first segment (phase 1), ending its last segment on
        // tick 11 -- 10 state ticks in all (1 entry + 9 of the 0.15s wait).
        for _ in 0..9 {
            p.step(&Input::default(), std::slice::from_ref(&room));
        }
        assert_eq!(p.state, 0, "the dash coroutine ended (122830)");
        // Cooldown gate (`dashCooldownTimer <= 0` in `CanDash` 119325-119340): reset the
        // cooldown to its fresh-start value; pressing again immediately must be blocked,
        // and it is only allowed once it has fully decayed.
        p.dash_cooldown = DASH_COOLDOWN_TICKS;
        let again = Input {
            dash: true,
            ..Input::default()
        };
        p.step(&again, std::slice::from_ref(&room));
        assert_eq!(p.state, 0, "dashCooldownTimer blocks while > 0");
        for _ in 0..DASH_COOLDOWN_TICKS {
            p.step(&Input::default(), std::slice::from_ref(&room));
        }
        p.step(&again, std::slice::from_ref(&room));
        assert_eq!(p.state, 2, "after the cooldown a fresh dash starts");
        // The next tick after a ground dash the head refills again (119855-119863:
        // standing means standing on a solid) -> lastDashes(0) != Dashes(1) flashes white
        // once more -- exactly how a ground dash behaves in vanilla.
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.dashes, 1, "ground dash refills next tick");
        assert_eq!(p.hair_color(), crate::sprites::hair_color::FLASH);
        // White flash 0.12s = 7 ticks: every tick with the timer > 0 shows FLASH (the else
        // branch at 120286-120290); on the **next** tick after it reaches zero, the
        // discrete branch returns NORMAL.
        for _ in 0..8 {
            p.step(&Input::default(), std::slice::from_ref(&room));
        }
        assert_eq!(p.hair_color(), crate::sprites::hair_color::NORMAL);
        // Air exhaustion -> ease into USED (120258-120262): first spend the only dash
        // while airborne.
        let mut p2 = settled(&room, 40.0);
        let jump_then_dash = Input {
            jump: true,
            ..Input::default()
        };
        p2.step(&jump_then_dash, std::slice::from_ref(&room));
        let air_dash = Input {
            dash: true,
            ..Input::default()
        };
        p2.step(&air_dash, std::slice::from_ref(&room));
        p2.step(&Input::default(), std::slice::from_ref(&room));
        let before = p2.hair_color();
        p2.step(&Input::default(), std::slice::from_ref(&room));
        let after = p2.hair_color();
        assert!(
            after[0] < before[0] && after[1] > before[1],
            "hair lerps toward Used 44B7FF at 6/s ({before:?} -> {after:?})"
        );
    }

    /// Wall jump: pressing jump against a wall -> horizontal 130 px/s away, vertical
    /// -105, forceMoveX overriding the input direction for 0.16s.
    #[test]
    fn wall_jump_off_a_wall_matches_vanilla_speeds() {
        // A wall column on the left + a floor.
        let mut tiles = Tileset::new("Gameplay", 10, 16);
        for y in 0..16 {
            tiles.set(0, y, 1);
        }
        for x in 0..10 {
            tiles.set(x, 15, 1);
        }
        let room = Room {
            name: "w".into(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: 80.0,
                h: 128.0,
            },
            tiles,
            bg: Tileset::new("Gameplay", 10, 16),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
            ..Default::default()
        };
        // Player hanging against the wall (x = 8 -> hitbox [8,16], wall right edge 8).
        let mut p = Player::new(Vec2::from_f32s(8.0, 100.0));
        // Drop one tick to settle flush (grounded is not required: the probe only looks at
        // the wall).
        p.step(&Input::default(), std::slice::from_ref(&room));
        // Jump while holding toward the wall -- vanilla sets forceMoveX only when
        // moveX != 0 (the `else if (moveX != 0)` at 120828-120838).
        let jump = Input {
            left: true,
            jump: true,
            ..Input::default()
        };
        p.step(&jump, std::slice::from_ref(&room));
        assert_eq!(
            p.vel.x, WALLJUMP_X,
            "WallJump: Speed.X = 130 * dir (120836)"
        );
        assert_eq!(p.vel.y, JUMP_SPEED, "WallJump: Speed.Y = -105 (120841)");
        assert_eq!(
            p.force_move_x, 1,
            "WallJump forces moveX away from the wall (120828-120838)"
        );
        // Within the forceMoveX window, opposite input is overridden (119883-119891).
        let push_left = Input {
            left: true,
            ..Input::default()
        };
        p.step(&push_left, std::slice::from_ref(&room));
        assert_eq!(p.move_x, Fx::one(), "forceMoveX overrides input for 0.16s");
    }

    /// Jumping during a ground horizontal dash = super jump: 260 px/s * Facing, -105
    /// (120740-120778).
    #[test]
    fn jumping_during_a_ground_dash_is_a_super_jump() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        let dash = Input {
            right: true,
            dash: true,
            ..Input::default()
        };
        p.step(&dash, std::slice::from_ref(&room)); // enter State 2
        p.step(&dash, std::slice::from_ref(&room)); // speed takes effect (still grounded)
        assert!(p.on_ground, "ground dash keeps the player grounded");
        let jump = Input {
            jump: true,
            ..Input::default()
        };
        p.step(&jump, std::slice::from_ref(&room));
        println!(
            "DEBUG after jump: state={} on_ground={} ducking={} dash_dir=({},{}) jump_buffer={} vel=({},{})",
            p.state, p.on_ground, p.ducking, p.dash_dir.x.to_f32(), p.dash_dir.y.to_f32(),
            p.jump_buffer, p.vel.x.to_f32(), p.vel.y.to_f32()
        );
        assert_eq!(p.state, 0, "SuperJump returns to Normal");
        assert_eq!(
            p.vel.x, SUPER_JUMP_X,
            "SuperJump: Speed.X = 260 * Facing (120748)"
        );
        assert_eq!(p.vel.y, JUMP_SPEED, "SuperJump: Speed.Y = -105 (120749)");
    }

    /// Vanilla `OnCollideV` (121577-121582): a **down-diagonal dash that lands** becomes a
    /// horizontal duck slide -- `DashDir.Y = 0`, `Speed.Y = 0`, `Speed.X *= 1.2`,
    /// `Ducking = true`.
    ///
    /// Regression: the conversion used to live in the dash's first tick only, guarded by
    /// `on_ground`, so it never ran for a dash that started in the air and landed later --
    /// the air half of the hyper was impossible.
    #[test]
    fn landing_a_down_diagonal_dash_converts_it_to_a_duck_slide() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        let dash = Input {
            right: true,
            down: true,
            dash: true,
            ..Input::default()
        };
        p.step(&dash, std::slice::from_ref(&room)); // enter State 2, Speed = aim * 240
        p.step(&dash, std::slice::from_ref(&room)); // MoveV hits the floor -> OnCollideV
        assert_eq!(
            p.dash_dir.y,
            Fx::zero(),
            "DashDir.Y must be zeroed on landing (121579)"
        );
        assert_eq!(
            p.dash_dir.x,
            Fx::one(),
            "DashDir.X collapses to the sign (121578)"
        );
        assert_eq!(p.vel.y, Fx::zero(), "Speed.Y = 0 (121580)");
        assert!(
            p.ducking,
            "Ducking = true: the landed diagonal dash is a duck slide (121582)"
        );
        // Down-right dash is 240/sqrt(2) = 2.828 px/tick, x1.2 = 3.394 px/tick.
        let vx_ps = p.vel.x.to_f32() * 60.0;
        assert!(
            (vx_ps - 203.6).abs() < 0.5,
            "Speed.X must be x1.2 of the diagonal dash (121581), got {vx_ps} px/s"
        );
    }

    /// The full **air hyper**: jump, dash down-diagonal in the air, land, then jump inside
    /// the dash window.
    ///
    /// The landed dash is horizontal and ducking, so `DashUpdate`'s jump branch
    /// (122632-122641, `|DashDir.Y| < 0.1`) reaches `SuperJump` and its ducked branch
    /// (120753-120760) doubles down on the launch: `Speed.X = 260 * 1.25 = 325` px/s and
    /// `Speed.Y = -105 * 0.5 = -52.5` px/s.
    #[test]
    fn air_hyper_reaches_the_ducked_super_jump() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        let jump = Input {
            jump: true,
            ..Input::default()
        };
        p.step(&jump, std::slice::from_ref(&room)); // leave the ground
        p.step(&jump, std::slice::from_ref(&room));
        // Dash down-right while airborne.
        let dash = Input {
            right: true,
            down: true,
            dash: true,
            ..Input::default()
        };
        p.step(&dash, std::slice::from_ref(&room));
        assert_eq!(p.state, 2, "the dash starts in the air");
        assert!(!p.on_ground, "fixture: still airborne");
        // Fall until the dash lands (the conversion happens in the collision handler).
        let hold = Input {
            right: true,
            down: true,
            ..Input::default()
        };
        // `on_ground` is the 1px probe vanilla uses too (119828), so it can read true one
        // tick before the movement actually collides; the conversion runs in the collision
        // handler (`OnCollideV` needs `Speed.Y > 0`), so wait for it to have happened.
        let mut ticks = 0;
        while (p.dash_dir.y != Fx::zero() || !p.on_ground) && ticks < 60 {
            p.step(&hold, std::slice::from_ref(&room));
            ticks += 1;
        }
        assert!(p.on_ground, "fixture: the diagonal dash must land");
        assert_eq!(p.dash_dir.y, Fx::zero(), "the landed dash is horizontal");
        assert!(p.ducking, "landing a diagonal dash duck-slides (121582)");
        assert_eq!(p.state, 2, "still inside the dash window");
        // Jump inside the window: the ducked SuperJump (325 / -52.5).
        p.step(&jump, std::slice::from_ref(&room));
        assert_eq!(p.state, 0, "SuperJump returns to Normal");
        let vx_ps = p.vel.x.to_f32() * 60.0;
        let vy_ps = p.vel.y.to_f32() * 60.0;
        assert!(
            (vx_ps - 325.0).abs() < 1.0,
            "hyper horizontal speed: 260 * 1.25 = 325 px/s (120748 + 120756), got {vx_ps}"
        );
        assert!(
            (vy_ps + 52.5).abs() < 1.0,
            "hyper vertical speed: -105 * 0.5 = -52.5 px/s (120749 + 120757), got {vy_ps}"
        );
        assert!(!p.ducking, "the ducked branch clears Ducking (120755)");
    }

    /// A room with a non-(0,0) origin: hitting a wall / landing / hitting the ceiling must
    /// all snap to **world** cell edges.
    ///
    /// Regression: `edge_x`/`edge_y` once treated world cell indices as room-local indices
    /// and added the room origin back -- a room with ox=64 bounced east 64px on touching a
    /// wall, and a room with oy=-88 bounced 88px up on hitting the ceiling. In 0-Intro
    /// every room except `lvl_0` has a non-zero origin, so the old code teleported hugely
    /// on any wall touch.
    fn offset_room() -> Room {
        let mut tiles = Tileset::new("Test", 8, 8);
        for x in 0..8 {
            tiles.set(x, 7, 1); // floor: world y[-32,-24)
        }
        for y in 0..7 {
            tiles.set(4, y, 1); // vertical wall: world x[96,104)
        }
        for x in 0..4 {
            tiles.set(x, 2, 1); // ceiling: world x[64,96), y[-72,-64)
        }
        Room {
            name: "off".into(),
            bounds: RectDef {
                x: 64.0,
                y: -88.0,
                w: 64.0,
                h: 64.0,
            },
            tiles,
            bg: Tileset::new("Test", 8, 8),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
            ..Default::default()
        }
    }

    #[test]
    fn collisions_snap_to_world_edges_regardless_of_room_origin() {
        let room = offset_room();

        // Landing: floor top y = -32 -> the hitbox top edge stops at -40.
        let mut p = Player::new(Vec2::from_f32s(72.0, -64.0));
        for _ in 0..60 {
            p.step(&Input::default(), std::slice::from_ref(&room));
        }
        assert!(p.on_ground, "fixture: must land on the offset floor");
        assert_eq!(
            p.pos.y.to_f32(),
            -43.0,
            "landing must snap to the world edge of the floor (hitbox 11 tall, 118856)"
        );

        // Wall hit: the wall's left edge x = 96 -> the hitbox stops at 88 (the old
        // implementation bounced to 152).
        let walk = Input {
            right: true,
            ..Input::default()
        };
        for _ in 0..90 {
            p.step(&walk, std::slice::from_ref(&room));
        }
        assert_eq!(
            p.pos.x.to_f32(),
            88.0,
            "wall snap must be the world edge of the blocking column"
        );

        // Ceiling hit: ceiling bottom y = -64 -> the head stops at -64 (the old
        // implementation bounced to -152). Standing on the floor: floor top -32, hitbox 11
        // tall -> top edge -43.
        let mut q = Player::new(Vec2::from_f32s(72.0, -43.0));
        q.on_ground = true;
        let jump = Input {
            jump: true,
            ..Input::default()
        };
        let mut min_y = q.pos.y;
        for _ in 0..30 {
            q.step(&jump, std::slice::from_ref(&room));
            if q.pos.y < min_y {
                min_y = q.pos.y;
            }
        }
        assert_eq!(
            min_y.to_f32(),
            -64.0,
            "ceiling snap must be the world edge of the blocking row"
        );
    }

    /// Two flush rooms with a solid bottom row above and an opening in the top row below:
    /// a jump must stop its head at the seam (y=0); the head must not drive into the solid
    /// of the upper room, and certainly must not get stuck inside a wall.
    ///
    /// Measured in 0-Intro: `lvl_0`'s ceiling is open at x[288,400), and directly above is
    /// `lvl_0b`'s solid floor -- the old implementation only checked the tiles of the room
    /// containing the center, so the head drove in first and it was only noticed once the
    /// center crossed the seam, getting the player stuck / teleported.
    #[test]
    fn a_solid_ceiling_in_the_room_above_stops_the_head_at_the_seam() {
        let lower = seam_room(0.0, true);
        let mut upper = seam_room(0.0, true);
        upper.bounds.y = -64.0;
        upper.name = "up".into();
        let rooms = [lower, upper];

        // The head is 8px below the seam and rising at 4 px/tick: it crosses y=0 within a
        // tick or two.
        let mut p = Player::new(Vec2::from_f32s(32.0, 8.0));
        p.vel.y = -Fx::from_int(4);
        let hold = Input {
            jump: true,
            ..Input::default()
        };
        let mut min_y = p.pos.y;
        for _ in 0..60 {
            p.step(&hold, &rooms);
            if p.pos.y < min_y {
                min_y = p.pos.y;
            }
        }
        assert_eq!(
            min_y.to_f32(),
            0.0,
            "the head must stop exactly at the seam, not enter the solid room above"
        );
        assert!(
            p.pos.y.to_f32() > 0.0,
            "the player must fall back into the lower room"
        );
        assert!(p.on_ground, "and settle on the lower room's floor");
    }

    /// An 8x48 cell deep shaft: the floor is far below, so the fall-speed tests can
    /// observe terminal speed and easing without **landing** (384px, unreachable in 75
    /// ticks).
    fn shaft_room() -> Room {
        let mut tiles = Tileset::new("Test", 8, 48);
        for x in 0..8 {
            tiles.set(x, 47, 1);
        }
        Room {
            name: "t".into(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: 64.0,
                h: 384.0,
            },
            tiles,
            bg: Tileset::new("Test", 8, 48),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
            ..Default::default()
        }
    }

    /// Reference value: the terminal speed of a normal fall is 160 px/s (8/3 px/tick), not
    /// the old implementation's 360.
    #[test]
    fn fall_speed_caps_at_vanilla_max_fall() {
        let room = tall_room();
        let mut p = Player::new(Vec2::from_f32s(32.0, 8.0));
        for _ in 0..40 {
            p.step(&Input::default(), std::slice::from_ref(&room));
            if p.vel.y == MAX_FALL {
                return; // reaching terminal speed passes (vel.y going to zero after landing
                        // is normal)
            }
        }
        panic!(
            "terminal fall speed must be exactly 160 px/s (8/3 px/tick), last = {:?}",
            p.vel.y
        );
    }

    /// Holding "down": the fall-speed cap eases from 160 to 240 px/s (4 px/tick), then
    /// back to 160 after release. Reference `maxFall = Approach(maxFall, 240, 300*dt)`.
    #[test]
    fn holding_down_ramps_the_fall_cap_to_fast_fall_and_back() {
        let room = shaft_room();
        let down = Input {
            down: true,
            ..Input::default()
        };
        let mut p = Player::new(Vec2::from_f32s(32.0, 8.0));

        // Holding down -> the fast-fall cap must be reached.
        let mut reached_fast = false;
        for _ in 0..45 {
            p.step(&down, std::slice::from_ref(&room));
            if p.vel.y == FAST_MAX_FALL {
                reached_fast = true;
                break;
            }
        }
        assert!(
            reached_fast,
            "holding down must ramp the cap to 240 px/s (4 px/tick), last = {:?}",
            p.vel.y
        );

        // Control: without down, the fall speed never exceeds the normal cap.
        let mut q = Player::new(Vec2::from_f32s(32.0, 8.0));
        for _ in 0..45 {
            q.step(&Input::default(), std::slice::from_ref(&room));
            assert!(
                q.vel.y <= MAX_FALL,
                "without down the cap must stay at 160 px/s, got {:?}",
                q.vel.y
            );
        }

        // Releasing down -> the cap eases back to normal fall speed and the fall speed
        // follows it down.
        let mut settled_back = false;
        for _ in 0..30 {
            p.step(&Input::default(), std::slice::from_ref(&room));
            if p.vel.y == MAX_FALL {
                settled_back = true;
                break;
            }
        }
        assert!(
            settled_back,
            "releasing down must settle the fall back at 160 px/s, last = {:?}",
            p.vel.y
        );
    }

    /// Apex half gravity: while |vy| < 40 px/s and jump is held, gravity is halved --
    /// reference `(|Speed.Y| < 40f && Jump.Check) ? 0.5f : 1f`.
    #[test]
    fn gravity_is_halved_near_the_apex_while_jump_is_held() {
        let room = tall_room();
        // Both players are placed in the slow-rise band (|vy| < 2/3 px/tick); the only
        // difference is the jump button. Note neither has a release edge, so the variable
        // jump height cut is not involved.
        let mut held = Player::new(Vec2::from_f32s(32.0, 60.0));
        held.on_ground = false;
        held.vel.y = -Fx::from_ratio(3, 5); // -0.6 px/tick, within the threshold
        let mut idle = Player::new(Vec2::from_f32s(32.0, 60.0));
        idle.on_ground = false;
        idle.vel.y = held.vel.y;

        let jump = Input {
            jump: true,
            ..Input::default()
        };
        held.step(&jump, std::slice::from_ref(&room));
        idle.step(&Input::default(), std::slice::from_ref(&room));

        assert!(
            held.vel.y < idle.vel.y,
            "holding jump near the apex must halve gravity: held {:?} vs idle {:?}",
            held.vel.y,
            idle.vel.y
        );
    }
    /// Ducking: press down while grounded to enter (122004-122008), the hitbox switches
    /// to 8x6 with its bottom edge aligned (118858), and releasing stands back up
    /// (121996-122003).
    #[test]
    fn ducking_enters_exits_and_shrinks_the_hitbox() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        let down = Input {
            down: true,
            ..Input::default()
        };
        p.step(&down, std::slice::from_ref(&room));
        assert!(
            p.ducking,
            "crouch on the ground with down held (122004-122008)"
        );
        assert_eq!(p.box_dims(), (PLAYER_W_PX, DUCK_H, DUCK_Y_OFF));
        assert!(p.on_ground, "duck hitbox keeps the bottom edge aligned");
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert!(!p.ducking, "release down to stand back up (121996-122003)");
    }

    /// Climb: grab against a wall -> State 1; hanging drains stamina continuously (still
    /// 10/s, 122278); landing refills it (119818-119823); releasing the grab lets go
    /// (122211-122216).
    #[test]
    fn climb_grabs_drains_stamina_and_lets_go() {
        let mut tiles = Tileset::new("Gameplay", 10, 16);
        for y in 0..16 {
            tiles.set(0, y, 1);
        }
        for x in 0..10 {
            tiles.set(x, 15, 1);
        }
        let room = Room {
            name: "w".into(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: 80.0,
                h: 128.0,
            },
            tiles,
            bg: Tileset::new("Gameplay", 10, 16),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
            ..Default::default()
        };
        let mut p = settled(&room, 8.0);
        assert_eq!(p.stamina, CLIMB_MAX_STAMINA);
        let grab = Input {
            left: true,
            grab: true,
            ..Input::default()
        };
        p.step(&grab, std::slice::from_ref(&room));
        assert_eq!(p.state, 1, "grabbing a wall enters Climb (121843-121849)");
        let before = p.stamina;
        for _ in 0..12 {
            p.step(&grab, std::slice::from_ref(&room));
        }
        assert!(
            p.stamina < before,
            "hanging drains stamina at 10/s (122278-122280)"
        );
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.stamina, CLIMB_MAX_STAMINA, "ground refills stamina");
        assert_eq!(p.state, 0, "releasing grab leaves Climb (122211-122216)");
    }

    // -----------------------------------------------------------------
    // Sprite animation (the `Sprite` state machine) -- regression: walking looked
    // like it was only "sliding".
    // -----------------------------------------------------------------

    /// A single animation table good enough for these tests: only id / frame count /
    /// interval / goto matter, and the frame names are arbitrary.
    ///
    /// Keys must be lowercase (the Monocle dictionary is `OrdinalIgnoreCase`, 10206);
    /// `goto` keeps the exact casing from the XML (vanilla
    /// `CurrentAnimationID = Goto`, 10279-10284).
    fn anim_table() -> crate::sprites::AnimationTable {
        use crate::sprites::{AnimDef, AnimationTable, SpriteDef};
        use std::collections::HashMap;

        fn a(frames: &[&str], delay: f32, goto: Option<&str>) -> AnimDef {
            AnimDef {
                frames: frames.iter().map(|f| (*f).to_string()).collect(),
                delay,
                goto: goto.map(|g| g.to_string()),
                hair: Vec::new(),
            }
        }

        let mut anims = HashMap::new();
        // Follows the real Sprites.xml: idle/edge/push are `goto=self` with `<Loop>`;
        // runSlow gotos runFast when done; runFast loops itself; flip gotos runFast when
        // done (references/Celeste/Content/Graphics/Sprites.xml).
        anims.insert("idle".into(), a(&["idle00", "idle01"], 0.1, Some("idle")));
        anims.insert(
            "runslow".into(),
            a(
                &["runSlow00", "runSlow01", "runSlow02"],
                0.07,
                Some("runFast"),
            ),
        );
        anims.insert(
            "runfast".into(),
            a(
                &["runFast00", "runFast01", "runFast02"],
                0.05,
                Some("runFast"),
            ),
        );
        anims.insert("edge".into(), a(&["edge00"], 0.1, Some("edge")));
        anims.insert("edgeback".into(), a(&["edgeBack00"], 0.1, Some("edgeBack")));
        anims.insert("push".into(), a(&["push00"], 0.1, Some("push")));
        anims.insert(
            "flip".into(),
            a(&["flip00", "flip01"], 0.04, Some("runFast")),
        );
        anims.insert("skid".into(), a(&["skid00"], 0.04, Some("skid")));
        anims.insert("duck".into(), a(&["duck00"], 0.0, Some("duck")));
        anims.insert("lookup".into(), a(&["lookUp00"], 0.1, None));

        let mut sprites = HashMap::new();
        sprites.insert(
            "player".into(),
            SpriteDef {
                start: "idle".into(),
                anims,
            },
        );
        AnimationTable { sprites }
    }

    /// Walk right two steps then left two steps: it must actually play a run animation
    /// (frames advancing), not slide while holding the idle pose.
    ///
    /// Regression point: `PlayerSprite.Running` (pureCeleste.cs:111375-111388) reads
    /// `LastAnimationID`. It was once written as "the current animation has a goto", and
    /// since `idle` carries `<Loop>` (goto=idle) -> `!Sprite.Running` was always false ->
    /// the `runSlow`/`runFast` at 120447-120455 could never play and the character merely
    /// appeared to slide. Storing the **old** id in `Play` caused the same symptom after a
    /// stop; see `walking_again_after_stopping_animates`.
    #[test]
    fn walking_left_and_right_animates_instead_of_sliding() {
        use std::collections::BTreeSet;

        let room = wide_room();
        let table = anim_table();
        let mut p = settled(&room, 100.0);
        let right = Input {
            right: true,
            ..Input::default()
        };
        let left = Input {
            left: true,
            ..Input::default()
        };

        // Standing still: the start animation is idle (the sprite's start, 10189).
        p.update_visuals(&table, &Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.anim_id, "idle");
        assert!(
            !p.running(),
            "just started from idle, so LastAnimationID is not run*"
        );

        // ---- Two steps right (20 ticks ~ 1/3 second) ----
        let x0 = p.pos.x;
        let mut seen = BTreeSet::new();
        for _ in 0..20 {
            p.step(&right, std::slice::from_ref(&room));
            p.update_visuals(&table, &right, std::slice::from_ref(&room));
            assert!(
                matches!(p.anim_id.as_str(), "runSlow" | "runFast"),
                "walking right but playing {:?} (120464-120470)",
                p.anim_id
            );
            seen.insert((p.anim_id.clone(), p.anim_frame));
        }
        assert!(
            seen.len() >= 3,
            "20 ticks of walking right produced only {} (animation, frame) pairs -- frames are not advancing, so it is just sliding",
            seen.len()
        );
        assert!(p.pos.x > x0, "walking right must actually move right");

        // ---- Turn and go left: flip first (120455-120459); when flip ends it gotos runFast ----
        p.step(&left, std::slice::from_ref(&room));
        p.update_visuals(&table, &left, std::slice::from_ref(&room));
        assert!(
            matches!(p.anim_id.as_str(), "flip" | "skid"),
            "a fast reversal should play flip/skid first (120441-120459), actually {:?}",
            p.anim_id
        );

        let x1 = p.pos.x;
        let mut seen = BTreeSet::new();
        for _ in 0..40 {
            p.step(&left, std::slice::from_ref(&room));
            p.update_visuals(&table, &left, std::slice::from_ref(&room));
            seen.insert((p.anim_id.clone(), p.anim_frame));
        }
        assert!(
            matches!(p.anim_id.as_str(), "runSlow" | "runFast"),
            "after turning it must return to a run animation (flip's goto=runFast), actually {:?}",
            p.anim_id
        );
        assert!(
            seen.len() >= 3,
            "going left must not be a static frame either"
        );
        assert!(p.pos.x < x1, "walking left must actually move left");
    }

    /// Stop, then walk again: every moving tick must play a run animation.
    ///
    /// Regression: `Play` used to store the **old** id in `LastAnimationID`, so after
    /// "run -> stop (Play idle)" the value stayed `runFast`; `Running` therefore stayed
    /// true, the `!Sprite.Running` guard at 120447 never fired again and the character
    /// slid at full run speed holding the idle pose.
    #[test]
    fn walking_again_after_stopping_animates() {
        let room = wide_room();
        let table = anim_table();
        let right = Input {
            right: true,
            ..Input::default()
        };
        let idle = Input::default();
        let mut p = settled(&room, 40.0);

        // Walk, then come to a full stop (which plays idle).
        for _ in 0..40 {
            p.step(&right, std::slice::from_ref(&room));
            p.update_visuals(&table, &right, std::slice::from_ref(&room));
        }
        for _ in 0..60 {
            p.step(&idle, std::slice::from_ref(&room));
            p.update_visuals(&table, &idle, std::slice::from_ref(&room));
        }
        assert_eq!(p.vel.x, Fx::zero(), "fixture: must have stopped");
        assert_eq!(p.anim_id, "idle");

        // Walk again: from the first moving tick on, a run animation must play.
        for tick in 0..40 {
            p.step(&right, std::slice::from_ref(&room));
            p.update_visuals(&table, &right, std::slice::from_ref(&room));
            assert!(
                matches!(p.anim_id.as_str(), "runSlow" | "runFast"),
                "tick {tick} of the second walk plays {:?} instead of a run animation \
                 (LastAnimationID = {:?})",
                p.anim_id,
                p.last_anim_id
            );
        }
    }

    /// `LastAnimationID` holds the animation **just played** (pureCeleste.cs:10491-10492:
    /// `(CurrentAnimationID = id)` evaluates to `id`), and `Running` (111375-111388) reads
    /// it as "the most recently played animation starts with `run`, or is `flip`".
    ///
    /// That is what re-arms the `!Sprite.Running` guard at 120447 after a stop:
    /// `Play("idle")` leaves `LastAnimationID = "idle"`, so the next run animation plays.
    /// Recording the *old* id here was the "walks in the idle pose" regression.
    #[test]
    fn last_anim_id_records_the_animation_just_played() {
        let table = anim_table();
        let room = floor_room();
        let mut p = Player::new(Vec2::from_f32s(8.0, 13.0));

        // Standing still: the start animation (idle) is played and it is not a run.
        p.update_visuals(&table, &Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.anim_id, "idle");
        assert_eq!(p.last_anim_id, "idle");
        assert!(!p.running());

        // idle -> runSlow: LastAnimationID is the animation just played, so this counts as
        // running (the guard therefore does not restart it every tick).
        p.play(&table, "runSlow");
        assert_eq!(p.last_anim_id, "runSlow");
        assert!(p.running());

        // flip special case: `if (LastAnimationID == "flip") return true` (111381-111384).
        p.play(&table, "flip");
        assert!(p.running());
        p.play(&table, "runFast");
        assert!(p.running());

        // runFast -> idle: the last played animation is now idle -> not running, so the
        // next movement tick may (re)play a run animation. Before the fix this stayed
        // "runFast" and the guard blocked the run branch forever.
        p.play(&table, "idle");
        assert_eq!(p.last_anim_id, "idle");
        assert!(!p.running());

        // ... so walking again animates.
        p.play(&table, "runSlow");
        assert!(p.running());
    }

    /// The goto branch (pureCeleste.cs:10260-10282) writes the new id into
    /// `LastAnimationID` -- the same "the animation just played" rule as `Play`.
    #[test]
    fn animation_goto_writes_the_target_into_last_anim_id() {
        let table = anim_table();
        let mut p = Player::new(Vec2::ZERO);
        // runSlow: 3 frames x 0.07s ~ 15 ticks, then goto runFast.
        p.play(&table, "runSlow");
        for _ in 0..20 {
            p.update_animation(&table);
        }
        assert_eq!(
            p.anim_id, "runFast",
            "runSlow should goto runFast when done"
        );
        assert_eq!(
            p.last_anim_id, "runFast",
            "the goto branch records the target it switched to (pureCeleste.cs:10267)"
        );
        assert!(p.running());
    }

    /// The idle branch's speed threshold is 25 px/s, **not** the 45 px/s of
    /// runSlow/runFast (pureCeleste.cs:120402 vs pureCeleste.cs:120449).
    #[test]
    fn idle_branch_uses_the_vanilla_25px_s_threshold() {
        let room = wide_room();
        let table = anim_table();
        let mut p = settled(&room, 100.0);
        // 30 px/s: > 25 (must not enter the idle/edge branch), < 45 (should be runSlow).
        p.vel.x = Fx::from_ratio(1, 2);
        p.update_visuals(&table, &Input::default(), std::slice::from_ref(&room));
        assert!(
            matches!(p.anim_id.as_str(), "runSlow" | "runFast"),
            "30 px/s is above the idle threshold and below the runSlow threshold -> the run branch should be taken, actually {:?}",
            p.anim_id
        );

        // 20 px/s: < 25 -> enter the idle/edge branch (there is no edge ahead here, so it
        // lands on idle).
        p.vel.x = Fx::from_ratio(1, 3);
        p.update_visuals(&table, &Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.anim_id, "idle");
    }

    /// Super wall jump pins the vanilla values (pureCeleste.cs:120877-120880).
    #[test]
    fn super_wall_jump_pins_vanilla_speeds() {
        let mut p = Player::new(Vec2::ZERO);
        p.super_wall_jump(1);
        assert_eq!(p.vel.x, Fx::from_ratio(17, 6), "Speed.X = 170f * dir");
        assert_eq!(p.vel.y, Fx::from_ratio(-8, 3), "Speed.Y = -160f");
        assert_eq!(p.var_jump_speed, p.vel.y, "varJumpSpeed = Speed.Y");
        assert_eq!(p.var_jump, 15, "varJumpTimer = 0.25s (120870)");
        assert_eq!(p.auto_jump, 0, "AutoJump = false (120871)");
        assert_eq!(p.state, 0);
    }

    /// A per-tick sprite trace of walking two steps each way (the manual checklist for
    /// tracking down "it only looks like sliding"):
    ///
    /// ```text
    /// cargo test -p releste-game print_walk_cycle_trace -- --ignored --nocapture
    /// ```
    ///
    /// It only prints and never asserts, hence `#[ignore]` (AGENTS.md 9: pure rendering
    /// code is exempt from tests but needs a manual checklist). Before the fix this was
    /// `idle` all the way: position moving, legs not.
    #[test]
    #[ignore = "diagnostic print; run with --nocapture"]
    fn print_walk_cycle_trace() {
        let room = wide_room();
        let table = anim_table();
        let mut p = settled(&room, 100.0);
        let right = Input {
            right: true,
            ..Input::default()
        };
        let left = Input {
            left: true,
            ..Input::default()
        };
        println!("tick  dir   pos.x   vel.x(px/s)  anim      frame  texture");
        for tick in 0..80 {
            let (dir, input) = if tick < 40 {
                ("->", &right)
            } else {
                ("<-", &left)
            };
            p.step(input, std::slice::from_ref(&room));
            p.update_visuals(&table, input, std::slice::from_ref(&room));
            if tick % 2 == 0 {
                let tex = p
                    .anim_frame_texture(&table)
                    .unwrap_or_else(|| "<static>".into());
                println!(
                    "{:>4}  {:>3}  {:>7.2}  {:>10.1}  {:<9} {:>5}  {}",
                    tick,
                    dir,
                    p.pos.x.to_f32(),
                    p.vel.x.to_f32() * 60.0,
                    p.anim_id,
                    p.anim_frame,
                    tex
                );
            }
        }
    }
}
