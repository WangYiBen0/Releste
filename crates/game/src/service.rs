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
use crate::player::{Input, Player, PLAYER_H, PLAYER_W};
use crate::session::Session;

/// `FileWatcher` 的防抖窗口，ROADMAP 第 10 步：100ms。
const WATCH_DEBOUNCE_MS: u64 = 100;
/// 相机跟随系数（每 tick 追目标的 1/4）。
const CAMERA_LERP: Fx = Fx::from_ratio(1, 4);

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
    static_dirty: bool,
    /// 静态批次当前对应的房间（切房即重建 —— 原版一次只画一个房间）。
    built_room: String,
    /// 相机当前锁定的房间（跨房间不平滑，直接落位）。
    camera_room: String,
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
        let camera_room = spawn_room.clone();
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
            static_dirty: gpu.is_none(),
            built_room,
            camera_room,
            overflow_warned: false,
            reloads: 0,
            reported_room: spawn_room,
        }
    }

    /// 原版相机中心：`Player.CameraTarget` 是 `Position + (-160,-90)` 的
    /// 相机**左上角**钳进 `[Left, Top, Right-320, Bottom-180]`；等价的
    /// 中心钳制是 `[Left+160, Top+90, Right-160, Bottom-90]`。窄/矮于
    /// 视图的轴取房间中心。
    ///
    /// `anchor` 是玩家的**底中心**（原版 Position 语义），`room_name`
    /// 是玩家当前所在房间 —— 相机只在这一间里打转。
    fn vanilla_camera_center(session: &Session, anchor: Vec2, room_name: &str) -> Vec2 {
        let Some(room) = session.doc.rooms.iter().find(|r| r.name == room_name) else {
            return anchor;
        };
        let b = &room.bounds;
        let clamp_axis = |v: Fx, lo: f32, size: f32, view: f32| -> Fx {
            if size <= view {
                Fx::from_f32(lo + size * 0.5)
            } else {
                v.clamp(
                    Fx::from_f32(lo + view * 0.5),
                    Fx::from_f32(lo + size - view * 0.5),
                )
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
        // `room_at` 接收 hitbox 左上角，自己取中心。
        self.session.room_at(self.player.pos).name.as_str()
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

        // 原版一次只画一个房间：切房（或热重载）就重建当前房间的静态批次。
        let room_name = self.session.room_at(self.player.pos).name.clone();
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

    fn update_camera(&mut self) {
        // 玩家锚点 = hitbox 底中心（原版 Position 语义）。
        let anchor = self.player.pos + Vec2::from_f32s(PLAYER_W * 0.5, PLAYER_H);
        let room_name = self.session.room_at(self.player.pos).name.clone();

        self.prev_camera = self.camera;
        if self.camera_room != room_name {
            // 原版跨房间是切屏：相机**直接落位**到新房间的钳制位置，
            // 绝不做跨房间的平滑 —— 那会让镜头扫过房间之间的黑色虚空。
            self.camera = Camera::at(Self::vanilla_camera_center(
                &self.session,
                anchor,
                &room_name,
            ));
            self.prev_camera = self.camera;
            self.camera_room = room_name;
        } else {
            let target = Self::vanilla_camera_center(&self.session, anchor, &room_name);
            // 平滑跟随。原版的具体缓动待对 pureCeleste 的 Level.Update
            // 确认；每 tick 追 1/4（约 0.3s 收敛）先对齐"快而不抖"的手感。
            let delta = target - self.camera.position;
            self.camera.position += delta * CAMERA_LERP;
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
        self.player.update(&self.input, &self.session);
        self.update_camera();

        // 房间身份变化（中心跨缝）：`-v` 以上打一行，供排查"玩家到底
        // 站在哪个房间"。物理本身不选房间，这只影响标题栏/相机。
        let room = self.session.room_at(self.player.pos).name.to_string();
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
}
