//! 游戏 service：固定步长推进玩家 + 插件，渲染步把快照喂给 wgpu。
//!
//! 它是窗口宿主与 [`releste_kernel`] 之间的唯一桥梁（AGENTS.md §4.1：
//! kernel 只认识 `Service`）。地图是数据，改地图不重启（§4.3）。

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

/// `FileWatcher` 的防抖窗口，ROADMAP 第 10 步：100ms。
const WATCH_DEBOUNCE_MS: u64 = 100;
/// 相机跟随系数（每 tick 追目标的 1/4）。
/// 原版相机跟随系数（`Player.Update` 120137-120145）：
/// `pos += (target - pos) * (1 - 0.01^dt)`；dt = 1/60 时系数 ≈ 0.0739，
/// 取 19/256 = 0.0742（Fx 1/256 步长内最接近的有理数）。
const CAMERA_LERP: Fx = Fx::from_ratio(19, 256);

/// 房间转场时长 0.65s = 39 tick（`Level.NextTransitionDuration`，
/// pureCeleste.cs:113847，Reset 115428）。
const TRANSITION_TICKS: u32 = 39;

/// 进行中的房间转场（原版 `Level.transition` 协程的显式状态版）。
#[derive(Debug, Clone, Copy)]
struct RoomTransition {
    /// 玩家 hitbox 左上角的滑动终点（原版 `playerTo`）。
    player_to: Vec2,
    /// 相机起点（视口中心，原版 `cameraFrom`）。
    camera_from: Vec2,
    /// 相机终点（新房间里钳制后的目标，原版 `cameraTo`）。
    camera_to: Vec2,
    /// 已进行的 tick（原版 `cameraAt`，按 dt/0.65 逼近 1）。
    ticks: u32,
}

/// 插件启动：扫描并加载 `plugins/` 下的每个插件。
///
/// 任何失败都只 `warn`，游戏照常跑（ROADMAP 3.6：`rm -rf plugins/`
/// 必须能启动）。目录不存在由 `scan` 自己告警。
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

/// 打开 `.map` 监听器。失败只是没有热重载，不是致命错误。
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

/// 游戏主 service。
pub struct GameService {
    /// 当前地图 + 图集。
    pub session: Session,
    /// 玩家状态（热重载必须保留，§4.3）。
    pub player: Player,
    /// 本 tick 的输入。
    pub input: Input,
    /// 暂停（Esc 菜单）。
    pub paused: bool,
    /// 相机（渲染时按插值取 `prev` → `cur`）。
    camera: Camera,
    prev_camera: Camera,
    host: Option<PluginHost>,
    watcher: Option<FileWatcher>,
    static_draws: Vec<SpriteDraw>,
    /// 转场期间保留下来的**上一个房间**静态批次（原版转场结束时才
    /// `UnloadEntities` 旧房间实体，115415/115432 —— 切屏动画进行中，
    /// 旧房间的瓦片/实体必须继续渲染，不能提前变成虚无）。
    transition_statics: Option<Vec<SpriteDraw>>,
    static_dirty: bool,
    /// 静态批次当前对应的房间（切房即重建 —— 原版一次只画一个房间）。
    built_room: String,
    /// 当前房间（原版 `Session.Level` 语义：转场开始时即切到新房间，
    /// 相机钳制、房间过滤、EnforceBounds 都以它为准）。
    current_room: String,
    /// 进行中的房间转场；`Some` 时实体冻结，只有玩家滑动与相机平移
    /// （原版 `Level.Update` 的 Transitioning 分支，115659-115668）。
    transition: Option<RoomTransition>,
    overflow_warned: bool,
    reloads: u64,
    /// 上次报告过的房间名（`-v` 下房间切换只在这变化时打一行）。
    reported_room: String,
}

impl GameService {
    /// 建服务。`gpu` 为 `None`（headless）时不建静态批次。
    pub fn new(
        session: Session,
        host: Option<PluginHost>,
        watcher: Option<FileWatcher>,
        gpu: Option<&GpuRenderer>,
    ) -> Self {
        // 原版：`new Player(Session.RespawnPoint.Value, ...)`（115105），
        // Position = 出生锚点 = hitbox 底中心；我们的 Player 存 hitbox
        // 左上角 → 左上角 = 锚点 − (W/2, H)（hitbox 8×11，offset (-4,-11)，
        // 118856）。
        let player = Player::new(session.spawn - Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H));
        let spawn_room = session.spawn_room.clone();
        let current_room = spawn_room.clone();
        // 相机初始落位即按原版钳制（见 `update_camera`）。
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
        // 相机钳进出生房间（原版：相机永远在房间界内）。
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

    /// 原版相机中心：`Player.CameraTarget` 是 `Position + (-160,-90)` 的
    /// 相机**中心**钳进 `[Left+160, Right-160] × [Top+90, Bottom-90]`
    /// （等价于原版把相机左上角钳进 `[Left, Top, Right-320, Bottom-180]`
    /// 再加回半屏，`pureCeleste.cs:119108-119111`）。
    ///
    /// 钳制取 XNA `MathHelper.Clamp(v, min, max)` 的逐值语义 —— max <
    /// min（房间窄/矮于视图）时**不**取中心：值偏左给 min，偏右给 max。
    ///
    /// `anchor` 是玩家的**底中心**（原版 Position 语义），`room_name`
    /// 是目标房间。引擎 `Camera.position` 即视口中心。
    fn vanilla_camera_center(session: &Session, anchor: Vec2, room_name: &str) -> Vec2 {
        let Some(room) = session.doc.rooms.iter().find(|r| r.name == room_name) else {
            return anchor;
        };
        let b = &room.bounds;
        // XNA `MathHelper.Clamp`：先查下界再查上界（max < min 时按值落边）。
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

    /// 热重载累计次数。
    pub fn reloads(&self) -> u64 {
        self.reloads
    }

    /// 静态批次里画了多少个精灵。
    pub fn static_draw_count(&self) -> usize {
        self.static_draws.len()
    }

    /// 当前房间名。
    pub fn room_name(&self) -> &str {
        &self.current_room
    }

    /// 已加载的插件数。
    pub fn plugin_count(&self) -> usize {
        self.host.as_ref().map(|h| h.loaded().count()).unwrap_or(0)
    }

    /// 渲染步：（重）建静态批次、填满 `batch`、排序。
    ///
    /// `alpha` ∈ [0, 1] 是上个 tick 到这个 tick 的插值进度，玩家位置按
    /// 它取 `prev_pos` → `pos` 的线性插值，避免 60Hz 模拟在 144Hz 屏幕上抖动。
    pub fn render(&mut self, gpu: &mut GpuRenderer, alpha: f32, batch: &mut SpriteBatch) {
        // 清屏色来自地图样式（`<Style color=...>` → `MapStyle::background_color`）。
        gpu.set_clear_color(self.session.clear_color());

        // 原版一次只画一个房间：转场开始（`Session.Level` 已切到新房间）
        // 或热重载时，以当前房间重建静态批次。
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
        // 视差背景/前景依赖相机位置，每帧按插值相机重建
        // （`Parallax.Render`，`pureCeleste.cs:112163-112236`）。
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

    /// 玩家本帧的 draw（头发 + 身体，位置按 alpha 插值）。
    fn player_draws(&self, gpu: &GpuRenderer, alpha: f32) -> Vec<SpriteDraw> {
        let a = Fx::from_f32(alpha.clamp(0.0, 1.0));
        let pos = self.player.prev_pos + (self.player.pos - self.player.prev_pos) * a;
        self.session.player_draws(gpu, pos, self.player.facing < 0)
    }

    /// 插值后的相机位置（视口**中心**）。
    fn camera_pos(&self, alpha: f32) -> Vec2 {
        let a = Fx::from_f32(alpha.clamp(0.0, 1.0));
        self.prev_camera.position + (self.camera.position - self.prev_camera.position) * a
    }

    /// 插值后的相机（供渲染使用）。
    pub fn render_camera(&self, alpha: f32) -> Camera {
        Camera::at(self.camera_pos(alpha))
    }

    /// 轮询 `.map` 变更；成功则换掉文档、重建静态批次。
    ///
    /// 玩家位置/速度/朝向**不重置**（AGENTS.md §4.3）。
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

        // `room_at` 接收 hitbox 左上角（它内部取中心）；这里再用同一
        // 中心做 contains 判断，保证"回落 + 告警"判定的是同一个点。
        let room = self.session.room_at(self.player.pos);
        let centre = self.player.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H * 0.5);
        if !room.bounds.to_rect().contains(centre) {
            warn!(
                room = %room.name,
                "the player is outside every room after the reload; it was left untouched (AGENTS.md 4.3)"
            );
        }
    }

    /// 一个模拟 tick：转场中只推进协程；否则物理 → 相机 → 转场判定 →
    /// backdrop 可见性刷新。
    fn simulate_tick(&mut self) {
        // 转场期间实体冻结（原版 `Level.Update` 的 Transitioning 分支，
        // 115659-115668 只跑转场协程），只有玩家滑动与相机平移。
        if self.transition.is_some() {
            self.advance_transition();
        } else {
            self.player.update(&self.input, &self.session);
            self.update_camera();
            // EnforceBounds：转场触发/边界钳制，在物理之后（120168）。
            self.check_room_transition();
        }

        // backdrop 可见性每 tick 刷新（原版 `Backdrop.Update` 的节奏，
        // 109468-109489）；房间过滤用当前房间名（`Session.Level` 语义），
        // 转场期间按 InstantIn/InstantOut 规则保留或点亮。
        let transitioning = self.transition.is_some();
        let room = self.current_room.clone();
        self.session
            .update_backdrop_visibility(&room, transitioning);

        // 房间身份变化：`-v` 以上打一行，供排查"玩家到底站在哪个房间"。
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

    /// 常规 tick 的相机跟随（原版 `Player.Update` 120133-120146）：
    /// 目标 = 玩家锚点在当前房间内钳制后的位置，指数逼近。
    fn update_camera(&mut self) {
        // 玩家锚点 = hitbox 底中心（原版 Position 语义）。
        let anchor = self.player.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H);
        let target = Self::vanilla_camera_center(&self.session, anchor, &self.current_room);
        self.prev_camera = self.camera;
        let delta = target - self.camera.position;
        self.camera.position += delta * CAMERA_LERP;
    }

    /// 原版 `Level.EnforceBounds`（pureCeleste.cs:116517-116607）的转场
    /// 触发与边界钳制。玩家 hitbox 越过当前房间边界且探针点落在另一间
    /// 房里 → 开转场；否则按原版钳位（左右硬夹、上 24px 余量）。
    ///
    /// 两侧同时越界时按原版 if/else 优先级只触发一个：左 → 右 → 上 → 下。
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
                    // 原版钳位：player.Left = bounds.Left
                    self.player.pos.x = Fx::from_f32(left);
                    self.player.vel.x = Fx::zero();
                }
            }
        } else if p_right > right && p_top >= top && p_bottom < bottom {
            let probe = center + Vec2::from_f32s(8.0, 0.0);
            match self.room_at_point(probe) {
                Some(next) => self.start_transition(next, (1.0, 0.0)),
                None => {
                    self.player.pos.x = Fx::from_f32(right - PLAYER_W as f32);
                    self.player.vel.x = Fx::zero();
                }
            }
        }

        // 上：中心越过房间顶边（119560-119577）；没有目标房间时钳到
        // 顶上 24px（允许探头看上面）。
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
            // 下：脚下 4px 无实体才转场（116589-116598）；原版底部越界
            // 未转场即死亡（116599-116606），死亡尚未实现，沿用世界夹取。
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

    /// 点所在的房间（原版 `MapData.GetAt`；无回落）。
    fn room_at_point(&self, point: Vec2) -> Option<String> {
        self.session
            .doc
            .rooms
            .iter()
            .find(|r| r.bounds.to_rect().contains(point))
            .map(|r| r.name.clone())
    }

    /// 开转场（原版 `Level.TransitionRoutine` 115282-115423 的准备段）：
    /// `Session.Level` 立即切到新房间（世界/静态批次随后以新房间重建），
    /// 算出玩家滑动终点与相机起终点。
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
        // dirPad：direction * 4，竖直向下是 * 12（115345-115349）。
        let dir_pad = if direction == (0.0, 1.0) {
            dir * Fx::from_int(12)
        } else {
            dir * Fx::from_int(4)
        };

        // playerTo：玩家**底中心**空间（原版 Position 语义）。
        let (dir_x, _) = direction;
        let mut player_to = self.player.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H);
        if dir_x != 0.0 {
            // 横向转场先把终点从地板里拔出来（115351-115355）。
            let nb_bottom = nb.y + nb.h;
            while player_to.y.to_f32() >= nb_bottom {
                player_to.y = player_to.y - Fx::one();
            }
        }
        // 一直沿方向滑动，直到落进新房间（带 dirPad 的内缩判定，
        // `IsInBounds(Vector2, Vector2)` 116641-116657）。
        let mut guard = 0u32;
        while !Self::in_bounds_padded(player_to, dir_pad, &nb) {
            player_to = player_to + dir;
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
        // 原版转场结束时才卸载旧房间的实体（`UnloadEntities(toRemove)`，
        // 115415；`UnloadEntities` 115432）—— 动画进行中旧房间照常渲染。
        self.transition_statics = Some(std::mem::take(&mut self.static_draws));
        self.current_room = next_room;
        // hitbox 左上角 = 底中心 − (W/2, H)。
        let player_to = player_to - Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H);
        self.transition = Some(RoomTransition {
            player_to,
            camera_from,
            camera_to,
            ticks: 0,
        });
    }

    /// 原版 `IsInBounds(Vector2, Vector2 dirPad)`（116641-116657）：
    /// 房间矩形沿 `dirPad` 的正分量内缩后是否包含该点（右/下开区间）。
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

    /// 推进转场（原版 `TransitionRoutine` 的逐帧段 + `Player.TransitionTo`
    /// 120551-120566）：玩家以 60 px/s 滑向终点（无碰撞，X 先 Y 后），
    /// 相机以 CubeOut 缓动滑 0.65s，> 0.9 时直接落位。
    ///
    /// 转场期间实体全部冻结（`Level.Update` 的 Transitioning 分支只跑
    /// 转场协程），输入被忽略 —— 与原版一致。
    fn advance_transition(&mut self) {
        let Some(t) = self.transition.as_mut() else {
            return;
        };
        self.player.prev_pos = self.player.pos;
        // MoveTowardsX / MoveTowardsY：每轴最多 1 px/tick（60 px/s）。
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

        // cameraAt = Approach(cameraAt, 1, dt / 0.65)（115366）。
        // 缓动是渲染侧表现（AGENTS.md 15.7 允许），用 f32 算完折回 Fx。
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
            // 转场结束：旧房间实体在这里才卸载（此时相机已整体在新房间，
            // 丢弃旧批次与原版 UnloadEntities 一样是不可见的）。
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

    /// 自足的测试地图：一个房间 + 一个玩家出生点。
    ///
    /// 真实的 `assets/` 被 gitignore（Appendix C 规则 6），nix/CI 的构建
    /// 源里没有它 —— 测试必须自己造数据，而不是依赖开发树里的转换产物。
    fn fixture_doc() -> releste_map::MapDocument {
        let mut tiles = Tileset::new("Test", 8, 4);
        for x in 0..8 {
            tiles.set(x, 3, 1); // 一行地板
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

    /// 造一个最小资源根：`maps/<地图>` + `assets/atlas/{Test.atlas,tilesets.toml}`。
    /// 图集零纹理、tileset 零定义 —— `Session::load` 只要求文件能解析。
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

    /// 热重载：改一个瓦片 → 文档换掉、计数 +1、玩家原地不动（§4.3）。
    ///
    /// 地图与图集全部合成（见 `fixture_root`），debounce 是 100ms，
    /// 所以写完要等一会儿再 poll。
    #[test]
    fn map_hot_reload_swaps_the_doc_and_keeps_the_player() {
        let paths = fixture_root().expect("fixture root");
        let map_path = paths.map("reload-fixture.map");

        let session = Session::load("reload-fixture.map", None, &paths)
            .expect("session from the fixture map");
        let spawn_before = session.spawn;
        let watcher = watch_maps(&paths).expect("watcher over the maps dir");
        let mut service = GameService::new(session, None, Some(watcher), None);

        // 改一个瓦片并保存 —— 这才是热重载要吃到的真实变更。
        let mut doc = releste_map::load_map(&map_path).expect("reload the fixture");
        let (room, tx, ty) = {
            let room = &mut doc.rooms[0];
            room.tiles.set(1, 1, 1);
            (room.name.clone(), 1, 1)
        };
        let before = doc.rooms[0].tiles.get(tx, ty);
        assert_eq!(before, Some(1), "the edit landed in the doc");
        releste_map::save_map(&doc, &map_path).expect("save the edited map");

        // debounce 从**看到事件**那一刻起算。事件投递是后台线程的事，
        // 在并行测试负载下可能晚到，所以不能写死"poll 两次"——
        // 在截止时间内反复 poll，重载发生（且只发生一次）即算过。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while service.reloads() == 0 && std::time::Instant::now() < deadline {
            service.poll_reload();
            if service.reloads() == 0 {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
        service.poll_reload(); // 收尾：若窗口刚好在两次 poll 之间成熟，再吸一次

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

    /// 两间贴边房间（A x[0,320)、B x[320,640)，地板同高）构成的最小
    /// 转场地图：玩家从 A 朝右走出右缘，必须触发原版转场。
    fn seam_map() -> releste_map::MapDocument {
        let mut tiles = Tileset::new("Test", 40, 23);
        for x in 0..40 {
            tiles.set(x, 22, 1); // 地板一行（y 176..184）
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

    /// 走出房间右缘 → 原版转场：玩家冻结滑进新房间，相机 CubeOut 平移
    /// 0.65s，结束后玩家落在新房间的地板上、房间身份已切换。
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

        // 转场期间输入被忽略（实体冻结）：反着按也不影响滑动方向。
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
        assert_eq!(service.player.pos.y.to_f32(), 176.0 - PLAYER_H as f32);
        // 相机落位在新房间的钳制目标上（对玩家在新房间里的位置）。
        let anchor = service.player.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H);
        assert_eq!(
            service.camera.position,
            GameService::vanilla_camera_center(&service.session, anchor, "lvl_b")
        );
    }

    /// 相机跟随系数必须是原版的 `1 - 0.01^dt`（≈0.074/tick），不是旧的
    /// 1/4 —— 追得太快会失去原版"相机拖拽"的手感。
    #[test]
    fn camera_follow_uses_the_vanilla_exponential_rate() {
        let doc = seam_map();
        let session = Session::from_doc(doc, "test");
        let mut service = GameService::new(session, None, None, None);
        let anchor = service.player.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H);
        let target = GameService::vanilla_camera_center(&service.session, anchor, "lvl_a");
        // 把相机拖到别处，走一 tick：位移必须是 (target - pos) * 19/256。
        service.camera.position = Vec2::from_f32s(40.0, 90.0);
        service.update_camera();
        let expected =
            Vec2::from_f32s(40.0, 90.0) + (target - Vec2::from_f32s(40.0, 90.0)) * CAMERA_LERP;
        assert_eq!(service.camera.position, expected);
    }
}
