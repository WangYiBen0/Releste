//! Player simulation: an 8x8 AABB against the tile grid, solved with separated axes.
//!
//! All state is [`Fx`] (AGENTS.md 15.7), and the time step is always
//! [`FIXED_DT`](releste_math::FIXED_DT).
//!
//! Relationship to vanilla (`references/Celeste.exe.cs.d`, behavioral reference):
//! - **Aligned**: run acceleration (1000/400/0.65), gravity 900, fall-speed cap
//!   160/240 easing (the `maxFall` state), apex half gravity (40 threshold),
//!   coyote / jump buffer / release cut (landed in Phase 4.1).
//! - **Continuous-world semantics** (vanilla uses screen transitions; here it is
//!   continuous walking in one world): collision queries cover **every** room the
//!   hitbox touches (on a seam both sides are checked, so the head never drives into
//!   the solid interior of the neighboring room); wall and floor landings snap to
//!   **world cell edges**, independent of the room origin. When the center falls out
//!   of every room, the nearest room clamps it back.
//! - **Jump is aligned**: `Jump()`'s -105 + `JumpHBoost` (references/Celeste.exe.cs.d/Celeste/Player.cs:2453-2454) + varJump
//!   clamping (references/Celeste.exe.cs.d/Celeste/Player.cs:3791-3797) + 0.08s buffer (references/Celeste.exe.cs.d/Celeste/Input.cs:153) / 0.1s coyote (references/Celeste.exe.cs.d/Celeste/Player.cs:1581-1588);
//!   see the provenance comments on each constant.
//! - **Engine-invented (no vanilla counterpart; deviation declared)**: collision in
//!   a continuous multi-room world (`Player::hit_in`'s seam double check), the
//!   world-level fallback clamp (`Player::clamp_to_world`) and the fixed 1/2px
//!   advance step (`STEP`). Vanilla is **one tile grid per map** (references/Celeste.exe.cs.d/Celeste/LevelLoader.cs:270-272) plus a
//!   single room boundary, so collision is naturally map-wide; the engine stores tiles
//!   per room and can only check seams explicitly. Each function's docs note this.
//! - **Implemented states**: 0 Normal (references/Celeste.exe.cs.d/Celeste/Player.cs:1146), 1 Climb with stamina (references/Celeste.exe.cs.d/Celeste/Player.cs:1147),
//!   2 Dash with eight-way/refill/cooldown (references/Celeste.exe.cs.d/Celeste/Player.cs:1148); `Player::step` dispatches on
//!   `state`.
//! - **Not yet implemented states** (vanilla `StateMachine.SetCallbacks`
//!   references/Celeste.exe.cs.d/Celeste/Player.cs:1146-1160): 3 Swim, 4 Boost, 5 RedDash, 6 HitSquash, 7 Launch, 8 Pickup,
//!   9 DreamDash, 10 SummitLaunch, 11 Dummy, and the 12-15 intro coroutines; these
//!   `state` values currently all fall back to Normal. ROADMAP Phase 4.1. The hair
//!   color USED/FLASH/TWO_DASHES states are already prepared in `sprites::hair_color`
//!   and activate as dash lands.

use releste_map::Room;
use releste_math::{Fx, Vec2};

use crate::session::{nearest_room, room_containing, Session};

/// Hitbox width (pixels, integer, for room collision).
///
/// Vanilla: `references/Celeste.exe.cs.d/Celeste/Player.cs:605`
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
/// (references/Celeste.exe.cs.d/Celeste/Player.cs:135 `MaxRun = 90f`).
const RUN: Fx = Fx::from_ratio(3, 2);
/// Speed threshold for the idle / edge branch: 25 px/s (5/12 px/tick), vanilla
/// `Math.Abs(Speed.X) <= 25f && moveX == 0`
/// (references/Celeste.exe.cs.d/Celeste/Player.cs:2151 `Math.Abs(Speed.X) <= 25f && moveX == 0`).
///
/// **This is not the same threshold as [`RUN`]**: the runSlow/runFast split is
/// 45 px/s (references/Celeste.exe.cs.d/Celeste/Player.cs:2198 `Math.Abs(Speed.X) < 45f`); 25 px/s only decides
/// "have we finished moving". Mixing the two selects the wrong frame while
/// decelerating (this engine previously used 45).
const IDLE_SPEED: Fx = Fx::from_ratio(5, 12);
/// Horizontal acceleration 1000 px/s^2 (~0.278 px/tick^2) -- starting from rest does
/// not snap to top speed. Vanilla `RunAccel = 1000f`
/// (references/Celeste.exe.cs.d/Celeste/Player.cs:137 `RunAccel = 1000f`).
const RUN_ACCEL: Fx = Fx::from_ratio(5, 18);
/// Overspeed reduction 400 px/s^2: used only for **same-direction overspeed**
/// (|vx| > 90 and the same sign as the input); easing to a stop with no input and
/// braking in the opposite direction both use the full 1000 rate
/// (references/Celeste.exe.cs.d/Celeste/Player.cs:3714-3724). Definition: `RunReduce = 400f`
/// (references/Celeste.exe.cs.d/Celeste/Player.cs:139 `RunReduce = 400f`).
const RUN_REDUCE: Fx = Fx::from_ratio(1, 9);
/// Air control factor 0.65 -- air acceleration is a bit more than half of ground.
/// Vanilla `AirMult = 0.65f` (references/Celeste.exe.cs.d/Celeste/Player.cs:141 `AirMult = 0.65f`).
const AIR_MULT: Fx = Fx::from_ratio(13, 20);
/// Gravity 900 px/s^2 (0.25 px/tick^2).
/// Vanilla `Gravity = 900f` (references/Celeste.exe.cs.d/Celeste/Player.cs:127 `Gravity = 900f`).
const GRAVITY: Fx = Fx::from_ratio(1, 4);
/// Apex half gravity: while rising or falling very slowly and holding jump, gravity
/// is halved (450 px/s^2). Vanilla multiplies gravity by 0.5
/// (references/Celeste.exe.cs.d/Celeste/Player.cs:3780 `? 0.5f : 1f`).
const HALF_GRAVITY: Fx = Fx::from_ratio(1, 8);
/// Normal fall-speed cap 160 px/s (8/3 px/tick).
///
/// **Reference value**: `MaxFall = 160f` (references/Celeste.exe.cs.d/Celeste/Player.cs:125 `MaxFall = 160f`).
/// It is an **eased state** (see [`Player::max_fall`]): holding "down" ramps the cap
/// toward [`FAST_MAX_FALL`], releasing eases it back -- not a hard cut.
const MAX_FALL: Fx = Fx::from_ratio(8, 3);
/// Fast-fall cap 240 px/s (4 px/tick). Reference `FastMaxFall = 240f`
/// (references/Celeste.exe.cs.d/Celeste/Player.cs:131 `FastMaxFall = 240f`).
const FAST_MAX_FALL: Fx = Fx::from_int(4);
/// Easing rate of the fall-speed cap: 300 px/s^2 (1/12 px/tick^2). Reference
/// `FastMaxAccel = 300f` (references/Celeste.exe.cs.d/Celeste/Player.cs:133 `FastMaxAccel = 300f`).
const FAST_MAX_ACCEL: Fx = Fx::from_ratio(1, 12);
/// Apex half-gravity threshold 40 px/s (2/3 px/tick). Reference
/// `HalfGravThreshold = 40f` (references/Celeste.exe.cs.d/Celeste/Player.cs:129 `HalfGravThreshold = 40f`).
const HALF_GRAV_THRESHOLD: Fx = Fx::from_ratio(2, 3);
/// Jump vertical speed -105 px/s (-7/4 px/tick). `Player.Jump`:
/// `Speed.Y = -105f` (references/Celeste.exe.cs.d/Celeste/Player.cs:2454).
const JUMP_SPEED: Fx = Fx::from_ratio(-7, 4);
/// Jump horizontal boost 40 px/s (2/3 px/tick): `Speed.X += 40f * moveX`
/// (references/Celeste.exe.cs.d/Celeste/Player.cs:2453). Jumping while running briefly overspeeds, then gets
/// pulled back at the 400 rate.
const JUMP_H_BOOST: Fx = Fx::from_ratio(2, 3);
/// Quarter turn in Q16.16 radians (`MathHelper.PiOver2`). The math crate keeps its
/// own copy private to `trig`, so the angle constants the dust directions need are
/// declared here.
const PI_OVER_2: Fx = Fx::from_bits(102_944);
/// `Math.PI / 4f`.
const PI_OVER_4: Fx = Fx::from_bits(51_472);
/// `Math.PI * 3f / 4f`.
const THREE_PI_OVER_4: Fx = Fx::from_bits(154_415);
/// Landing-dust speed gate 80 px/s (4/3 px/tick): `if (Speed.Y >= 80f)` before the
/// landing `Dust.Burst` (references/Celeste.exe.cs.d/Celeste/Player.cs:3358).
const LAND_DUST_SPEED: Fx = Fx::from_ratio(4, 3);
/// Landing dust only fires once the fall is this fast -- vanilla uses the same
/// threshold for the burst (8 particles, references/Celeste.exe.cs.d/Celeste/Player.cs:3360).
const LAND_DUST_COUNT: u8 = 8;

/// varJump duration 0.2s = 12 ticks (references/Celeste.exe.cs.d/Celeste/Player.cs:2447).
///
/// While jump is held, vertical speed is clamped back to the jump value
/// (references/Celeste.exe.cs.d/Celeste/Player.cs:3791-3797) -- gravity is fully cancelled, giving a full jump of
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
/// (references/Celeste.exe.cs.d/Celeste/Player.cs:4494).
const DASH_SPEED: Fx = Fx::from_int(4);
/// Dash end speed 160 px/s (8/3 px/tick): after 0.15s `Speed = DashDir * 160f`
/// (references/Celeste.exe.cs.d/Celeste/Player.cs:4573, when `DashDir.Y <= 0`).
const DASH_END_SPEED: Fx = Fx::from_ratio(8, 3);
/// Dash duration 0.15s = 9 ticks (`yield return 0.15f`, references/Celeste.exe.cs.d/Celeste/Player.cs:4566).
const DASH_TICKS: u16 = 9;
/// Dash cooldown 0.2s = 12 ticks (`DashBegin`: `dashCooldownTimer = 0.2f`, references/Celeste.exe.cs.d/Celeste/Player.cs:4290).
const DASH_COOLDOWN_TICKS: u16 = 12;
/// Cooldown after a ground refill during which dashing is blocked: 0.1s = 6 ticks
/// (references/Celeste.exe.cs.d/Celeste/Player.cs:4294; vanilla only writes it and never reads it, ported anyway to keep the
/// field complete).
const DASH_REFILL_COOLDOWN_TICKS: u16 = 6;
/// Dash attack window 0.3s = 18 ticks (`dashAttackTimer = 0.3f`, references/Celeste.exe.cs.d/Celeste/Player.cs:4303).
const DASH_ATTACK_TICKS: u16 = 18;
/// Upward corner correction reach in pixels (vanilla `UpwardCornerCorrection = 4`,
/// references/Celeste.exe.cs.d/Celeste/Player.cs:171; the constant is inlined at its use site, references/Celeste.exe.cs.d/Celeste/Player.cs:3374-3395).
const UPWARD_CORNER_CORRECTION: i32 = 4;
/// Upward corner correction reach while dash-attacking with no horizontal speed
/// (vanilla `DashingUpwardCornerCorrection = 5`, references/Celeste.exe.cs.d/Celeste/Player.cs:173; selected at
/// references/Celeste.exe.cs.d/Celeste/Player.cs:3376).
const DASHING_UPWARD_CORNER_CORRECTION: i32 = 5;
/// Dash corner correction reach in pixels (vanilla `DashCornerCorrection = 4`,
/// references/Celeste.exe.cs.d/Celeste/Player.cs:225; the loop bound at references/Celeste.exe.cs.d/Celeste/Player.cs:3189 is the literal 4).
const DASH_CORNER_CORRECTION: i32 = 4;
/// The `0.01f` speed comparisons vanilla uses to pick a corner-correction direction
/// (references/Celeste.exe.cs.d/Celeste/Player.cs:3293, references/Celeste.exe.cs.d/Celeste/Player.cs:3379-3395).
const SPEED_EPS: Fx = Fx::from_ratio(1, 100);
/// Wall jump horizontal speed 130 px/s (13/6 px/tick, `WallJump`: `Speed.X = 130f*dir`,
/// references/Celeste.exe.cs.d/Celeste/Player.cs:2585).
const WALLJUMP_X: Fx = Fx::from_ratio(13, 6);
/// Wall jump probe distance 5px: `CollideCheck<Solid>(Position + UnitX*dir*5)`
/// (`WallJumpCheck`, the `num = 5` normal branch at references/Celeste.exe.cs.d/Celeste/Player.cs:2528-2553).
const WALLJUMP_PROBE: Fx = Fx::from_int(5);
/// Duration of the forced moveX after a wall jump: 0.16s = 10 ticks (references/Celeste.exe.cs.d/Celeste/Player.cs:2581
/// `forceMoveXTimer = 0.16f`; the anti-re-grab window while a direction is held).
const FORCE_MOVE_X_TICKS: u16 = 10;
/// Super wall jump horizontal speed 170 px/s (17/6 px/tick, `SuperWallJump`:
/// `Speed.X = 170f*dir`, references/Celeste.exe.cs.d/Celeste/Player.cs:2626).
///
/// Note: the value was once mis-written as 240 (4 px/tick) -- the section comment was
/// fixed but the constant was not. `super_wall_jump_pins_vanilla_speeds` now pins
/// these two values.
const SUPER_WALLJUMP_X: Fx = Fx::from_ratio(17, 6);
/// Super wall jump vertical speed -160 px/s (-8/3 px/tick, `varJumpSpeed =
/// (Speed.Y = -160f)`, references/Celeste.exe.cs.d/Celeste/Player.cs:2627).
const SUPER_WALLJUMP_Y: Fx = Fx::from_ratio(-8, 3);
/// Super jump (jumping during a ground horizontal dash) horizontal speed 260 px/s
/// (13/3 px/tick, `SuperJump`: `Speed.X = 260f*Facing`, references/Celeste.exe.cs.d/Celeste/Player.cs:2497).
const SUPER_JUMP_X: Fx = Fx::from_ratio(13, 3);
/// Hair flash duration 0.12s = 7 ticks (`UpdateHair`: `hairFlashTimer = 0.12f`,
/// references/Celeste.exe.cs.d/Celeste/Player.cs:2020).
const HAIR_FLASH_TICKS: u8 = 7;
/// Hair chain defaults (vanilla `PlayerHair` fields, references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:25-28,
/// re-assigned by `Player.Update`'s final else branch, references/Celeste.exe.cs.d/Celeste/Player.cs:1731-1736):
/// `StepPerSegment = (0, 2)`, `StepInFacingPerSegment = 0.5`, `StepApproach = 64`,
/// `StepYSinePerSegment = 0`.
const HAIR_STEP_PER_SEGMENT: Vec2 = Vec2::new(Fx::zero(), Fx::from_int(2));
const HAIR_STEP_IN_FACING: Fx = Fx::from_ratio(1, 2);
const HAIR_STEP_APPROACH: Fx = Fx::from_int(64);
const HAIR_STEP_Y_SINE: Fx = Fx::zero();
/// Two-dash hair config (vanilla `Player.Update`'s `Dashes > 1` branch,
/// references/Celeste.exe.cs.d/Celeste/Player.cs:1714-1724): `StepInFacingPerSegment = 0`, `StepApproach = 90`,
/// `StepYSinePerSegment = 1`, and `StepPerSegment` becomes a wave at
/// `sin(TimeActive * 2) * 0.7 - Facing * 3` / `sin(TimeActive)`.
const HAIR_TWO_DASH_APPROACH: Fx = Fx::from_int(90);
/// Longest distance allowed between two hair nodes (`PlayerHair.AfterUpdate`'s
/// `float num = 3f`, references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:69/references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:78).
const HAIR_MAX_SEGMENT: Fx = Fx::from_int(3);
/// Hair color ease-in rate 6/s (`Lerp(Hair.Color, Used, 6f*dt)`, references/Celeste.exe.cs.d/Celeste/Player.cs:2007-2011; the
/// per-tick lerp factor = 6/60 = 0.1).
/// Max climb stamina 110 (`ClimbMaxStamina`, references/Celeste.exe.cs.d/Celeste/Player.cs:253; full at spawn,
/// references/Celeste.exe.cs.d/Celeste/Player.cs:429 `Stamina = 110f`).
const CLIMB_MAX_STAMINA: Fx = Fx::from_ratio(110, 1);
/// Upward climb stamina drain: vanilla's literal `45.454544f`/s (references/Celeste.exe.cs.d/Celeste/Player.cs:4015-4017,
/// draining 110 in ~2.42s) converted per tick by /60.
const CLIMB_UP_DRAIN_PER_TICK: Fx = Fx::from_ratio(45454544, 60000000);
/// Stamina drain for holding still 10/s (references/Celeste.exe.cs.d/Celeste/Player.cs:4027-4029).
const CLIMB_IDLE_DRAIN_PER_TICK: Fx = Fx::from_ratio(1, 6);
/// Stamina drain for a climb jump (without leaving the ground) 27.5 (references/Celeste.exe.cs.d/Celeste/Player.cs:2653-2655).
const CLIMBJUMP_DRAIN: Fx = Fx::from_ratio(55, 2);
/// Below 20 effective stamina the player is too tired to grab a wall
/// (`references/Celeste.exe.cs.d/Celeste/Player.cs:1048` `private bool IsTired => CheckStamina < 20f`).
const CLIMB_TIRED_THRESHOLD: Fx = Fx::from_int(20);
/// Upward climb speed -45 px/s (-3/4 px/tick, references/Celeste.exe.cs.d/Celeste/Player.cs:3969-3971).
const CLIMB_UP_SPEED: Fx = Fx::from_ratio(-3, 4);
/// Downward climb speed 80 px/s (4/3 px/tick, references/Celeste.exe.cs.d/Celeste/Player.cs:3977-3979).
const CLIMB_DOWN_SPEED: Fx = Fx::from_ratio(4, 3);
/// Slip speed when holding without input 30 px/s (1/2 px/tick, `num = 30f` at
/// references/Celeste.exe.cs.d/Celeste/Player.cs:3995-3997).
const CLIMB_SLIP_SPEED: Fx = Fx::from_ratio(1, 2);
/// Hop off the top of a climbed wall: horizontal 100 px/s (5/3 px/tick, `ClimbHop`
/// references/Celeste.exe.cs.d/Celeste/Player.cs:4117), vertical clamped to <= -120 px/s (-2 px/tick, references/Celeste.exe.cs.d/Celeste/Player.cs:4121).
const CLIMB_HOP_X: Fx = Fx::from_ratio(5, 3);
const CLIMB_HOP_Y: Fx = Fx::from_int(-2);
/// Duck hitbox: 8x6, offset (-4,-6) (`duckHitbox`, references/Celeste.exe.cs.d/Celeste/Player.cs:607; its bottom
/// edge aligns with the normal hitbox -> the engine's top-left y offset = 11-6 = 5).
const DUCK_H: i32 = 6;
const DUCK_Y_OFF: i32 = 5;
/// Ground deceleration while ducking 500 px/s^2 (references/Celeste.exe.cs.d/Celeste/Player.cs:3796-3798:
/// `Approach(Speed.X, 0, 500*dt)`; per tick^2, 500/3600 = 5/36 px/tick^2).
const DUCK_DECEL: Fx = Fx::from_ratio(5, 36);
/// Wall slide speed eased from 160 to 20 px/s (references/Celeste.exe.cs.d/Celeste/Player.cs:3989: `Lerp(160, 20,
/// wallSlideTimer/1.2)`).
const WALLSLIDE_FAST: Fx = Fx::from_ratio(8, 3);
const WALLSLIDE_SLOW: Fx = Fx::from_ratio(1, 3);
/// Wall slide timer maximum 1.2s = 72 ticks (`wallSlideTimer = 1.2f`, references/Celeste.exe.cs.d/Celeste/Player.cs:509).
const WALLSLIDE_TICKS: u16 = 72;
/// Climb start stagger 0.1s = 6 ticks (`climbNoMoveTimer = 0.1f`, references/Celeste.exe.cs.d/Celeste/Player.cs:3895).
const CLIMB_NO_MOVE_TICKS: u16 = 6;
/// Push window toward the wall after a climb jump: 0.2s = 12 ticks (`ClimbJump`:
/// `wallBoostTimer = 0.2f`, references/Celeste.exe.cs.d/Celeste/Player.cs:2659-2662).
const WALL_BOOST_TICKS: u16 = 12;
const HAIR_LERP_PER_TICK: f32 = 0.1;

/// Jump input buffer: 0.08s = 5 ticks (including the press frame).
///
/// Vanilla `VirtualButton`'s `BufferTime`: Jump = 0.08f (references/Celeste.exe.cs.d/Celeste/Input.cs:153); it
/// fills on press, decays every frame, and `Pressed` is true while the count > 0
/// (references/Celeste.exe.cs.d/Monocle/VirtualButton.cs:43-64 and
/// references/Celeste.exe.cs.d/Monocle/VirtualButton.cs:107-125) -- a jump pressed
/// within 4 ticks before landing still fires on
/// landing.
const JUMP_BUFFER_FRAMES: u32 = 5;
/// Coyote time: 0.1s (`jumpGraceTimer`, references/Celeste.exe.cs.d/Celeste/Player.cs:1581-1588 -- refilled to
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
    /// Dash button (vanilla `Input.Dash`, default X, references/Celeste.exe.cs.d/Celeste/Settings.cs:212). Buffering matches jump
    /// (`VirtualButton.BufferTime = 0.08f`, references/Celeste.exe.cs.d/Celeste/Input.cs:153).
    pub dash: bool,
    /// Grab button (vanilla `Input.Grab`, default Z/V/LeftShift, references/Celeste.exe.cs.d/Celeste/Settings.cs:203-206).
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
    /// `Axis` semantics, references/Celeste.exe.cs.d/Monocle/MInput.cs:1469-1478).
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
    /// (`Axis(bool, bool, bothValue)`, references/Celeste.exe.cs.d/Monocle/MInput.cs:1469-1478: pressing both returns 0).
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
    /// [`FAST_MAX_FALL`] at [`FAST_MAX_ACCEL`] -- reference vanilla's `maxFall`
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
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:3791-3797).
    var_jump: u32,
    /// Remaining ticks of forced varJump clamping (vanilla `AutoJump` +
    /// `AutoJumpTimer`, references/Celeste.exe.cs.d/Celeste/Player.cs:2339-2343: auto-jump on an up transition, clamping for
    /// 0.2s regardless of input).
    auto_jump: u32,
    /// Current dash count (vanilla `Player.Dashes`, constructed as MaxDashes around
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:609).
    pub dashes: u8,
    /// Dash inventory maximum (vanilla `Player.MaxDashes`; the Prologue
    /// `Inventory.Prologue` has no second dash -> 1, references/Celeste.exe.cs.d/Celeste/Player.cs:1170).
    pub max_dashes: u8,
    /// Previous tick's dash count (vanilla `lastDashes`, used for the hair flash
    /// check, references/Celeste.exe.cs.d/Celeste/Player.cs:2044).
    last_dashes: u8,
    /// Remaining hair flash time (vanilla `hairFlashTimer`, references/Celeste.exe.cs.d/Celeste/Player.cs:2020).
    hair_flash: u8,
    /// The **current** hair color (the carrier for `UpdateHair`'s per-frame lerp,
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:2007-2011).
    hair_color_current: [f32; 4],
    /// Simulated hair node positions in **world space** (vanilla `PlayerHair.Nodes`,
    /// references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:34). Node 0 is pinned to the head every tick (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:62) and the
    /// later nodes chase their parents, so the chain genuinely trails the player -- storing
    /// it relative to the player would hide the drag.
    hair_nodes: [Vec2; crate::sprites::HAIR_MAX],
    /// Live segment count (vanilla `PlayerSprite.HairCount`: 4 normally, 5 with two
    /// dashes -- references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:115 and references/Celeste.exe.cs.d/Celeste/Player.cs:1735).
    hair_count: usize,
    /// Seconds since spawn (vanilla `Scene.TimeActive`), read by the two-dash hair
    /// config (references/Celeste.exe.cs.d/Celeste/Player.cs:1716-1718).
    hair_time: Fx,
    /// `PlayerHair.wave` (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:88: `wave += dt * 4`), the phase of the per-segment sine.
    hair_wave: Fx,
    /// `PlayerHair.Start` has run (vanilla `startHairCalled`, references/Celeste.exe.cs.d/Celeste/Player.cs:1986-1994).
    hair_started: bool,
    /// `Player.Sprite.Scale` (vanilla starts at `Vector2.One`); springs back to 1 in
    /// `UpdateSprite` (references/Celeste.exe.cs.d/Celeste/Player.cs:2051-2052) and is setback by dash/jump/hit actions
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:2473/references/Celeste.exe.cs.d/Celeste/Player.cs:2744/references/Celeste.exe.cs.d/Celeste/Player.cs:2786/references/Celeste.exe.cs.d/Celeste/Player.cs:3336/references/Celeste.exe.cs.d/Celeste/Player.cs:3741). Read twice by the sim: the hair root
    /// Y-offset is `-9 * Scale.Y` (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:63) and the hair rigidity gate below.
    sprite_scale: [Fx; 2],
    /// `PlayerHair.SimulateMotion` (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:23, set at references/Celeste.exe.cs.d/Celeste/Player.cs:2045): when false the hair chain does
    /// **not** chase its parent (the references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:73-77 approach is skipped), so it goes rigid
    /// and is only held within the max-segment constraint (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:78-80). Gated on
    /// `applyGravity`, which vanilla passes through `UpdateHair(applyGravity)`.
    simulate_motion: bool,
    /// State machine (vanilla `StateMachine.State`, numbered as vanilla: 0 = Normal,
    /// 2 = Dash, references/Celeste.exe.cs.d/Celeste/Player.cs:1146-1158; 1/3/8 etc. not implemented).
    state: u8,
    /// Dash direction (vanilla `DashDir`, an eight-way unit vector with components
    /// +/-1/0/+/-0.707).
    dash_dir: Vec2,
    /// Dash coroutine phase: 0 = entry tick (`yield return null`, speed not yet set),
    /// 1..=9 = the 0.15s wait, ending on tick 10 (`DashCoroutine` references/Celeste.exe.cs.d/Celeste/Player.cs:4480-4580).
    dash_phase: u16,
    /// Snapshot of speed at the jump/dash instant (vanilla `beforeDashSpeed`, references/Celeste.exe.cs.d/Celeste/Player.cs:4308).
    before_dash_speed: Vec2,
    /// Dash cooldown (vanilla `dashCooldownTimer`, 0.2s, references/Celeste.exe.cs.d/Celeste/Player.cs:4290).
    dash_cooldown: u16,
    /// Dash refill cooldown (vanilla `dashRefillCooldownTimer`, 0.1s, references/Celeste.exe.cs.d/Celeste/Player.cs:4294).
    dash_refill_cooldown: u16,
    /// Dash attack window (vanilla `dashAttackTimer`, 0.3s, references/Celeste.exe.cs.d/Celeste/Player.cs:4303).
    dash_attack: u16,
    /// Vanilla `dashStartedOnGround` (references/Celeste.exe.cs.d/Celeste/Player.cs:543): `onGround` captured at `DashBegin`
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:4286). The air-dash landing correction only runs when it is **false**
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:3291), so a dash that started on the ground keeps ordinary landing rules.
    dash_started_on_ground: bool,
    /// Target speed for varJump clamping (vanilla `varJumpSpeed` field: `Jump()` sets
    /// -105 (references/Celeste.exe.cs.d/Celeste/Player.cs:2447), `WallJump` -105 (references/Celeste.exe.cs.d/Celeste/Player.cs:2586), `BeforeUpTransition` -105 (references/Celeste.exe.cs.d/Celeste/Player.cs:2337),
    /// `Rebound` -120; the clamp is `min(Speed.Y, varJumpSpeed)`, references/Celeste.exe.cs.d/Celeste/Player.cs:3883).
    var_jump_speed: Fx,
    /// Forced horizontal input (vanilla `forceMoveX` + `forceMoveXTimer`,
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:1632-1638: moveX is overridden for 0.16s after a wall jump, preventing an
    /// immediate re-grab).
    force_move_x: i32,
    force_move_x_ticks: u16,
    /// Horizontal input in effect this tick (`moveX`: input or the forceMoveX
    /// override, references/Celeste.exe.cs.d/Celeste/Player.cs:1632-1640).
    move_x: Fx,
    /// Previous tick's dash button (buffer edge).
    prev_dash: bool,
    /// Dash button buffer (`Input.Dash`'s VirtualButton.BufferTime = 0.08s, references/Celeste.exe.cs.d/Celeste/Input.cs:153).
    dash_buffer: u16,
    /// Freeze request at dash start (`Celeste.Freeze(0.05f)`, references/Celeste.exe.cs.d/Celeste/Player.cs:4289; consumed by the
    /// service).
    pub freeze_ticks: u8,
    /// Scene clock in seconds, advanced once per tick (vanilla `Scene.TimeActive`,
    /// references/Celeste.exe.cs.d/Monocle/Scene.cs:67 `TimeActive += Engine.DeltaTime`). Drives the
    /// `Scene.OnInterval` gates on the periodic dust emitters.
    scene_time: Fx,
    /// Queued `Dust.Burst` emissions for this tick, drained by the service into
    /// [`crate::particles::ParticleSystem`] (vanilla emits straight into
    /// `Level.Particles`, but the player does not own that system here).
    dust: Vec<crate::particles::Burst>,
    /// Queued screen-shake request at dash start
    /// (`SceneAs<Level>().DirectionalShake(DashDir, 0.2f)`, references/Celeste.exe.cs.d/Celeste/Player.cs:4532), drained by the
    /// service into [`crate::shake::Shake`] (vanilla keeps the shake on the level).
    shake_request: Option<(Vec2, Fx)>,
    /// Animation state (vanilla `Sprite`: current animation id, frame, timer, playing).
    pub anim_id: String,
    pub anim_frame: usize,
    anim_timer: f32,
    anim_playing: bool,
    /// The id most recently passed to `play` (vanilla `Sprite.LastAnimationID`,
    /// references/Celeste.exe.cs.d/Monocle/Sprite.cs:56, written at references/Celeste.exe.cs.d/Monocle/Sprite.cs:366-367). `PlayerSprite.Running`
    /// (references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:169-182) and `UpdateSprite`'s run guard (references/Celeste.exe.cs.d/Celeste/Player.cs:2196) both read it -- "is it
    /// running" is **not** the same as "does the current animation have a goto"
    /// (`idle`/`edge`/`walk` all carry `<Loop>`).
    last_anim_id: String,
    /// Fast jump/fall flag (`fastJump`, references/Celeste.exe.cs.d/Celeste/Player.cs:2186/references/Celeste.exe.cs.d/Celeste/Player.cs:2218).
    fast_jump: bool,
    /// Stamina (`Stamina`, 110 at spawn per references/Celeste.exe.cs.d/Celeste/Player.cs:429; drained by climbing, refilled on
    /// landing references/Celeste.exe.cs.d/Celeste/Player.cs:1567-1572).
    pub stamina: Fx,
    /// Ducking (`Ducking`; hitbox 8x6, references/Celeste.exe.cs.d/Celeste/Player.cs:607).
    pub ducking: bool,
    /// Climb start stagger (`climbNoMoveTimer`, 0.1s, references/Celeste.exe.cs.d/Celeste/Player.cs:3895).
    climb_no_move_ticks: u16,
    /// Previous tick's climb move direction (`lastClimbMove`, references/Celeste.exe.cs.d/Celeste/Player.cs:4000).
    last_climb_move: i32,
    /// Wall slide direction/timer (`wallSlideDir`/`wallSlideTimer`, references/Celeste.exe.cs.d/Celeste/Player.cs:507-509).
    wall_slide_dir: i32,
    wall_slide_ticks: u16,
    /// Climb jump boost (`wallBoostDir`/`wallBoostTimer`, references/Celeste.exe.cs.d/Celeste/Player.cs:868-1569).
    wall_boost_dir: i32,
    wall_boost_ticks: u16,
    /// Solid still beside the player at `ClimbHop()` time (vanilla `climbHopSolid`,
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:4130-4135). While set, the horizontal push is deferred to `hopWaitX` /
    /// `hopWaitXSpeed` instead of being applied immediately.
    climb_hop_solid: bool,
    /// Pending horizontal push direction (vanilla `hopWaitX`, references/Celeste.exe.cs.d/Celeste/Player.cs:4132/references/Celeste.exe.cs.d/Celeste/Player.cs:4135: the
    /// facing sign, or 0 when no solid was found).
    hop_wait_x: i32,
    /// Pending horizontal push speed (vanilla `hopWaitXSpeed`, references/Celeste.exe.cs.d/Celeste/Player.cs:4133:
    /// `Facing * 100f`).
    hop_wait_x_speed: Fx,
}

/// Fx version of `Math.Sign(before.X) == Math.Sign(speed.X)` (references/Celeste.exe.cs.d/Celeste/Player.cs:4519).
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
            hair_nodes: [Vec2::ZERO; crate::sprites::HAIR_MAX],
            hair_count: crate::sprites::HAIR_COUNT,
            hair_time: Fx::zero(),
            hair_wave: Fx::zero(),
            hair_started: false,
            sprite_scale: [Fx::one(), Fx::one()],
            simulate_motion: true,
            state: 0,
            dash_dir: Vec2::ZERO,
            dash_phase: 0,
            before_dash_speed: Vec2::ZERO,
            dash_cooldown: 0,
            dash_refill_cooldown: 0,
            dash_attack: 0,
            dash_started_on_ground: false,
            var_jump_speed: JUMP_SPEED,
            force_move_x: 0,
            force_move_x_ticks: 0,
            move_x: Fx::zero(),
            prev_dash: false,
            dash_buffer: 0,
            freeze_ticks: 0,
            scene_time: Fx::zero(),
            dust: Vec::new(),
            shake_request: None,
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
            climb_hop_solid: false,
            hop_wait_x: 0,
            hop_wait_x_speed: Fx::zero(),
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
    /// Mirrors the structure of vanilla `Player.Update` (references/Celeste.exe.cs.d/Celeste/Player.cs:1449-1917):
    /// timers/refill/facing (references/Celeste.exe.cs.d/Celeste/Player.cs:1576-1670) -> `base.Update()` into the state machine
    /// (registered references/Celeste.exe.cs.d/Celeste/Player.cs:1146-1158: 0 = NormalUpdate, 2 = DashUpdate) -> MoveH/MoveV
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:1798-1805) -> EnforceBounds is handled by the service.
    ///
    /// `rooms` is passed explicitly so unit tests can build room geometry directly
    /// without spinning up a whole Session.
    pub(crate) fn step(&mut self, input: &Input, rooms: &[Room]) {
        // Vanilla samples every `VirtualButton` once per tick (`MInput.Update` runs before
        // `Player.Update`), so the buffered-input edges belong here rather than in the
        // state-specific updates -- `ClimbUpdate` reads the same buffers.
        self.sample_input_edges(input);
        // `Scene.TimeActive += Engine.DeltaTime` happens before the scene update, so the
        // player always sees a clock that already includes this tick
        // (references/Celeste.exe.cs.d/Monocle/Scene.cs:67).
        self.scene_time += Fx::from_bits(Fx::DT_ONE_SIXTIETH);
        self.update_head(input, rooms);
        match self.state {
            1 => self.climb_update(input, rooms),
            2 => self.dash_update(input, rooms),
            _ => self.normal_update(input, rooms),
        }
        self.clamp_to_world(rooms);
        self.update_hair_color();
    }

    /// Animation advance + state mapping (vanilla calls `UpdateSprite` after movement,
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:1817; `Sprite.Update` runs every frame via `base.Update`).
    pub fn update_visuals(
        &mut self,
        table: &crate::sprites::AnimationTable,
        input: &Input,
        rooms: &[Room],
    ) {
        // Never played an animation -> the start animation (the sprite element's
        // `start`, validated at references/Celeste.exe.cs.d/Monocle/SpriteData.cs:42-45).
        if self.anim_id.is_empty() {
            if let Some(def) = table.sprites.get("player") {
                let start = def.start.clone();
                self.play(table, &start);
            }
        }
        self.update_animation(table);
        self.update_sprite(table, input, rooms);
        // `Player.UpdateHair(applyGravity: true)` arms the chain again every normal tick
        // (references/Celeste.exe.cs.d/Celeste/Player.cs:1811-1813, which sets `Hair.SimulateMotion = applyGravity` at
        // references/Celeste.exe.cs.d/Celeste/Player.cs:2045). Without this the flag would stay false after a transition and the hair
        // would never chase its target again.
        self.simulate_motion = true;
        self.update_hair(table);
    }

    /// Advance only the hair chain during a room transition, matching vanilla
    /// `Player.TransitionTo` -> `UpdateHair(applyGravity: false)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2300-2301,
    /// which sets `Hair.SimulateMotion = applyGravity` at references/Celeste.exe.cs.d/Celeste/Player.cs:2045). The main `Update` (and so
    /// `UpdateSprite`/`Sprite.Update`) is skipped while transitioning (Level.Update's
    /// `else if (!Transitioning)` branch, references/Celeste.exe.cs.d/Celeste/Level.cs:1869-1878), so the body pose stays frozen; the
    /// hair, however, is still advanced: node 0 follows the gliding head (`Nodes[0]`
    /// references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:62-63) while the chase toward `target` is skipped (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:73 is gated off), so
    /// the chain runs rigid and is only held within the max-segment clamp (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:78-80).
    /// This is the only observable effect of `SimulateMotion == false`, so keep the flag
    /// persistent until the next normal `update_visuals` restores it (`UpdateHair(true)`).
    pub fn update_hair_rigid(&mut self, table: &crate::sprites::AnimationTable) {
        self.simulate_motion = false;
        self.update_hair(table);
    }

    /// The simulated hair chain for this tick, in **world space** like vanilla
    /// `PlayerHair.Nodes` (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:34); node 0 is the head.
    ///
    /// Its length is the live `PlayerSprite.HairCount` (4, or 5 while two dashes are
    /// stocked, references/Celeste.exe.cs.d/Celeste/Player.cs:1735).
    pub fn hair_nodes(&self) -> &[Vec2] {
        &self.hair_nodes[..self.hair_count]
    }

    /// The sprite/hair anchor: the **hitbox bottom centre** (vanilla `Player.Position`,
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:605). The hitbox's bottom edge is `pos.y + PLAYER_H` both standing
    /// and ducking (`DUCK_H + DUCK_Y_OFF = 11`), so the anchor never moves on a crouch.
    pub fn anchor(&self) -> Vec2 {
        self.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H)
    }

    /// The hitbox centre (vanilla `Monocle.Entity.Center`, which is collider-relative:
    /// `CenterX = Left + Width * 0.5`, `CenterY = Top + Height * 0.5`,
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:361).
    fn center(&self) -> Vec2 {
        let (w, h, y_off) = self.box_dims();
        // The ducking hitbox is the normal box moved down by `y_off`, so its centre is
        // `y_off + h / 2` below the stored top-left.
        self.pos
            + Vec2::new(
                Fx::from_int(w) / Fx::from_int(2),
                Fx::from_int(y_off) + Fx::from_int(h) / Fx::from_int(2),
            )
    }

    /// Queued dust bursts for this tick, drained by `GameService` into the particle
    /// system (vanilla emits straight into `Level.Particles`; the player does not own
    /// that system here).
    pub(crate) fn dust(&mut self) -> &mut Vec<crate::particles::Burst> {
        &mut self.dust
    }

    /// Take the pending screen-shake request, if any (see
    /// [`crate::shake::Shake`]).
    pub(crate) fn take_shake_request(&mut self) -> Option<(Vec2, Fx)> {
        self.shake_request.take()
    }

    /// Queue a single `Dust.Burst` (vanilla `Dust.Burst(position, direction, count)`,
    /// references/Celeste.exe.cs.d/Celeste/Dust.cs:9-23).
    fn burst(&mut self, position: Vec2, direction: Fx, count: u8) {
        self.dust
            .push(crate::particles::Burst::new(position, direction, count));
    }

    /// `CreateWallSlideParticles(dir)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2746-2760): one dust particle
    /// per `OnInterval(0.01f)` tick, 5px toward the wall and 4px below the centre.
    fn wall_slide_dust(&mut self) {
        if !self.on_interval(Fx::from_ratio(1, 100)) {
            return;
        }
        let dir = self.wall_slide_dir;
        // `float num = (particleType == ParticleTypes.Dust) ? 5f : 2f;` -- always Dust
        // here, so 5px.
        let offset = Vec2::new(Fx::from_int(5) * Fx::from_int(dir), Fx::from_int(4));
        let position = self.center() + offset;
        self.burst(position, -PI_OVER_2, 1);
    }

    /// `Scene.OnInterval(interval)`: true on the tick the integer interval slot
    /// advances (`(int)((TimeActive - DeltaTime) / interval) < (int)(TimeActive /
    /// interval)`, references/Celeste.exe.cs.d/Celeste/Player.cs:74-79).
    ///
    /// The player's periodic dust emitters use it: the wall-slide puff is
    /// `OnInterval(0.01f)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2746) -- shorter than a tick, so it fires every tick --
    /// and the skid puff is `OnInterval(0.02f)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2319), which fires on roughly every
    /// other tick.
    fn on_interval(&self, interval: Fx) -> bool {
        let span = i64::from(interval.bits());
        if span <= 0 {
            return true;
        }
        let previous = self.scene_time - Fx::from_bits(Fx::DT_ONE_SIXTIETH);
        previous.bits() as i64 / span < self.scene_time.bits() as i64 / span
    }

    /// Advance the hair: vanilla `PlayerHair.Update`'s wave (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:88), `Player.Update`'s
    /// hair config (references/Celeste.exe.cs.d/Celeste/Player.cs:1704-1739) and `PlayerHair.AfterUpdate` (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:60-85).
    ///
    /// The chain is a spring: the root is pinned to the head this tick, and each later
    /// node eases toward a target one `StepPerSegment` beyond its parent at
    /// `(1 - i/HairCount*0.5) * StepApproach` per second, never more than
    /// [`HAIR_MAX_SEGMENT`] from that parent.
    fn update_hair(&mut self, table: &crate::sprites::AnimationTable) {
        self.hair_time += releste_math::FIXED_DT;
        // `PlayerHair.Update`: `wave += Engine.DeltaTime * 4f` (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:88).
        self.hair_wave += Fx::from_int(4) * releste_math::FIXED_DT;
        let facing = Fx::from_int(self.facing);
        let anchor = self.anchor();

        // The wind branch (`windTimeout > 0`, references/Celeste.exe.cs.d/Celeste/Player.cs:1704-1712) is unreachable while wind
        // zones are unimplemented.
        let (step_per_segment, step_in_facing, step_approach, step_y_sine) = if self.dashes > 1 {
            // Two dashes (references/Celeste.exe.cs.d/Celeste/Player.cs:1714-1724): a wide wave, no facing step, approach 90, and a
            // per-segment sine (`StepYSinePerSegment = 1`).
            (
                Vec2::new(
                    (self.hair_time * Fx::from_int(2)).sin() * Fx::from_ratio(7, 10)
                        - facing * Fx::from_int(3),
                    self.hair_time.sin(),
                ),
                Fx::zero(),
                HAIR_TWO_DASH_APPROACH,
                Fx::one(),
            )
        } else {
            (
                HAIR_STEP_PER_SEGMENT,
                HAIR_STEP_IN_FACING,
                HAIR_STEP_APPROACH,
                HAIR_STEP_Y_SINE,
            )
        };

        // `Sprite.HairCount = ((Dashes > 1) ? 5 : startHairCount)` (references/Celeste.exe.cs.d/Celeste/Player.cs:1735; the Prologue
        // starts at 4, references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:115). The `State == 5` (RedDash) `HairCount = 1` override
        // (references/Celeste.exe.cs.d/Celeste/Player.cs:1729-1732) needs a state this port does not have yet.
        let count = if self.dashes > 1 {
            crate::sprites::HAIR_MAX
        } else {
            crate::sprites::HAIR_COUNT
        };
        // Growing the tail duplicates the previous last node (`Nodes.Add(Nodes[i - 1])`,
        // references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:72).
        for i in self.hair_count..count {
            self.hair_nodes[i] = self.hair_nodes[i - 1];
        }
        self.hair_count = count;

        if !self.hair_started {
            // `StartHair` -> `PlayerHair.Start` (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:50-57): the nodes begin 200px to
            // the side and below, so the tail swings in instead of stacking on the head.
            self.hair_started = true;
            let start = anchor + Vec2::new(-facing * Fx::from_int(200), Fx::from_int(200));
            self.hair_nodes = [start; crate::sprites::HAIR_MAX];
        }

        // `Nodes[0] = RenderPosition + (0, -9 * Scale.Y) + HairOffset * (Facing, 1)`
        // (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:62-63). `Scale.Y` springs back to 1 (references/Celeste.exe.cs.d/Celeste/Player.cs:2051-2052), but a dash
        // squeeze finds it < 1, pulling the root down toward the body.
        let offset = self.anim_frame_hair(table).offset;
        let root_y = Fx::from_int(offset.1) - Fx::from_int(9) * self.sprite_scale[1];
        self.hair_nodes[0] = anchor + Vec2::new(Fx::from_int(offset.0) * facing, root_y);

        let mut target = self.hair_nodes[0]
            + Vec2::new(
                -facing * step_in_facing * Fx::from_int(2),
                self.hair_wave.sin() * step_y_sine,
            )
            + step_per_segment;
        let mut previous = self.hair_nodes[0];
        for i in 1..self.hair_count {
            // `num2 = (1 - i / HairCount * 0.5) * StepApproach` (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:73-75), applied
            // per tick (`num2 * Engine.DeltaTime`); skipped entirely when
            // `SimulateMotion` is false (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:73) -- the chain goes rigid and is only held
            // within the max-segment constraint below.
            if self.simulate_motion {
                let rate = (Fx::one() - Fx::from_ratio(i as i32, (2 * self.hair_count) as i32))
                    * step_approach
                    * releste_math::FIXED_DT;
                self.hair_nodes[i] = approach_vec(self.hair_nodes[i], target, rate);
            }
            let delta = self.hair_nodes[i] - previous;
            if delta.length() > HAIR_MAX_SEGMENT {
                self.hair_nodes[i] = previous + delta.safe_normalized() * HAIR_MAX_SEGMENT;
            }
            target = self.hair_nodes[i]
                + Vec2::new(
                    -facing * step_in_facing,
                    (self.hair_wave + Fx::from_ratio(i as i32 * 8, 10)).sin() * step_y_sine,
                )
                + step_per_segment;
            previous = self.hair_nodes[i];
        }
    }

    /// Samples the buffered input edges, mirroring vanilla `VirtualButton`
    /// (`Monocle/VirtualButton.cs`).
    ///
    /// `VirtualButton.Update` decrements `bufferCounter` by `Engine.DeltaTime` on *every*
    /// tick, refills it to `BufferTime` only on a fresh `Binding.Pressed` edge, and zeroes
    /// it once the button is released. `Input.Jump`/`Input.Dash` both use 0.08s
    /// (`references/Celeste.exe.cs.d/Celeste/Input.cs:153-157`), i.e. the press frame plus four more -- hence
    /// [`JUMP_BUFFER_FRAMES`]. The engine therefore decays first and refills second, so a
    /// press that cannot be spent expires instead of waiting for a dash refill.
    fn sample_input_edges(&mut self, input: &Input) {
        // `bufferCounter -= Engine.DeltaTime;` (unconditional, before any decision).
        self.jump_buffer = self.jump_buffer.saturating_sub(1);
        self.dash_buffer = self.dash_buffer.saturating_sub(1);

        // `if (Binding.Pressed(GamepadIndex, Threshold)) bufferCounter = BufferTime;`
        let jump_pressed = input.jump && !self.prev_jump;
        self.prev_jump = input.jump;
        if jump_pressed {
            self.jump_buffer = JUMP_BUFFER_FRAMES;
        }
        let dash_pressed = input.dash && !self.prev_dash;
        self.prev_dash = input.dash;
        if dash_pressed {
            self.dash_buffer = JUMP_BUFFER_FRAMES as u16; // VirtualButton.BufferTime, likewise 0.08s (references/Celeste.exe.cs.d/Celeste/Input.cs:153)
        }
    }

    /// The shared timers and refill at the head of vanilla `Player.Update`
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:1576-1670), plus the `forceMoveX` / `climbHopSolid` / `hopWaitX` timers
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:1632-1669).
    fn update_head(&mut self, input: &Input, rooms: &[Room]) {
        // dashAttackTimer -= dt (references/Celeste.exe.cs.d/Celeste/Player.cs:1576-1579).
        self.dash_attack = self.dash_attack.saturating_sub(1);
        // Ground refill (references/Celeste.exe.cs.d/Celeste/Player.cs:1604-1612): the state 3 (swim) branch does not apply;
        // standing on a solid (`on_ground` means there is solid under the feet,
        // equivalent to `CollideCheck<Solid>(Position + UnitY)`) -> RefillDash
        // (references/Celeste.exe.cs.d/Celeste/Player.cs:2831-2839: writes only when not already full).
        //
        // The refill is an `else` of the refill-cooldown decay (references/Celeste.exe.cs.d/Celeste/Player.cs:1592-1612):
        // `if (dashRefillCooldownTimer > 0f) { dashRefillCooldownTimer -= Engine.DeltaTime; }
        // else if (...) { RefillDash(); }`. `DashBegin` arms it with 0.1s = 6 ticks
        // (references/Celeste.exe.cs.d/Celeste/Player.cs:4283-4314), so an upward ground dash gets no dash back for the first six
        // frames. This check therefore runs BEFORE the decay below.
        if self.dash_refill_cooldown == 0 && self.on_ground && self.dashes < self.max_dashes {
            self.dashes = self.max_dashes;
        }
        // jumpGraceTimer (references/Celeste.exe.cs.d/Celeste/Player.cs:1579-1589) -- already handled by the coyote counter at the
        // end of physics (semantically equivalent; see the COYOTE_FRAMES docs).
        // varJumpTimer / AutoJumpTimer decay (references/Celeste.exe.cs.d/Celeste/Player.cs:1613-1626).
        if self.var_jump > 0 {
            self.var_jump -= 1;
        }
        if self.auto_jump > 0 {
            self.auto_jump -= 1;
        }
        // dashCooldownTimer / dashRefillCooldownTimer (references/Celeste.exe.cs.d/Celeste/Player.cs:1592-1597).
        self.dash_cooldown = self.dash_cooldown.saturating_sub(1);
        self.dash_refill_cooldown = self.dash_refill_cooldown.saturating_sub(1);
        // Climb jump boost (references/Celeste.exe.cs.d/Celeste/Player.cs:1560-1569): push toward wallBoostDir -> horizontal speed
        // 130 + stamina +27.5, once.
        if self.wall_boost_ticks > 0 {
            self.wall_boost_ticks -= 1;
            if self.move_x == Fx::from_int(self.wall_boost_dir) {
                self.vel.x = WALLJUMP_X * self.move_x;
                self.stamina += CLIMBJUMP_DRAIN;
                self.wall_boost_ticks = 0;
            }
        }
        // Wall slide timer (references/Celeste.exe.cs.d/Celeste/Player.cs:1557-1561): the direction is cleared at the end of each
        // tick and the timer decays; NormalUpdate re-arms it while its condition holds.
        if self.wall_slide_dir != 0 {
            self.wall_slide_ticks = self.wall_slide_ticks.saturating_sub(1);
            self.wall_slide_dir = 0;
        }
        // Refill stamina on landing + reset the wall slide (references/Celeste.exe.cs.d/Celeste/Player.cs:1567-1572).
        if self.on_ground && self.state != 1 {
            self.auto_jump = 0;
            self.stamina = CLIMB_MAX_STAMINA;
            self.wall_slide_ticks = WALLSLIDE_TICKS;
        }
        // moveX: the forceMoveX override window (references/Celeste.exe.cs.d/Celeste/Player.cs:1632-1640), otherwise read input.
        // The `else` branch also drops the remembered hop solid (references/Celeste.exe.cs.d/Celeste/Player.cs:1638-1640), so a
        // deferred `ClimbHop` push is forgotten once the forced-move window closes.
        let move_x = if self.force_move_x_ticks > 0 {
            self.force_move_x_ticks -= 1;
            Fx::from_int(self.force_move_x)
        } else {
            self.climb_hop_solid = false;
            input.axis_x()
        };
        self.move_x = move_x;
        // A deferred `ClimbHop` push resolves as soon as the solid beside the player is
        // gone (references/Celeste.exe.cs.d/Celeste/Player.cs:1664-1669): `hopWaitX` is dropped when the player already moves the
        // other way or starts falling, otherwise `Speed.X` is set to the saved speed.
        if self.hop_wait_x != 0 {
            let wants_out = self.vel.x != Fx::zero() && self.vel.x.signum() == -self.hop_wait_x;
            if wants_out || self.vel.y > Fx::zero() {
                self.hop_wait_x = 0;
            } else if !self.hits(
                rooms,
                self.pos.x + Fx::from_int(self.hop_wait_x),
                self.pos.y,
            ) {
                self.vel.x = self.hop_wait_x_speed;
                self.hop_wait_x = 0;
            }
        }
        // Facing follows input, **except in Climb** (references/Celeste.exe.cs.d/Celeste/Player.cs:1657-1666: `if (moveX != 0 &&
        // InControl && StateMachine.State != 1 && ... != 8 && ... != 5 && ... != 6)`).
        // States 5/6/8 (red dash, swim, star fly) do not exist in this port; state 1 is
        // the one that matters: while climbing, holding a direction away from the wall
        // must NOT flip `Facing`, or the following `ClimbUpdate` jump branch reads the
        // wrong side and turns the intended `WallJump` into a `ClimbJump`.
        if move_x != Fx::zero() && self.state != 1 {
            self.facing = move_x.signum();
        }
    }

    /// Hair color: the non-gradient part of vanilla `Player.UpdateHair`
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:1993-2045) -- USED when the dash is spent, FLASH at the
    /// instant of refill (previous tick's count differs, or the white flash is still
    /// running), otherwise `Dashes == 2 ? TWO_DASHES : NORMAL`.
    ///
    /// `used` ease-in (`Lerp(Hair.Color, Used, 6*dt)`, references/Celeste.exe.cs.d/Celeste/Player.cs:2007-2011) and
    /// `hairFlashTimer = 0.12s` (references/Celeste.exe.cs.d/Celeste/Player.cs:2020) need per-frame color state; when dash lands,
    /// this function is upgraded to a version that carries the current color. For now
    /// it returns a discrete state, and the Prologue (one full dash in inventory) is
    /// always NORMAL -- consistent with vanilla.
    pub fn hair_color(&self) -> [f32; 4] {
        // The current per-frame lerp value (`UpdateHair` writes it into `Hair.Color`,
        // read directly by rendering).
        self.hair_color_current
    }

    /// `Player.Sprite.Scale`, converted for the renderer (render f32 is allowed; the
    /// sim tracks it in `Fx`, references/Celeste.exe.cs.d/Celeste/Player.cs:2051-2052 + the dash/jump setters).
    pub fn sprite_scale(&self) -> [f32; 2] {
        [self.sprite_scale[0].to_f32(), self.sprite_scale[1].to_f32()]
    }

    /// Adjustment before an up transition (vanilla `BeforeUpTransition`,
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:2339-2347): zero horizontal, auto-jump upward (-105 + 0.2s
    /// of forced varJump clamping), landing in the room above.
    pub fn before_up_transition(&mut self) {
        self.vel.x = Fx::zero();
        self.vel.y = JUMP_SPEED;
        self.var_jump = VAR_JUMP_TICKS;
        self.auto_jump = VAR_JUMP_TICKS;
    }

    /// Adjustment before a down transition (vanilla `BeforeDownTransition`,
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:2321-2332): cut any upward speed, clear varJump.
    pub fn before_down_transition(&mut self) {
        self.vel.y = self.vel.y.max(Fx::zero());
        self.var_jump = 0;
        self.auto_jump = 0;
    }

    /// On-room-transition refresh (vanilla `Player.OnTransition`,
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:2289-2296): refill the dash and stamina, re-arm the wall
    /// slide timer, drop the jump grace and any force-move override. Called once the
    /// transition actually completes (the old room's entities unload only here).
    pub fn on_transition(&mut self) {
        // wallSlideTimer = 1.2f (references/Celeste.exe.cs.d/Celeste/Player.cs:2290) -> 72 ticks.
        self.wall_slide_ticks = WALLSLIDE_TICKS;
        // jumpGraceTimer = 0f (references/Celeste.exe.cs.d/Celeste/Player.cs:2291).
        self.coyote = 0;
        // forceMoveXTimer = 0f (references/Celeste.exe.cs.d/Celeste/Player.cs:2292).
        self.force_move_x_ticks = 0;
        // ChaserStates.Clear() (references/Celeste.exe.cs.d/Celeste/Player.cs:2294) - no chaser state is ported yet; documented.
        // RefillDash() (references/Celeste.exe.cs.d/Celeste/Player.cs:2295, references/Celeste.exe.cs.d/Celeste/Player.cs:2831-2839: only when not already full).
        if self.dashes < self.max_dashes {
            self.dashes = self.max_dashes;
        }
        // RefillStamina() (references/Celeste.exe.cs.d/Celeste/Player.cs:2296, references/Celeste.exe.cs.d/Celeste/Player.cs:2857-2860).
        self.stamina = CLIMB_MAX_STAMINA;
        // Leader.TransferFollowers() (references/Celeste.exe.cs.d/Celeste/Player.cs:2297) - no leader follows are ported yet.
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

    /// Vanilla `CanUnDuck` (references/Celeste.exe.cs.d/Celeste/Player.cs:1030-1044): standing up must be collision-free for the
    /// **normal** hitbox -- the getter swaps `Collider = normalHitbox` around the check,
    /// which is exactly what stops Madeline from standing under a 1-tile gap.
    fn can_unduck(&self, rooms: &[Room]) -> bool {
        !self.ducking || !self.hits_normal(rooms, self.pos.x, self.pos.y)
    }

    /// Vanilla `CanUnDuckAt(position)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2917-2924): [`Player::can_unduck`] evaluated
    /// at another x (used while walking under a low ceiling, references/Celeste.exe.cs.d/Celeste/Player.cs:3633-3638).
    fn can_unduck_at(&self, rooms: &[Room], x: Fx) -> bool {
        !self.ducking || !self.hits_normal(rooms, x, self.pos.y)
    }

    /// Vanilla `DuckFreeAt(position)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2926-2936): the **ducking** hitbox fits at
    /// that position -- the probe the dash-into-wall crouch uses (references/Celeste.exe.cs.d/Celeste/Player.cs:3187).
    fn duck_free_at(&self, rooms: &[Room], x: Fx) -> bool {
        !self.hits_duck(rooms, x, self.pos.y)
    }

    /// Vanilla `SlipCheck(addY)` (references/Celeste.exe.cs.d/Celeste/Player.cs:4144-4156): no solid 4px below the facing-side
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

    /// The normal branch of vanilla `ClimbCheck(dir, yAdd)` (references/Celeste.exe.cs.d/Celeste/Player.cs:3564-3571): probe 2px
    /// toward the wall for a solid (no ClimbBlocker entity -> EdgeCheck is always
    /// false). `ClimbBoundsCheck` (references/Celeste.exe.cs.d/Celeste/Player.cs:3548-3555) uses room bounds.
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
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:3889-3913): zero horizontal speed, vertical speed x0.2, snap
    /// 2px to the wall.
    fn enter_climb(&mut self, rooms: &[Room]) {
        self.state = 1;
        self.auto_jump = 0;
        self.vel.x = Fx::zero();
        self.vel.y *= Fx::from_ratio(1, 5); // Speed.Y *= 0.2f (references/Celeste.exe.cs.d/Celeste/Player.cs:3895)
        self.wall_slide_ticks = WALLSLIDE_TICKS;
        self.climb_no_move_ticks = CLIMB_NO_MOVE_TICKS;
        // `references/Celeste.exe.cs.d/Celeste/Player.cs:3896` `wallBoostTimer = 0f`: re-grabbing cancels an
        // unused climb-jump boost, including its effective-stamina refund.
        self.wall_boost_ticks = 0;
        self.last_climb_move = 0;
        for _ in 0..2 {
            if self.hits(rooms, self.pos.x + Fx::from_int(self.facing), self.pos.y) {
                break;
            }
            self.pos.x += Fx::from_int(self.facing); // references/Celeste.exe.cs.d/Celeste/Player.cs:3902-3909 snap to the wall
        }
    }

    /// Effective stamina for grabbing. During the post-climb-jump boost vanilla refunds
    /// the jump's 27.5 cost for this check only (`references/Celeste.exe.cs.d/Celeste/Player.cs:1050-1059`
    /// `if (wallBoostTimer > 0f) { return Stamina + 27.5f; }`).
    fn check_stamina(&self) -> Fx {
        if self.wall_boost_ticks > 0 {
            self.stamina + CLIMBJUMP_DRAIN
        } else {
            self.stamina
        }
    }

    /// `references/Celeste.exe.cs.d/Celeste/Player.cs:1048` `private bool IsTired => CheckStamina < 20f`.
    fn is_tired(&self) -> bool {
        self.check_stamina() < CLIMB_TIRED_THRESHOLD
    }

    /// Whether a jump against a wall becomes a `ClimbJump` instead of a wall jump.
    ///
    /// Vanilla gates `ClimbJump()` on six conditions before calling it from the jump
    /// branch of `NormalUpdate`/`DashUpdate`/`ClimbUpdate` (references/Celeste.exe.cs.d/Celeste/Player.cs:3812-3815,
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:3822-3825): the wall is on the side the player is facing, grab is held,
    /// grabbing is not disabled by an assist, stamina remains, nothing is carried and
    /// no `ClimbBlocker` entity sits in front. This engine has no grab assist, no
    /// carried entity and no `ClimbBlocker` (see the module docs), so three of the six
    /// reduce to constants; the facing/stamina/grab half is what still varies.
    ///
    /// This is the branch that makes a **corner boost** possible: grab the wall, press
    /// jump while pointing into it, and the climb jump zeroes the horizontal speed and
    /// grants the 0.2 s `wallBoostTimer` (`climb_jump`).
    fn climb_jump_check(&self, input: &Input, dir: i32) -> bool {
        dir == self.facing && input.grab && self.stamina > Fx::zero()
    }

    /// Vanilla `ClimbJump()` (references/Celeste.exe.cs.d/Celeste/Player.cs:2651-2677): deduct 27.5 stamina when
    /// not grounded; a normal Jump; with no horizontal input, grant a 0.2s boost window
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:2659-2662).
    fn climb_jump(&mut self) {
        if !self.on_ground {
            self.stamina -= CLIMBJUMP_DRAIN;
        }
        self.jump(Fx::zero(), false); // Jump(particles: false, playSfx: false)
        if self.move_x == Fx::zero() {
            self.wall_boost_dir = -self.facing;
            self.wall_boost_ticks = WALL_BOOST_TICKS;
        }
        // `if (Facing == Facings.Right) Dust.Burst(Center + UnitX * 2, -3Pi/4, 4)` else
        // the mirrored burst (references/Celeste.exe.cs.d/Celeste/Player.cs:2664-2677). Note this one keys off FACING, not the
        // jump direction.
        let center = self.center();
        if self.facing == 1 {
            self.burst(
                center + Vec2::new(Fx::from_int(2), Fx::zero()),
                -THREE_PI_OVER_4,
                4,
            );
        } else {
            self.burst(
                center - Vec2::new(Fx::from_int(2), Fx::zero()),
                -PI_OVER_4,
                4,
            );
        }
    }

    /// Vanilla `ClimbHop()` (references/Celeste.exe.cs.d/Celeste/Player.cs:4129-4151): hop away from the wall, but
    /// only push horizontally right away when nothing solid is left beside the player
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:4130-4137). Over the top of a wall, where the wall is still there for one
    /// more frame, vanilla saves `hopWaitX`/`hopWaitXSpeed` and applies the push in the
    /// next `Player.Update` once the solid clears (references/Celeste.exe.cs.d/Celeste/Player.cs:1664-1669).
    ///
    /// Deliberately not ported (no counterpart in this engine, each with a note):
    /// `playFootstepOnLand = 0.5f` (footstep sfx), `lowFrictionStopTimer = 0.15f` (the
    /// LowFriction assist, references/Celeste.exe.cs.d/Celeste/Player.cs:3243-3249), `noWindTimer = 0.3f` (no wind model), and
    /// `climbHopSolidPosition` / `MoveHExact` (moving-platform ride, references/Celeste.exe.cs.d/Celeste/Player.cs:4141-4149 --
    /// this engine has no moving solids).
    fn climb_hop(&mut self, rooms: &[Room]) {
        self.climb_hop_solid = self.hits(rooms, self.pos.x + Fx::from_int(self.facing), self.pos.y);
        if self.climb_hop_solid {
            self.hop_wait_x = self.facing;
            self.hop_wait_x_speed = CLIMB_HOP_X * Fx::from_int(self.facing);
        } else {
            self.hop_wait_x = 0;
            self.vel.x = CLIMB_HOP_X * Fx::from_int(self.facing);
        }
        self.vel.y = self.vel.y.min(CLIMB_HOP_Y);
        self.force_move_x = 0;
        self.force_move_x_ticks = 12; // 0.2s (references/Celeste.exe.cs.d/Celeste/Player.cs:4126)
        self.fast_jump = false;
    }

    /// Vanilla `Jump()` (references/Celeste.exe.cs.d/Celeste/Player.cs:2443-2465): vertical -105, horizontal
    /// +40*moveX (JumpHBoost), varJump filled to 0.2s, varJumpSpeed saved, coyote and
    /// buffer zeroed. (`LiftBoost` is 0 on static ground and is omitted.)
    fn jump(&mut self, move_x: Fx, particles: bool) {
        self.jump_buffer = 0;
        self.coyote = 0;
        self.var_jump = VAR_JUMP_TICKS;
        self.auto_jump = 0;
        self.dash_attack = 0;
        // `references/Celeste.exe.cs.d/Celeste/Player.cs:2452` `wallBoostTimer = 0f`. ClimbJump calls this
        // helper first and then deliberately arms its own fresh boost window.
        self.wall_boost_ticks = 0;
        self.vel.x += move_x * JUMP_H_BOOST;
        self.vel.y = JUMP_SPEED;
        self.var_jump_speed = JUMP_SPEED;
        // `Sprite.Scale = new Vector2(0.6f, 1.4f)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2473) -- the vertical jump
        // stretch; it springs back to 1 in `UpdateSprite` (references/Celeste.exe.cs.d/Celeste/Player.cs:2051-2052).
        self.sprite_scale = [Fx::from_ratio(3, 5), Fx::from_ratio(7, 5)];
        self.on_ground = false;
        // `if (particles) { ... Dust.Burst(BottomCenter, -PiOver2, 4, ...) }`
        // (references/Celeste.exe.cs.d/Celeste/Player.cs:2474-2482). `ClimbJump` calls `Jump(particles: false, ...)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2655), so
        // the climb jump stays dust-free.
        if particles {
            let anchor = self.anchor();
            self.burst(anchor, -PI_OVER_2, 4);
        }
    }

    /// The normal branch of vanilla `WallJumpCheck(dir)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2528-2553)
    /// (no spike/star-fly corrections, `num = 5`): probe 5px toward the wall for a
    /// solid. `ClimbBoundsCheck` / `ClimbBlocker.EdgeCheck` (the references/Celeste.exe.cs.d/Celeste/Player.cs:2528 preamble) are
    /// always false/true in this engine, which has no climb-boundary entities (deviation
    /// declared: see the module docs).
    fn wall_jump_check(&self, rooms: &[Room], dir: i32) -> bool {
        let dx = Fx::from_int(dir) * WALLJUMP_PROBE;
        self.hits(rooms, self.pos.x + dx, self.pos.y)
    }

    /// Vanilla `WallJump(dir)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2555-2609): horizontal 130*dir,
    /// vertical -105, varJump filled, `forceMoveX = dir` for 0.16s (references/Celeste.exe.cs.d/Celeste/Player.cs:2581-2587),
    /// consumes the jump buffer, zeroes coyote.
    fn wall_jump(&mut self, rooms: &[Room], dir: i32) {
        self.jump_buffer = 0;
        self.coyote = 0;
        self.var_jump = VAR_JUMP_TICKS;
        self.auto_jump = 0;
        self.dash_attack = 0;
        // `references/Celeste.exe.cs.d/Celeste/Player.cs:2565` `wallBoostTimer = 0f`.
        self.wall_boost_ticks = 0;
        if self.move_x != Fx::zero() {
            // references/Celeste.exe.cs.d/Celeste/Player.cs:2577-2587: force moveX while a direction is held, preventing an
            // immediate re-grab.
            self.force_move_x = dir;
            self.force_move_x_ticks = FORCE_MOVE_X_TICKS;
        }
        self.vel.x = WALLJUMP_X * Fx::from_int(dir);
        self.vel.y = JUMP_SPEED;
        self.var_jump_speed = JUMP_SPEED;
        // `Sprite.Scale = (0.6, 1.4)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2602) -- the wall-jump vertical stretch.
        self.sprite_scale = [Fx::from_ratio(3, 5), Fx::from_ratio(7, 5)];
        self.on_ground = false;
        // `if (dir == -1) Dust.Burst(Center + UnitX * 2, -3Pi/4, 4)` else the mirrored
        // burst at `Center - UnitX * 2` with `-Pi/4` (references/Celeste.exe.cs.d/Celeste/Player.cs:2604-2609).
        let center = self.center();
        if dir == -1 {
            self.burst(
                center + Vec2::new(Fx::from_int(2), Fx::zero()),
                -THREE_PI_OVER_4,
                4,
            );
        } else {
            self.burst(
                center - Vec2::new(Fx::from_int(2), Fx::zero()),
                -PI_OVER_4,
                4,
            );
        }
        let _ = rooms;
    }

    /// Vanilla `StartDash()` (references/Celeste.exe.cs.d/Celeste/Player.cs:4218-4228) + `DashBegin`
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:4283-4314): consume one dash, enter State 2, record `beforeDashSpeed`, zero
    /// speed (no movement this tick), start the cooldown and attack window.
    fn start_dash(&mut self, input: &Input, rooms: &[Room]) {
        self.dashes = self.dashes.saturating_sub(1);
        self.dash_buffer = 0; // Input.Dash.ConsumeBuffer() (references/Celeste.exe.cs.d/Celeste/Player.cs:4223)
        self.state = 2;
        self.dash_phase = 0;
        self.dash_cooldown = DASH_COOLDOWN_TICKS;
        self.dash_refill_cooldown = DASH_REFILL_COOLDOWN_TICKS;
        self.dash_attack = DASH_ATTACK_TICKS;
        // Leaving Normal runs `NormalEnd` (`references/Celeste.exe.cs.d/Celeste/Player.cs:3543-3546`,
        // `wallBoostTimer = 0f`), so a dash cannot preserve a stale boost.
        self.wall_boost_ticks = 0;
        // DashBegin (references/Celeste.exe.cs.d/Celeste/Player.cs:4286): the flag drives the air-dash landing correction.
        self.dash_started_on_ground = self.on_ground;
        self.before_dash_speed = self.vel;
        self.vel = Vec2::ZERO;
        self.dash_dir = Vec2::ZERO;
        // references/Celeste.exe.cs.d/Celeste/Player.cs:4315-4322: an airborne ducked dash stands up; a grounded down-push dash
        // crouches.
        if !self.on_ground && self.ducking && self.can_unduck(rooms) {
            self.ducking = false;
        } else if !self.ducking && input.down {
            self.ducking = true;
        }
        // Note: vanilla DashBegin does **not** change onGround -- a ground dash stays
        // grounded, and the next tick's head refill (references/Celeste.exe.cs.d/Celeste/Player.cs:1604-1612) works as usual.
        // DashBegin's `Celeste.Freeze(0.05f)` (references/Celeste.exe.cs.d/Celeste/Player.cs:4289): 0.05s = 3 ticks of global
        // freeze, consumed by the service (equivalent to skipping 3 simulation ticks in
        // the engine's frame rhythm).
        self.freeze_ticks = 3;
    }

    /// Vanilla `GetAimVector(Facing)` (Celeste.Input, references/Celeste.exe.cs.d/Celeste/Input.cs:331-377, no
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
            // Diagonal: `Math.Sign(value).SafeNormalize()` (references/Celeste.exe.cs.d/Celeste/Input.cs:373).
            let inv = std::f32::consts::FRAC_1_SQRT_2;
            Vec2::from_f32s(mx.signum() * inv, my.signum() * inv)
        }
    }

    /// State 2: an explicit-phase version of `DashUpdate` (references/Celeste.exe.cs.d/Celeste/Player.cs:4347-4476)
    /// + `DashCoroutine` (references/Celeste.exe.cs.d/Celeste/Player.cs:4480-4580).
    ///
    /// One tick's order matches vanilla `StateMachine.Update` (references/Celeste.exe.cs.d/Monocle/StateMachine.cs:168-184): first
    /// `DashUpdate` (the jump branch, references/Celeste.exe.cs.d/Celeste/Player.cs:4376-4425), then advance the coroutine, and
    /// finally the shared MoveH/MoveV (references/Celeste.exe.cs.d/Celeste/Player.cs:1798-1805) -- after a wall jump / super jump
    /// rewrites the speed, the frame moves at the new speed. Phase 0 runs the
    /// coroutine's first segment (references/Celeste.exe.cs.d/Celeste/Player.cs:4505-4555, after `yield return null`: speed = aim
    /// x 240, DashDir, facing), phases 1..=9 wait 0.15s, and tick 10 runs the last
    /// segment (references/Celeste.exe.cs.d/Celeste/Player.cs:4566-4579). There is no gravity/running during a dash (`DashUpdate`
    /// does not contain them).
    fn dash_update(&mut self, input: &Input, rooms: &[Room]) {
        self.prev_pos = self.pos;

        // ---- DashUpdate (references/Celeste.exe.cs.d/Celeste/Player.cs:4347-4476) ----
        // The jump branch during a dash (references/Celeste.exe.cs.d/Celeste/Player.cs:4376-4425). A horizontal dash
        // (|DashDir.Y| < 0.1) while grounded -> SuperJump (references/Celeste.exe.cs.d/Celeste/Player.cs:2489); a vertical dash
        // (|DashDir.X| <= 0.2) -> SuperWallJump (references/Celeste.exe.cs.d/Celeste/Player.cs:2614); otherwise -> WallJump
        // (ClimbJump needs a grab, not implemented, skipped -- deviation declared).
        if self.dash_dir != Vec2::ZERO && self.jump_buffer > 0 {
            let horizontal_dash = self.dash_dir.y.abs() < Fx::from_ratio(1, 10);
            let vertical_dash = self.dash_dir.x.abs() <= Fx::from_ratio(1, 5);
            if horizontal_dash && self.on_ground {
                // references/Celeste.exe.cs.d/Celeste/Player.cs:4391-4403 -> `SuperJump` (references/Celeste.exe.cs.d/Celeste/Player.cs:2489-2527).
                self.super_jump(rooms);
            } else if vertical_dash {
                if self.wall_jump_check(rooms, 1) {
                    self.super_wall_jump(-1);
                } else if self.wall_jump_check(rooms, -1) {
                    self.super_wall_jump(1);
                }
            } else if self.wall_jump_check(rooms, 1) {
                // references/Celeste.exe.cs.d/Celeste/Player.cs:4417-4421: grabbing the wall the jump points away from turns the
                // wall jump into a `ClimbJump` -- this is the corner-boost branch.
                if self.climb_jump_check(input, 1) {
                    self.climb_jump();
                } else {
                    self.wall_jump(rooms, -1);
                }
                self.state = 0;
            } else if self.wall_jump_check(rooms, -1) {
                if self.climb_jump_check(input, -1) {
                    self.climb_jump();
                } else {
                    self.wall_jump(rooms, 1);
                }
                self.state = 0;
            }
        }

        // ---- DashCoroutine (references/Celeste.exe.cs.d/Celeste/Player.cs:4480-4580) ----
        if self.state == 2 {
            if self.dash_phase == 0 {
                // Coroutine first segment (references/Celeste.exe.cs.d/Celeste/Player.cs:4505-4555).
                let aim = self.aim_dir(input);
                let mut speed = aim * DASH_SPEED;
                // Horizontal overspeed retention (references/Celeste.exe.cs.d/Celeste/Player.cs:4519-4523): keep the original value
                // when the horizontal speed before the dash was faster and in the same
                // direction (the wall-speed semantics of `beforeDashSpeed`).
                if vel_x_same_dir(&self.before_dash_speed, speed.x)
                    && self.before_dash_speed.x.abs() > speed.x.abs()
                {
                    speed.x = self.before_dash_speed.x;
                }
                self.vel = speed;
                self.dash_dir = aim;
                // `SceneAs<Level>().DirectionalShake(DashDir, 0.2f)` (references/Celeste.exe.cs.d/Celeste/Player.cs:4531-4534).
                // The service owns the shake (vanilla keeps it on `Level`), so it is
                // queued here and drained after the tick.
                self.shake_request = Some((aim, Fx::from_ratio(1, 5)));
                if aim.x != Fx::zero() {
                    self.facing = aim.x.signum(); // references/Celeste.exe.cs.d/Celeste/Player.cs:4528-4531
                }
                // The grounded down-diagonal dash converts to a duck slide on the same
                // tick, but through the collision handler: `Speed = aim * 240` is applied
                // here, `MoveV` then hits the floor and `OnCollideV`
                // (references/Celeste.exe.cs.d/Celeste/Player.cs:3326-3331) performs the conversion. It is implemented once, in
                // `slide_y`, so the airborne landing path gets it too.
                self.dash_phase = 1;
            } else {
                self.dash_phase += 1;
                if self.dash_phase > DASH_TICKS {
                    // Last segment (references/Celeste.exe.cs.d/Celeste/Player.cs:4566-4579).
                    self.auto_jump = 0; // AutoJumpTimer = 0f (references/Celeste.exe.cs.d/Celeste/Player.cs:4569)
                    if self.dash_dir.y <= Fx::zero() {
                        self.vel = self.dash_dir * DASH_END_SPEED;
                        if self.vel.y < Fx::zero() {
                            // references/Celeste.exe.cs.d/Celeste/Player.cs:4575-4578: `if (Speed.Y < 0f) Speed.Y *= 0.75f`.
                            self.vel.y *= Fx::from_ratio(3, 4);
                        }
                    }
                    self.state = 0;
                }
            }
        }

        // ---- MoveH / MoveV (references/Celeste.exe.cs.d/Celeste/Player.cs:1798-1805) ----
        self.slide_x(rooms);
        self.on_ground = false;
        self.slide_y(rooms);
        if !self.on_ground {
            self.on_ground = self.hits(rooms, self.pos.x, self.pos.y + Fx::one());
        }
    }

    /// Vanilla `SuperJump()` (references/Celeste.exe.cs.d/Celeste/Player.cs:2487-2526, no super-slide/super-glide
    /// branch): horizontal 260*Facing, vertical -105, varJump filled, back to State 0.
    fn super_jump(&mut self, rooms: &[Room]) {
        self.jump_buffer = 0;
        self.coyote = 0;
        self.var_jump = VAR_JUMP_TICKS;
        self.auto_jump = 0;
        self.dash_attack = 0;
        // `references/Celeste.exe.cs.d/Celeste/Player.cs:2496` `wallBoostTimer = 0f`.
        self.wall_boost_ticks = 0;
        self.vel.x = SUPER_JUMP_X * Fx::from_int(self.facing);
        self.vel.y = JUMP_SPEED;
        // references/Celeste.exe.cs.d/Celeste/Player.cs:2502-2509: ducked super jump -- horizontal speed x1.25, vertical speed x0.5.
        if self.ducking {
            self.ducking = false;
            self.vel.x *= Fx::from_ratio(5, 4);
            self.vel.y *= Fx::from_ratio(1, 2);
        }
        // `varJumpSpeed = Speed.Y` (references/Celeste.exe.cs.d/Celeste/Player.cs:2516) is assigned **after** the duck branch, so the
        // varJump clamp on the next tick keeps the *halved* vertical speed. Setting it
        // before the duck branch left it at -105 while vel.y was -52.5, so the clamp
        // yanked the wave dash back up to full height at full horizontal speed.
        self.var_jump_speed = self.vel.y;
        // `Sprite.Scale = (0.6, 1.4)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2517) -- the super-jump vertical stretch; it
        // springs back to 1 in `UpdateSprite` (references/Celeste.exe.cs.d/Celeste/Player.cs:2051-2052).
        self.sprite_scale = [Fx::from_ratio(3, 5), Fx::from_ratio(7, 5)];
        self.on_ground = false;
        self.state = 0;
        // `Dust.Burst(BottomCenter, -PiOver2, 4, ...)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2517-2525) -- unlike `Jump`
        // this one is unconditional.
        let anchor = self.anchor();
        self.burst(anchor, -PI_OVER_2, 4);
        let _ = rooms;
    }

    /// Vanilla `SuperWallJump(dir)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2614-2649): horizontal 170*dir,
    /// vertical -160 (references/Celeste.exe.cs.d/Celeste/Player.cs:2626-2629, with varJumpSpeed synced), varJumpTimer = 0.25s
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:2619), **AutoJump = false** (references/Celeste.exe.cs.d/Celeste/Player.cs:2620), no forceMoveX.
    fn super_wall_jump(&mut self, dir: i32) {
        self.jump_buffer = 0;
        self.coyote = 0;
        self.var_jump = 15; // varJumpTimer = 0.25f (references/Celeste.exe.cs.d/Celeste/Player.cs:2619)
        self.auto_jump = 0; // AutoJump = false (references/Celeste.exe.cs.d/Celeste/Player.cs:2620)
        self.dash_attack = 0;
        // `references/Celeste.exe.cs.d/Celeste/Player.cs:2625` `wallBoostTimer = 0f`.
        self.wall_boost_ticks = 0;
        self.vel.x = SUPER_WALLJUMP_X * Fx::from_int(dir);
        self.vel.y = SUPER_WALLJUMP_Y;
        self.var_jump_speed = SUPER_WALLJUMP_Y;
        // `Sprite.Scale = (0.6, 1.4)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2633) -- the super wall-jump vertical stretch.
        self.sprite_scale = [Fx::from_ratio(3, 5), Fx::from_ratio(7, 5)];
        self.on_ground = false;
        self.state = 0;
        // Same dir-conditional burst as `WallJump` (references/Celeste.exe.cs.d/Celeste/Player.cs:2635-2640).
        let center = self.center();
        if dir == -1 {
            self.burst(
                center + Vec2::new(Fx::from_int(2), Fx::zero()),
                -THREE_PI_OVER_4,
                4,
            );
        } else {
            self.burst(
                center - Vec2::new(Fx::from_int(2), Fx::zero()),
                -PI_OVER_4,
                4,
            );
        }
    }

    /// State 1: `ClimbUpdate` (references/Celeste.exe.cs.d/Celeste/Player.cs:3933-4111). Order: stagger decay ->
    /// stamina refill while grounded -> jump branch (reverse wall jump / ClimbJump) ->
    /// dash -> release grab on landing -> leave wall (apex ClimbHop / let-go) -> vertical
    /// climb (up -45, down 80, slip 30 when SlipCheck at the top) -> stamina drain (up
    /// 45.45/s, still 10/s) -> let go when exhausted. WallBooster / sprite sweat effect
    /// are not ported (declared).
    ///
    /// Vanilla's state function only chooses the next state; the outer `Player.Update`
    /// still runs `MoveH`/`MoveV` afterward (`references/Celeste.exe.cs.d/Celeste/Player.cs:1798-1805`). Keeping
    /// that outer movement here means jump, release, dash, hop and exhaustion returns
    /// all move on their activation tick instead of one tick later.
    fn climb_update(&mut self, input: &Input, rooms: &[Room]) {
        self.climb_update_logic(input, rooms);
        self.slide_x(rooms);
        self.on_ground = false;
        self.slide_y(rooms);
        if !self.on_ground {
            self.on_ground = self.hits(rooms, self.pos.x, self.pos.y + Fx::one());
        }
    }

    /// The state-specific part of `ClimbUpdate`; early returns still pass through the
    /// outer movement in [`Player::climb_update`].
    fn climb_update_logic(&mut self, input: &Input, rooms: &[Room]) {
        self.prev_pos = self.pos;
        self.climb_no_move_ticks = self.climb_no_move_ticks.saturating_sub(1);
        if self.on_ground {
            self.stamina = CLIMB_MAX_STAMINA; // references/Celeste.exe.cs.d/Celeste/Player.cs:3937-3940
        }
        // Jump branch (references/Celeste.exe.cs.d/Celeste/Player.cs:3942-3954): push the other way -> wall jump; otherwise a
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
        // Dash (references/Celeste.exe.cs.d/Celeste/Player.cs:3955-3958).
        if self.dash_buffer > 0 && self.dash_cooldown == 0 && self.dashes > 0 {
            self.start_dash(input, rooms);
            return;
        }
        // Grab released (references/Celeste.exe.cs.d/Celeste/Player.cs:3960-3965).
        if !input.grab {
            self.state = 0;
            return;
        }
        // Past the top of the wall (references/Celeste.exe.cs.d/Celeste/Player.cs:3967-3980): while rising -> ClimbHop; otherwise
        // let go.
        if !self.hits(rooms, self.pos.x + Fx::from_int(self.facing), self.pos.y) {
            if self.vel.y < Fx::zero() {
                self.climb_hop(rooms); // references/Celeste.exe.cs.d/Celeste/Player.cs:3975-3979 (wallBoosting is always false)
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
                // the ClimbBlocker branch at references/Celeste.exe.cs.d/Celeste/Player.cs:3973-3989 has no entity, always false.
                if input.axis_y() == -Fx::one() {
                    num = CLIMB_UP_SPEED; // -45 px/s (references/Celeste.exe.cs.d/Celeste/Player.cs:3969-3971)
                    if self.hits(rooms, self.pos.x, self.pos.y - Fx::one())
                        || (self.climb_hop_blocked(rooms) && self.slip_check(rooms, -Fx::one()))
                    {
                        if self.vel.y < Fx::zero() {
                            self.vel.y = Fx::zero();
                        }
                        num = Fx::zero();
                        flag = true;
                    } else if self.slip_check(rooms, Fx::zero()) {
                        self.climb_hop(rooms); // references/Celeste.exe.cs.d/Celeste/Player.cs:3981-3985: hop off the top of the wall
                        self.state = 0;
                        return;
                    }
                } else if input.axis_y() == Fx::one() {
                    num = CLIMB_DOWN_SPEED; // 80 px/s (references/Celeste.exe.cs.d/Celeste/Player.cs:3977-3979)
                    if self.on_ground {
                        if self.vel.y > Fx::zero() {
                            self.vel.y = Fx::zero();
                        }
                        num = Fx::zero();
                    }
                } else {
                    flag = true; // references/Celeste.exe.cs.d/Celeste/Player.cs:3992-3994: no vertical input
                }
            } else {
                flag = true; // no movement during the stagger
            }
            self.last_climb_move = num.signum(); // references/Celeste.exe.cs.d/Celeste/Player.cs:4000
            if flag && self.slip_check(rooms, Fx::zero()) {
                num = CLIMB_SLIP_SPEED; // references/Celeste.exe.cs.d/Celeste/Player.cs:4001-4004: 30 slow descent while hanging at the top
            }
            // references/Celeste.exe.cs.d/Celeste/Player.cs:4006: `Approach(Speed.Y, num, 900*dt)`.
            approach(&mut self.vel.y, num, GRAVITY);
        }
        // references/Celeste.exe.cs.d/Celeste/Player.cs:4009-4013: no longer pushing down and the wall has ended below the front ->
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
        // Stamina (references/Celeste.exe.cs.d/Celeste/Player.cs:4015-4049): up 45.454544/s, still 10/s; let go when exhausted.
        if self.climb_no_move_ticks == 0 {
            if self.last_climb_move == -1 {
                self.stamina -= CLIMB_UP_DRAIN_PER_TICK;
            } else if self.last_climb_move == 0 {
                self.stamina -= CLIMB_IDLE_DRAIN_PER_TICK;
            }
        }
        if self.stamina <= Fx::zero() {
            self.state = 0; // references/Celeste.exe.cs.d/Celeste/Player.cs:4051-4055
        }
    }

    /// The reachable part of vanilla `ClimbHopBlockedCheck()` (references/Celeste.exe.cs.d/Celeste/Player.cs:4158-4173): a solid
    /// 6px above the head -> the hop is blocked (no Follower/LedgeBlocker entities).
    fn climb_hop_blocked(&self, rooms: &[Room]) -> bool {
        self.hits(rooms, self.pos.x, self.pos.y - Fx::from_int(6))
    }

    /// Vanilla `UpdateHair` (references/Celeste.exe.cs.d/Celeste/Player.cs:1996-2047): dash spent -> ease into USED
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
        self.last_dashes = self.dashes; // references/Celeste.exe.cs.d/Celeste/Player.cs:2044
    }

    /// Vanilla `Sprite.Play(id, restart = false)` (references/Celeste.exe.cs.d/Monocle/Sprite.cs:358-378): the same
    /// id is a no-op (references/Celeste.exe.cs.d/Monocle/Sprite.cs:360 `(CurrentAnimationID != id) | restart`); on a
    /// switch, `LastAnimationID` records the **new** id (references/Celeste.exe.cs.d/Monocle/Sprite.cs:366-367;
    /// `(CurrentAnimationID = id)` evaluates to `id`), `Animating = currentAnimation.Delay > 0f`
    /// (references/Celeste.exe.cs.d/Monocle/Sprite.cs:369 `Animating = currentAnimation.Delay > 0f`), and the timer
    /// and frame reset to zero. A missing table / unknown id is still a no-op (rendering
    /// falls back to static frames).
    pub fn play(&mut self, table: &crate::sprites::AnimationTable, id: &str) {
        if self.anim_id == id {
            return;
        }
        let Some(def) = table.sprites.get("player") else {
            return;
        };
        // Dictionary keys are case-insensitive (Monocle `OrdinalIgnoreCase`, references/Celeste.exe.cs.d/Monocle/Sprite.cs:81; the
        // XML has `climbup` while `UpdateSprite` plays `climbUp`); but CurrentAnimationID
        // stores the **argument** (references/Celeste.exe.cs.d/Celeste/Player.cs:2023 behavior), so the no-op comparison uses the
        // string as-is.
        let Some(anim) = def.anims.get(&id.to_ascii_lowercase()) else {
            return;
        };
        // `string lastAnimationID = (CurrentAnimationID = id); LastAnimationID =
        // lastAnimationID;` (references/Celeste.exe.cs.d/Monocle/Sprite.cs:366-367): a C# assignment expression evaluates to the
        // **new** id, so `LastAnimationID` ends up holding the animation just played, not
        // the previous one. `Running` (references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:169-182) reads it as "the most recently
        // played animation is a run*". Recording the old id instead leaves `Running` true
        // after a stop -> idle, so the `!Sprite.Running` guard (references/Celeste.exe.cs.d/Celeste/Player.cs:2196) never fires again
        // and the character slides at full speed holding the idle pose.
        self.last_anim_id = id.to_string(); // references/Celeste.exe.cs.d/Monocle/Sprite.cs:366-367
        self.anim_id = id.to_string();
        self.anim_frame = 0;
        self.anim_timer = 0.0;
        self.anim_playing = anim.delay > 0.0; // references/Celeste.exe.cs.d/Monocle/Sprite.cs:369 `Animating = currentAnimation.Delay > 0f`
    }

    /// Vanilla `Sprite.Update` (references/Celeste.exe.cs.d/Monocle/Sprite.cs:45-183): advance the timer by the frame interval,
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
        // references/Celeste.exe.cs.d/Monocle/Sprite.cs:109-118: `animationTimer += DeltaTime` (Rate = 1); only advance a frame
        // when `|timer| >= Delay`, then subtract Delay from the timer.
        self.anim_timer += 1.0 / 60.0;
        if self.anim_timer < anim.delay {
            return;
        }
        self.anim_timer -= anim.delay;
        self.anim_frame += 1;
        if self.anim_frame >= anim.frames.len() {
            // references/Celeste.exe.cs.d/Monocle/Sprite.cs:153-180.
            match &anim.goto {
                Some(next) => {
                    // Goto branch (references/Celeste.exe.cs.d/Monocle/Sprite.cs:135-157):
                    // `CurrentAnimationID = currentAnimation.Goto.Choose()` (references/Celeste.exe.cs.d/Monocle/Sprite.cs:137), and it
                    // writes the **new** id into `LastAnimationID`
                    // (references/Celeste.exe.cs.d/Monocle/Sprite.cs:142 `LastAnimationID = CurrentAnimationID` --
                    // **different** from `play`'s "record the old id" rule); the frame
                    // resets to 0 (references/Celeste.exe.cs.d/Monocle/Sprite.cs:150) and the timer and `Animating` are untouched.
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
                    // references/Celeste.exe.cs.d/Monocle/Sprite.cs:158-177: `Animating = false`, frame stays at the
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
    /// `PlayerSprite.HasHair` / `HairOffset` / `HairFrame`, references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:145-165),
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

    /// Vanilla `PlayerSprite.Running` (references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:169-182): the **previous**
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
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:2049-2257) (no held item / wind / climb / dream / space /
    /// duck -- deviation declared in the module docs): dash attack window -> skid/dash;
    /// grounded -> push/edge/edgeBack/lookUp/idle/skid/flip/runSlow/runFast; airborne ->
    /// jumpSlow/jumpFast/fallSlow/fallFast.
    pub fn update_sprite(
        &mut self,
        table: &crate::sprites::AnimationTable,
        input: &Input,
        rooms: &[Room],
    ) {
        // `Sprite.Scale.X/Y = Approach(Scale.X/Y, 1, 1.75 * dt)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2051-2052): the
        // dash/jump stretch springs back to 1 at 105 px/s-> 7/240 per tick.
        const SPRING_RATE: Fx = Fx::from_ratio(7, 240);
        approach(&mut self.sprite_scale[0], Fx::one(), SPRING_RATE);
        approach(&mut self.sprite_scale[1], Fx::one(), SPRING_RATE);
        let dash_attacking = self.dash_attack > 0;
        if dash_attacking {
            // 2097 `onGround && DashDir.Y == 0f && !Ducking && Speed.X != 0f &&
            // moveX == -Sign(Speed.X)` -> skid; 2105 `else if (Ducking)` -> duck;
            // 2111 otherwise -> dash. The engine previously missed the duck branch, so a
            // grounded crouch-dash played the standing dash animation.
            if self.on_ground
                && self.dash_dir.y == Fx::zero()
                && !self.ducking
                && self.vel.x != Fx::zero()
                && self.move_x == Fx::from_int(-self.vel.x.signum())
            {
                // `if (Scene.OnInterval(0.02f)) { Dust.Burst(Position, -PiOver2); }`
                // (references/Celeste.exe.cs.d/Celeste/Player.cs:2318-2321): the skid puff, 1 particle, paced on the 0.02 s
                // interval (roughly every other tick).
                if self.on_interval(Fx::from_ratio(2, 100)) {
                    let anchor = self.anchor();
                    self.burst(anchor, -PI_OVER_2, 1);
                }
                self.play(table, "skid");
            } else if self.ducking {
                self.play(table, "duck");
            } else {
                self.play(table, "dash");
            }
        } else if self.state == 1 {
            // references/Celeste.exe.cs.d/Celeste/Player.cs:2114-2132: climb animations (lastClimbMove / dangling / look back).
            if self.last_climb_move < 0 {
                self.play(table, "climbUp"); // XML id `climbup`; case-insensitive
            } else if self.last_climb_move > 0 {
                self.play(table, "wallslide");
            } else if !self.hits(
                rooms,
                self.pos.x + Fx::from_int(self.facing * 6),
                self.pos.y,
            ) {
                self.play(table, "dangling"); // references/Celeste.exe.cs.d/Celeste/Player.cs:2120-2122: probe at (Facing, 6)
            } else if self.move_x == Fx::from_int(-self.facing) {
                self.play(table, "climbLookBackStart");
            } else {
                self.play(table, "wallslide");
            }
        } else if self.ducking && self.state == 0 {
            self.play(table, "duck"); // references/Celeste.exe.cs.d/Celeste/Player.cs:2157-2161
        } else if self.state == 0 {
            if self.on_ground {
                self.fast_jump = false;
                // references/Celeste.exe.cs.d/Celeste/Player.cs:2147-2149: push against a wall (`Holding == null &&
                // moveX != 0 && CollideCheck<Solid>(Position + UnitX * moveX)`; vanilla also
                // has `ClimbBlocker.EdgeCheck`, but this engine has no ClimbBlocker yet).
                if self.move_x != Fx::zero()
                    && self.hits(rooms, self.pos.x + self.move_x, self.pos.y)
                {
                    self.play(table, "push");
                } else if self.vel.x.abs() <= IDLE_SPEED && self.move_x == Fx::zero() {
                    // references/Celeste.exe.cs.d/Celeste/Player.cs:2151 `Math.Abs(Speed.X) <= 25f && moveX == 0`.
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
                        self.play(table, "edge"); // references/Celeste.exe.cs.d/Celeste/Player.cs:2157-2159
                    } else if !back {
                        self.play(table, "edgeBack"); // references/Celeste.exe.cs.d/Celeste/Player.cs:2161-2163
                    } else if input.up {
                        // references/Celeste.exe.cs.d/Celeste/Player.cs:2167 `Sprite.LastAnimationID != "lookUp"` --
                        // vanilla replays only then (lookUp does not loop, and after it ends
                        // CurrentAnimationID is cleared; replaying unconditionally would stay
                        // on frame 0 forever).
                        if self.last_anim_id != "lookUp" {
                            self.play(table, "lookUp");
                        }
                    } else if !self.anim_id.is_empty() && !self.anim_id.contains("idle") {
                        // references/Celeste.exe.cs.d/Celeste/Player.cs:2172 `Sprite.CurrentAnimationID.Contains("idle")`
                        // -- the full condition is: `CurrentAnimationID != null &&
                        // (!Contains("idle") || (== "idle_carry" && Holding == null))`
                        // (references/Celeste.exe.cs.d/Celeste/Player.cs:2172-2174).
                        self.play(table, "idle");
                    }
                } else if self.move_x != Fx::zero() && self.vel.x.signum() == -self.move_x.signum()
                {
                    // references/Celeste.exe.cs.d/Celeste/Player.cs:2181-2189: opposite to the input and still sliding
                    // (`Math.Sign(Speed.X) == -moveX && moveX != 0`, references/Celeste.exe.cs.d/Celeste/Player.cs:2181).
                    if self.vel.x.abs() > RUN {
                        self.play(table, "skid");
                    } else if self.anim_id != "skid" {
                        self.play(table, "flip");
                    }
                } else if !self.running() {
                    // references/Celeste.exe.cs.d/Celeste/Player.cs:2196 `!Sprite.Running` -- the full condition also has
                    // `CurrentAnimationID == "runWind" || (== "runSlow_carry" &&
                    // Holding == null)`; the latter two are wind / held item, not implemented
                    // in this engine (references/Celeste.exe.cs.d/Celeste/Player.cs:2196-2204).
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
                // references/Celeste.exe.cs.d/Celeste/Player.cs:2235 `Sprite.LastAnimationID != "fallFast"`.
                if self.last_anim_id != "fallFast" {
                    self.play(table, "fallFast");
                }
            } else {
                self.play(table, "fallSlow");
            }
        }
    }

    /// State 0 (NormalUpdate, references/Celeste.exe.cs.d/Celeste/Player.cs:3573-3850).
    ///
    /// The within-tick order mirrors vanilla: grab/duck (not implemented, see the
    /// deviation declaration) -> dash trigger (references/Celeste.exe.cs.d/Celeste/Player.cs:3615-3619 `if (CanDash)`) -> running
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:3714-3724) -> maxFall (references/Celeste.exe.cs.d/Celeste/Player.cs:3726-3736) -> gravity (references/Celeste.exe.cs.d/Celeste/Player.cs:3776-3789, airborne
    /// only) -> varJump clamp (references/Celeste.exe.cs.d/Celeste/Player.cs:3791-3797) -> jump decision (references/Celeste.exe.cs.d/Celeste/Player.cs:3802-3849, including
    /// the wall jump) -> movement solve -> coyote/buffer decay. As in vanilla, the outer
    /// movement still runs when NormalUpdate returns a different state
    /// (`references/Celeste.exe.cs.d/Celeste/Player.cs:1798-1805`).
    fn normal_update(&mut self, input: &Input, rooms: &[Room]) {
        self.normal_update_logic(input, rooms);
        self.normal_movement_tail(rooms);
    }

    /// The state-specific part of NormalUpdate; early state returns still pass through
    /// [`Player::normal_movement_tail`].
    fn normal_update_logic(&mut self, input: &Input, rooms: &[Room]) {
        self.prev_pos = self.pos;

        // Wall grab (`references/Celeste.exe.cs.d/Celeste/Player.cs:3581-3613`, no pickups): probe when the
        // horizontal speed is not away from the wall and vertical speed >= 0; when it is
        // 1-2px short, vanilla lifts the player with MoveVExact. `ClimbTrigger` is a
        // write-only field in vanilla (references/Celeste.exe.cs.d/Celeste/Player.cs:463/references/Celeste.exe.cs.d/Celeste/Player.cs:1437/references/Celeste.exe.cs.d/Celeste/Player.cs:3016, with no consumer), skipped.
        // `references/Celeste.exe.cs.d/Celeste/Player.cs:3581`
        // `Input.GrabCheck && !IsTired && !Ducking`: an exhausted player cannot re-grab.
        if input.grab && !self.is_tired() && !self.ducking && self.vel.y >= Fx::zero() {
            let behind = self.vel.x.signum() == -self.facing && self.vel.x != Fx::zero();
            if !behind {
                if self.climb_check(rooms, self.facing, 0) {
                    self.ducking = false;
                    self.enter_climb(rooms);
                    return;
                }
                // `references/Celeste.exe.cs.d/Celeste/Player.cs:3601` `(float)Input.MoveY < 1f`: holding down
                // deliberately skips only the upward 1-2px snap, not a direct wall grab.
                if input.axis_y() < Fx::one() {
                    for i in 1..=2i32 {
                        // references/Celeste.exe.cs.d/Celeste/Player.cs:3603-3611: when 1-2px short, lift, then grab.
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
        }

        // Duck enter/exit (references/Celeste.exe.cs.d/Celeste/Player.cs:3745-3757): press down while grounded to duck; standing up
        // needs CanUnDuck, and when blocked with zero horizontal speed, vanilla probes a
        // 4px crawl position (references/Celeste.exe.cs.d/Celeste/Player.cs:3750).
        if self.ducking {
            if self.on_ground && input.axis_y() != Fx::one() {
                if self.can_unduck(rooms) {
                    self.ducking = false;
                } else if self.vel.x == Fx::zero() {
                    for i in (1..=4i32).rev() {
                        let px = Fx::from_int(i);
                        if self.can_unduck_at(rooms, self.pos.x + px) {
                            self.pos.x += Fx::from_ratio(5, 6); // MoveH(50*dt) (references/Celeste.exe.cs.d/Celeste/Player.cs:3753)
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
            self.ducking = true; // references/Celeste.exe.cs.d/Celeste/Player.cs:3753-3757
        }

        // Dash trigger (references/Celeste.exe.cs.d/Celeste/Player.cs:3590-3592 + `CanDash` references/Celeste.exe.cs.d/Celeste/Player.cs:1074-1089): the dash button was
        // pressed (within the buffer window), the cooldown has ended, and dashes remain.
        if self.dash_buffer > 0 && self.dash_cooldown == 0 && self.dashes > 0 {
            self.start_dash(input, rooms); // StartDash (references/Celeste.exe.cs.d/Celeste/Player.cs:4218-4228) -> State 2
            return; // speed is 0 this tick (DashBegin zeroes it); go straight to movement
        }

        // Horizontal: vanilla's two branches (references/Celeste.exe.cs.d/Celeste/Player.cs:3714-3724) -- same-direction overspeed
        // (|vx| > 90 and the same sign as the input) uses the 400 reduction; everything
        // else (no input, opposite braking, starting) uses the full 1000 rate. In the air
        // both are multiplied by 0.65.
        let move_x = self.move_x;
        // Grounded ducking is a *separate* branch: `Speed.X = Approach(Speed.X, 0,
        // 500*dt)` (references/Celeste.exe.cs.d/Celeste/Player.cs:3681-3684). The normal run acceleration is the `else`, so a
        // crouched player decelerates to a stop and cannot run -- the engine previously
        // ran both, so ducking still accelerated sideways.
        if self.ducking && self.on_ground {
            approach(&mut self.vel.x, Fx::zero(), DUCK_DECEL);
        } else {
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
        }
        // Facing follows input, not speed (references/Celeste.exe.cs.d/Celeste/Player.cs:1657-1666): holding into a wall leaves
        // speed at 0, so facing must still point at the wall for the jump boost direction
        // to be right.
        if move_x != Fx::zero() {
            self.facing = move_x.signum();
        }

        // The fall-speed cap is an **eased state**: holding "down" while already
        // accelerating downward -> the cap ramps toward fast fall; otherwise it eases back
        // to normal fall speed. Rate 300 px/s^2. Reference vanilla:
        // `maxFall = Approach(maxFall, 240, 300*dt)`.
        if input.down && self.vel.y >= MAX_FALL {
            approach(&mut self.max_fall, FAST_MAX_FALL, FAST_MAX_ACCEL);
        } else {
            approach(&mut self.max_fall, MAX_FALL, FAST_MAX_ACCEL);
        }

        // Gravity: applied only in the air (references/Celeste.exe.cs.d/Celeste/Player.cs:3789). Near the apex (|vy| < 40 px/s) while
        // holding jump (or AutoJump) -> gravity is halved (num7 at references/Celeste.exe.cs.d/Celeste/Player.cs:3780).
        // Wall slide (references/Celeste.exe.cs.d/Celeste/Player.cs:3854-3888): pushing into the wall (or holding grab with no
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
                // `if (wallSlideTimer / 1.2f > 0.65f) CreateWallSlideParticles(...)`
                // (references/Celeste.exe.cs.d/Celeste/Player.cs:2778-2784): the puff only starts once the slow part of the slide
                // is reached, then the callee paces it on `OnInterval(0.01f)`
                // (references/Celeste.exe.cs.d/Celeste/Player.cs:2746-2760) -- shorter than a tick, so one particle per tick,
                // 5px to the wall side and 4px below the centre.
                if Fx::from_f32(t) > Fx::from_ratio(65, 100) {
                    self.wall_slide_dust();
                }
            }
            let half = (input.jump || self.auto_jump > 0) && self.vel.y.abs() < HALF_GRAV_THRESHOLD;
            let rate = if half { HALF_GRAVITY } else { GRAVITY };
            approach(&mut self.vel.y, target, rate);
        }

        // varJump clamp (references/Celeste.exe.cs.d/Celeste/Player.cs:3791-3797): while the timer is running and jump is held ->
        // vertical speed is clamped back to the jump value (gravity cancelled); on
        // release -> the timer is zeroed. This is "variable jump height", with no extra
        // release speed cut.
        if self.var_jump > 0 {
            if input.jump || self.auto_jump > 0 {
                // references/Celeste.exe.cs.d/Celeste/Player.cs:3883: `Speed.Y = Math.Min(Speed.Y, varJumpSpeed)`.
                self.vel.y = self.vel.y.min(self.var_jump_speed);
            } else {
                self.var_jump = 0;
            }
        }

        // Jump decision (references/Celeste.exe.cs.d/Celeste/Player.cs:3839-3850): inside the buffer window -> grounded/coyote ->
        // `Jump()` (references/Celeste.exe.cs.d/Celeste/Player.cs:2443-2465: vertical -105, horizontal +40*moveX (JumpHBoost),
        // varJump filled to 0.2s); airborne -> ClimbJump when grabbing the wall the jump
        // points away from, otherwise a wall jump (references/Celeste.exe.cs.d/Celeste/Player.cs:3806-3849, `WallJumpCheck`
        // references/Celeste.exe.cs.d/Celeste/Player.cs:2528-2553 -> `ClimbJump` references/Celeste.exe.cs.d/Celeste/Player.cs:2651-2677 / `WallJump` references/Celeste.exe.cs.d/Celeste/Player.cs:2555-2609).
        if self.jump_buffer > 0 {
            if self.on_ground || self.coyote > 0 {
                self.jump(move_x, true);
            } else if self.wall_jump_check(rooms, 1) {
                if self.climb_jump_check(input, 1) {
                    self.climb_jump();
                } else {
                    self.wall_jump(rooms, -1);
                }
            } else if self.wall_jump_check(rooms, -1) {
                if self.climb_jump_check(input, -1) {
                    self.climb_jump();
                } else {
                    self.wall_jump(rooms, 1);
                }
            }
        }
    }

    /// Shared post-state movement and grounded/coyote bookkeeping for NormalUpdate.
    fn normal_movement_tail(&mut self, rooms: &[Room]) {
        self.slide_x(rooms);
        self.on_ground = false;
        self.slide_y(rooms);
        if !self.on_ground {
            self.on_ground = self.hits(rooms, self.pos.x, self.pos.y + Fx::one());
        }
        // Stand up in the air (references/Celeste.exe.cs.d/Celeste/Player.cs:1807-1809): falling with nothing overhead -> cancel
        // ducking.
        if !self.on_ground && self.vel.y > Fx::zero() && self.coyote == 0 && self.can_unduck(rooms)
        {
            self.ducking = false;
        }
        // Coyote time: refresh while grounded, decay once airborne. The jump buffer is
        // not decayed here -- `sample_input_edges` already ages it every tick, matching
        // `VirtualButton.Update` (`Monocle/VirtualButton.cs`).
        if self.on_ground {
            self.coyote = COYOTE_FRAMES;
        } else if self.coyote > 0 {
            self.coyote -= 1;
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
    /// -6f)`, references/Celeste.exe.cs.d/Celeste/Player.cs:607).
    pub(crate) fn box_dims(&self) -> (i32, i32, i32) {
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
        self.hit_in_box(rooms, x, y, self.box_dims())
    }

    /// Whether an explicit hitbox intersects a solid: `w` x `h` pixels with its top-left
    /// corner at `(x, y + y_off)`.
    ///
    /// Vanilla probes a second box by swapping `Collider` around a `CollideCheck<Solid>()`
    /// (`CanUnDuck` references/Celeste.exe.cs.d/Celeste/Player.cs:1030-1044 uses `normalHitbox`, `DuckFreeAt` references/Celeste.exe.cs.d/Celeste/Player.cs:2926-2936 uses
    /// `duckHitbox`); this is the same query without mutating the player.
    fn hit_in_box<'r>(
        &self,
        rooms: &'r [Room],
        x: Fx,
        y: Fx,
        (w, h, y_off): (i32, i32, i32),
    ) -> Option<&'r Room> {
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

    /// Whether an explicit hitbox intersects a solid (the boolean view of
    /// [`Player::hit_in_box`]).
    fn hits_box(&self, rooms: &[Room], x: Fx, y: Fx, dims: (i32, i32, i32)) -> bool {
        self.hit_in_box(rooms, x, y, dims).is_some()
    }

    /// Whether the **normal** (standing) hitbox intersects a solid at `(x, y)`,
    /// whatever the current pose is (vanilla's `Collider = normalHitbox` probe).
    fn hits_normal(&self, rooms: &[Room], x: Fx, y: Fx) -> bool {
        self.hits_box(rooms, x, y, (PLAYER_W_PX, PLAYER_H_PX, 0))
    }

    /// Whether the **ducking** hitbox intersects a solid at `(x, y)` (vanilla's
    /// `Collider = duckHitbox` probe).
    fn hits_duck(&self, rooms: &[Room], x: Fx, y: Fx) -> bool {
        self.hits_box(rooms, x, y, (PLAYER_W_PX, DUCK_H, DUCK_Y_OFF))
    }

    /// Vanilla `OnGround(position)` (references/Celeste.exe.cs.d/Celeste/Actor.cs:147-155) evaluated at another x: is there a
    /// solid 1px below that point? Vanilla also accepts a `JumpThru` there
    /// (`CollideCheckOutside<JumpThru>`, references/Celeste.exe.cs.d/Celeste/Actor.cs:136-145); this engine has no JumpThru yet.
    fn grounded_at(&self, rooms: &[Room], x: Fx) -> bool {
        self.hits(rooms, x, self.pos.y + Fx::one())
    }

    /// Vanilla `DashCorrectCheck(add)` (references/Celeste.exe.cs.d/Celeste/Player.cs:4198-4211): shift the player by `add`, swap to
    /// the **hurtbox** and ask every `LedgeBlocker` whether it blocks the dash. Nothing in
    /// this engine registers a `LedgeBlocker`, so the query is always false -- the same
    /// answer vanilla gives when no blocker is in range.
    fn dash_correct_check(&self, _rooms: &[Room], _add: Vec2) -> bool {
        false
    }

    /// Vanilla `OnCollideH`'s dash corner correction (references/Celeste.exe.cs.d/Celeste/Player.cs:3188-3205).
    ///
    /// Only for a horizontal dash (`Speed.Y == 0 && Speed.X != 0`). It walks the reach
    /// 1..=4 (`DashCornerCorrection`, references/Celeste.exe.cs.d/Celeste/Player.cs:225) crossing both vertical directions, and
    /// accepts the first cell that is free, has solid **below** it (so it is a real
    /// corner, not open air) and is not blocked by a `LedgeBlocker`; the player then moves
    /// vertically onto that cell and one pixel horizontally into it. The early `return`
    /// skips `Speed.X = 0`, so the dash slides around the corner instead of stopping.
    fn corner_correct_dash_h(&mut self, rooms: &[Room]) -> bool {
        if self.vel.y != Fx::zero() || self.vel.x == Fx::zero() {
            return false;
        }
        let sign = self.vel.x.signum();
        for i in 1..=DASH_CORNER_CORRECTION {
            for num in [1, -1] {
                let add = Vec2::new(Fx::from_int(sign), Fx::from_int(i * num));
                let at = self.pos + add;
                if !self.hits(rooms, at.x, at.y)
                    && self.hits(rooms, at.x, self.pos.y + Fx::from_int((i - 1) * num))
                    && !self.dash_correct_check(rooms, add)
                {
                    self.pos = at;
                    return true;
                }
            }
        }
        false
    }

    /// Vanilla `OnCollideV`'s upward corner correction (references/Celeste.exe.cs.d/Celeste/Player.cs:3374-3395).
    ///
    /// When the head clips a ceiling corner, shift up to 4px sideways towards the free
    /// side (5 while dash-attacking with no horizontal speed,
    /// `DashingUpwardCornerCorrection` references/Celeste.exe.cs.d/Celeste/Player.cs:173) and 1px up. The early `return` in vanilla
    /// skips the `Speed.Y = 0` tail, so the jump keeps its upward speed after squeezing
    /// past the corner; without a correction the caller zeroes it and cuts a nearly
    /// finished `varJump` (references/Celeste.exe.cs.d/Celeste/Player.cs:3411-3414).
    fn corner_correct_up(&mut self, rooms: &[Room]) -> bool {
        let mut reach = UPWARD_CORNER_CORRECTION;
        if self.dash_attack > 0 && self.vel.x.abs() < SPEED_EPS {
            reach = DASHING_UPWARD_CORNER_CORRECTION;
        }
        let up = self.pos.y - Fx::one();
        // references/Celeste.exe.cs.d/Celeste/Player.cs:3379-3390: `Speed.X <= 0.01f` tries the left side.
        if self.vel.x <= SPEED_EPS {
            for j in 1..=reach {
                let at = Vec2::new(self.pos.x - Fx::from_int(j), up);
                if !self.hits(rooms, at.x, at.y) {
                    self.pos = at;
                    return true;
                }
            }
        }
        // references/Celeste.exe.cs.d/Celeste/Player.cs:3392-3404: `Speed.X >= -0.01f` tries the right side.
        if self.vel.x >= -SPEED_EPS {
            for j in 1..=reach {
                let at = Vec2::new(self.pos.x + Fx::from_int(j), up);
                if !self.hits(rooms, at.x, at.y) {
                    self.pos = at;
                    return true;
                }
            }
        }
        false
    }

    /// Vanilla `OnCollideV`'s air-dash landing correction (references/Celeste.exe.cs.d/Celeste/Player.cs:3285-3305).
    ///
    /// A dash that **started in the air** (`dashStartedOnGround`, references/Celeste.exe.cs.d/Celeste/Player.cs:4286) whose hitbox
    /// clips the side of a platform while moving down slides horizontally off it (1..=4px,
    /// towards the dash direction) and 1px further down instead of landing; the early
    /// `return` keeps `Speed.Y`, so the dash falls past the corner. A ground-started dash
    /// is excluded, which is what keeps ordinary dash landings crisp.
    fn corner_correct_landing(&mut self, rooms: &[Room]) -> bool {
        if self.state != 2 || self.dash_started_on_ground {
            return false;
        }
        // references/Celeste.exe.cs.d/Celeste/Player.cs:3293-3305: `Speed.X <= 0.01f` -> offsets -1..=-4.
        if self.vel.x <= SPEED_EPS {
            for num in 1..=DASH_CORNER_CORRECTION {
                let x = self.pos.x - Fx::from_int(num);
                if !self.grounded_at(rooms, x) {
                    self.pos = Vec2::new(x, self.pos.y + Fx::one());
                    return true;
                }
            }
        }
        // references/Celeste.exe.cs.d/Celeste/Player.cs:3307-3319: `Speed.X >= -0.01f` -> offsets 1..=4.
        if self.vel.x >= -SPEED_EPS {
            for num in 1..=DASH_CORNER_CORRECTION {
                let x = self.pos.x + Fx::from_int(num);
                if !self.grounded_at(rooms, x) {
                    self.pos = Vec2::new(x, self.pos.y + Fx::one());
                    return true;
                }
            }
        }
        false
    }

    /// Advance along X, snapping to the outside edge of that column on hitting a solid.
    ///
    /// In the dash state a wall contact first tries vanilla's `OnCollideH` handling
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:3181-3208): a **grounded** dash that can crouch at the wall ducks instead of
    /// stopping, otherwise a horizontal dash slides around a 1px-out corner (see
    /// [`Player::corner_correct_dash_h`]).
    fn slide_x(&mut self, rooms: &[Room]) {
        let mut dx = self.vel.x;
        while dx != Fx::zero() {
            let step = clamp_step(dx);
            let nx = self.pos.x + step;
            if self.hits(rooms, nx, self.pos.y) {
                if self.state == 2 {
                    // references/Celeste.exe.cs.d/Celeste/Player.cs:3181-3186: `onGround && DuckFreeAt(Position + UnitX*sign(Speed.X))`
                    // -> `Ducking = true` and return (the dash becomes a slide).
                    let sign = self.vel.x.signum();
                    if self.on_ground && self.duck_free_at(rooms, self.pos.x + Fx::from_int(sign)) {
                        self.ducking = true;
                        return;
                    }
                    if self.corner_correct_dash_h(rooms) {
                        return;
                    }
                }
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
                if step > Fx::zero() {
                    // references/Celeste.exe.cs.d/Celeste/Player.cs:3285-3305: the air-dash landing correction runs before any
                    // landing bookkeeping (and before the wavedash conversion below).
                    if self.corner_correct_landing(rooms) {
                        return;
                    }
                }
                self.pos.y = edge_y(ny, step, self.box_dims().2);
                if step > Fx::zero() {
                    self.on_ground = true;
                    // Vanilla `OnCollideV` (references/Celeste.exe.cs.d/Celeste/Player.cs:3326-3331): a **down-diagonal dash that
                    // lands** becomes a horizontal one -- `DashDir.Y = 0`, `Speed.Y = 0`,
                    // `Speed.X *= 1.2`, `Ducking = true`. This is what makes the hyper
                    // reachable from the air as well as from the ground: the dash-state
                    // jump branch only SuperJumps for `|DashDir.Y| < 0.1`
                    // (references/Celeste.exe.cs.d/Celeste/Player.cs:4391-4403), and a down-diagonal dash never satisfies that
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
                    // references/Celeste.exe.cs.d/Celeste/Player.cs:3333-3338: the landing squash (`Sprite.Scale.X =
                    // Lerp(1,1.6,amount)`, `Scale.Y = Lerp(1,0.4,amount)` with
                    // `amount = min(Speed.Y / 240, 1)`), gated on not climbing. Because it
                    // reads `Speed.Y` *after* the dash conversion zeroes it, a
                    // down-diagonal dash landing keeps scale (1,1) while a hard fall
                    // stretches horizontally. 240 px/s = 4 px/tick.
                    if self.state != 1 {
                        let amount = self.vel.y.min(Fx::from_int(4)) / Fx::from_int(4);
                        let sx = Fx::one() + Fx::from_ratio(3, 5) * amount;
                        let sy = Fx::one() - Fx::from_ratio(3, 5) * amount;
                        self.sprite_scale = [sx, sy];
                        // `if (Speed.Y >= 80f) { Dust.Burst(Position, new Vector2(0,-1)
                        // .Angle(), 8, ...) }` (references/Celeste.exe.cs.d/Celeste/Player.cs:3358-3361) -- a landing puff only
                        // once the fall is fast enough, 8 particles, straight up. It
                        // reads `Speed.Y` after the dash conversion above zeroed it, so
                        // a down-diagonal dash landing stays puff-free.
                        if self.vel.y >= LAND_DUST_SPEED {
                            let anchor = self.anchor();
                            self.burst(anchor, -PI_OVER_2, LAND_DUST_COUNT);
                        }
                    }
                } else {
                    // references/Celeste.exe.cs.d/Celeste/Player.cs:3374-3395: a ceiling contact first tries the upward corner
                    // correction; on success the upward speed survives the squeeze.
                    if self.corner_correct_up(rooms) {
                        return;
                    }
                    // references/Celeste.exe.cs.d/Celeste/Player.cs:3411-3414: `if (varJumpTimer < 0.15f) varJumpTimer = 0f` -- a
                    // bonk with less than 0.15s left (9 of the 12 ticks) kills it.
                    if self.var_jump < 9 {
                        self.var_jump = 0;
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

/// Vanilla `Calc.Approach(Vector2 value, Vector2 target, float maxMove)`: move `value`
/// toward `target` by at most `max_move`, **along the normalised direction** (not
/// component-wise), which is what `PlayerHair.AfterUpdate` relies on for the spring.
fn approach_vec(value: Vec2, target: Vec2, max_move: Fx) -> Vec2 {
    let delta = target - value;
    if delta.length() <= max_move {
        target
    } else {
        value + delta.safe_normalized() * max_move
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
        // The floor is row 3 (y = 24..32) and the hitbox is 11 tall (references/Celeste.exe.cs.d/Celeste/Player.cs:605) -> the top
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
        // tall, references/Celeste.exe.cs.d/Celeste/Player.cs:605).
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
        // `step` does not run `update_visuals`, so the landing squash (set in `slide_y`)
        // would be left stuck at its collision value; the fixture's intent is a neutral,
        // rested player, so restore scale to 1 (as the real per-frame `update_visuals`
        // decay would after enough frames).
        p.sprite_scale = [Fx::one(), Fx::one()];
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
    fn a_dash_press_expires_instead_of_waiting_for_the_cooldown() {
        let room = tall_room();
        let mut p = settled(&room, 32.0);
        // Keep the dash gate shut (`CanDash` needs `dashCooldownTimer <= 0`,
        // references/Celeste.exe.cs.d/Celeste/Player.cs:1074-1089) for longer than the 0.08s buffer window.
        p.dash_cooldown = DASH_COOLDOWN_TICKS + 10;
        let press = Input {
            dash: true,
            ..Input::default()
        };
        p.step(&press, std::slice::from_ref(&room));
        assert!(p.dash_buffer > 0, "the press fills the buffer");

        // `VirtualButton.Update` ages `bufferCounter` every tick, so the press is spent
        // after `JUMP_BUFFER_FRAMES` ticks even though the dash gate stays shut.
        for _ in 0..JUMP_BUFFER_FRAMES {
            p.step(&Input::default(), std::slice::from_ref(&room));
        }
        assert_eq!(p.dash_buffer, 0, "the buffer must expire while unspendable");

        // Let the cooldown run out: the stale press must NOT fire on the refill.
        for _ in 0..(DASH_COOLDOWN_TICKS + 12) {
            p.step(&Input::default(), std::slice::from_ref(&room));
        }
        assert_eq!(p.dash_cooldown, 0, "fixture: the cooldown has ended");
        assert!(p.dashes > 0, "fixture: a dash is available again");
        assert_eq!(p.state, 0, "a stale dash press must not start a dash");
    }

    #[test]
    fn an_upward_ground_dash_does_not_refund_the_dash_immediately() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        let up_dash = Input {
            up: true,
            dash: true,
            ..Input::default()
        };
        p.step(&up_dash, std::slice::from_ref(&room));
        assert_eq!(p.state, 2, "the up dash starts");
        assert_eq!(
            p.dashes, 0,
            "StartDash spends the dash (references/Celeste.exe.cs.d/Celeste/Player.cs:4220)"
        );
        // `DashBegin` arms `dashRefillCooldownTimer = 0.1f` = 6 ticks (references/Celeste.exe.cs.d/Celeste/Player.cs:4283-4314) and
        // the ground refill is the `else` of that timer (references/Celeste.exe.cs.d/Celeste/Player.cs:1592-1612), so the dash is
        // not handed straight back -- this is the reported "extra dash".
        for _ in 0..DASH_REFILL_COOLDOWN_TICKS {
            p.step(&Input::default(), std::slice::from_ref(&room));
            assert_eq!(p.dashes, 0, "the refill cooldown blocks the refund");
        }
        // Airborne now: the dash only comes back on landing.
        let mut landed = false;
        for _ in 0..120 {
            p.step(&Input::default(), std::slice::from_ref(&room));
            if p.on_ground {
                landed = true;
                break;
            }
            assert_eq!(p.dashes, 0, "an air dash does not refill in the air");
        }
        assert!(landed, "fixture: the player must come back down");
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.dashes, 1, "landing refills once the cooldown is over");
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
        // Vanilla's release only stops the varJump clamp (references/Celeste.exe.cs.d/Celeste/Player.cs:3902-3904), with **no**
        // extra speed cut: gravity continues from the current speed (references/Celeste.exe.cs.d/Celeste/Player.cs:3872). The second
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
        assert_eq!(
            p.vel,
            Vec2::ZERO,
            "DashBegin zeroes Speed (references/Celeste.exe.cs.d/Celeste/Player.cs:4309)"
        );
        assert_eq!(
            p.dashes, 0,
            "StartDash consumes one dash (references/Celeste.exe.cs.d/Celeste/Player.cs:4220)"
        );
        assert_eq!(
            p.freeze_ticks, 3,
            "Celeste.Freeze(0.05f) = 3 ticks (references/Celeste.exe.cs.d/Celeste/Player.cs:4289)"
        );
        // tick 2: coroutine first segment -> speed = 4 px/tick.
        p.step(&dash, std::slice::from_ref(&room));
        assert_eq!(
            p.vel.x, DASH_SPEED,
            "Speed = aim * 240 px/s (references/Celeste.exe.cs.d/Celeste/Player.cs:4494)"
        );
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
        assert_eq!(
            p.state, 0,
            "the coroutine returns to Normal (references/Celeste.exe.cs.d/Celeste/Player.cs:4579)"
        );
        assert_eq!(
            p.vel.x, DASH_END_SPEED,
            "Speed = DashDir * 160 (references/Celeste.exe.cs.d/Celeste/Player.cs:4573)"
        );
        let _ = x0;
    }

    /// Dash count: exhausting it with an air dash -> hair eases toward USED; refill on
    /// landing -> white flash, then back to NORMAL after 0.12s.
    #[test]
    fn dash_refill_and_hair_states_follow_update_hair() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        let dash = Input {
            right: true,
            dash: true,
            ..Input::default()
        };
        p.step(&dash, std::slice::from_ref(&room));
        assert_eq!(p.dashes, 0);
        // `DashBegin` arms `dashRefillCooldownTimer = 0.1f` = 6 ticks (references/Celeste.exe.cs.d/Celeste/Player.cs:4283-4314) and
        // the ground refill is the `else` of that timer (references/Celeste.exe.cs.d/Celeste/Player.cs:1592-1612), so the dash is
        // NOT refunded on the frame after the dash -- it comes back on tick 8.
        for _ in 0..DASH_REFILL_COOLDOWN_TICKS {
            p.step(&Input::default(), std::slice::from_ref(&room));
            assert_eq!(
                p.dashes, 0,
                "the refill cooldown blocks the ground refill (references/Celeste.exe.cs.d/Celeste/Player.cs:1592-1612)"
            );
        }
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert_eq!(
            p.dashes, 1,
            "RefillDash once the 0.1s refill cooldown ran out (references/Celeste.exe.cs.d/Celeste/Player.cs:1592-1612)"
        );
        assert_eq!(
            p.hair_color(),
            crate::sprites::hair_color::FLASH,
            "lastDashes != Dashes flashes white (references/Celeste.exe.cs.d/Celeste/Player.cs:2017-2020)"
        );
        // Dash 0.2s cooldown (references/Celeste.exe.cs.d/Celeste/Player.cs:4290): it starts on tick 1 and the coroutine ends on tick
        // 10 (phase 10 > DASH_TICKS), so the cooldown only reaches zero on tick 12.
        // Coroutine phases: tick 2 = first segment (phase 1), ending its last segment on
        // tick 11 -- 10 state ticks in all (1 entry + 9 of the 0.15s wait).
        for _ in 0..9 {
            p.step(&Input::default(), std::slice::from_ref(&room));
        }
        assert_eq!(
            p.state, 0,
            "the dash coroutine ended (references/Celeste.exe.cs.d/Celeste/Player.cs:4579)"
        );
        // Cooldown gate (`dashCooldownTimer <= 0` in `CanDash` references/Celeste.exe.cs.d/Celeste/Player.cs:1074-1089): reset the
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
        // The ground refill is again behind the 0.1s refill cooldown armed by `DashBegin`
        // (references/Celeste.exe.cs.d/Celeste/Player.cs:1592-1612, references/Celeste.exe.cs.d/Celeste/Player.cs:4283-4314), so the dash comes back 6 ticks later and the
        // hair flashes white at that point.
        for _ in 0..DASH_REFILL_COOLDOWN_TICKS {
            p.step(&Input::default(), std::slice::from_ref(&room));
            assert_eq!(p.dashes, 0, "still inside the refill cooldown");
        }
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.dashes, 1, "ground dash refills after the cooldown");
        assert_eq!(p.hair_color(), crate::sprites::hair_color::FLASH);
        // White flash 0.12s = 7 ticks: every tick with the timer > 0 shows FLASH (the else
        // branch at references/Celeste.exe.cs.d/Celeste/Player.cs:2035-2039); on the **next** tick after it reaches zero, the
        // discrete branch returns NORMAL.
        for _ in 0..8 {
            p.step(&Input::default(), std::slice::from_ref(&room));
        }
        assert_eq!(p.hair_color(), crate::sprites::hair_color::NORMAL);
        // Air exhaustion -> ease into USED (references/Celeste.exe.cs.d/Celeste/Player.cs:2007-2011): first spend the only dash
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
        // moveX != 0 (the `else if (moveX != 0)` at references/Celeste.exe.cs.d/Celeste/Player.cs:2577-2587).
        let jump = Input {
            left: true,
            jump: true,
            ..Input::default()
        };
        p.step(&jump, std::slice::from_ref(&room));
        assert_eq!(
            p.vel.x, WALLJUMP_X,
            "WallJump: Speed.X = 130 * dir (references/Celeste.exe.cs.d/Celeste/Player.cs:2585)"
        );
        assert_eq!(
            p.vel.y, JUMP_SPEED,
            "WallJump: Speed.Y = -105 (references/Celeste.exe.cs.d/Celeste/Player.cs:2590)"
        );
        assert_eq!(
            p.force_move_x, 1,
            "WallJump forces moveX away from the wall (references/Celeste.exe.cs.d/Celeste/Player.cs:2577-2587)"
        );
        // Within the forceMoveX window, opposite input is overridden (references/Celeste.exe.cs.d/Celeste/Player.cs:1632-1640).
        let push_left = Input {
            left: true,
            ..Input::default()
        };
        p.step(&push_left, std::slice::from_ref(&room));
        assert_eq!(p.move_x, Fx::one(), "forceMoveX overrides input for 0.16s");
    }

    /// Jumping during a ground horizontal dash = super jump: 260 px/s * Facing, -105
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:2489-2527).
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
            "SuperJump: Speed.X = 260 * Facing (references/Celeste.exe.cs.d/Celeste/Player.cs:2497)"
        );
        assert_eq!(
            p.vel.y, JUMP_SPEED,
            "SuperJump: Speed.Y = -105 (references/Celeste.exe.cs.d/Celeste/Player.cs:2498)"
        );
    }

    /// `Sprite.Scale` set by the super jump (0.6,1.4 at references/Celeste.exe.cs.d/Celeste/Player.cs:2517) then springs back to 1
    /// each tick through `UpdateSprite` (references/Celeste.exe.cs.d/Celeste/Player.cs:2051-2052, rate 1.75 * dt). The value is
    /// consumed by the renderer (body squash about the origin) and by the hair root
    /// offset (`-9 * Scale.Y`, references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:63), so it must be tracked in the sim.
    /// `Jump(particles: true)` queues the 4-particle landing-style puff at the bottom
    /// centre (`Dust.Burst(BottomCenter, -PiOver2, 4, ...)`, references/Celeste.exe.cs.d/Celeste/Player.cs:2474-2482) and sets the
    /// vertical stretch (references/Celeste.exe.cs.d/Celeste/Player.cs:2473). `ClimbJump` passes `particles: false` and must stay
    /// dust-free (references/Celeste.exe.cs.d/Celeste/Player.cs:2655).
    #[test]
    fn a_normal_jump_queues_dust_but_a_climb_jump_does_not() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        // `settled` lands the player, which already queues a landing puff; start clean.
        p.dust().clear();
        let jump = Input {
            jump: true,
            ..Input::default()
        };
        let anchor = p.anchor();
        p.step(&jump, std::slice::from_ref(&room));
        let bursts = p.dust();
        assert_eq!(bursts.len(), 1, "one burst per jump, got {bursts:?}");
        assert_eq!(
            bursts[0].count, 4,
            "Jump bursts 4 dust particles (references/Celeste.exe.cs.d/Celeste/Player.cs:2482)"
        );
        assert_eq!(bursts[0].position, anchor, "burst sits at BottomCenter");
        assert!(
            (p.sprite_scale[0].to_f32() - 0.6).abs() < 0.01,
            "Jump Scale.X = 0.6 (references/Celeste.exe.cs.d/Celeste/Player.cs:2473, got {})",
            p.sprite_scale[0].to_f32()
        );

        // `ClimbJump` calls `Jump(particles: false, ...)` (references/Celeste.exe.cs.d/Celeste/Player.cs:2655), so the jump half of
        // that move stays dust-free. The wall-jump puff it emits separately is covered
        // by the wall-jump path, so this asserts the `particles: false` gate directly.
        let mut p = settled(&room, 40.0);
        p.dust().clear();
        p.jump(Fx::zero(), false);
        assert!(
            p.dust().is_empty(),
            "Jump(particles: false) must not queue dust, got {:?}",
            p.dust()
        );
    }

    /// `DirectionalShake(DashDir, 0.2f)` fires once the dash direction is chosen
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:4531-4534), and the service drains the request.
    #[test]
    fn a_dash_queues_a_directional_shake() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        assert!(p.take_shake_request().is_none(), "no shake while idle");
        let dash = Input {
            right: true,
            dash: true,
            ..Input::default()
        };
        p.step(&dash, std::slice::from_ref(&room)); // StartDash (state 2, phase 0)
        assert!(
            p.take_shake_request().is_none(),
            "the direction is only known once the dash tick runs"
        );
        p.step(&dash, std::slice::from_ref(&room)); // phase 0 -> picks the aim
        let (dir, time) = p.take_shake_request().expect("dash must request a shake");
        assert_eq!(
            dir,
            Vec2::new(Fx::one(), Fx::zero()),
            "shake follows DashDir"
        );
        assert_eq!(
            time,
            Fx::from_ratio(1, 5),
            "0.2s (references/Celeste.exe.cs.d/Celeste/Player.cs:4532)"
        );
        assert!(
            p.take_shake_request().is_none(),
            "the request is drained exactly once"
        );
    }

    /// A hard landing puffs 8 particles straight up; a slow one does not
    /// (`if (Speed.Y >= 80f)`, references/Celeste.exe.cs.d/Celeste/Player.cs:3358-3361).
    #[test]
    fn only_a_fast_landing_queues_dust() {
        let room = tall_room();
        // Fall from high: the drop is long enough to exceed 80 px/s (4/3 px/tick).
        let mut p = Player::new(Vec2::from_f32s(32.0, 20.0));
        for _ in 0..80 {
            p.step(&Input::default(), std::slice::from_ref(&room));
        }
        assert!(p.on_ground, "fixture: the player must land");
        let anchor = p.anchor();
        let bursts = p.dust();
        assert_eq!(bursts.len(), 1, "a hard landing puffs once, got {bursts:?}");
        assert_eq!(
            bursts[0].count, LAND_DUST_COUNT,
            "8 particles (references/Celeste.exe.cs.d/Celeste/Player.cs:3360)"
        );
        assert_eq!(bursts[0].position, anchor, "burst sits at Position");

        // A tiny drop (a few pixels) stays below the gate.
        let mut low = Player::new(Vec2::from_f32s(32.0, 118.0));
        for _ in 0..10 {
            low.step(&Input::default(), std::slice::from_ref(&room));
        }
        assert!(low.on_ground);
        assert!(
            low.dust().is_empty(),
            "a slow landing stays below the 80 px/s gate, got {:?}",
            low.dust()
        );
    }

    #[test]
    fn super_jump_sets_scale_then_it_springs_back() {
        let room = wide_room();
        let table = anim_table();
        let mut p = settled(&room, 40.0);
        // Ground dash -> super jump on the next tick, mirroring the wavedash scenario.
        let dash = Input {
            right: true,
            dash: true,
            ..Input::default()
        };
        p.step(&dash, std::slice::from_ref(&room)); // enter State 2
        p.step(&dash, std::slice::from_ref(&room)); // speed takes effect (still grounded)
        let jump = Input {
            jump: true,
            ..Input::default()
        };
        p.step(&jump, std::slice::from_ref(&room));
        assert!(
            (p.sprite_scale[0].to_f32() - 0.6).abs() < 0.01,
            "SuperJump Scale.X = 0.6 (references/Celeste.exe.cs.d/Celeste/Player.cs:2517, got {})",
            p.sprite_scale[0].to_f32()
        );
        assert!(
            (p.sprite_scale[1].to_f32() - 1.4).abs() < 0.01,
            "SuperJump Scale.Y = 1.4 (references/Celeste.exe.cs.d/Celeste/Player.cs:2517, got {})",
            p.sprite_scale[1].to_f32()
        );
        // The spring-back runs per frame in `update_sprite` (references/Celeste.exe.cs.d/Celeste/Player.cs:2051-2052): `step` only
        // does movement/state, so call `update_visuals` to observe the decay toward 1.
        let grounded = Input::default();
        for _ in 0..3 {
            p.update_visuals(&table, &grounded, std::slice::from_ref(&room));
        }
        let mid_x = p.sprite_scale[0].to_f32();
        assert!(
            mid_x > 0.6 && mid_x < 1.0,
            "scale approaches 1 from 0.6 (got {mid_x})"
        );
        for _ in 0..120 {
            p.update_visuals(&table, &grounded, std::slice::from_ref(&room));
        }
        let settled_y = p.sprite_scale[1].to_f32();
        assert!(
            (settled_y - 1.0).abs() < 0.01,
            "scale settles back at 1 (references/Celeste.exe.cs.d/Celeste/Player.cs:2051-2052, got {settled_y})"
        );
    }

    /// `Player.OnTransition` (references/Celeste.exe.cs.d/Celeste/Player.cs:2289-2296) refills the dash and stamina
    /// and re-arms the wall slide once a room transition completes (called at
    /// references/Celeste.exe.cs.d/Celeste/Level.cs:1626). Regression: a wave dash over a room seam previously kept
    /// the spent dash and drained stamina.
    #[test]
    fn on_transition_refills_dash_and_stamina() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        // Spend the dash and drain some stamina.
        let dash = Input {
            right: true,
            dash: true,
            ..Input::default()
        };
        p.step(&dash, std::slice::from_ref(&room));
        assert_eq!(
            p.dashes, 0,
            "the dash is consumed (references/Celeste.exe.cs.d/Celeste/Player.cs:4283)"
        );
        p.stamina = Fx::from_ratio(-1, 1); // -1 (arbitrary drained value)
        p.coyote = 5;
        p.force_move_x_ticks = 3;
        p.wall_slide_ticks = 0;
        p.on_transition();
        assert_eq!(
            p.dashes, p.max_dashes,
            "RefillDash (references/Celeste.exe.cs.d/Celeste/Player.cs:2295)"
        );
        assert_eq!(
            p.stamina, CLIMB_MAX_STAMINA,
            "RefillStamina (references/Celeste.exe.cs.d/Celeste/Player.cs:2296)"
        );
        assert_eq!(
            p.coyote, 0,
            "jumpGraceTimer = 0 (references/Celeste.exe.cs.d/Celeste/Player.cs:2291)"
        );
        assert_eq!(
            p.force_move_x_ticks, 0,
            "forceMoveXTimer = 0 (references/Celeste.exe.cs.d/Celeste/Player.cs:2292)"
        );
        assert_eq!(
            p.wall_slide_ticks, WALLSLIDE_TICKS,
            "wallSlideTimer = 1.2 (references/Celeste.exe.cs.d/Celeste/Player.cs:2290)"
        );
    }

    /// Vanilla `OnCollideV` (references/Celeste.exe.cs.d/Celeste/Player.cs:3326-3331): a **down-diagonal dash that lands** becomes a
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
            "DashDir.Y must be zeroed on landing (references/Celeste.exe.cs.d/Celeste/Player.cs:3328)"
        );
        assert_eq!(
            p.dash_dir.x,
            Fx::one(),
            "DashDir.X collapses to the sign (references/Celeste.exe.cs.d/Celeste/Player.cs:3327)"
        );
        assert_eq!(
            p.vel.y,
            Fx::zero(),
            "Speed.Y = 0 (references/Celeste.exe.cs.d/Celeste/Player.cs:3329)"
        );
        assert!(
            p.ducking,
            "Ducking = true: the landed diagonal dash is a duck slide (references/Celeste.exe.cs.d/Celeste/Player.cs:3331)"
        );
        // Down-right dash is 240/sqrt(2) = 2.828 px/tick, x1.2 = 3.394 px/tick.
        let vx_ps = p.vel.x.to_f32() * 60.0;
        assert!(
            (vx_ps - 203.6).abs() < 0.5,
            "Speed.X must be x1.2 of the diagonal dash (references/Celeste.exe.cs.d/Celeste/Player.cs:3330), got {vx_ps} px/s"
        );
    }

    /// The full **air hyper**: jump, dash down-diagonal in the air, land, then jump inside
    /// the dash window.
    ///
    /// The landed dash is horizontal and ducking, so `DashUpdate`'s jump branch
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:4391-4403, `|DashDir.Y| < 0.1`) reaches `SuperJump` and its ducked branch
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:2502-2509) doubles down on the launch: `Speed.X = 260 * 1.25 = 325` px/s and
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
        // `on_ground` is the 1px probe vanilla uses too (references/Celeste.exe.cs.d/Celeste/Player.cs:1577), so it can read true one
        // tick before the movement actually collides; the conversion runs in the collision
        // handler (`OnCollideV` needs `Speed.Y > 0`), so wait for it to have happened.
        let mut ticks = 0;
        while (p.dash_dir.y != Fx::zero() || !p.on_ground) && ticks < 60 {
            p.step(&hold, std::slice::from_ref(&room));
            ticks += 1;
        }
        assert!(p.on_ground, "fixture: the diagonal dash must land");
        assert_eq!(p.dash_dir.y, Fx::zero(), "the landed dash is horizontal");
        assert!(p.ducking, "landing a diagonal dash duck-slides (references/Celeste.exe.cs.d/Celeste/Player.cs:3331)");
        assert_eq!(p.state, 2, "still inside the dash window");
        // Jump inside the window: the ducked SuperJump (325 / -52.5).
        p.step(&jump, std::slice::from_ref(&room));
        assert_eq!(p.state, 0, "SuperJump returns to Normal");
        let vx_ps = p.vel.x.to_f32() * 60.0;
        let vy_ps = p.vel.y.to_f32() * 60.0;
        assert!(
            (vx_ps - 325.0).abs() < 1.0,
            "hyper horizontal speed: 260 * 1.25 = 325 px/s (references/Celeste.exe.cs.d/Celeste/Player.cs:2497 + references/Celeste.exe.cs.d/Celeste/Player.cs:2505), got {vx_ps}"
        );
        assert!(
            (vy_ps + 52.5).abs() < 1.0,
            "hyper vertical speed: -105 * 0.5 = -52.5 px/s (references/Celeste.exe.cs.d/Celeste/Player.cs:2498 + references/Celeste.exe.cs.d/Celeste/Player.cs:2506), got {vy_ps}"
        );
        assert!(
            !p.ducking,
            "the ducked branch clears Ducking (references/Celeste.exe.cs.d/Celeste/Player.cs:2504)"
        );
        // The varJump clamp must keep the **halved** vertical speed for the whole window.
        // Regression: `varJumpSpeed` was set to -105 before the duck branch, so on the
        // next held-jump tick the clamp (`Speed.Y = min(Speed.Y, varJumpSpeed)`,
        // references/Celeste.exe.cs.d/Celeste/Player.cs:3883) yanked the wavedash back up to -105 at full speed -- high horizontal,
        // vertical not reduced. Vanilla assigns `varJumpSpeed = Speed.Y` *after* the duck
        // branch (references/Celeste.exe.cs.d/Celeste/Player.cs:2516), so it is -52.5 here.
        assert_eq!(
            p.var_jump_speed, p.vel.y,
            "varJumpSpeed = halved Speed.Y (references/Celeste.exe.cs.d/Celeste/Player.cs:2516)"
        );
        for _ in 0..5 {
            // Hold jump: the clamp keeps vertical pinned at -52.5 through the window.
            p.step(&jump, std::slice::from_ref(&room));
            if p.var_jump > 0 {
                assert!(
                    (p.vel.y.to_f32() * 60.0 + 52.5).abs() < 1.0,
                    "held-jump varJump must stay at -52.5, got {} px/s (references/Celeste.exe.cs.d/Celeste/Player.cs:3883)",
                    p.vel.y.to_f32() * 60.0
                );
            }
        }
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
            "landing must snap to the world edge of the floor (hitbox 11 tall, references/Celeste.exe.cs.d/Celeste/Player.cs:605)"
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
    /// Ducking: press down while grounded to enter (references/Celeste.exe.cs.d/Celeste/Player.cs:3753-3757), the hitbox switches
    /// to 8x6 with its bottom edge aligned (references/Celeste.exe.cs.d/Celeste/Player.cs:607), and releasing stands back up
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:3745-3752).
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
            "crouch on the ground with down held (references/Celeste.exe.cs.d/Celeste/Player.cs:3753-3757)"
        );
        assert_eq!(p.box_dims(), (PLAYER_W_PX, DUCK_H, DUCK_Y_OFF));
        assert!(p.on_ground, "duck hitbox keeps the bottom edge aligned");
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert!(!p.ducking, "release down to stand back up (references/Celeste.exe.cs.d/Celeste/Player.cs:3745-3752)");
    }

    /// A grounded crouch cannot run: vanilla's duck branch replaces the run
    /// acceleration entirely (`references/Celeste.exe.cs.d/Celeste/Player.cs:3681-3684` `if (Ducking && onGround) Speed.X =
    /// Approach(Speed.X, 0, 500*dt); else { ... }`), so holding a direction while
    /// crouched only decelerates.
    #[test]
    fn crouching_does_not_allow_running() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        // Enter the crouch first.
        let down = Input {
            down: true,
            ..Input::default()
        };
        p.step(&down, std::slice::from_ref(&room));
        assert!(p.ducking);
        // Now hold down + right for many ticks: speed must stay at rest, not reach RUN.
        let down_right = Input {
            down: true,
            right: true,
            ..Input::default()
        };
        for _ in 0..30 {
            p.step(&down_right, std::slice::from_ref(&room));
        }
        assert!(
            p.vel.x.abs() < Fx::from_ratio(1, 2),
            "a crouched player must not run (references/Celeste.exe.cs.d/Celeste/Player.cs:3681-3684); vx={:?}",
            p.vel.x
        );
    }

    /// A grounded crouch-dash plays the `duck` animation, not the standing `dash`
    /// (`references/Celeste.exe.cs.d/Celeste/Player.cs:2105-2107` `Sprite.Play("duck");`). The engine
    /// previously fell through to `dash`.
    #[test]
    fn crouch_dash_uses_the_duck_animation() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        let table = anim_table();
        // Crouch, then press dash while holding down.
        let down = Input {
            down: true,
            ..Input::default()
        };
        p.step(&down, std::slice::from_ref(&room));
        assert!(p.ducking);
        let down_dash = Input {
            down: true,
            dash: true,
            ..Input::default()
        };
        p.step(&down_dash, std::slice::from_ref(&room));
        // Advance into the dash-attack window so `update_sprite` runs its dash branch.
        p.update_visuals(&table, &down_dash, std::slice::from_ref(&room));
        assert!(
            p.ducking,
            "a grounded crouch-dash keeps Ducking (references/Celeste.exe.cs.d/Celeste/Player.cs:4315-4322)"
        );
        assert_eq!(
            p.anim_id.to_ascii_lowercase(),
            "duck",
            "the crouch-dash must play `duck` (references/Celeste.exe.cs.d/Celeste/Player.cs:2105-2107)"
        );
    }

    /// Climb: grab against a wall -> State 1; hanging drains stamina continuously (still
    /// 10/s, references/Celeste.exe.cs.d/Celeste/Player.cs:4027); landing refills it (references/Celeste.exe.cs.d/Celeste/Player.cs:1567-1572); releasing the grab lets go
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:3960-3965).
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
        assert_eq!(p.state, 1, "grabbing a wall enters Climb (references/Celeste.exe.cs.d/Celeste/Player.cs:3592-3598)");
        let before = p.stamina;
        for _ in 0..12 {
            p.step(&grab, std::slice::from_ref(&room));
        }
        assert!(
            p.stamina < before,
            "hanging drains stamina at 10/s (references/Celeste.exe.cs.d/Celeste/Player.cs:4027-4029)"
        );
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.stamina, CLIMB_MAX_STAMINA, "ground refills stamina");
        assert_eq!(
            p.state, 0,
            "releasing grab leaves Climb (references/Celeste.exe.cs.d/Celeste/Player.cs:3960-3965)"
        );
    }

    /// NormalUpdate returning state 1 still runs the outer movement after ClimbBegin
    /// (`references/Celeste.exe.cs.d/Celeste/Player.cs:3592-3598` and `references/Celeste.exe.cs.d/Celeste/Player.cs:1798-1805`).
    #[test]
    fn grabbing_applies_the_scaled_fall_speed_immediately() {
        let room = wall_room(true);
        let mut p = settled(&room, 56.0);
        p.facing = 1;
        p.pos.y -= Fx::from_int(16);
        p.on_ground = false;
        p.vel.y = Fx::from_int(60);
        let before = p.pos.y;
        let grab = Input {
            grab: true,
            ..Input::default()
        };
        p.step(&grab, std::slice::from_ref(&room));
        assert_eq!(p.state, 1);
        assert_eq!(
            p.vel.y,
            Fx::from_int(60) * Fx::from_ratio(1, 5),
            "ClimbBegin scales Y by 0.2"
        );
        assert!(
            p.pos.y > before,
            "the scaled fall speed is applied on the grab tick"
        );
    }

    /// The outer `Player.Update` applies `MoveV(Speed.Y * dt)` after `ClimbUpdate`
    /// returns (`references/Celeste.exe.cs.d/Celeste/Player.cs:1802-1805`), including the tick that ClimbJump
    /// selects state 0 (`references/Celeste.exe.cs.d/Celeste/Player.cs:3942-3954`).
    #[test]
    fn climb_jump_moves_on_its_activation_tick() {
        let room = wall_room(true);
        let mut p = settled(&room, 56.0);
        p.facing = 1;
        p.pos.y -= Fx::from_int(32);
        p.on_ground = false;
        p.state = 1;
        p.vel = Vec2::ZERO;
        p.climb_no_move_ticks = 0;
        let before = p.pos.y;
        let jump = Input {
            jump: true,
            grab: true,
            ..Input::default()
        };
        p.step(&jump, std::slice::from_ref(&room));
        assert_eq!(p.state, 0);
        assert_eq!(p.vel.y, JUMP_SPEED);
        assert!(
            p.pos.y < before,
            "the -105 px/s jump must displace the player immediately"
        );
        assert_eq!(p.stamina, CLIMB_MAX_STAMINA - CLIMBJUMP_DRAIN);
    }

    /// Exhaustion also returns to state 0 before the outer movement
    /// (`references/Celeste.exe.cs.d/Celeste/Player.cs:4103-4107` and `references/Celeste.exe.cs.d/Celeste/Player.cs:1798-1805`).
    #[test]
    fn climb_exhaustion_still_applies_the_last_climb_velocity() {
        let room = wall_room(true);
        let mut p = settled(&room, 56.0);
        p.facing = 1;
        p.pos.y -= Fx::from_int(32);
        p.on_ground = false;
        p.state = 1;
        p.vel.y = CLIMB_UP_SPEED;
        p.climb_no_move_ticks = 0;
        // One fixed-point unit short of this tick's upward drain.
        p.stamina = CLIMB_UP_DRAIN_PER_TICK - Fx::from_bits(1);
        let before = p.pos.y;
        let up = Input {
            up: true,
            grab: true,
            ..Input::default()
        };
        p.step(&up, std::slice::from_ref(&room));
        assert_eq!(p.state, 0, "Stamina <= 0 leaves Climb");
        assert_eq!(
            p.stamina,
            Fx::from_bits(-1),
            "upward climbing drains exactly 45.454544 / 60 stamina"
        );
        assert!(p.pos.y < before, "the final climb velocity is not lost");
    }

    /// ClimbBegin and the jump/dash transitions clear a stale wall boost
    /// (`references/Celeste.exe.cs.d/Celeste/Player.cs:3896`, `references/Celeste.exe.cs.d/Celeste/Player.cs:2452`, `references/Celeste.exe.cs.d/Celeste/Player.cs:2496`, `references/Celeste.exe.cs.d/Celeste/Player.cs:2565`, `references/Celeste.exe.cs.d/Celeste/Player.cs:2625`,
    /// `references/Celeste.exe.cs.d/Celeste/Player.cs:3543-3546`, each `wallBoostTimer = 0f`).
    #[test]
    fn transitions_cancel_an_unused_wall_boost() {
        let room = wall_room(true);
        let rooms = std::slice::from_ref(&room);
        let mut p = settled(&room, 56.0);
        let arm = |p: &mut Player| {
            p.wall_boost_dir = -1;
            p.wall_boost_ticks = WALL_BOOST_TICKS;
        };

        arm(&mut p);
        p.enter_climb(rooms);
        assert_eq!(p.wall_boost_ticks, 0, "ClimbBegin");
        arm(&mut p);
        p.jump(Fx::zero(), false);
        assert_eq!(p.wall_boost_ticks, 0, "Jump");
        arm(&mut p);
        p.wall_jump(rooms, -1);
        assert_eq!(p.wall_boost_ticks, 0, "WallJump");
        arm(&mut p);
        p.super_jump(rooms);
        assert_eq!(p.wall_boost_ticks, 0, "SuperJump");
        arm(&mut p);
        p.super_wall_jump(-1);
        assert_eq!(p.wall_boost_ticks, 0, "SuperWallJump");
        arm(&mut p);
        p.start_dash(&Input::default(), rooms);
        assert_eq!(p.wall_boost_ticks, 0, "NormalEnd on a dash");
    }

    /// `references/Celeste.exe.cs.d/Celeste/Player.cs:3581` `Input.GrabCheck && !IsTired && !Ducking` prevents an
    /// exhausted player from grabbing again. The check uses `CheckStamina`, so the climb
    /// jump's temporary boost adds its 27.5 cost back (`references/Celeste.exe.cs.d/Celeste/Player.cs:1054-1056`).
    #[test]
    fn tired_grabbing_uses_effective_stamina() {
        let room = wall_room(true);
        let grab = Input {
            grab: true,
            ..Input::default()
        };
        let mut tired = settled(&room, 56.0);
        tired.facing = 1;
        tired.pos.y -= Fx::from_int(16);
        tired.on_ground = false;
        tired.stamina = Fx::from_int(19);
        tired.step(&grab, std::slice::from_ref(&room));
        assert_eq!(
            tired.state, 0,
            "CheckStamina < 20 cannot grab (references/Celeste.exe.cs.d/Celeste/Player.cs:1048)"
        );

        let mut boosted = settled(&room, 56.0);
        boosted.facing = 1;
        boosted.pos.y -= Fx::from_int(16);
        boosted.on_ground = false;
        boosted.stamina = Fx::from_int(19);
        boosted.wall_boost_ticks = WALL_BOOST_TICKS;
        boosted.step(&grab, std::slice::from_ref(&room));
        assert_eq!(
            boosted.state, 1,
            "wallBoostTimer adds 27.5 for this check (references/Celeste.exe.cs.d/Celeste/Player.cs:1054-1056)"
        );

        boosted.stamina = CLIMB_TIRED_THRESHOLD;
        boosted.wall_boost_ticks = 0;
        assert!(
            !boosted.is_tired(),
            "the threshold is strictly below 20, not less-than-or-equal"
        );
    }

    /// The 1-2px upward grab correction is gated on not holding down
    /// (`references/Celeste.exe.cs.d/Celeste/Player.cs:3601` `(float)Input.MoveY < 1f`). A direct wall grab is still
    /// allowed with down held; only the vertical snap is skipped.
    #[test]
    fn holding_down_skips_the_two_pixel_grab_snap() {
        let mut room = wall_room(false);
        for y in 0..8 {
            room.tiles.set(8, y, 1);
        }
        let down_grab = Input {
            down: true,
            grab: true,
            ..Input::default()
        };
        let grab = Input {
            grab: true,
            ..Input::default()
        };
        let mut down = Player::new(Vec2::from_f32s(56.0, 64.0));
        down.facing = 1;
        down.on_ground = false;
        let rooms = std::slice::from_ref(&room);
        assert!(!down.climb_check(rooms, 1, 0), "fixture: below the wall");
        assert!(
            down.climb_check(rooms, 1, -1),
            "fixture: one pixel up reaches the wall"
        );
        down.step(&down_grab, rooms);
        assert_eq!(down.state, 0, "down suppresses the upward grab snap");

        let mut neutral = Player::new(Vec2::from_f32s(56.0, 64.0));
        neutral.facing = 1;
        neutral.on_ground = false;
        neutral.step(&grab, rooms);
        assert_eq!(neutral.state, 1, "neutral input performs the snap");
        assert_eq!(neutral.pos.y, Fx::from_int(63), "MoveVExact(-1)");
    }

    /// A jump while grabbing the wall the player faces is a `ClimbJump`, not a wall jump.
    ///
    /// Vanilla branches on six conditions (references/Celeste.exe.cs.d/Celeste/Player.cs:3812-3825): facing the wall, grab held,
    /// stamina left, no carry, no grab assist, no `ClimbBlocker`. The first three are the
    /// ones that vary here. The distinguishing numbers come from `ClimbJump()`
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:2651-2677): a plain `Jump()` vertical (-105 px/s, references/Celeste.exe.cs.d/Celeste/Player.cs:2454) with **no**
    /// horizontal impulse -- `WallJump` would instead set 130 px/s away from the wall
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:2585) -- and 27.5 stamina spent when airborne (references/Celeste.exe.cs.d/Celeste/Player.cs:2653-2655).
    ///
    /// This branch is what makes the corner boost work: with no horizontal input the climb
    /// jump also arms the 0.2 s `wallBoostTimer` (references/Celeste.exe.cs.d/Celeste/Player.cs:2659-2662), which re-applies 130 px/s
    /// toward the wall once the player pushes that way.
    #[test]
    fn a_grab_jump_into_the_wall_is_a_climb_jump() {
        let room = wall_room(true);
        let mut p = settled(&room, 56.0);
        p.facing = 1;
        let grab = Input {
            grab: true,
            ..Input::default()
        };
        // Lift the player off the floor first: `ClimbJump` only spends stamina when
        // airborne (references/Celeste.exe.cs.d/Celeste/Player.cs:2653-2655), and standing on the ground next to a wall keeps
        // `onGround` true through the climb tick.
        p.pos.y -= Fx::from_int(16);
        p.on_ground = false;
        p.step(&grab, std::slice::from_ref(&room));
        assert_eq!(
            p.state, 1,
            "grab enters Climb (references/Celeste.exe.cs.d/Celeste/Player.cs:3592-3598)"
        );
        assert!(!p.on_ground, "fixture: the climb starts airborne");
        let stamina = p.stamina;

        // Jump with grab still held and no direction: ClimbJump.
        let jump = Input {
            grab: true,
            jump: true,
            ..Input::default()
        };
        p.step(&jump, std::slice::from_ref(&room));
        assert_eq!(
            p.state, 0,
            "the climb jump leaves Climb (references/Celeste.exe.cs.d/Celeste/Player.cs:3942-3954)"
        );
        assert_eq!(
            p.vel.x,
            Fx::zero(),
            "ClimbJump adds no horizontal impulse, unlike WallJump's 130 px/s (references/Celeste.exe.cs.d/Celeste/Player.cs:2585)"
        );
        assert_eq!(
            p.vel.y, JUMP_SPEED,
            "the climb jump is a plain Jump() vertical (references/Celeste.exe.cs.d/Celeste/Player.cs:2454)"
        );
        assert_eq!(
            p.stamina,
            stamina - CLIMBJUMP_DRAIN,
            "27.5 stamina when airborne (references/Celeste.exe.cs.d/Celeste/Player.cs:2653-2655)"
        );
        assert_eq!(
            p.wall_boost_ticks, WALL_BOOST_TICKS,
            "no horizontal input arms the 0.2 s wall boost (references/Celeste.exe.cs.d/Celeste/Player.cs:2659-2662)"
        );
    }

    /// Holding a direction into the wall does not arm the wall boost: vanilla only sets
    /// `wallBoostTimer` when `moveX == 0` (references/Celeste.exe.cs.d/Celeste/Player.cs:2659-2662).
    #[test]
    fn a_climb_jump_with_direction_skips_the_wall_boost() {
        let room = wall_room(true);
        let mut p = settled(&room, 56.0);
        p.facing = 1;
        p.step(
            &Input {
                grab: true,
                ..Input::default()
            },
            std::slice::from_ref(&room),
        );
        assert_eq!(p.state, 1);
        p.step(
            &Input {
                grab: true,
                jump: true,
                right: true,
                ..Input::default()
            },
            std::slice::from_ref(&room),
        );
        assert_eq!(
            p.wall_boost_ticks, 0,
            "moveX != 0 skips the boost window (references/Celeste.exe.cs.d/Celeste/Player.cs:2659-2662)"
        );
    }

    /// Without grab the same jump is a normal wall jump: 130 px/s away from the wall.
    ///
    /// The fixture grabs the wall first (which sets `Facing` toward it) and then releases
    /// grab while pressing jump, so `WallJumpCheck` finds the solid and `ClimbJump`'s
    /// grab condition fails -- exactly the vanilla branch split at references/Celeste.exe.cs.d/Celeste/Player.cs:3812-3825.
    #[test]
    fn a_jump_off_the_wall_without_grab_is_a_wall_jump() {
        let room = wall_room(true);
        let mut p = settled(&room, 56.0);
        p.facing = 1;
        let grab = Input {
            grab: true,
            ..Input::default()
        };
        p.pos.y -= Fx::from_int(16);
        p.on_ground = false;
        p.step(&grab, std::slice::from_ref(&room));
        assert_eq!(p.state, 1, "fixture: climbing before the jump");
        // Pressing *away* from the wall is the `WallJump` half of `ClimbUpdate`'s jump
        // branch (references/Celeste.exe.cs.d/Celeste/Player.cs:3942-3954: `if (moveX == -Facing) WallJump(-Facing) else ClimbJump()`);
        // with no direction it would be a `ClimbJump` instead.
        let away = Input {
            jump: true,
            left: true,
            ..Input::default()
        };
        p.step(&away, std::slice::from_ref(&room));
        assert_eq!(
            p.vel.x, -WALLJUMP_X,
            "a wall jump pushes 130 px/s away from the wall (references/Celeste.exe.cs.d/Celeste/Player.cs:2585)"
        );
    }

    // -----------------------------------------------------------------
    // Sprite animation (the `Sprite` state machine) -- regression: walking looked
    // like it was only "sliding".
    // -----------------------------------------------------------------

    /// A single animation table good enough for these tests: only id / frame count /
    /// interval / goto matter, and the frame names are arbitrary.
    ///
    /// Keys must be lowercase (the Monocle dictionary is `OrdinalIgnoreCase`, references/Celeste.exe.cs.d/Monocle/Sprite.cs:81);
    /// `goto` keeps the exact casing from the XML (vanilla
    /// `CurrentAnimationID = Goto`, references/Celeste.exe.cs.d/Monocle/Sprite.cs:154-159).
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
    /// Regression point: `PlayerSprite.Running` (references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:169-182) reads
    /// `LastAnimationID`. It was once written as "the current animation has a goto", and
    /// since `idle` carries `<Loop>` (goto=idle) -> `!Sprite.Running` was always false ->
    /// the `runSlow`/`runFast` at references/Celeste.exe.cs.d/Celeste/Player.cs:2196-2204 could never play and the character merely
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

        // Standing still: the start animation is idle (the sprite's start, references/Celeste.exe.cs.d/Monocle/SpriteData.cs:87-90).
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
                "walking right but playing {:?} (references/Celeste.exe.cs.d/Celeste/Player.cs:2213-2219)",
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

        // ---- Turn and go left: flip first (references/Celeste.exe.cs.d/Celeste/Player.cs:2204-2208); when flip ends it gotos runFast ----
        p.step(&left, std::slice::from_ref(&room));
        p.update_visuals(&table, &left, std::slice::from_ref(&room));
        assert!(
            matches!(p.anim_id.as_str(), "flip" | "skid"),
            "a fast reversal should play flip/skid first (references/Celeste.exe.cs.d/Celeste/Player.cs:2190-2208), actually {:?}",
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
    /// true, the `!Sprite.Running` guard at references/Celeste.exe.cs.d/Celeste/Player.cs:2196 never fired again and the character
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

    /// `LastAnimationID` holds the animation **just played** (references/Celeste.exe.cs.d/Monocle/Sprite.cs:366-367:
    /// `(CurrentAnimationID = id)` evaluates to `id`), and `Running` (references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:169-182) reads
    /// it as "the most recently played animation starts with `run`, or is `flip`".
    ///
    /// That is what re-arms the `!Sprite.Running` guard at references/Celeste.exe.cs.d/Celeste/Player.cs:2196 after a stop:
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

        // flip special case: `if (LastAnimationID == "flip") return true` (references/Celeste.exe.cs.d/Celeste/PlayerSprite.cs:175-178).
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

    /// The goto branch (references/Celeste.exe.cs.d/Monocle/Sprite.cs:135-157) writes the new id into
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
            "the goto branch records the target it switched to (references/Celeste.exe.cs.d/Monocle/Sprite.cs:142)"
        );
        assert!(p.running());
    }

    /// The idle branch's speed threshold is 25 px/s, **not** the 45 px/s of
    /// runSlow/runFast (references/Celeste.exe.cs.d/Celeste/Player.cs:2151 vs references/Celeste.exe.cs.d/Celeste/Player.cs:2198).
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

    /// Super wall jump pins the vanilla values (references/Celeste.exe.cs.d/Celeste/Player.cs:2626-2629).
    #[test]
    fn super_wall_jump_pins_vanilla_speeds() {
        let mut p = Player::new(Vec2::ZERO);
        p.super_wall_jump(1);
        assert_eq!(p.vel.x, Fx::from_ratio(17, 6), "Speed.X = 170f * dir");
        assert_eq!(p.vel.y, Fx::from_ratio(-8, 3), "Speed.Y = -160f");
        assert_eq!(p.var_jump_speed, p.vel.y, "varJumpSpeed = Speed.Y");
        assert_eq!(
            p.var_jump, 15,
            "varJumpTimer = 0.25s (references/Celeste.exe.cs.d/Celeste/Player.cs:2619)"
        );
        assert_eq!(
            p.auto_jump, 0,
            "AutoJump = false (references/Celeste.exe.cs.d/Celeste/Player.cs:2620)"
        );
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

    // -- corner correction + pose probes (references/Celeste.exe.cs.d/Celeste/Player.cs:3184-3404) ----------------

    /// A room with a floor on the last row plus the given extra solid cells.
    fn corner_room(w: u32, h: u32, extra: &[(u32, u32)]) -> Room {
        let mut tiles = Tileset::new("Gameplay", w, h);
        for x in 0..w {
            tiles.set(x, h - 1, 1);
        }
        for (x, y) in extra {
            tiles.set(*x, *y, 1);
        }
        Room {
            name: "corner".into(),
            bounds: RectDef {
                x: 0.0,
                y: 0.0,
                w: (w * 8) as f32,
                h: (h * 8) as f32,
            },
            tiles,
            bg: Tileset::new("Gameplay", w, h),
            entities: Vec::new(),
            triggers: Vec::new(),
            script: None,
            ..Default::default()
        }
    }

    /// Vanilla `OnCollideV` (references/Celeste.exe.cs.d/Celeste/Player.cs:3374-3395): clipping the corner of a ceiling tile nudges
    /// the head sideways and **keeps** `Speed.Y`, because the early `return` skips the
    /// `Speed.Y = 0` tail.
    #[test]
    fn ceiling_corner_correction_keeps_the_upward_speed() {
        // 4x6 room, floor on row 5, one ceiling tile at (1, 1) = x 8..16, y 8..16.
        let room = corner_room(4, 6, &[(1, 1)]);
        let mut p = Player::new(Vec2::from_f32s(4.0, 16.0));
        p.vel.y = -Fx::one();
        p.slide_y(std::slice::from_ref(&room));
        // The box [4,12)x[16,27) clips the tile by 4px; walking left to x = 0 clears it,
        // so the correction moves up 1px and keeps going.
        assert_eq!(p.pos, Vec2::from_f32s(0.0, 15.0), "nudged to the free side");
        assert_eq!(p.vel.y, -Fx::one(), "still moving up");
        assert!(!p.on_ground);
    }

    /// A ceiling with no free corner within the 4px reach stops the jump and cuts a
    /// nearly finished `varJump` (references/Celeste.exe.cs.d/Celeste/Player.cs:3411-3414).
    #[test]
    fn ceiling_bonk_stops_the_jump_and_cuts_var_jump() {
        let room = corner_room(4, 6, &[(0, 1), (1, 1), (2, 1), (3, 1)]);
        let mut p = Player::new(Vec2::from_f32s(4.0, 16.0));
        p.vel.y = -Fx::one();
        p.var_jump = 5; // under 0.15s (9 of the 12 ticks) -> killed
        p.slide_y(std::slice::from_ref(&room));
        assert_eq!(p.pos.y, Fx::from_int(16));
        assert_eq!(p.vel.y, Fx::zero(), "the bonk zeroes the upward speed");
        assert_eq!(p.var_jump, 0, "0.15s left is not enough to keep the clamp");
    }

    #[test]
    fn ceiling_bonk_keeps_a_fresh_var_jump() {
        let room = corner_room(4, 6, &[(0, 1), (1, 1), (2, 1), (3, 1)]);
        let mut p = Player::new(Vec2::from_f32s(4.0, 16.0));
        p.vel.y = -Fx::one();
        p.var_jump = 10;
        p.slide_y(std::slice::from_ref(&room));
        assert_eq!(p.var_jump, 10, "0.15s or more survives the bonk");
    }

    /// Vanilla `CanUnDuck` (references/Celeste.exe.cs.d/Celeste/Player.cs:1030-1044) probes the **normal** hitbox, so a ducked
    /// player under a 1-tile gap cannot stand up; `CanUnDuckAt` (references/Celeste.exe.cs.d/Celeste/Player.cs:2917-2924) answers
    /// for a neighbouring column (references/Celeste.exe.cs.d/Celeste/Player.cs:3633-3638).
    #[test]
    fn ducking_under_a_gap_cannot_stand() {
        // Overhang at (0, 3) = x 0..8, y 24..32: the 6px duck box fits in y 34..40, the
        // 11px normal box does not.
        let room = corner_room(4, 6, &[(0, 3)]);
        let mut p = Player::new(Vec2::from_f32s(0.0, 29.0));
        p.ducking = true;
        assert!(
            !p.can_unduck(std::slice::from_ref(&room)),
            "the normal box clips the overhang"
        );
        assert!(!p.can_unduck_at(std::slice::from_ref(&room), Fx::zero()));
        assert!(
            p.can_unduck_at(std::slice::from_ref(&room), Fx::from_int(8)),
            "the next column is clear"
        );
        p.pos.x = Fx::from_int(8);
        assert!(p.can_unduck(std::slice::from_ref(&room)));
    }

    /// Vanilla `OnCollideH` (references/Celeste.exe.cs.d/Celeste/Player.cs:3181-3186): a **grounded** dash whose duck box fits past
    /// the obstacle crouches and keeps sliding instead of stopping.
    #[test]
    fn grounded_dash_into_an_overhang_crouches() {
        // Overhang at (2, 1) = x 16..24, y 8..16: the 11px box [13,24) clips it while
        // dashing right along the floor, the 6px duck box [18,24) does not.
        let room = corner_room(4, 4, &[(2, 1)]);
        let mut p = Player::new(Vec2::from_f32s(0.0, 13.0));
        p.state = 2;
        p.on_ground = true;
        // A dash moves 4px per tick (240 px/s), so it takes three ticks to reach the
        // overhang at x = 16.
        p.vel = Vec2::new(Fx::from_int(4), Fx::zero());
        for _ in 0..3 {
            p.slide_x(std::slice::from_ref(&room));
        }
        assert!(p.ducking, "a ground dash that fits while ducking crouches");
        assert_eq!(p.pos.x, Fx::from_int(8), "flush against the overhang");
        assert_eq!(
            p.vel.x,
            Fx::from_int(4),
            "the early return keeps the dash speed"
        );
    }

    /// Vanilla `OnCollideH`'s dash branch (references/Celeste.exe.cs.d/Celeste/Player.cs:3188-3205): a horizontal dash that clips a
    /// corner by a pixel or two slips past it instead of stopping.
    #[test]
    fn horizontal_dash_slips_past_a_ceiling_corner() {
        // Floating block at (2, 1) = x 16..24, y 8..16; the dash box [15,26) clips its
        // bottom edge by exactly 1px.
        let room = corner_room(4, 6, &[(2, 1)]);
        let mut p = Player::new(Vec2::from_f32s(0.0, 15.0));
        p.state = 2;
        p.dash_started_on_ground = true;
        // A dash moves 4px per tick (240 px/s), so it takes three ticks to reach the
        // block at x = 16.
        p.vel = Vec2::new(Fx::from_int(4), Fx::zero());
        for _ in 0..3 {
            p.slide_x(std::slice::from_ref(&room));
        }
        assert_eq!(
            p.pos,
            Vec2::from_f32s(9.0, 16.0),
            "down 1px and forward 1px"
        );
        assert_eq!(p.vel.x, Fx::from_int(4), "the dash keeps its speed");
        assert_eq!(p.vel.y, Fx::zero());
    }

    /// Vanilla `OnCollideV` (references/Celeste.exe.cs.d/Celeste/Player.cs:3285-3305): a dash that **started in the air** and clips
    /// the side of a platform slides off it instead of landing; a ground-started dash
    /// lands normally (`dashStartedOnGround`, references/Celeste.exe.cs.d/Celeste/Player.cs:4286).
    #[test]
    fn air_dash_landing_correction_slips_off_the_ledge() {
        // Floor over columns 0..2 only: column 3 is a hole.
        let mut room = corner_room(4, 4, &[]);
        room.tiles.set(3, 3, 0);
        let mut p = Player::new(Vec2::from_f32s(20.0, 13.0));
        p.state = 2;
        p.vel = Vec2::new(Fx::one(), Fx::one());
        p.slide_y(std::slice::from_ref(&room));
        assert_eq!(p.pos, Vec2::from_f32s(24.0, 14.0), "slid off the corner");
        assert!(!p.on_ground, "did not land");
        assert_eq!(p.vel.y, Fx::one(), "the falling speed survives");
    }

    #[test]
    fn ground_dash_lands_on_the_ledge() {
        let mut room = corner_room(4, 4, &[]);
        room.tiles.set(3, 3, 0);
        let mut p = Player::new(Vec2::from_f32s(20.0, 13.0));
        p.state = 2;
        p.dash_started_on_ground = true;
        p.vel = Vec2::new(Fx::one(), Fx::one());
        p.slide_y(std::slice::from_ref(&room));
        assert!(p.on_ground, "a ground dash lands");
        assert_eq!(p.pos.y, Fx::from_int(13));
        assert_eq!(p.vel.y, Fx::zero());
    }

    /// The shared table with per-frame hair metadata on every player animation, so the
    /// spring has an offset to follow whichever animation is playing.
    fn hair_table(offset: (i32, i32)) -> crate::sprites::AnimationTable {
        let mut table = anim_table();
        let frame = crate::sprites::HairFrame {
            has_hair: true,
            offset,
            frame: 0,
        };
        if let Some(def) = table.sprites.get_mut("player") {
            for anim in def.anims.values_mut() {
                anim.hair = vec![frame; anim.frames.len()];
            }
        }
        table
    }

    /// The hair root is the frame's `HairOffset` mirrored by facing, 9px above the anchor
    /// (vanilla `Nodes[0] = RenderPosition + (0, -9) + HairOffset * (Facing, 1)`,
    /// references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:62-63). Regression: this used to be the fixed offset `(0, -2)`.
    #[test]
    fn hair_root_follows_the_per_frame_offset_and_facing() {
        let room = wide_room();
        let table = hair_table((1, -1));
        let mut p = settled(&room, 40.0);
        p.facing = 1;
        p.update_visuals(&table, &Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.hair_nodes()[0], p.anchor() + Vec2::from_f32s(1.0, -10.0));
        p.facing = -1;
        p.update_visuals(&table, &Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.hair_nodes()[0], p.anchor() + Vec2::from_f32s(-1.0, -10.0));
    }

    /// `PlayerHair.Start` drops the chain 200px to the side (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:50-57) and the first
    /// tick only clamps it back to [`HAIR_MAX_SEGMENT`] from the head (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:78), so the
    /// hair swings in instead of teleporting onto the skull.
    #[test]
    fn hair_chain_starts_at_the_clamp_behind_the_head() {
        let room = wide_room();
        let table = hair_table((0, -2));
        let mut p = settled(&room, 40.0);
        p.update_visuals(&table, &Input::default(), std::slice::from_ref(&room));
        let nodes = p.hair_nodes();
        let gap = (nodes[1] - nodes[0]).length();
        assert!(
            (gap - HAIR_MAX_SEGMENT).abs() <= Fx::from_ratio(1, 100),
            "clamped 3px behind the head, gap {gap:?}"
        );
        assert!(
            nodes[1].y < p.anchor().y - Fx::from_int(8),
            "the tail hangs below the root, got {:?}",
            nodes[1]
        );
    }

    /// Standing still, the chain converges on the static step chain
    /// `(0-Facing)*StepInFacingPerSegment + (0, 2)` per node with node 1 using twice the
    /// facing step (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:64) and the defaults of references/Celeste.exe.cs.d/Celeste/Player.cs:1731-1736.
    #[test]
    fn hair_chain_settles_on_the_step_chain() {
        let room = wide_room();
        let table = hair_table((0, -2));
        let mut p = settled(&room, 40.0);
        for _ in 0..400 {
            p.update_visuals(&table, &Input::default(), std::slice::from_ref(&room));
        }
        // Facing right and offset (0,-2): node 0 = anchor + (0,-11), then (-1,-9),
        // (-1.5,-7), (-2,-5).
        let anchor = p.anchor();
        let expected = [
            anchor + Vec2::from_f32s(0.0, -11.0),
            anchor + Vec2::from_f32s(-1.0, -9.0),
            anchor + Vec2::from_f32s(-1.5, -7.0),
            anchor + Vec2::from_f32s(-2.0, -5.0),
        ];
        for (i, want) in expected.iter().enumerate() {
            let got = p.hair_nodes()[i];
            assert!(
                (got - *want).length() <= Fx::from_ratio(1, 2),
                "node {i} settled at {got:?}, want {want:?}"
            );
        }
    }

    /// A room transition freezes the chain (`TransitionTo` -> `UpdateHair(false)`,
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:1354-1357 / references/Celeste.exe.cs.d/Celeste/Player.cs:2043-2045), but the very next normal tick arms it again
    /// (`Player.UpdateHair(applyGravity: true)`, references/Celeste.exe.cs.d/Celeste/Player.cs:1811-1813). Without the re-arm the
    /// flag stays false forever and the hair hangs mid-air as soon as the player crosses
    /// a room seam.
    #[test]
    fn update_visuals_rearms_the_hair_after_a_transition() {
        let room = wide_room();
        let table = hair_table((0, -2));
        let mut p = settled(&room, 40.0);
        p.update_hair_rigid(&table);
        assert!(
            !p.simulate_motion,
            "a transition freezes the chain (references/Celeste.exe.cs.d/Celeste/Player.cs:2043-2045)"
        );
        p.update_visuals(&table, &Input::default(), std::slice::from_ref(&room));
        assert!(
            p.simulate_motion,
            "a normal tick re-arms the chain (references/Celeste.exe.cs.d/Celeste/Player.cs:1811-1813)"
        );
    }

    /// A 32-cell-wide room with a floor row at row 15, optionally with a wall column at
    /// cell 8 (x = 64..72) running from the top down to the floor.
    fn wall_room(with_wall: bool) -> Room {
        let mut tiles = Tileset::new("Gameplay", 32, 16);
        for x in 0..32 {
            tiles.set(x, 15, 1);
        }
        if with_wall {
            for y in 0..15 {
                tiles.set(8, y, 1);
            }
        }
        Room {
            name: "w".into(),
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

    /// `ClimbHop()` over the top of a wall must NOT launch immediately: while the wall
    /// is still beside the player vanilla stores `hopWaitX`/`hopWaitXSpeed` and only
    /// applies the 100 px/s push once the solid clears (references/Celeste.exe.cs.d/Celeste/Player.cs:4130-4137 and
    /// references/Celeste.exe.cs.d/Celeste/Player.cs:1664-1669). Without the wall beside the player the push is immediate.
    #[test]
    fn climb_hop_defers_the_push_until_the_wall_is_clear() {
        // Wall still beside the player -> the push is deferred, Speed.X untouched.
        let walled = wall_room(true);
        let mut p = settled(&walled, 56.0);
        p.facing = 1;
        assert!(p.hits(std::slice::from_ref(&walled), p.pos.x + Fx::one(), p.pos.y));
        p.climb_hop(std::slice::from_ref(&walled));
        assert!(p.climb_hop_solid, "the wall is still beside the player");
        assert_eq!(
            p.hop_wait_x, 1,
            "hopWaitX = Facing (references/Celeste.exe.cs.d/Celeste/Player.cs:4132)"
        );
        assert_eq!(
            p.hop_wait_x_speed, CLIMB_HOP_X,
            "hopWaitXSpeed = Facing * 100 px/s (references/Celeste.exe.cs.d/Celeste/Player.cs:4133)"
        );
        assert_eq!(
            p.vel.x,
            Fx::zero(),
            "the horizontal push waits for the solid to clear"
        );

        // The wall clears on a later tick -> `update_head` applies the saved speed. The
        // rest of the tick then runs normal friction (as vanilla's NormalUpdate does),
        // so only the sign and the magnitude band are asserted here.
        let clear = wall_room(false);
        p.step(&Input::default(), std::slice::from_ref(&clear));
        assert_eq!(p.hop_wait_x, 0, "the deferred push resolves once clear");
        assert!(
            p.vel.x > Fx::from_int(1),
            "the saved 100 px/s push is applied (references/Celeste.exe.cs.d/Celeste/Player.cs:4137), vx={:?}",
            p.vel.x
        );

        // No solid beside the player -> the push applies immediately.
        let open = wall_room(false);
        let mut q = settled(&open, 56.0);
        q.facing = 1;
        q.climb_hop(std::slice::from_ref(&open));
        assert!(!q.climb_hop_solid);
        assert_eq!(q.hop_wait_x, 0);
        assert_eq!(
            q.vel.x, CLIMB_HOP_X,
            "an open ledge hops at 100 px/s at once"
        );
    }

    /// Running stretches the tail: each node eases toward a target one step behind its
    /// parent, so at speed it sits exactly on the 3px clamp (references/Celeste.exe.cs.d/Celeste/PlayerHair.cs:69-78).
    #[test]
    fn hair_chain_is_stretched_while_running() {
        let room = wide_room();
        let table = hair_table((0, -2));
        let mut p = settled(&room, 40.0);
        let right = Input {
            right: true,
            ..Input::default()
        };
        for _ in 0..120 {
            p.update_visuals(&table, &Input::default(), std::slice::from_ref(&room));
        }
        for _ in 0..60 {
            p.step(&right, std::slice::from_ref(&room));
            p.update_visuals(&table, &right, std::slice::from_ref(&room));
        }
        let nodes = p.hair_nodes();
        for i in 1..nodes.len() {
            let gap = (nodes[i] - nodes[i - 1]).length();
            assert!(
                gap <= HAIR_MAX_SEGMENT,
                "node {i} is past the 3px clamp, gap {gap:?}"
            );
        }
        let gap = (nodes[3] - nodes[2]).length();
        assert!(
            gap > Fx::from_ratio(5, 2),
            "running must pull the chain taut, gap {gap:?}"
        );
    }

    /// Two stocked dashes lengthen the tail to five segments (references/Celeste.exe.cs.d/Celeste/Player.cs:1735) and pull it
    /// backwards with `StepPerSegment.x = sin(2 * TimeActive) * 0.7 - Facing * 3`
    /// (references/Celeste.exe.cs.d/Celeste/Player.cs:1714-1724).
    #[test]
    fn two_dashes_lengthen_and_pull_back_the_hair() {
        let room = wide_room();
        let table = hair_table((0, -2));
        let mut p = settled(&room, 40.0);
        p.update_visuals(&table, &Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.hair_nodes().len(), crate::sprites::HAIR_COUNT);
        p.dashes = 2;
        p.update_visuals(&table, &Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.hair_nodes().len(), crate::sprites::HAIR_MAX);
        for _ in 0..300 {
            p.update_visuals(&table, &Input::default(), std::slice::from_ref(&room));
        }
        let tail = p.hair_nodes()[4];
        let head = p.hair_nodes()[0];
        assert!(
            tail.x < head.x - Fx::from_int(6),
            "the two-dash tail trails backwards, got {tail:?} vs head {head:?}"
        );
    }
}
