//! 玩家仿真：8×8 AABB 对 tile grid 的轴分离解算。
//!
//! 所有状态都是 [`Fx`]（AGENTS.md §15.7），时间步恒为
//! [`FIXED_DT`](releste_math::FIXED_DT)。
//!
//! 与原版的关系（`references/pureCeleste.cs`，行为参考）：
//! - **已对齐**：跑动加速（1000/400/0.65）、重力 900、落速上限
//!   160/240 缓动（`maxFall` 状态）、顶点半重力（40 阈值）、
//!   coyote/跳跃缓冲/松键削减（Phase 4.1 已落地）。
//! - **连续世界语义**（原版是切屏转场，这里是同一世界连续行走）：
//!   碰撞查询覆盖 hitbox 触及的**所有**房间（骑缝时两边都查，
//!   头不会扎进隔壁房间的实心里）；撞墙/落地吸附到**世界格边**，
//!   与房间原点无关。中心掉出所有房间时由最近房间兜底夹回。
//! - **仍是初值**：跳跃（原版是 `JumpSpeed = -105` + varJump 钳制 +
//!   `JumpHBoost = 40`，本实现是 -240 单冲量）—— 下一轮对齐；
//!   dash / 爬墙等尚未开始（ROADMAP Phase 4.1）。

use releste_map::Room;
use releste_math::{Fx, Vec2};

use crate::session::{nearest_room, room_containing, Session};

/// hitbox 宽（像素，整数，供房间碰撞使用）。
///
/// 原版：`pureCeleste.cs:118856`
/// `normalHitbox = new Hitbox(8f, 11f, -4f, -11f)` —— 8 宽 11 高，
/// `Position` 是 hitbox **底中心**（脚下锚点）。
pub const PLAYER_W_PX: i32 = 8;
/// hitbox 高（像素，整数）。出处见 [`PLAYER_W_PX`]。
pub const PLAYER_H_PX: i32 = 11;
/// hitbox 宽（f32 视图，供渲染使用）。
pub const PLAYER_W: f32 = PLAYER_W_PX as f32;
/// hitbox 高（f32 视图，供渲染使用）。
pub const PLAYER_H: f32 = PLAYER_H_PX as f32;

/// 水平跑速 90 px/s（1.5 px/tick @ 60Hz）。
const RUN: Fx = Fx::from_ratio(3, 2);
/// 水平加速度 1000 px/s²（≈0.278 px/tick²）—— 起步不是瞬间满速。
const RUN_ACCEL: Fx = Fx::from_ratio(5, 18);
/// 超速回归 400 px/s²：仅当 **同向超速**（|vx| > 90 且与输入同号）时用；
/// 无输入停下、反向刹车都走 1000 的全速档（`pureCeleste.cs:122068-122074`）。
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
/// 起跳竖直速度 -105 px/s（-7/4 px/tick）。`Player.Jump`：
/// `Speed.Y = -105f`（pureCeleste.cs:120714）。
const JUMP_SPEED: Fx = Fx::from_ratio(-7, 4);
/// 跳跃水平助力 40 px/s（2/3 px/tick）：`Speed.X += 40f * moveX`
/// （pureCeleste.cs:120713）。跑动中起跳会短暂超速，再被 400 档拉回。
const JUMP_H_BOOST: Fx = Fx::from_ratio(2, 3);
/// varJump 时长 0.2s = 12 tick（pureCeleste.cs:120715）。
///
/// 按住跳跃期间竖直速度被钳回起跳值（122130-122138）—— 重力被完全
/// 抵消，跳满 ≈ 12×1.75 + 105²/1800 ≈ 27px（3.4 格）；松手计时清零、
/// 只剩重力，即得可变跳高。**没有**"松手瞬间砍速度"——削高全靠钳制。
const VAR_JUMP_TICKS: u32 = 12;
/// 碰撞解算的推进步长：1/2 px。
const STEP: Fx = Fx::from_ratio(1, 2);

/// 起跳输入缓冲：0.08s = 5 tick（含按下当帧）。
///
/// 原版 `VirtualButton` 的 `BufferTime`：Jump = 0.08f
/// （pureCeleste.cs:110376），按下即计满、每帧递减、`Pressed` 在
/// 计数 > 0 时为真（16715-16840）—— 落地前 4 tick 内按的跳跃，
/// 落地瞬间仍会触发。
const JUMP_BUFFER_FRAMES: u32 = 5;
/// 土狼时间：0.1s（`jumpGraceTimer`，pureCeleste.cs:119832-119839 ——
/// 接地每帧刷满 0.1，离地后逐帧递减，> 0 即可起跳）。
///
/// 引擎在 tick 末段递减（物理已用完上一 tick 的接地态），6 次计数
/// 恰好给出 5 个可起跳的离地 tick，与原版逐帧等价。
const COYOTE_FRAMES: u32 = 6;

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
    /// varJump 倒计时（tick）：> 0 且按住跳跃时竖直速度被钳回起跳值
    /// （pureCeleste.cs:122130-122138 的 `varJumpTimer`）。
    var_jump: u32,
    /// 强制 varJump 钳制的剩余 tick（原版 `AutoJump` + `AutoJumpTimer`，
    /// 120590-120594：向上转场时自动跳一下，0.2s 内不管按键都钳住）。
    auto_jump: u32,
    /// 当前冲刺数（原版 `Player.Dashes`，118860 附近构造为 MaxDashes）。
    /// dash 状态机落地前恒为库存值，头发颜色据此取 NORMAL。
    pub dashes: u8,
    /// 冲刺库存上限（原版 `Player.MaxDashes`；Prologue `Inventory.Prologue`
    /// 无二段冲刺 → 1，见 118856 附近的构造与 119421）。
    pub max_dashes: u8,
    /// 头发白闪剩余时间（原版 `hairFlashTimer`，120284）。
    hair_flash: u8,
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
            var_jump: 0,
            auto_jump: 0,
            dashes: 1,
            max_dashes: 1,
            hair_flash: 0,
        }
    }

    /// 推进一个固定 tick。房间身份（标题栏/相机/热重载检查）由
    /// [`Session::room_at`] 决定；物理不选房间 —— 见 [`Player::hit_in`]。
    pub fn update(&mut self, input: &Input, session: &Session) {
        self.step(input, &session.doc.rooms);
    }

    /// 单步仿真：物理（跨房间碰撞）→ 世界级兜底。
    ///
    /// `rooms` 显式传入，单测可以直接构造房间几何，不必起整个 Session。
    pub(crate) fn step(&mut self, input: &Input, rooms: &[Room]) {
        self.physics_in(input, rooms);
        self.clamp_to_world(rooms);
    }

    /// 头发颜色：原版 `Player.UpdateHair`（pureCeleste.cs:120244-120296）
    /// 的非渐变部分 —— 冲刺耗尽取 USED，回满瞬间（上一 tick 数量不同或
    /// 白闪未结束）取 FLASH，否则 `Dashes == 2 ? TWO_DASHES : NORMAL`。
    ///
    /// `used` 渐入（`Lerp(Hair.Color, Used, 6*dt)`，120258-120262）和
    /// `hairFlashTimer = 0.12s`（120284）需要逐帧颜色状态，随 dash 落地
    /// 时把本函数升级为带当前颜色的版本；现在返回离散状态，Prologue
    /// （库存 1 段且满）恒为 NORMAL —— 与原版一致。
    pub fn hair_color(&self) -> [f32; 4] {
        let dashes = self.dashes;
        let max = self.max_dashes;
        if dashes == 0 && dashes < max {
            crate::sprites::hair_color::USED
        } else if self.hair_flash > 0 {
            crate::sprites::hair_color::FLASH
        } else if dashes == 2 {
            crate::sprites::hair_color::TWO_DASHES
        } else {
            crate::sprites::hair_color::NORMAL
        }
    }

    /// 向上转场的前置调整（原版 `BeforeUpTransition`，
    /// pureCeleste.cs:120578-120595）：水平清零、自动向上跳
    /// （-105 + 0.2s 强制 varJump 钳制），落进上方的房间。
    pub fn before_up_transition(&mut self) {
        self.vel.x = Fx::zero();
        self.vel.y = JUMP_SPEED;
        self.var_jump = VAR_JUMP_TICKS;
        self.auto_jump = VAR_JUMP_TICKS;
    }

    /// 向下转场的前置调整（原版 `BeforeDownTransition`，
    /// pureCeleste.cs:120572-120583）：砍掉向上的速度、清 varJump。
    pub fn before_down_transition(&mut self) {
        self.vel.y = self.vel.y.max(Fx::zero());
        self.var_jump = 0;
        self.auto_jump = 0;
    }

    /// 物理推进（不含世界夹取，夹取在 [`Player::step`] 尾部统一做）。
    ///
    /// tick 内顺序对照原版（`pureCeleste.cs`）：varJump 递减（Update
    /// 119864-119867，先于状态机）→ 跑动（122050-122074）→ maxFall
    /// （122088-122106）→ 重力（122123-122146，仅空中）→ varJump 钳制
    /// （122148-122156）→ 跳跃判定（122158-122163）→ 移动解算 →
    /// 土狼/缓冲递减（119830-119840 的节奏）。
    fn physics_in(&mut self, input: &Input, rooms: &[Room]) {
        self.prev_pos = self.pos;

        // varJump 计时：原版在 Update 头部递减（119864-119867）。
        // AutoJump 同理（119868-119877）。
        if self.var_jump > 0 {
            self.var_jump -= 1;
        }
        if self.auto_jump > 0 {
            self.auto_jump -= 1;
        }

        // 水平：原版两分支（122068-122074）—— 同向超速（|vx| > 90 且与
        // 输入同号）用 400 回归；其余（无输入、反向刹车、起步）一律
        // 1000 全速档。空中两档都乘 0.65。
        let move_x = input.axis_x();
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
        // 朝向跟随输入而不是速度（119908-119917）：贴墙按住方向时
        // 速度为 0，朝向必须仍朝墙，起跳助力方向才对。
        if move_x != Fx::zero() {
            self.facing = move_x.signum();
        }

        // 落速上限是**缓动状态**：按住"下"且已在加速下落 → 上限爬向
        // 快落；否则回落到普通落速。速率 300 px/s²。
        // 参考 pureCeleste：`maxFall = Approach(maxFall, 240, 300*dt)`。
        if input.down && self.vel.y >= MAX_FALL {
            approach(&mut self.max_fall, FAST_MAX_FALL, FAST_MAX_ACCEL);
        } else {
            approach(&mut self.max_fall, MAX_FALL, FAST_MAX_ACCEL);
        }

        // 重力：只在空中施加（122123）。靠近顶点（|vy| < 40 px/s）且
        // 按住跳跃（或 AutoJump，向上转场）→ 重力减半（122140-122142）。
        if !self.on_ground {
            let half = (input.jump || self.auto_jump > 0) && self.vel.y.abs() < HALF_GRAV_THRESHOLD;
            let rate = if half { HALF_GRAVITY } else { GRAVITY };
            approach(&mut self.vel.y, self.max_fall, rate);
        }

        // varJump 钳制（122148-122156）：计时未尽且按住跳跃 → 竖直速度
        // 钳回起跳值（重力被抵消）；松开 → 计时清零。这就是"可变跳高"，
        // 没有额外的松手砍速。
        if self.var_jump > 0 {
            if input.jump || self.auto_jump > 0 {
                self.vel.y = self.vel.y.min(JUMP_SPEED);
            } else {
                self.var_jump = 0;
            }
        }

        // 跳跃输入边沿：按下 → 缓冲计满（VirtualButton.BufferTime，
        // 110376；缓冲语义 16824-16840）。
        let jump_pressed = input.jump && !self.prev_jump;
        self.prev_jump = input.jump;
        if jump_pressed {
            self.jump_buffer = JUMP_BUFFER_FRAMES;
        }

        // 起跳（122158-122163 → Jump() 120694-120716）：缓冲窗口内 +
        // 接地/土狼 → 竖直 -105、水平 +40*moveX（JumpHBoost）、
        // varJump 计满 0.2s、土狼与缓冲清零。
        if self.jump_buffer > 0 && (self.on_ground || self.coyote > 0) {
            self.vel.y = JUMP_SPEED;
            self.vel.x += move_x * JUMP_H_BOOST;
            self.var_jump = VAR_JUMP_TICKS;
            self.jump_buffer = 0;
            self.coyote = 0;
            self.on_ground = false;
        }

        self.slide_x(rooms);
        self.on_ground = false;
        self.slide_y(rooms);
        if !self.on_ground {
            self.on_ground = self.hits(rooms, self.pos.x, self.pos.y + Fx::one());
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

    /// hitbox 是否与**任何**触及房间的固体相交；`Some` 返回挡路的房间。
    ///
    /// hitbox 骑在房间接缝上时（≤8px）两边的房间都要查 —— 只查中心
    /// 所在房间的话，头会先扎进隔壁房间的实心瓦片，等中心跨过接缝
    /// 才发现，玩家就卡在墙里了（0-Intro 的 `lvl_0` 顶板开孔正对
    /// `lvl_0b` 的实心地板，就是这么卡人的）。
    ///
    /// 房间按各自原点把世界格换算回局部格；矩形与 hitbox 格范围
    /// 不相交的房间直接跳过（贴边接触不算相交）。
    fn hit_in<'r>(&self, rooms: &'r [Room], x: Fx, y: Fx) -> Option<&'r Room> {
        let right = x + Fx::from_int(PLAYER_W_PX) - Fx::from_bits(1);
        let bottom = y + Fx::from_int(PLAYER_H_PX) - Fx::from_bits(1);
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
                continue; // 这个房间根本不在 hitbox 的格范围里
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

    /// 当前 hitbox 是否与固体相交（[`Player::hit_in`] 的布尔视图）。
    pub(crate) fn hits(&self, rooms: &[Room], x: Fx, y: Fx) -> bool {
        self.hit_in(rooms, x, y).is_some()
    }

    /// 沿 X 轴推进，撞到固体即贴到那列的外沿。
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

    /// 沿 Y 轴推进，撞到固体即贴到那行的外沿并记下落地。
    fn slide_y(&mut self, rooms: &[Room]) {
        let mut dy = self.vel.y;
        while dy != Fx::zero() {
            let step = clamp_step(dy);
            let ny = self.pos.y + step;
            if self.hits(rooms, self.pos.x, ny) {
                self.pos.y = edge_y(ny, step);
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

/// 撞墙后把 hitbox 贴到刚进入那一列的外沿。
///
/// `cell()` 给出的是**世界**格下标，所以吸附边也是世界坐标 ——
/// 不掺任何房间原点。旧实现把世界格下标当房间局部下标用、再补一次
/// 原点：在 `lvl_-1`（ox=-368）碰墙会被向西弹 368px，在
/// `lvl_2`/`lvl_3`（oy=-88）落地会被向上弹 88px —— 任何原点非零
/// 的房间一碰墙就是一次大瞬移。
///
/// 直接把坐标减到格边界，而不是停在半像素上 —— 否则 1/2px 的步长
/// 会让角色永远差一点点贴不住地面。
fn edge_x(nx: Fx, step: Fx) -> Fx {
    if step > Fx::zero() {
        let right = nx + Fx::from_int(PLAYER_W_PX) - Fx::from_bits(1);
        Fx::from_int(cell(right) * 8 - PLAYER_W_PX)
    } else {
        Fx::from_int(cell(nx) * 8 + 8)
    }
}

/// 撞地 / 撞天花板后把 hitbox 贴到刚进入那一行的外沿（世界格边）。
fn edge_y(ny: Fx, step: Fx) -> Fx {
    if step > Fx::zero() {
        let bottom = ny + Fx::from_int(PLAYER_H_PX) - Fx::from_bits(1);
        Fx::from_int(cell(bottom) * 8 - PLAYER_H_PX)
    } else {
        Fx::from_int(cell(ny) * 8 + 8)
    }
}

/// 世界坐标 → 世界格子下标（算术右移 = 向下取整，负数也对）。
fn cell(v: Fx) -> i32 {
    (v.bits() >> 16).div_euclid(8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use releste_map::{RectDef, Tileset};

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
        // 地板是第 3 行（y = 24..32），hitbox 11 高（118856）→ 顶边停在 y = 13。
        assert_eq!(p.pos.y.to_f32(), 13.0, "hitbox must snap onto the floor");
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
        // 站在第 3 行地板上（top = 24）→ hitbox 顶边是 y = 13（11 高，118856）。
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
            ..Default::default()
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
            ..Default::default()
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
            ..Default::default()
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
    fn releasing_jump_mid_rise_resumes_gravity_without_a_cut() {
        // 原版松手只停掉 varJump 钳制（122153-122155），**没有**额外的
        // 砍速：重力从当前速度继续（122123）。跳起后第二 tick 的速度
        // 必须正好是 起跳速 + 一帧重力，且钳制不再把它拉回去。
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
            ..Default::default()
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
        // 直接把玩家放进缝里（中心 74 在所有房间之外）：一 tick 内必须
        // 被最近的房间 A 夹回右缘（64 − 8 = 56）—— 这就是兜底。
        let mut p = Player::new(Vec2::from_f32s(70.0, 48.0));
        p.step(&Input::default(), &rooms);
        assert_eq!(
            p.pos.x.to_f32(),
            56.0,
            "the gap must be guarded by the nearest room's right edge"
        );
        // 落到 A 的地板上（夹回只定 x，落地靠后续重力）。
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
        // 绝不允许被拽去 B（x ≥ 120）。
        assert!(p.pos.x.to_f32() < 100.0, "must not teleport across the map");
    }

    /// 原点非 (0,0) 的房间：撞墙 / 落地 / 顶头都必须吸附到**世界**格边。
    ///
    /// 回归：`edge_x`/`edge_y` 曾把世界格下标当房间局部下标用、再补一次
    /// 房间原点 —— ox=64 的房间碰墙被向东弹 64px，oy=-88 的房间顶头
    /// 被向上弹 88px。0-Intro 里除 `lvl_0` 外全是原点非零的房间，
    /// 旧代码一碰墙就是一次大瞬移。
    fn offset_room() -> Room {
        let mut tiles = Tileset::new("Test", 8, 8);
        for x in 0..8 {
            tiles.set(x, 7, 1); // 地板：世界 y[-32,-24)
        }
        for y in 0..7 {
            tiles.set(4, y, 1); // 竖墙：世界 x[96,104)
        }
        for x in 0..4 {
            tiles.set(x, 2, 1); // 天花板：世界 x[64,96)、y[-72,-64)
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

        // 落地：地板顶边 y = -32 → hitbox 顶边停在 -40。
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

        // 撞墙：墙的左沿 x = 96 → hitbox 停在 88（旧实现弹到 152）。
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

        // 顶头：天花板底边 y = -64 → 头停在 -64（旧实现弹到 -152）。
        // 站在地板上：地板顶 -32，hitbox 11 高 → 顶边 -43。
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

    /// 上间底行实心、下间顶行开孔的贴边双房间：起跳顶头必须停在
    /// 接缝（y=0），头不许扎进上间的实心里，更不许被卡进墙里。
    ///
    /// 0-Intro 实测：`lvl_0` 顶板在 x[288,400) 开着，正上方是
    /// `lvl_0b` 的实心地板 —— 旧实现只查中心所在房间的瓦片，头先扎
    /// 进去，等中心跨缝才发现，玩家被卡死/瞬移。
    #[test]
    fn a_solid_ceiling_in_the_room_above_stops_the_head_at_the_seam() {
        let lower = seam_room(0.0, true);
        let mut upper = seam_room(0.0, true);
        upper.bounds.y = -64.0;
        upper.name = "up".into();
        let rooms = [lower, upper];

        // 头在接缝下方 8px、正以 4 px/tick 上升：一两 tick 内跨过 y=0。
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
            ..Default::default()
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
