//! mlua integration: the Lua cutscene scripting engine.
//!
//! Requires the `lua` feature (vendored LuaJIT; needs `make` at build time).
//!
//! # Design
//! Lua can only affect the simulation through the command queue (AGENTS.md
//! 4.5):
//!
//! 1. The engine creates the Lua state and attaches a [`PendingCommands`] to
//!    the `Lua` app data.
//! 2. The functions `world.*` / `camera.*` / `audio.*` / `spawn` /
//!    `set_player_state` **only** push [`SimCommand`] onto the queue; they
//!    never touch physics state.
//! 3. After a chunk finishes, the Rust side takes the queue and hands it to
//!    the [`CutsceneBridge`], where the simulation consumes it on the next
//!    frame.
//!
//! Because side effects are deferred until the queue is consumed, a script can
//! be interrupted / replayed at any time without leaving the world in a
//! half-updated intermediate state.

use std::cell::RefCell;
use std::path::Path;

use mlua::{Lua, Table};

use crate::bridge::CutsceneBridge;
use crate::command::{ActorKind, DurationFrames, PlayerState, SimCommand};

/// Commands accumulated on the Lua side.
///
/// Attached to the `Lua` app data; API closures push directly, and
/// [`ScriptEngine::collect`] takes them all at once after the chunk finishes.
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

/// Script error.
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

/// Lua cutscene engine.
pub struct ScriptEngine {
    lua: Lua,
    bridge: CutsceneBridge,
}

impl ScriptEngine {
    /// Create and initialise the Lua state.
    ///
    /// Registers high-level APIs such as `world` / `camera` / `audio`; they
    /// only call the command queue and produce no immediate side effects.
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

    /// The command bridge (consumed by the simulation side).
    pub fn bridge(&self) -> &CutsceneBridge {
        &self.bridge
    }

    /// The command bridge (mutable).
    pub fn bridge_mut(&mut self) -> &mut CutsceneBridge {
        &mut self.bridge
    }

    /// Load and execute a Lua script file.
    pub fn run_file(&mut self, path: &Path) -> Result<(), ScriptError> {
        let src = std::fs::read_to_string(path).map_err(|source| ScriptError::Read {
            path: path.display().to_string(),
            source,
        })?;
        self.run_source(&src)
    }

    /// Execute Lua source and enqueue the produced commands into the bridge.
    pub fn run_source(&mut self, src: &str) -> Result<(), ScriptError> {
        self.lua.load(src).exec()?;
        for command in self.collect()? {
            self.bridge.enqueue(command);
        }
        Ok(())
    }

    /// Take the commands accumulated by this execution.
    pub fn collect(&self) -> Result<Vec<SimCommand>, ScriptError> {
        let pending = self
            .lua
            .app_data_ref::<PendingCommands>()
            .ok_or(ScriptError::NoQueue)?;
        Ok(pending.take())
    }

    /// Get a reference to the command queue (used by the API closures).
    fn queue(lua: &Lua) -> Result<mlua::AppDataRef<'_, PendingCommands>, mlua::Error> {
        lua.app_data_ref::<PendingCommands>()
            .ok_or_else(|| mlua::Error::runtime("Releste command queue is not initialised"))
    }

    /// Register the Lua-side API.
    fn register_api(&mut self) -> Result<(), ScriptError> {
        let globals = self.lua.globals();

        // -- world.* ------------------------------------------
        let world = self.lua.create_table()?;

        // world.walk_to(actor_id, x, y, seconds)
        world.set(
            "walk_to",
            self.lua
                .create_function(|lua, (actor, x, y, secs): (u64, f32, f32, f32)| {
                    Self::queue(lua)?.push(SimCommand::DummyWalkTo {
                        actor: releste_world::EntityId::new(actor),
                        target: releste_math::Vec2::from_f32s(x, y),
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

        // -- camera.* -----------------------------------------
        let camera = self.lua.create_table()?;

        // camera.zoom(x, y, zoom, seconds)
        camera.set(
            "zoom",
            self.lua
                .create_function(|lua, (x, y, zoom, secs): (f32, f32, f32, f32)| {
                    Self::queue(lua)?.push(SimCommand::CameraZoom {
                        target: releste_math::Vec2::from_f32s(x, y),
                        zoom,
                        duration: DurationFrames::from_secs(secs),
                    });
                    Ok(())
                })?,
        )?;

        globals.set("camera", camera)?;

        // -- audio.* ------------------------------------------
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

        // -- top-level functions ------------------------------
        // spawn(kind, x, y)
        globals.set(
            "spawn",
            self.lua
                .create_function(|lua, (kind, x, y): (String, f32, f32)| {
                    Self::queue(lua)?.push(SimCommand::SpawnActor {
                        kind: ActorKind::Npc(kind),
                        pos: releste_math::Vec2::from_f32s(x, y),
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

        // Lets a script self-check: read the number of currently queued
        // commands.
        globals.set(
            "pending_count",
            self.lua.create_function(|lua, ()| {
                // Store the length in a local variable before leaving the
                // scope: if `q.0.borrow().len()` were the block tail
                // expression, the temporary `Ref` would be dropped after `q`,
                // so the borrow would not live long enough.
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

    /// Execute a "definition" (not enqueued into the bridge) for subsequent
    /// repeated [`ScriptEngine::call`].
    pub fn load_definition(&mut self, src: &str) -> Result<(), ScriptError> {
        self.lua.load(src).exec()?;
        Ok(())
    }

    /// Call a global function defined by the script and enqueue the produced
    /// commands into the bridge.
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

    /// Collect the commands produced by one script execution.
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
        // But they really are in the queue (not yet handed to the bridge).
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
        // The engine gives Lua no entry point that writes physics state
        // directly.
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
        // The example script in the repo must be actually executable.
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
