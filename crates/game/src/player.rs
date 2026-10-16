//! 玩家仿真：8×8 AABB 对 tile grid 的轴分离解算。
//!
//! 所有状态都是 [`Fx`]（AGENTS.md §15.7），时间步恒为
//! [`FIXED_DT`](reles_math::FIXED_DT)。
//!
//! 与原版的关系（`references/pureCeleste.cs`，行为参考）：
//! - **已对齐**：跑动加速（1000/400/0.65）、重力 900、落速上限
//!   160/240 缓动（`maxFall` 状态）、顶点半重力（40 阈值）、
//!   coyote/跳跃缓冲/松键削减（Phase 4.1 已落地）；房间选择与兜底
//!   （中心选房、缝隙由最近房间接住）。
//! - **仍是初值**：跳跃（原版是 `JumpSpeed = -105` + varJump 钳制 +
//!   `JumpHBoost = 40`，本实现是 -240 单冲量）—— 下一轮对齐；
//!   dash / 爬墙等尚未开始（ROADMAP Phase 4.1）。

use reles_map::Room;
use reles_math::{Fx, Vec2};

use crate::session::{nearest_room, room_containing, Session};

/// hitbox 宽（像素，整数，供房间碰撞使用）。
pub const PLAYER_W_PX: i32 = 8;
/// hitbox 高（像素，整数）。
pub const PLAYER_H_PX: i32 = 8;
/// hitbox 宽（f32 视图，供渲染使用）。
pub const PLAYER_W: f32 = PLAYER_W_PX as f32;
/// hitbox 高（f32 视图，供渲染使用）。
pub const PLAYER_H: f32 = PLAYER_H_PX as f32;

/// 水平跑速 90 px/s（1.5 px/tick @ 60Hz）。
const RUN: Fx = Fx::from_ratio(3, 2);
/// 水平加速度 1000 px/s²（≈0.278 px/tick²）—— 起步不是瞬间满速。
const RUN_ACCEL: Fx = Fx::from_ratio(5, 18);
/// 超速回归 / 反向刹车 400 px/s²：冲下坡或急转身时收得比起步更慢。
const RUN_REDUCE: Fx = Fx::from_ratio(1, 9);
/// 空中操控系数 0.65 —— 空中加速是地面的一半多点。
const AIR_MULT: Fx = Fx::from_ratio(13, 20);
/// 重力 900 px/s²（0.25 px/tick²）。
const GRAVITY: Fx = Fx::from_ratio(1, 4);
/// 顶点半重力：上升/下落都很慢且按住跳跃时重力减半（450 px/s²）。
const HALF_GRAVITY: Fx = Fx::from_ratio(1, 8);
/// 普通落速上限 160 px/s（8/3 px/tick）。
///
/// **参考值**：`pureCeleste.cs` Player 的 `MaxFall = 160f`。它是一个
/// **缓动状态**（见 [`Player::max_fall`]），按住"下"时上限爬向
/// [`FAST_MAX_FALL`]，松开再缓回来 —— 不是硬截断。
const MAX_FALL: Fx = Fx::from_ratio(8, 3);
/// 快落上限 240 px/s（4 px/tick）。参考 `FastMaxFall = 240f`。
const FAST_MAX_FALL: Fx = Fx::from_int(4);
/// 落速上限的缓动速率 300 px/s²（1/12 px/tick²）。参考 `FastMaxAccel = 300f`。
const FAST_MAX_ACCEL: Fx = Fx::from_ratio(1, 12);
/// 顶点半重力阈值 40 px/s（2/3 px/tick）。参考 `HalfGravThreshold = 40f`。
const HALF_GRAV_THRESHOLD: Fx = Fx::from_ratio(2, 3);
/// 起跳初速度 -240 px/s，冲量高度约 32px（4 格）。
const JUMP: Fx = Fx::from_int(-4);
/// 碰撞解算的推进步长：1/2 px。
const STEP: Fx = Fx::from_ratio(1, 2);

/// 起跳输入缓冲：落地前多少帧内按过跳跃，落地瞬间仍会触发（Phase 4.1）。
const JUMP_BUFFER_FRAMES: u32 = 6;
/// 土狼时间：离开平台后多少帧内仍可起跳（Phase 4.1）。
const COYOTE_FRAMES: u32 = 6;
/// 松开跳跃时对上升速度的削减系数（= 0.4，Celeste 风格的一次性 dampening）。
///
/// 按下跳跃→按住不放能跳到全高；快速松手则跃得更矮更平，这是
/// "可变跳高"的核心。削减只做一次（松手那一刻），不是每帧。
const JUMP_CUT: Fx = Fx::from_ratio(2, 5);

/// 一帧的按键状态（由窗口宿主喂进来）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Input {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
    pub jump: bool,
}

impl Input {
    /// 水平轴：-1 / 0 / +1（同时按左右则不移动）。
    pub fn axis_x(self) -> Fx {
        match (self.left, self.right) {
            (true, false) => -Fx::one(),
            (false, true) => Fx::one(),
            _ => Fx::zero(),
        }
    }
}

/// 玩家当前状态。
#[derive(Debug, Clone)]
pub struct Player {
    /// hitbox 左上角（世界坐标）。
    pub pos: Vec2,
    /// 上一 tick 的位置，渲染时做线性插值。
    pub prev_pos: Vec2,
    /// 速度（px/tick）。
    pub vel: Vec2,
    /// 是否踩在固体上。
    pub on_ground: bool,
    /// 朝向：1 右，-1 左。
    pub facing: i32,
    /// 当前落速上限（px/tick）：在 [`MAX_FALL`] 与 [`FAST_MAX_FALL`] 之间
    /// 按 [`FAST_MAX_ACCEL`] 缓动 —— 参考 pureCeleste 的 `maxFall` 局部状态。
    max_fall: Fx,
    /// 上一 tick 的跳跃键（边沿检测：按下 / 松开）。
    prev_jump: bool,
    /// 起跳缓冲倒计时：0 = 无。按下跳跃时设满，起跳后清空。
    jump_buffer: u32,
    /// 土狼时间倒计时：0 = 无。接地时重置，离地后递减。
    coyote: u32,
}

impl Player {
    /// 在 `pos`（世界坐标，hitbox 左上角）生成玩家。
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
        }
    }

    /// 推进一个固定 tick。房间由 [`Session::room_at`] 决定。
    pub fn update(&mut self, input: &Input, session: &Session) {
        self.step(input, &session.doc.rooms);
    }

    /// 单步仿真：按 hitbox 中心选房 → 物理 → 世界级兜底。
    ///
    /// `rooms` 显式传入，单测可以直接构造房间几何，不必起整个 Session。
    pub(crate) fn step(&mut self, input: &Input, rooms: &[Room]) {
        let centre = self.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H * 0.5);
        let room = match room_containing(rooms, self.pos) {
            Some(room) => room,
            // 中心掉出所有房间：用**最近的**房间的几何做碰撞，
            // 夹取也回最近房间 —— 绝不会被拽去出生房间。
            None => nearest_room(rooms, centre),
        };
        self.physics_in(input, room);
        self.clamp_to_world(rooms);
    }

    /// 物理推进（不含房间/世界夹取，夹取在 [`Player::step`] 尾部统一做）。
    fn physics_in(&mut self, input: &Input, room: &Room) {
        self.prev_pos = self.pos;

        // 水平：朝目标速度做加速 / 减速，而不是瞬间赋值。
        // 地面用全加速度，空中打折（AIR_MULT）—— 这正是"空中不能急转"的来源。
        let target = input.axis_x() * RUN;
        let accel = if self.on_ground {
            RUN_ACCEL
        } else {
            RUN_ACCEL * AIR_MULT
        };
        let reduce = if self.on_ground {
            RUN_REDUCE
        } else {
            RUN_REDUCE * AIR_MULT
        };
        // 同向且没超速 → 加速；反向 / 超速 → 用较小的减速率。
        // 注意静止起步（vel.x == 0）必须走加速分支：signum() 为 0，若不显式
        // 判定就会掉进 reduce，起步反而比刹车还慢。
        let speeding_up = target != Fx::zero()
            && (self.vel.x == Fx::zero() || self.vel.x.signum() == target.signum())
            && self.vel.x.abs() <= target.abs();
        let rate = if speeding_up { accel } else { reduce };
        approach(&mut self.vel.x, target, rate);
        if self.vel.x != Fx::zero() {
            self.facing = self.vel.x.signum();
        }

        // 跳跃输入边沿：按下→缓冲，松开→（若在上升）削减速度。
        let jump_pressed = input.jump && !self.prev_jump;
        let jump_released = !input.jump && self.prev_jump;
        self.prev_jump = input.jump;
        if jump_pressed {
            self.jump_buffer = JUMP_BUFFER_FRAMES;
        }

        // 起跳：在地面或土狼时间内，且缓冲里有跳跃按下。
        // 用起跳前的接地状态（上一 tick 的解算结果），与平台游戏惯例一致。
        if self.jump_buffer > 0 && (self.on_ground || self.coyote > 0) {
            self.vel.y = JUMP;
            self.on_ground = false;
            self.jump_buffer = 0;
            self.coyote = 0;
        }

        // 可变跳高：松开跳跃且正在上升 → 一次性削减上升速度。
        if jump_released && self.vel.y < Fx::zero() {
            self.vel.y *= JUMP_CUT;
        }

        // 落速上限是**缓动状态**：按住"下"且已在加速下落 → 上限爬向
        // 快落；否则回落到普通落速。速率 300 px/s²。
        // 参考 pureCeleste：`maxFall = Approach(maxFall, 240, 300*dt)`。
        if input.down && self.vel.y >= MAX_FALL {
            approach(&mut self.max_fall, FAST_MAX_FALL, FAST_MAX_ACCEL);
        } else {
            approach(&mut self.max_fall, MAX_FALL, FAST_MAX_ACCEL);
        }

        // 重力：只在空中施加。靠近顶点（|vy| < 40 px/s）且按住跳跃 →
        // 重力减半。参考 pureCeleste Player.Update：
        //   float g = (Math.Abs(Speed.Y) < 40f && Input.Jump.Check) ? 0.5f : 1f;
        //   Speed.Y = Calc.Approach(Speed.Y, maxFall, 900f * g * dt);
        if !self.on_ground {
            let half = input.jump && self.vel.y.abs() < HALF_GRAV_THRESHOLD;
            let rate = if half { HALF_GRAVITY } else { GRAVITY };
            approach(&mut self.vel.y, self.max_fall, rate);
        }

        self.slide_x(room);
        self.on_ground = false;
        self.slide_y(room);
        if !self.on_ground {
            self.on_ground = self.hits(room, self.pos.x, self.pos.y + Fx::one());
        }
        // 土狼时间 + 跳跃缓冲的过期：接地就刷新土狼；离地则两者一起递减。
        // 缓冲要到"着陆的下一帧"才可能触发（接地状态是本 tick 尾才更新），
        // 所以接地那一帧不递减，避免把落地的余量吃掉。
        if self.on_ground {
            self.coyote = COYOTE_FRAMES;
        } else {
            if self.coyote > 0 {
                self.coyote -= 1;
            }
            self.jump_buffer = self.jump_buffer.saturating_sub(1);
        }
    }

    /// 当前 hitbox 是否与固体相交。
    fn hits(&self, room: &Room, x: Fx, y: Fx) -> bool {
        let right = x + Fx::from_int(PLAYER_W_PX) - Fx::from_bits(1);
        let bottom = y + Fx::from_int(PLAYER_H_PX) - Fx::from_bits(1);
        for cy in cell(y)..=cell(bottom) {
            for cx in cell(x)..=cell(right) {
                if Session::solid_cell(room, cx, cy) {
                    return true;
                }
            }
        }
        false
    }

    /// 沿 X 轴推进，撞到固体即贴到那列的外沿。
    fn slide_x(&mut self, room: &Room) {
        let mut dx = self.vel.x;
        while dx != Fx::zero() {
            let step = clamp_step(dx);
            let nx = self.pos.x + step;
            if self.hits(room, nx, self.pos.y) {
                self.pos.x = edge_x(room, nx, step);
                self.vel.x = Fx::zero();
                return;
            }
            self.pos.x = nx;
            dx -= step;
        }
    }

    /// 沿 Y 轴推进，撞到固体即贴到那行的外沿并记下落地。
    fn slide_y(&mut self, room: &Room) {
        let mut dy = self.vel.y;
        while dy != Fx::zero() {
            let step = clamp_step(dy);
            let ny = self.pos.y + step;
            if self.hits(room, self.pos.x, ny) {
                self.pos.y = edge_y(room, ny, step);
                if step > Fx::zero() {
                    self.on_ground = true;
                }
                self.vel.y = Fx::zero();
                return;
            }
            self.pos.y = ny;
            dy -= step;
        }
    }

    /// 世界级兜底：只有当中心不在**任何**房间里才夹取。
    ///
    /// 之前无条件把玩家夹进"中心所在房间"的矩形 —— 房间贴边处中心刚
    /// 跨缝，夹取就把玩家拽进隔壁房间（≈4px 瞬移）。现在贴缝行走是
    /// 连续的；只有真正掉出世界（房间之间的缝隙、热重载缩小房间）时，
    /// 玩家才会被**最近的**房间接住，绝不会被拽去出生房间。
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

/// 把一步限制在 `±STEP` 内，同时不超过剩余位移。
fn clamp_step(d: Fx) -> Fx {
    if d > Fx::zero() {
        d.min(STEP)
    } else {
        d.max(-STEP)
    }
}

/// 以每 tick 最多 `rate` 的速度把 `v` 推向 `target`，不会越过目标。
///
/// 加速曲线的核心：`v` 朝 `target` 移动，但一步最多 `rate`，所以
/// 起步是渐进的；反过来 `target` 变为反向时也用它来刹车。
fn approach(v: &mut Fx, target: Fx, rate: Fx) {
    let diff = target - *v;
    if diff.abs() <= rate {
        *v = target;
    } else {
        *v += Fx::from_int(diff.signum()) * rate;
    }
}

/// 房间矩形原点（整数像素）。
fn room_origin(room: &Room) -> (i32, i32) {
    (room.bounds.x.round() as i32, room.bounds.y.round() as i32)
}

/// 撞墙后把 hitbox 贴到刚进入那一列的外沿。
///
/// 直接把坐标减到格边界，而不是停在半像素上 —— 否则 1/2px 的步长
/// 会让角色永远差一点点贴不住地面。
fn edge_x(room: &Room, nx: Fx, step: Fx) -> Fx {
    let (ox, _) = room_origin(room);
    if step > Fx::zero() {
        let right = nx + Fx::from_int(PLAYER_W_PX) - Fx::from_bits(1);
        Fx::from_int(cell(right) * 8 + ox - PLAYER_W_PX)
    } else {
        Fx::from_int(cell(nx) * 8 + ox + 8)
    }
}

/// 撞地 / 撞天花板后把 hitbox 贴到刚进入那一行的外沿。
fn edge_y(room: &Room, ny: Fx, step: Fx) -> Fx {
    let (_, oy) = room_origin(room);
    if step > Fx::zero() {
        let bottom = ny + Fx::from_int(PLAYER_H_PX) - Fx::from_bits(1);
        Fx::from_int(cell(bottom) * 8 + oy - PLAYER_H_PX)
    } else {
        Fx::from_int(cell(ny) * 8 + oy + 8)
    }
}

/// 世界坐标 → 世界格子下标（算术右移 = 向下取整，负数也对）。
fn cell(v: Fx) -> i32 {
    (v.bits() >> 16).div_euclid(8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reles_map::{RectDef, Tileset};

    /// 4×4 格房间，最后一行是地板。
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
        // 地板是第 3 行（y = 24..32），hitbox 8 高 → 顶边正好停在 y = 16。
        assert_eq!(p.pos.y.to_f32(), 16.0, "hitbox must snap onto the floor");
        assert_eq!(p.vel.y, Fx::zero());
    }

    #[test]
    fn player_walks_into_a_wall_and_stops() {
        let mut room = floor_room();
        // 竖一堵墙在第 2 列（x = 16..24）
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
        // 站在第 3 行地板上（top = 24）→ hitbox 顶边是 y = 16。
        let mut p = Player::new(Vec2::from_f32s(8.0, 16.0));
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

    /// 8×8 格房间，地板只铺右半边（列 4..8），左侧是空的深渊。
    ///
    /// 这样往左走就能精确地在「离开支撑」的那一刻做土狼时间实验，
    /// 左侧没有任何固体可以兜住玩家。
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
        }
    }

    /// 8 格宽、16 格高的房间，整行地板在第 15 行（y = 120..128）。
    ///
    /// 高是必要的：满按跳跃能上升约 30px，矮房间的 clamp 会截断高度，
    /// 可变跳高的对比就失真了。
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
        }
    }

    /// 32 格宽、16 格高的房间，整行地板在第 15 行（y = 120..128）。
    ///
    /// 够宽是必要的：跑速 / 刹车测试要跑满加速度收敛而不撞上右墙。
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
        }
    }

    /// 在实心地板上放一个玩家，跑到落稳为止（`x` 必须落在地板列上）。
    fn settled(room: &Room, x: f32) -> Player {
        let mut p = Player::new(Vec2::from_f32s(x, 8.0));
        for _ in 0..60 {
            p.step(&Input::default(), std::slice::from_ref(room));
        }
        assert!(p.on_ground, "fixture: player must settle on the floor");
        p
    }

    /// 从悬崖房间的右半地板一直往左走，返回刚离开支撑的那一帧。
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
        // 已离地，但土狼窗口还开着 —— 这一帧按跳仍应起跳。
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
        // 已经离地 1 帧，再等满整个窗口 —— 之后土狼时间必须归零。
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
        // 从空中落下，在**落地前一帧**按下跳跃并按住 —— 缓冲必须把这次
        // 输入带到地面上，落地瞬间直接起跳（而不是被丢掉）。
        let mut p = Player::new(Vec2::from_f32s(32.0, 90.0));
        let idle = Input::default();
        let buffered = Input {
            jump: true,
            ..Input::default()
        };
        // 地板顶边 = 120，hitbox 高 8 → 落定时 pos.y = 112。
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

        // 同一次起跳：held 全程按住，released 下一帧就松手。
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
    fn releasing_jump_mid_rise_cuts_the_upward_speed() {
        let room = tall_room();
        let mut p = settled(&room, 32.0);
        let jump = Input {
            jump: true,
            ..Input::default()
        };
        p.step(&jump, std::slice::from_ref(&room));
        assert!(p.vel.y < Fx::zero(), "must be rising after the jump");
        // 若不松手，本帧速度只会被重力自然拉小：
        let undamped = p.vel.y + GRAVITY;
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert!(p.vel.y < Fx::zero(), "the short hop must still rise");
        assert!(
            p.vel.y > undamped,
            "releasing mid-rise must cut the upward speed ({} vs undamped {})",
            p.vel.y,
            undamped
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
        // 第一帧不可能已是满速 —— 那正是"瞬间赋值"要禁止的。
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
        // 若干帧后收敛到满速，且不再增长。
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
        // 同一时刻、同一初速（0），唯一差别是接地与否。
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

    /// 8×8 格房间，`floor` 决定是否铺第 7 行地板；`x` 是房间原点。
    ///
    /// 与 `ledge_room` 同款，但原点可调 —— 拼接贴边/留缝的双房间用。
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
        }
    }

    /// 两间贴边房间（A x[0,64]、B x[64,128]，地板同高）：向右走跨缝，
    /// 每 tick 的位移不得超过一个正常步长。
    ///
    /// 回归：旧实现每 tick 无条件把玩家夹进"中心所在房间"的矩形，
    /// 中心刚跨过缝就把玩家拽进隔壁房间 —— 一次 ≈4px 的瞬移。
    /// （tick 数以玩家停在 B 中部为准，不走到 B 的远缘。）
    #[test]
    fn walking_across_a_seam_does_not_teleport() {
        let rooms = [seam_room(0.0, true), seam_room(64.0, true)];
        // 站在 A 的地板上（第 7 行，顶 y=56 → hitbox 顶边 y=48）。
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
        // 而且玩家确实走进了 B（中心已过缝：pos.x > 60）。
        assert!(
            p.pos.x.to_f32() > 60.0,
            "the player should end up past the seam, at {:?}",
            p.pos.x
        );
    }

    /// 两间房之间留一道缝（A x[0,64]、B x[128,192]）：掉出所有房间时
    /// 被**最近的**房间接住，而不是被拽去出生房间（旧实现的回落目标）。
    #[test]
    fn falling_into_a_gap_is_caught_by_the_nearest_room() {
        // 缝上没有房间；B 不参与（A 距离更近）。给 B 留地板但它不该被选中。
        let rooms = [seam_room(0.0, true), seam_room(128.0, true)];
        let mut p = Player::new(Vec2::from_f32s(40.0, 48.0));
        let walk = Input {
            right: true,
            ..Input::default()
        };
        // 走进缝 → 中心离开所有房间 → 兜底把玩家夹回 A 的右缘。
        // 夹回后若继续按方向会"走-夹"振荡，所以松键让玩家停稳再断言。
        for _ in 0..90 {
            p.step(&walk, &rooms);
        }
        for _ in 0..60 {
            p.step(&Input::default(), &rooms);
        }
        assert_eq!(
            p.pos.x.to_f32(),
            56.0,
            "the gap must be guarded by the nearest room's right edge"
        );
        assert!(
            p.on_ground,
            "the guard stands the player on the room's bottom edge"
        );
        // 绝不允许被拽去 B（x ≥ 120）。
        assert!(p.pos.x.to_f32() < 100.0, "must not teleport across the map");
    }

    /// 8×48 格的深竖井：地板远在底部，供落速测试在**不落地**的前提下
    /// 观察收尾速度与缓动（384px，75 tick 内到不了底）。
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
        }
    }

    /// 参考值：普通下落的收尾速度是 160 px/s（8/3 px/tick），不是旧
    /// 实现的 360。
    #[test]
    fn fall_speed_caps_at_vanilla_max_fall() {
        let room = tall_room();
        let mut p = Player::new(Vec2::from_f32s(32.0, 8.0));
        for _ in 0..40 {
            p.step(&Input::default(), std::slice::from_ref(&room));
            if p.vel.y == MAX_FALL {
                return; // 到过收尾速度即通过（落地后 vel.y 归零，属正常）
            }
        }
        panic!(
            "terminal fall speed must be exactly 160 px/s (8/3 px/tick), last = {:?}",
            p.vel.y
        );
    }

    /// 按住"下"：落速上限从 160 缓动到 240 px/s（4 px/tick）；松开后
    /// 再缓动回 160。参考 `maxFall = Approach(maxFall, 240, 300*dt)`。
    #[test]
    fn holding_down_ramps_the_fall_cap_to_fast_fall_and_back() {
        let room = shaft_room();
        let down = Input {
            down: true,
            ..Input::default()
        };
        let mut p = Player::new(Vec2::from_f32s(32.0, 8.0));

        // 按住下 → 必须达到快落上限。
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

        // 对照：不按下时落速永远不超过普通上限。
        let mut q = Player::new(Vec2::from_f32s(32.0, 8.0));
        for _ in 0..45 {
            q.step(&Input::default(), std::slice::from_ref(&room));
            assert!(
                q.vel.y <= MAX_FALL,
                "without down the cap must stay at 160 px/s, got {:?}",
                q.vel.y
            );
        }

        // 松开下 → 上限缓回普通落速，落速跟着回落。
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

    /// 顶点半重力：|vy| < 40 px/s 且按住跳跃时，重力减半 ——
    /// 参考 `(|Speed.Y| < 40f && Jump.Check) ? 0.5f : 1f`。
    #[test]
    fn gravity_is_halved_near_the_apex_while_jump_is_held() {
        let room = tall_room();
        // 两个玩家都放在慢速上升段（|vy| < 2/3 px/tick），唯一差别是
        // 跳跃键。注意两个都**没有**松键边沿，可变跳高的 cut 不参与。
        let mut held = Player::new(Vec2::from_f32s(32.0, 60.0));
        held.on_ground = false;
        held.vel.y = -Fx::from_ratio(3, 5); // -0.6 px/tick，阈值内
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
}
