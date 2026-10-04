//! mlua 集成：Lua 过场脚本引擎。
//!
//! 需要 `lua` feature（vendored LuaJIT，构建期需要 `make`）。
//!
//! # 设计
//! Lua 只能通过命令队列影响仿真（AGENTS.md §4.5）：
//!
//! 1. 引擎创建 Lua 状态，并把一个 [`PendingCommands`] 挂到
//!    `Lua` 的 app data 上。
//! 2. `world.*` / `camera.*` / `audio.*` / `spawn` / `set_player_state`
//!    这些函数**只**往队列里 push [`SimCommand`]，从不触碰物理状态。
//! 3. chunk 执行完后，Rust 侧把队列取出交给 [`CutsceneBridge`]，
//!    由仿真在下一帧消费。
//!
//! 因为副作用被推迟到队列消费，脚本可以随时中断 / 重放，
//! 而不会让世界处于半更新的中间态。

use std::cell::RefCell;
use std::path::Path;

use mlua::{Lua, Table};

use crate::bridge::CutsceneBridge;
use crate::command::{ActorKind, DurationFrames, PlayerState, SimCommand};

/// Lua 侧累积的命令。
///
/// 挂在 `Lua` 的 app data 上，API 闭包直接 push；chunk 跑完后
/// 由 [`ScriptEngine::collect`] 一次性取走。
#[derive(Default)]
struct PendingCommands(RefCell<Vec<SimCommand>>);

impl PendingCommands {
    fn push(&self, command: SimCommand) {
        self.0.borrow_mut().push(command);
    }

    fn take(&self) -> Vec<SimCommand> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

/// 脚本错误。
#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    #[error("lua error: {0}")]
    Lua(#[from] mlua::Error),
    #[error("failed to read script {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("command queue is not initialised")]
    NoQueue,
}

/// Lua 过场引擎。
pub struct ScriptEngine {
    lua: Lua,
    bridge: CutsceneBridge,
}

impl ScriptEngine {
    /// 创建并初始化 Lua 状态。
    ///
    /// 注册 `world` / `camera` / `audio` 等高层 API；它们只调用
    /// 命令队列，不产生任何即时副作用。
    pub fn new() -> Result<Self, ScriptError> {
        let lua = Lua::new();
        lua.set_app_data(PendingCommands::default());

        let mut engine = ScriptEngine {
            lua,
            bridge: CutsceneBridge::new(),
        };
        engine.register_api()?;
        Ok(engine)
    }

    /// 命令桥（仿真侧消费）。
    pub fn bridge(&self) -> &CutsceneBridge {
        &self.bridge
    }

    /// 命令桥（可变）。
    pub fn bridge_mut(&mut self) -> &mut CutsceneBridge {
        &mut self.bridge
    }

    /// 加载并执行一个 Lua 脚本文件。
    pub fn run_file(&mut self, path: &Path) -> Result<(), ScriptError> {
        let src = std::fs::read_to_string(path).map_err(|source| ScriptError::Read {
            path: path.display().to_string(),
            source,
        })?;
        self.run_source(&src)
    }

    /// 执行 Lua 源码，并把产生的命令排入桥。
    pub fn run_source(&mut self, src: &str) -> Result<(), ScriptError> {
        self.lua.load(src).exec()?;
        for command in self.collect()? {
            self.bridge.enqueue(command);
        }
        Ok(())
    }

    /// 取出本次执行累积的命令。
    pub fn collect(&self) -> Result<Vec<SimCommand>, ScriptError> {
        let pending = self
            .lua
            .app_data_ref::<PendingCommands>()
            .ok_or(ScriptError::NoQueue)?;
        Ok(pending.take())
    }

    /// 取得命令队列的引用（供 API 闭包使用）。
    fn queue(lua: &Lua) -> Result<mlua::AppDataRef<'_, PendingCommands>, mlua::Error> {
        lua.app_data_ref::<PendingCommands>()
            .ok_or_else(|| mlua::Error::runtime("Releste command queue is not initialised"))
    }

    /// 注册 Lua 侧 API。
    fn register_api(&mut self) -> Result<(), ScriptError> {
        let globals = self.lua.globals();

        // ── world.* ──────────────────────────────────────────
        let world = self.lua.create_table()?;

        // world.walk_to(actor_id, x, y, seconds)
        world.set(
            "walk_to",
            self.lua
                .create_function(|lua, (actor, x, y, secs): (u64, f32, f32, f32)| {
                    Self::queue(lua)?.push(SimCommand::DummyWalkTo {
                        actor: reles_world::EntityId::new(actor),
                        target: reles_math::Vec2::from_f32s(x, y),
                        duration: DurationFrames::from_secs(secs),
                    });
                    Ok(())
                })?,
        )?;

        // world.wait(frames)
        world.set(
            "wait",
            self.lua.create_function(|lua, frames: u32| {
                Self::queue(lua)?.push(SimCommand::Wait { frames });
                Ok(())
            })?,
        )?;

        globals.set("world", world)?;

        // ── camera.* ─────────────────────────────────────────
        let camera = self.lua.create_table()?;

        // camera.zoom(x, y, zoom, seconds)
        camera.set(
            "zoom",
            self.lua
                .create_function(|lua, (x, y, zoom, secs): (f32, f32, f32, f32)| {
                    Self::queue(lua)?.push(SimCommand::CameraZoom {
                        target: reles_math::Vec2::from_f32s(x, y),
                        zoom,
                        duration: DurationFrames::from_secs(secs),
                    });
                    Ok(())
                })?,
        )?;

        globals.set("camera", camera)?;

        // ── audio.* ──────────────────────────────────────────
        let audio = self.lua.create_table()?;

        // audio.play(event, volume)
        audio.set(
            "play",
            self.lua
                .create_function(|lua, (event, volume): (String, f32)| {
                    Self::queue(lua)?.push(SimCommand::PlayAudio { event, volume });
                    Ok(())
                })?,
        )?;

        globals.set("audio", audio)?;

        // ── 顶层函数 ─────────────────────────────────────────
        // spawn(kind, x, y)
        globals.set(
            "spawn",
            self.lua
                .create_function(|lua, (kind, x, y): (String, f32, f32)| {
                    Self::queue(lua)?.push(SimCommand::SpawnActor {
                        kind: ActorKind::Npc(kind),
                        pos: reles_math::Vec2::from_f32s(x, y),
                    });
                    Ok(())
                })?,
        )?;

        // set_player_state("normal" | "frozen" | ...)
        globals.set(
            "set_player_state",
            self.lua.create_function(|lua, state: String| {
                let state = match state.to_ascii_lowercase().as_str() {
                    "normal" => PlayerState::Normal,
                    "frozen" => PlayerState::Frozen,
                    "invincible" => PlayerState::Invincible,
                    "nogravity" | "no_gravity" => PlayerState::NoGravity,
                    other => {
                        return Err(mlua::Error::runtime(format!(
                            "unknown player state: {other}"
                        )))
                    }
                };
                Self::queue(lua)?.push(SimCommand::SetPlayerState(state));
                Ok(())
            })?,
        )?;

        // end_cutscene()
        globals.set(
            "end_cutscene",
            self.lua.create_function(|lua, ()| {
                Self::queue(lua)?.push(SimCommand::EndCutscene);
                Ok(())
            })?,
        )?;

        // 便于脚本自检：读出当前已排队的命令数。
        globals.set(
            "pending_count",
            self.lua.create_function(|lua, ()| {
                // 先把长度存进局部变量再离开作用域：
                // 若直接把 `q.0.borrow().len()` 当块尾表达式，
                // 临时 `Ref` 会在 `q` 之后才析构，借用不够长。
                let queued = {
                    let q = Self::queue(lua)?;
                    let n = q.0.borrow().len();
                    n
                };
                Ok(queued)
            })?,
        )?;

        Ok(())
    }

    /// 执行一段"定义"（不排入桥），供随后反复 [`ScriptEngine::call`]。
    pub fn load_definition(&mut self, src: &str) -> Result<(), ScriptError> {
        self.lua.load(src).exec()?;
        Ok(())
    }

    /// 调用脚本里定义的全局函数，并把产生的命令排入桥。
    pub fn call(&mut self, name: &str) -> Result<(), ScriptError> {
        let globals: Table = self.lua.globals();
        let f: mlua::Function = globals.get(name)?;
        f.call::<()>(())?;
        for command in self.collect()? {
            self.bridge.enqueue(command);
        }
        Ok(())
    }
}

impl std::fmt::Debug for ScriptEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptEngine")
            .field("busy", &self.bridge.is_busy())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 收集一次脚本执行产生的命令。
    fn run(src: &str) -> Vec<SimCommand> {
        let mut engine = ScriptEngine::new().expect("engine");
        engine.run_source(src).expect("run");
        engine.bridge_mut().drain_new()
    }

    #[test]
    fn engine_constructs() {
        let engine = ScriptEngine::new().unwrap();
        assert!(!engine.bridge().is_busy());
    }

    #[test]
    fn wait_is_queued() {
        let cmds = run("world.wait(30)");
        assert_eq!(cmds, vec![SimCommand::Wait { frames: 30 }]);
    }

    #[test]
    fn camera_zoom_converts_seconds_to_frames() {
        let cmds = run("camera.zoom(120, 40, 1.75, 1.5)");
        match &cmds[0] {
            SimCommand::CameraZoom {
                target,
                zoom,
                duration,
            } => {
                assert_eq!(*zoom, 1.75);
                assert_eq!(duration.0, 90); // 1.5s * 60
                assert_eq!(target.x.to_f32(), 120.0);
                assert_eq!(target.y.to_f32(), 40.0);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn audio_play_is_queued() {
        let cmds = run(r#"audio.play("event:/sfx/jump", 0.8)"#);
        assert_eq!(
            cmds,
            vec![SimCommand::PlayAudio {
                event: "event:/sfx/jump".into(),
                volume: 0.8,
            }]
        );
    }

    #[test]
    fn spawn_creates_npc_actor() {
        let cmds = run(r#"spawn("granny", 150, 40)"#);
        match &cmds[0] {
            SimCommand::SpawnActor { kind, pos } => {
                assert_eq!(kind, &ActorKind::Npc("granny".into()));
                assert_eq!(pos.x.to_f32(), 150.0);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn player_state_accepts_known_names() {
        let cmds = run(r#"set_player_state("frozen")"#);
        assert_eq!(cmds, vec![SimCommand::SetPlayerState(PlayerState::Frozen)]);

        let cmds = run(r#"set_player_state("NOGRAVITY")"#);
        assert_eq!(
            cmds,
            vec![SimCommand::SetPlayerState(PlayerState::NoGravity)]
        );
    }

    #[test]
    fn unknown_player_state_is_an_error() {
        let mut engine = ScriptEngine::new().unwrap();
        assert!(engine.run_source(r#"set_player_state("bogus")"#).is_err());
    }

    #[test]
    fn multiple_commands_keep_order() {
        let cmds = run(r#"
            audio.play("a", 1.0)
            world.wait(5)
            audio.play("b", 1.0)
            end_cutscene()
            "#);
        assert_eq!(cmds.len(), 4);
        assert_eq!(cmds[1], SimCommand::Wait { frames: 5 });
        assert_eq!(cmds[3], SimCommand::EndCutscene);
    }

    #[test]
    fn load_definition_does_not_queue() {
        let mut engine = ScriptEngine::new().unwrap();
        engine.load_definition("world.wait(10)").unwrap();
        assert_eq!(engine.bridge().pending_len(), 0);
        // 但队列里确实有了（尚未交给桥）。
        assert_eq!(engine.collect().unwrap().len(), 1);
    }

    #[test]
    fn pending_count_visible_from_lua() {
        let mut engine = ScriptEngine::new().unwrap();
        engine
            .run_source(
                r#"
                world.wait(1)
                world.wait(2)
                assert(pending_count() == 2, "expected 2 queued")
                "#,
            )
            .expect("lua assert should pass");
    }

    #[test]
    fn no_physics_api_is_exposed() {
        // 引擎不给 Lua 任何直接写物理状态的入口。
        let mut engine = ScriptEngine::new().unwrap();
        engine
            .run_source(
                r#"
                assert(world.velocity == nil)
                assert(camera.position == nil)
                assert(set_position == nil)
                "#,
            )
            .expect("no physics handles should exist");
        assert_eq!(engine.collect().unwrap().len(), 0);
    }

    #[test]
    fn repeated_calls_accumulate() {
        let mut engine = ScriptEngine::new().unwrap();
        engine
            .load_definition("function poke() world.wait(3) end")
            .unwrap();
        engine.call("poke").unwrap();
        engine.call("poke").unwrap();
        assert_eq!(engine.bridge().pending_len(), 2);
    }

    #[test]
    fn runtime_error_propagates() {
        let mut engine = ScriptEngine::new().unwrap();
        assert!(engine.run_source("error('boom')").is_err());
    }

    #[test]
    fn example_cutscene_script_runs() {
        // 仓库里的示例脚本必须能被真正执行。
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../maps/example_cutscene.lua");
        let Ok(src) = std::fs::read_to_string(&path) else {
            eprintln!("skipping: {} not found", path.display());
            return;
        };
        let mut engine = ScriptEngine::new().unwrap();
        engine.run_source(&src).expect("example script should run");
        assert!(
            engine.bridge().pending_len() > 0,
            "example script should queue commands"
        );
    }
}
