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
//! - **跳跃已对齐**：`Jump()` 的 -105 + `JumpHBoost`（120713-120714）+
//!   varJump 钳制（122148-122156）+ 缓冲 0.08s（110376）/ 土狼 0.1s
//!   （119832-119839），见各常量的出处注释。
//! - **引擎自造（无原版对应，偏差声明）**：多房间连续世界的碰撞
//!   （`Player::hit_in` 的骑缝双查）、世界级兜底夹取
//!   （`Player::clamp_to_world`）与固定 1/2px 推进步长（`STEP`）。
//!   原版是**一张地图一份**瓦片网格（77231-77233）+ 单房间边界，
//!   碰撞天然全图；引擎按房间存储瓦片，只能显式骑缝查。各函数
//!   文档都注明了这一点。
//! - **尚未实现**：dash（State 2，119398-119409 的状态机全家）、
//!   爬墙/体力（State 1）、游泳（State 3）、拾取（State 8）等；
//!   ROADMAP Phase 4.1。头发颜色的 USED/FLASH/TWO_DASHES 状态已在
//!   `sprites::hair_color` 备好，随 dash 落地激活。

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
/// 冲刺速度 240 px/s（4 px/tick）。`DashCoroutine`：`speed = value * 240f`
/// （pureCeleste.cs:122768）。
const DASH_SPEED: Fx = Fx::from_int(4);
/// 冲刺末段速度 160 px/s（8/3 px/tick）：0.15s 后 `Speed = DashDir * 160f`
/// （122822-122822，`DashDir.Y <= 0` 时）。
const DASH_END_SPEED: Fx = Fx::from_ratio(8, 3);
/// 冲刺时长 0.15s = 9 tick（`yield return 0.15f`，122812）。
const DASH_TICKS: u16 = 9;
/// 冲刺冷却 0.2s = 12 tick（`DashBegin`：`dashCooldownTimer = 0.2f`，122541）。
const DASH_COOLDOWN_TICKS: u16 = 12;
/// 地面回充后不可立刻再冲的冷却 0.1s = 6 tick（122545；原版只写不读，
/// 一并移植保持字段齐全）。
const DASH_REFILL_COOLDOWN_TICKS: u16 = 6;
/// 冲刺攻击窗 0.3s = 18 tick（`dashAttackTimer = 0.3f`，122554）。
const DASH_ATTACK_TICKS: u16 = 18;
/// 蹬墙跳水平速度 130 px/s（13/6 px/tick，`WallJump`：`Speed.X = 130f*dir`，
/// pureCeleste.cs:120840）。
const WALLJUMP_X: Fx = Fx::from_ratio(13, 6);
/// 蹬墙跳探测距离 5px：`CollideCheck<Solid>(Position + UnitX*dir*5)`
/// （`WallJumpCheck`，120779-120804 的 `num = 5` 普通分支）。
const WALLJUMP_PROBE: Fx = Fx::from_int(5);
/// 蹬墙跳后强制 moveX 的时长 0.16s = 10 tick（120832
/// `forceMoveXTimer = 0.16f`；按住方向的防再吸附窗）。
const FORCE_MOVE_X_TICKS: u16 = 10;
/// 蹬墙跳（超级版）水平速度 240 px/s（4 px/tick，`SuperWallJump`：
/// `Speed.X = 240f*dir`，120947 附近）。
const SUPER_WALLJUMP_X: Fx = Fx::from_int(4);
/// 蹬墙跳（超级版）竖直速度 -140 px/s（-7/3 px/tick，`varJumpSpeed =
/// (Speed.Y = -140f)`，120948）。
const SUPER_WALLJUMP_Y: Fx = Fx::from_ratio(-7, 3);
/// 超级跳（地面横冲刺中起跳）水平速度 260 px/s（13/3 px/tick，
/// `SuperJump`：`Speed.X = 260f*Facing`，120748）。
const SUPER_JUMP_X: Fx = Fx::from_ratio(13, 3);
/// 头发白闪时长 0.12s = 7 tick（`UpdateHair`：`hairFlashTimer = 0.12f`，
/// pureCeleste.cs:120284）。
const HAIR_FLASH_TICKS: u8 = 7;
/// 头发颜色渐入速率 6/s（`Lerp(Hair.Color, Used, 6f*dt)`，120258-120262；
/// 每 tick lerp 系数 = 6/60 = 0.1）。
const HAIR_LERP_PER_TICK: f32 = 0.1;

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
    /// 冲刺键（原版 `Input.Dash`，默认 X，101632）。缓冲与跳跃相同
    /// （`VirtualButton.BufferTime = 0.08f`，110376）。
    pub dash: bool,
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
    pub dashes: u8,
    /// 冲刺库存上限（原版 `Player.MaxDashes`；Prologue `Inventory.Prologue`
    /// 无二段冲刺 → 1，119421）。
    pub max_dashes: u8,
    /// 上一 tick 的冲刺数（原版 `lastDashes`，用于头发白闪判定，120295）。
    last_dashes: u8,
    /// 头发白闪剩余时间（原版 `hairFlashTimer`，120284）。
    hair_flash: u8,
    /// 头发颜色的**当前**值（`UpdateHair` 逐帧 lerp 的载体，120258-120262）。
    hair_color_current: [f32; 4],
    /// 状态机（原版 `StateMachine.State`，编号与原版一致：0 = Normal、
    /// 2 = Dash，119397-119409；1/3/8 等未实现）。
    state: u8,
    /// 冲刺方向（原版 `DashDir`，八向单位向量，分量 ±1/0/±0.707）。
    dash_dir: Vec2,
    /// 冲刺协程相位：0 = 进入 tick（`yield return null`，速度未设），
    /// 1..=9 = 0.15s 等待，第 10 tick 结束（`DashCoroutine` 122731-122831）。
    dash_phase: u16,
    /// 起跳/冲刺瞬间的速度存档（原版 `beforeDashSpeed`，122559）。
    before_dash_speed: Vec2,
    /// 冲刺冷却（原版 `dashCooldownTimer`，0.2s，122541）。
    dash_cooldown: u16,
    /// 冲刺回充冷却（原版 `dashRefillCooldownTimer`，0.1s，122545）。
    dash_refill_cooldown: u16,
    /// 冲刺攻击窗（原版 `dashAttackTimer`，0.3s，122554）。
    dash_attack: u16,
    /// varJump 钳制的目标速度（原版 `varJumpSpeed` 字段：`Jump()` 设
    /// -105（120715），`WallJump` -105（120842），`BeforeUpTransition`
    /// -105（120588），`Rebound` -120；钳制取 `min(Speed.Y, varJumpSpeed)`，
    /// 122134）。
    var_jump_speed: Fx,
    /// 强制水平输入（原版 `forceMoveX` + `forceMoveXTimer`，119883-119889：
    /// 蹬墙跳后 0.16s 内 moveX 被覆盖，防止立刻再贴墙）。
    force_move_x: i32,
    force_move_x_ticks: u16,
    /// 本 tick 生效的水平输入（`moveX`：输入或 forceMoveX 覆盖，119883-119891）。
    move_x: Fx,
    /// 上一 tick 的冲刺键（缓冲边沿）。
    prev_dash: bool,
    /// 冲刺键缓冲（`Input.Dash` 的 VirtualButton.BufferTime = 0.08s，110376）。
    dash_buffer: u16,
    /// 冲刺起手的停帧请求（`Celeste.Freeze(0.05f)`，122540；service 消费）。
    pub freeze_ticks: u8,
    /// 动画状态（原版 `Sprite`：当前动画 id、帧、计时器、是否播放中）。
    pub anim_id: String,
    pub anim_frame: usize,
    anim_timer: f32,
    anim_playing: bool,
    /// 快速跳跃/下落标志（`fastJump`，120437/120469）。
    fast_jump: bool,
}

/// `Math.Sign(before.X) == Math.Sign(speed.X)`（122770）的 Fx 版。
fn vel_x_same_dir(before: &Vec2, other_x: Fx) -> bool {
    before.x != Fx::zero() && other_x != Fx::zero() && before.x.signum() == other_x.signum()
}

/// 逐通道线性插值（`Color.Lerp(a, b, amount)`；amount 为本 tick 系数）。
fn lerp_color(current: &mut [f32; 4], target: [f32; 4], t: f32) {
    for (c, t_target) in current.iter_mut().zip(target.iter()) {
        *c += (t_target - *c) * t;
    }
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
            fast_jump: false,
        }
    }

    /// 推进一个固定 tick。房间身份（标题栏/相机/热重载检查）由
    /// [`Session::room_at`] 决定；物理不选房间 —— 见 [`Player::hit_in`]。
    pub fn update(&mut self, input: &Input, session: &Session) {
        self.step(input, &session.doc.rooms);
    }

    /// 单步仿真：公共计时头 → 状态机（Normal/Dash）→ 世界级兜底。
    ///
    /// 对照原版 `Player.Update`（pureCeleste.cs:119700-120168）的结构：
    /// 计时/回充/朝向（119827-119921）→ `base.Update()` 进状态机
    /// （119397-119409 注册：0 = NormalUpdate、2 = DashUpdate）→
    /// MoveH/MoveV（120049-120056）→ EnforceBounds 由 service 负责。
    ///
    /// `rooms` 显式传入，单测可以直接构造房间几何，不必起整个 Session。
    pub(crate) fn step(&mut self, input: &Input, rooms: &[Room]) {
        self.update_head(input);
        match self.state {
            2 => self.dash_update(input, rooms),
            _ => self.normal_update(input, rooms),
        }
        self.clamp_to_world(rooms);
        self.update_hair_color();
    }

    /// 动画推进 + 状态映射（原版 `UpdateSprite` 在移动后调用，120068；
    /// `Sprite.Update` 经 `base.Update` 每帧跑）。
    pub fn update_visuals(
        &mut self,
        table: &crate::sprites::AnimationTable,
        input: &Input,
        rooms: &[Room],
    ) {
        // 从未播过动画 → 起始动画（sprite 元素的 `start`，10189 校验）。
        if self.anim_id.is_empty() {
            if let Some(def) = table.sprites.get("player") {
                let start = def.start.clone();
                self.play(table, &start);
            }
        }
        self.update_animation(table);
        self.update_sprite(table, input, rooms);
    }

    /// 原版 `Player.Update` 头部的公共计时与回充（119827-119921）。
    fn update_head(&mut self, input: &Input) {
        // dashAttackTimer -= dt（119827-119830）。
        self.dash_attack = self.dash_attack.saturating_sub(1);
        // 地面回充（119855-119863）：state 3（游泳）分支不适用；站在
        // 固体上（`on_ground` 即脚下有实心，与 `CollideCheck<Solid>(Position
        // + UnitY)` 等价）→ RefillDash（121082-121090：只有未满时才写）。
        if self.on_ground && self.dashes < self.max_dashes {
            self.dashes = self.max_dashes;
        }
        // jumpGraceTimer（119830-119840）—— 已由 physics 末尾的
        // coyote 计数承担（语义等价，见 COYOTE_FRAMES 文档）。
        // varJumpTimer / AutoJumpTimer 递减（119864-119877）。
        if self.var_jump > 0 {
            self.var_jump -= 1;
        }
        if self.auto_jump > 0 {
            self.auto_jump -= 1;
        }
        // dashCooldownTimer / dashRefillCooldownTimer（119843-119848）。
        self.dash_cooldown = self.dash_cooldown.saturating_sub(1);
        self.dash_refill_cooldown = self.dash_refill_cooldown.saturating_sub(1);
        // moveX：forceMoveX 覆盖窗（119883-119891），否则读输入。
        let move_x = if self.force_move_x_ticks > 0 {
            self.force_move_x_ticks -= 1;
            Fx::from_int(self.force_move_x)
        } else {
            input.axis_x()
        };
        self.move_x = move_x;
        // 朝向跟随输入（119908-119917）。
        if move_x != Fx::zero() {
            self.facing = move_x.signum();
        }
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
        // 逐帧 lerp 的当前值（`UpdateHair` 写进 `Hair.Color`，渲染直读）。
        self.hair_color_current
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

    /// 原版 `Jump()`（pureCeleste.cs:120694-120716）：竖直 -105、
    /// 水平 +40*moveX（JumpHBoost）、varJump 计满 0.2s、varJumpSpeed
    /// 存档、土狼与缓冲清零。（`LiftBoost` 静态地面为 0，省略。）
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

    /// 原版 `WallJumpCheck(dir)`（pureCeleste.cs:120779-120804）的普通
    /// 分支（无尖刺/星羽修正，`num = 5`）：朝墙 5px 探测实心。
    /// `ClimbBoundsCheck` / `ClimbBlocker.EdgeCheck`（120779 前置）在
    /// 本引擎没有爬墙边界实体，恒 false/true（偏差声明：见模块文档）。
    fn wall_jump_check(&self, rooms: &[Room], dir: i32) -> bool {
        let dx = Fx::from_int(dir) * WALLJUMP_PROBE;
        self.hits(rooms, self.pos.x + dx, self.pos.y)
    }

    /// 原版 `WallJump(dir)`（pureCeleste.cs:120806-120860）：水平
    /// 130*dir、竖直 -105、varJump 计满、`forceMoveX = dir` 0.16s
    /// （120832-120838）、消耗跳跃缓冲、土狼清零。
    fn wall_jump(&mut self, rooms: &[Room], dir: i32) {
        self.jump_buffer = 0;
        self.coyote = 0;
        self.var_jump = VAR_JUMP_TICKS;
        self.auto_jump = 0;
        self.dash_attack = 0;
        if self.move_x != Fx::zero() {
            // 120828-120838：按住方向时强制 moveX 防止立刻再贴墙。
            self.force_move_x = dir;
            self.force_move_x_ticks = FORCE_MOVE_X_TICKS;
        }
        self.vel.x = WALLJUMP_X * Fx::from_int(dir);
        self.vel.y = JUMP_SPEED;
        self.var_jump_speed = JUMP_SPEED;
        self.on_ground = false;
        let _ = rooms;
    }

    /// 原版 `StartDash()`（pureCeleste.cs:122469-122479）+ `DashBegin`
    /// （122534-122565）：扣一次冲刺、进入 State 2、记 `beforeDashSpeed`
    /// 后清零速度（当 tick 不动）、起冷却与攻击窗。
    fn start_dash(&mut self) {
        self.dashes = self.dashes.saturating_sub(1);
        self.dash_buffer = 0; // Input.Dash.ConsumeBuffer()（122474）
        self.state = 2;
        self.dash_phase = 0;
        self.dash_cooldown = DASH_COOLDOWN_TICKS;
        self.dash_refill_cooldown = DASH_REFILL_COOLDOWN_TICKS;
        self.dash_attack = DASH_ATTACK_TICKS;
        self.before_dash_speed = self.vel;
        self.vel = Vec2::ZERO;
        self.dash_dir = Vec2::ZERO;
        // 注意：原版 DashBegin **不改** onGround —— 地面冲刺保持接地，
        // 下一 tick 的头部回充（119855-119863）照常工作。
        // DashBegin 的 `Celeste.Freeze(0.05f)`（122540）：0.05s = 3 tick
        // 全局停帧，由 service 消费（引擎的帧节奏里等价于跳过 3 个模拟 tick）。
        self.freeze_ticks = 3;
    }

    /// 原版 `GetAimVector(Facing)`（Celeste.Input，pureCeleste.cs:110554-110600，
    /// 无辅助模式分支）：方向键合成八向；角度在 ±π/8 内吸附到正交轴；
    /// 无输入 → 朝向方向。
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
        // 角度吸附：原版对 0/π/±π/2 各留 π/8（正交轴下方）或
        // π/8 − 0.05（斜向优先），等价实现：|x| 与 |y| 比 ≥ tan(π/8)
        // 时取正交轴，否则取对角线。
        let ax = mx.abs();
        let ay = my.abs();
        if ax >= 2.414 * ay {
            Vec2::from_f32s(mx.signum(), 0.0)
        } else if ay >= 2.414 * ax {
            Vec2::from_f32s(0.0, my.signum())
        } else {
            // 对角线：`Math.Sign(value).SafeNormalize()`（110596）。
            let inv = std::f32::consts::FRAC_1_SQRT_2;
            Vec2::from_f32s(mx.signum() * inv, my.signum() * inv)
        }
    }

    /// State 2：`DashUpdate`（pureCeleste.cs:122598-122727）+ `DashCoroutine`
    /// （122731-122831）的显式相位版。
    ///
    /// 一个 tick 的顺序与原版 `StateMachine.Update`（12185-12201）一致：
    /// 先 `DashUpdate`（跳跃分支，122627-122676），再推进协程，最后统一
    /// MoveH/MoveV（120049-120056）—— 墙跳/超级跳改写速度后当帧就按新
    /// 速度移动。相位 0 执行协程首段（`yield return null` 之后的
    /// 122756-122806：速度 = 瞄准 × 240、DashDir、朝向），相位 1..=9
    /// 等待 0.15s，第 10 tick 执行末段（122812-122830）。冲刺期间无
    /// 重力/跑动（`DashUpdate` 不含它们）。
    fn dash_update(&mut self, input: &Input, rooms: &[Room]) {
        self.prev_pos = self.pos;

        // ---- DashUpdate（122598-122727）----
        // 冲刺中的跳跃分支（122627-122676）。横向冲刺（|DashDir.Y| < 0.1）
        // 且在地面 → SuperJump（120740）；竖向冲刺（|DashDir.X| <= 0.2）
        // → SuperWallJump（120865）；其余 → WallJump（ClimbJump 需要
        // 抓攀，未实现，跳过 —— 偏差声明）。
        let jump_pressed = input.jump && !self.prev_jump;
        self.prev_jump = input.jump;
        if jump_pressed {
            self.jump_buffer = JUMP_BUFFER_FRAMES;
        }
        if self.dash_dir != Vec2::ZERO && self.jump_buffer > 0 {
            let horizontal_dash = self.dash_dir.y.abs() < Fx::from_ratio(1, 10);
            let vertical_dash = self.dash_dir.x.abs() <= Fx::from_ratio(1, 5);
            if horizontal_dash && self.on_ground {
                // 122632-122641 → `SuperJump`（120740-120778）。
                self.super_jump();
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

        // ---- DashCoroutine（122731-122831）----
        if self.state == 2 {
            if self.dash_phase == 0 {
                // 协程首段（122756-122806）。
                let aim = self.aim_dir(input);
                let mut speed = aim * DASH_SPEED;
                // 水平超速保留（122770-122774）：起冲前的横向速度更快且
                // 同向时保留原值（`beforeDashSpeed` 的墙速语义）。
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
                self.dash_phase = 1;
            } else {
                self.dash_phase += 1;
                if self.dash_phase > DASH_TICKS {
                    // 末段（122812-122830）。
                    self.auto_jump = 0; // AutoJumpTimer = 0f（122820）
                    if self.dash_dir.y <= Fx::zero() {
                        self.vel = self.dash_dir * DASH_END_SPEED;
                        if self.vel.y < Fx::zero() {
                            // 122826-122829：`if (Speed.Y < 0f) Speed.Y *= 0.75f`。
                            self.vel.y *= Fx::from_ratio(3, 4);
                        }
                    }
                    self.state = 0;
                }
            }
        }

        // ---- MoveH / MoveV（120049-120056）----
        self.slide_x(rooms);
        self.on_ground = false;
        self.slide_y(rooms);
        if !self.on_ground {
            self.on_ground = self.hits(rooms, self.pos.x, self.pos.y + Fx::one());
        }
    }

    /// 原版 `SuperJump()`（pureCeleste.cs:120740-120778，无滑铲/滑翔
    /// 分支）：水平 260×Facing、竖直 -105、varJump 计满，回 State 0。
    fn super_jump(&mut self) {
        self.jump_buffer = 0;
        self.coyote = 0;
        self.var_jump = VAR_JUMP_TICKS;
        self.auto_jump = 0;
        self.dash_attack = 0;
        self.vel.x = SUPER_JUMP_X * Fx::from_int(self.facing);
        self.vel.y = JUMP_SPEED;
        self.var_jump_speed = JUMP_SPEED;
        self.on_ground = false;
        self.state = 0;
    }

    /// 原版 `SuperWallJump(dir)`（pureCeleste.cs:120865-120960 的核心段）：
    /// 水平 240*dir、竖直 -140（varJumpSpeed 同步）、AutoJump 钳制、
    /// `forceMoveX` 0.3s（18 tick，120902）。
    fn super_wall_jump(&mut self, dir: i32) {
        self.jump_buffer = 0;
        self.coyote = 0;
        self.var_jump = VAR_JUMP_TICKS;
        self.auto_jump = VAR_JUMP_TICKS;
        self.dash_attack = 0;
        self.force_move_x = dir;
        self.force_move_x_ticks = 18;
        self.vel.x = SUPER_WALLJUMP_X * Fx::from_int(dir);
        self.vel.y = SUPER_WALLJUMP_Y;
        self.var_jump_speed = SUPER_WALLJUMP_Y;
        self.on_ground = false;
        self.state = 0;
    }

    /// 原版 `UpdateHair`（pureCeleste.cs:120244-120296）：冲刺耗尽 → 以
    /// 6/s 渐入 USED；回满瞬间（lastDashes ≠ Dashes）→ FLASH +
    /// `hairFlashTimer` 0.12s；期间保持 FLASH；否则按段数取
    /// TWO_DASHES/NORMAL。State 19（星羽）与 `OverrideHairColor` 未实现。
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

    /// 原版 `Sprite.Play(id)`：同 id 且在播 → 不重启；表缺失/未知 id 是
    /// 无操作（渲染回落静态帧）。
    pub fn play(&mut self, table: &crate::sprites::AnimationTable, id: &str) {
        if self.anim_id == id && self.anim_playing {
            return;
        }
        let Some(def) = table.sprites.get("player") else {
            return;
        };
        let Some(anim) = def.anims.get(id) else {
            return;
        };
        self.anim_id = id.to_string();
        self.anim_frame = 0;
        self.anim_timer = 0.0;
        self.anim_playing = anim.delay > 0.0 && anim.frames.len() > 1;
    }

    /// 原版 `Sprite.Update`（10170-10308）：计时按帧间隔推进，循环回 0，
    /// 非循环停在末帧并停止播放。
    pub fn update_animation(&mut self, table: &crate::sprites::AnimationTable) {
        let Some(def) = table.sprites.get("player") else {
            return;
        };
        let Some(anim) = def.anims.get(&self.anim_id) else {
            return;
        };
        if !self.anim_playing || anim.delay <= 0.0 {
            return;
        }
        self.anim_timer += 1.0 / 60.0;
        if self.anim_timer < anim.delay {
            return;
        }
        self.anim_timer -= anim.delay;
        self.anim_frame += 1;
        if self.anim_frame >= anim.frames.len() {
            if anim.looped {
                self.anim_frame = 0;
            } else {
                self.anim_frame = anim.frames.len() - 1;
                self.anim_playing = false;
                self.anim_id.clear();
            }
        }
    }

    /// 当前帧的图集纹理名（缺表/缺帧 → `None`，渲染回落静态帧）。
    pub fn anim_frame_texture(&self, table: &crate::sprites::AnimationTable) -> Option<String> {
        let def = table.sprites.get("player")?;
        let anim = def.anims.get(&self.anim_id)?;
        anim.frames.get(self.anim_frame).cloned()
    }

    /// `Sprite.Running`：当前动画是否循环中。
    fn anim_is_looping(&self, table: &crate::sprites::AnimationTable) -> bool {
        table
            .sprites
            .get("player")
            .and_then(|d| d.anims.get(&self.anim_id))
            .is_some_and(|a| a.looped)
            && self.anim_playing
    }

    /// 原版 `Player.UpdateSprite`（pureCeleste.cs:120298-120481）的可及
    /// 分支（无持有物/风/攀爬/梦境/太空/下蹲 —— 偏差声明见模块文档）：
    /// 冲刺攻击窗 → skid/dash；地面 → push/edge/edgeBack/lookUp/idle/
    /// skid/flip/runSlow/runFast；空中 → jumpSlow/jumpFast/fallSlow/
    /// fallFast。
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
        } else if self.state == 0 {
            if self.on_ground {
                self.fast_jump = false;
                let run = self.vel.x.abs() > Fx::from_ratio(3, 4);
                if self.move_x != Fx::zero()
                    && self.hits(rooms, self.pos.x + self.move_x, self.pos.y)
                {
                    self.play(table, "push");
                } else if !run && self.move_x == Fx::zero() {
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
                        self.play(table, "edge");
                    } else if !back {
                        self.play(table, "edgeBack");
                    } else if input.up {
                        self.play(table, "lookUp");
                    } else {
                        self.play(table, "idle");
                    }
                } else if self.move_x != Fx::zero() && self.vel.x.signum() == -self.move_x.signum()
                {
                    if self.vel.x.abs() > Fx::from_ratio(3, 2) {
                        self.play(table, "skid");
                    } else {
                        self.play(table, "flip");
                    }
                } else if !self.anim_is_looping(table) {
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
                self.play(table, "fallFast");
            } else {
                self.play(table, "fallSlow");
            }
        }
    }

    /// State 0（NormalUpdate，pureCeleste.cs:121824-122101）。
    ///
    /// tick 内顺序对照原版：抓取/下蹲（未实现，见偏差声明）→ 冲刺触发
    /// （121841-121843 `if (CanDash)`）→ 跑动（122050-122074）→ maxFall
    /// （122088-122106）→ 重力（122123-122146，仅空中）→ varJump 钳制
    /// （122148-122156）→ 跳跃判定（122158-122163，含蹬墙跳）→ 移动
    /// 解算 → 土狼/缓冲递减。
    fn normal_update(&mut self, input: &Input, rooms: &[Room]) {
        self.prev_pos = self.pos;

        // 冲刺触发（121841-121843 + `CanDash` 119325-119340）：冲刺键
        // 按下（缓冲窗内）且冷却结束且还有冲刺次数。
        let dash_pressed = input.dash && !self.prev_dash;
        self.prev_dash = input.dash;
        if dash_pressed {
            self.dash_buffer = JUMP_BUFFER_FRAMES as u16; // VirtualButton.BufferTime 同 0.08s（110376）
        }
        if self.dash_buffer > 0 && self.dash_cooldown == 0 && self.dashes > 0 {
            self.start_dash(); // StartDash（122469-122479）→ State 2
            return; // 本 tick 速度为 0（DashBegin 清零），直接进入移动
        }

        // 水平：原版两分支（122068-122074）—— 同向超速（|vx| > 90 且与
        // 输入同号）用 400 回归；其余（无输入、反向刹车、起步）一律
        // 1000 全速档。空中两档都乘 0.65。
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
                // 122134：`Speed.Y = Math.Min(Speed.Y, varJumpSpeed)`。
                self.vel.y = self.vel.y.min(self.var_jump_speed);
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

        // 跳跃判定（122090-122101）：缓冲窗内 → 接地/土狼 → `Jump()`
        // （120694-120716：竖直 -105、水平 +40*moveX（JumpHBoost）、
        // varJump 计满 0.2s）；空中 → 蹬墙跳（122102-122130，
        // `WallJumpCheck` 120779-120804 → `WallJump` 120806-120860）。
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

    /// 冲刺：240 px/s（4 px/tick）跑 0.15s（9 tick），末段 160 px/s
    /// （8/3 px/tick）；次数扣 1；落地回充；冷却 0.2s。
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
        // tick 1：StartDash + DashBegin —— 速度清零、扣次数、请求停帧。
        p.step(&dash, std::slice::from_ref(&room));
        assert_eq!(p.state, 2, "dash enters vanilla state 2");
        assert_eq!(p.vel, Vec2::ZERO, "DashBegin zeroes Speed (122560)");
        assert_eq!(p.dashes, 0, "StartDash consumes one dash (122471)");
        assert_eq!(
            p.freeze_ticks, 3,
            "Celeste.Freeze(0.05f) = 3 ticks (122540)"
        );
        // tick 2：协程首段 → 速度 = 4 px/tick。
        p.step(&dash, std::slice::from_ref(&room));
        assert_eq!(p.vel.x, DASH_SPEED, "Speed = aim * 240 px/s (122768)");
        assert_eq!(p.dash_dir.x, Fx::one(), "DashDir snaps to the aim axis");
        let after_first = p.pos.x;
        // tick 3..10：匀速冲刺。
        for _ in 0..8 {
            p.step(&dash, std::slice::from_ref(&room));
            assert_eq!(p.vel.x, DASH_SPEED);
        }
        assert_eq!(
            (p.pos.x - after_first).to_f32(),
            32.0,
            "8 ticks at 4 px/tick"
        );
        // tick 11：协程结束 → 末段 8/3 px/tick，回 State 0。
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.state, 0, "the coroutine returns to Normal (122830)");
        assert_eq!(p.vel.x, DASH_END_SPEED, "Speed = DashDir * 160 (122822)");
        let _ = x0;
    }

    /// 冲刺次数：空中冲刺后耗尽 → 头发向 USED 渐入；落地回充 → 白闪
    /// 0.12s 后回 NORMAL。
    #[test]
    fn dash_refill_and_hair_states_follow_update_hair() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        // 冲刺后立刻被地面回充（119855-119863 的地面分支在头 tick 生效）。
        let dash = Input {
            right: true,
            dash: true,
            ..Input::default()
        };
        p.step(&dash, std::slice::from_ref(&room));
        assert_eq!(p.dashes, 0);
        // 落地后的头部回充（119855-119863）：冲刺速度生效的那一 tick
        // 仍接地（地面横冲），下一 tick 头部回充。
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert_eq!(
            p.dashes, 1,
            "RefillDash refills on the ground (119855-119863)"
        );
        assert_eq!(
            p.hair_color(),
            crate::sprites::hair_color::FLASH,
            "lastDashes != Dashes flashes white (120283-120284)"
        );
        // 冲刺 0.2s 冷却（122541）：起手在第 1 tick，协程第 10 tick 结束
        // （phase 10 > DASH_TICKS），冷却第 12 tick 才清零。
        // 协程相位：tick 2 = 首段（phase 1），第 11 tick 末段结束 ——
        // 共 10 个状态 tick（1 个进入 + 9 个 0.15s 等待）。
        for _ in 0..9 {
            p.step(&Input::default(), std::slice::from_ref(&room));
        }
        assert_eq!(p.state, 0, "the dash coroutine ended (122830)");
        // 冷却门（`CanDash` 119325-119340 的 `dashCooldownTimer <= 0`）：
        // 把冷却重置到刚起手的值，立即再按必须被挡；等它递减完才允许。
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
        // 地面冲刺的下一 tick 头部又回充（119855-119863：站着就是站在
        // 固体上）→ lastDashes(0) ≠ Dashes(1) 再次白闪 —— 这正是原版
        // 地面冲刺的表现。
        p.step(&Input::default(), std::slice::from_ref(&room));
        assert_eq!(p.dashes, 1, "ground dash refills next tick");
        assert_eq!(p.hair_color(), crate::sprites::hair_color::FLASH);
        // 白闪 0.12s = 7 tick：计时器 > 0 的每一 tick 都显示 FLASH
        // （120286-120290 的 else 分支），归零后的**下一** tick 走离散
        // 分支回 NORMAL。
        for _ in 0..8 {
            p.step(&Input::default(), std::slice::from_ref(&room));
        }
        assert_eq!(p.hair_color(), crate::sprites::hair_color::NORMAL);
        // 空中耗尽 → 渐入 USED（120258-120262）：先在空中耗掉唯一一次冲刺。
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

    /// 蹬墙跳：贴墙按跳 → 水平 130 px/s 反向、竖直 -105、forceMoveX
    /// 0.16s 覆盖输入方向。
    #[test]
    fn wall_jump_off_a_wall_matches_vanilla_speeds() {
        // 左侧一列墙 + 地板。
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
        // 玩家贴墙悬空（x = 8 → hitbox [8,16]，墙右缘 8）。
        let mut p = Player::new(Vec2::from_f32s(8.0, 100.0));
        // 先落一 tick 贴稳（不接地也行：探针只看墙）。
        p.step(&Input::default(), std::slice::from_ref(&room));
        // 按住朝墙方向起跳 —— 原版只在 moveX != 0 时设 forceMoveX
        //（120828-120838 的 `else if (moveX != 0)`）。
        let jump = Input {
            left: true,
            jump: true,
            ..Input::default()
        };
        p.step(&jump, std::slice::from_ref(&room));
        assert_eq!(
            p.vel.x, WALLJUMP_X,
            "WallJump: Speed.X = 130 * dir (120840)"
        );
        assert_eq!(p.vel.y, JUMP_SPEED, "WallJump: Speed.Y = -105 (120841)");
        assert_eq!(
            p.force_move_x, 1,
            "WallJump forces moveX away from the wall (120828-120838)"
        );
        // forceMoveX 窗内反向输入被覆盖（119883-119891）。
        let push_left = Input {
            left: true,
            ..Input::default()
        };
        p.step(&push_left, std::slice::from_ref(&room));
        assert_eq!(p.move_x, Fx::one(), "forceMoveX overrides input for 0.16s");
    }

    /// 地面横冲刺中起跳 = 超级跳：260 px/s × Facing、-105（120740-120778）。
    #[test]
    fn jumping_during_a_ground_dash_is_a_super_jump() {
        let room = wide_room();
        let mut p = settled(&room, 40.0);
        let dash = Input {
            right: true,
            dash: true,
            ..Input::default()
        };
        p.step(&dash, std::slice::from_ref(&room)); // 进入 State 2
        p.step(&dash, std::slice::from_ref(&room)); // 速度生效（仍在地面）
        assert!(p.on_ground, "ground dash keeps the player grounded");
        let jump = Input {
            jump: true,
            ..Input::default()
        };
        p.step(&jump, std::slice::from_ref(&room));
        assert_eq!(p.state, 0, "SuperJump returns to Normal");
        assert_eq!(
            p.vel.x, SUPER_JUMP_X,
            "SuperJump: Speed.X = 260 * Facing (120748)"
        );
        assert_eq!(p.vel.y, JUMP_SPEED, "SuperJump: Speed.Y = -105 (120749)");
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
