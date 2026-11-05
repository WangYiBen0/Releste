//! LLM-friendly debugging tools: deterministic replay, per-tick traces and a
//! runtime map snapshot.
//!
//! Design rules (and why they are what they are):
//!
//! 1. **Determinism**: scenario replay loads no plugins and watches nothing;
//!    the script states the input, so the same input yields the same trace --
//!    without that, neither an LLM nor a human can reproduce anything.
//! 2. **Machine-readable**: the trace is JSONL (one object per tick) whose
//!    first line is a self-describing banner; the map snapshot is JSON.
//!    Field names are stable and the unit lives in the name.
//! 3. **Failures must be explicit**: a failed assertion prints
//!    `tick=... field: expected ... got ... (delta ... > tol ...)`
//!    and exits 1 -- that single line is enough for an LLM to locate the issue.
//! 4. **Bad config must blow up**: every TOML struct is `deny_unknown_fields`,
//!    so a misspelled key is an error, never a silent default (AGENTS.md 15.9).
//!
//! All three entry points are in `releste --help`:
//!
//! ```text
//! releste --headless --trace  out.jsonl   [--frames N]      # per-tick trace
//! releste --script            scenario.toml                 # scripted replay + assertions
//! releste --dump-map          map.json                      # runtime map snapshot
//! ```

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use releste_kernel::Kernel;
use releste_map::MapDocument;
use serde::{Deserialize, Serialize};

use crate::paths::Paths;
use crate::player::Input;
use crate::service::GameService;
use crate::session::Session;

/// Trace format version. Bump it whenever a field changes -- every JSONL an
/// LLM receives carries this line.
pub const TRACE_SCHEMA: &str = "releste.trace.v1";

/// Player state snapshot for one tick.
///
/// Positions are px; velocities are given both in px/tick (the engine's
/// internal unit) and px/s (vanilla's unit in `pureCeleste.cs`), so a line can
/// be compared against the reference directly.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot<'a> {
    /// State after this many `kernel.tick()` calls (1-based, same axis as the
    /// script assertions).
    pub tick: u64,
    /// Name of the room containing the player's centre on this tick.
    pub room: &'a str,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    /// Vanilla unit (px/s) = px/tick x 60.
    pub vx_ps: f32,
    /// Vanilla unit (px/s).
    pub vy_ps: f32,
    pub on_ground: bool,
    pub facing: i32,
    /// Vanilla `StateMachine.State`: 0 = Normal, 2 = Dash.
    pub state: u8,
    pub dashes: u8,
    pub stamina: f32,
    pub ducking: bool,
    pub anim: &'a str,
    pub anim_frame: usize,
    /// Vanilla `Sprite.LastAnimationID`.
    pub last_anim: &'a str,
    /// Horizontal input in effect this tick (`moveX`).
    pub move_x: f32,
    /// Remaining `varJumpTimer` ticks.
    pub var_jump: u32,
    /// Remaining jump-buffer ticks.
    pub jump_buffer: u32,
    /// Remaining coyote-time ticks.
    pub coyote: u32,
}

impl Snapshot<'_> {
    /// One human-readable line (for manual digging outside `--trace`).
    pub fn to_line(&self) -> String {
        format!(
            "t={:<5} {:<10} pos=({:>8.3},{:>8.3}) v=({:>8.3},{:>8.3}) ps=({:>7.1},{:>7.1}) \
             ground={} state={} dashes={} stamina={:>6.1} anim={}#{}",
            self.tick,
            self.room,
            self.x,
            self.y,
            self.vx,
            self.vy,
            self.vx_ps,
            self.vy_ps,
            u8::from(self.on_ground),
            self.state,
            self.dashes,
            self.stamina,
            self.anim,
            self.anim_frame,
        )
    }
}

/// Per-tick trace writer (JSONL).
pub struct TraceWriter {
    out: BufWriter<File>,
    wrote: u64,
}

impl TraceWriter {
    /// Create the file and write the schema banner.
    ///
    /// The banner records the map and the tick rate: the trace alone says how it
    /// was produced.
    pub fn create(path: &Path, map: &str, root: &Path) -> Result<Self> {
        let file = File::create(path)
            .with_context(|| format!("failed to create trace {}", path.display()))?;
        let mut out = BufWriter::new(file);
        let banner = serde_json::json!({
            "schema": TRACE_SCHEMA,
            "map": map,
            "root": root.display().to_string(),
            "tick_rate": 60,
            "units": { "pos": "px", "vx": "px/tick", "vx_ps": "px/s" },
        });
        writeln!(out, "{banner}").context("failed to write the trace banner")?;
        Ok(TraceWriter { out, wrote: 0 })
    }

    /// Write one tick.
    pub fn write(&mut self, snap: &Snapshot<'_>) -> Result<()> {
        let line = serde_json::to_string(snap).context("failed to encode a trace snapshot")?;
        writeln!(self.out, "{line}").context("failed to write a trace snapshot")?;
        self.wrote += 1;
        Ok(())
    }

    /// Number of ticks written.
    pub fn ticks(&self) -> u64 {
        self.wrote
    }

    /// Flush to disk.
    pub fn finish(mut self) -> Result<()> {
        self.out.flush().context("failed to flush the trace")?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Scenario scripts
// ---------------------------------------------------------------------------

/// One scenario script (TOML).
///
/// Examples live in `tests/scenarios/`; a misspelled field is an error, never
/// silently ignored.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    /// Scenario metadata.
    pub scenario: ScenarioMeta,
    /// Input segments (may overlap; overlaps are OR-ed).
    #[serde(default)]
    pub input: Vec<InputSegment>,
    /// Assertions.
    #[serde(default)]
    pub expect: Vec<Expect>,
}

/// Scenario metadata.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioMeta {
    /// Total tick count (1-based: run `kernel.tick()` `ticks` times).
    pub ticks: u64,
    /// Expected map name (a mismatch means `--map`/the default is wrong).
    #[serde(default)]
    pub map: Option<String>,
    /// Write the per-tick trace here (JSONL).
    #[serde(default)]
    pub trace: Option<PathBuf>,
    /// One human-readable note for the trace/assertions.
    #[serde(default)]
    pub note: Option<String>,
}

/// One input segment: hold these keys for ticks `from..=to` (1-based,
/// inclusive).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputSegment {
    pub from: u64,
    pub to: u64,
    #[serde(default)]
    pub left: bool,
    #[serde(default)]
    pub right: bool,
    #[serde(default)]
    pub up: bool,
    #[serde(default)]
    pub down: bool,
    #[serde(default)]
    pub jump: bool,
    #[serde(default)]
    pub dash: bool,
    #[serde(default)]
    pub grab: bool,
}

impl InputSegment {
    /// The key state of this segment.
    pub fn keys(&self) -> Input {
        Input {
            left: self.left,
            right: self.right,
            up: self.up,
            down: self.down,
            jump: self.jump,
            dash: self.dash,
            grab: self.grab,
        }
    }

    /// Whether this segment covers `tick` (1-based, inclusive).
    pub fn covers(&self, tick: u64) -> bool {
        self.from <= tick && tick <= self.to
    }
}

/// One assertion: check the given fields at `tick` (1-based, same axis as the
/// trace).
///
/// Only the fields you write are checked; floats compare with `tol`
/// (default [`DEFAULT_TOL`]).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    pub tick: u64,
    /// Float tolerance (px). Default 0.05.
    #[serde(default)]
    pub tol: Option<f32>,
    #[serde(default)]
    pub x: Option<f32>,
    #[serde(default)]
    pub y: Option<f32>,
    /// Displacement relative to tick 1 (px): stable across maps and spawn
    /// points, so it is the preferred form for behaviour regressions.
    #[serde(default)]
    pub dx: Option<f32>,
    #[serde(default)]
    pub dy: Option<f32>,
    #[serde(default)]
    pub vx: Option<f32>,
    #[serde(default)]
    pub vy: Option<f32>,
    #[serde(default)]
    pub on_ground: Option<bool>,
    #[serde(default)]
    pub state: Option<u8>,
    #[serde(default)]
    pub dashes: Option<u8>,
    #[serde(default)]
    pub stamina: Option<f32>,
    #[serde(default)]
    pub anim: Option<String>,
    #[serde(default)]
    pub room: Option<String>,
}

/// Default float tolerance for assertions (px).
pub const DEFAULT_TOL: f32 = 0.05;

/// Result of a scenario replay.
#[derive(Debug, Default)]
pub struct ScenarioOutcome {
    /// Number of ticks run.
    pub ticks: u64,
    /// Number of fields checked.
    pub checks: usize,
    /// Failed checks (one human-readable line each).
    pub failures: Vec<String>,
}

impl ScenarioOutcome {
    /// One-line summary (printed on success too).
    pub fn summary(&self) -> String {
        format!(
            "scenario: {} ticks, {} checks, {} failure(s)",
            self.ticks,
            self.checks,
            self.failures.len()
        )
    }
}

/// Read a scenario script.
pub fn load_script(path: &Path) -> Result<Script> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read the scenario {}", path.display()))?;
    toml::from_str(&text)
        .with_context(|| format!("failed to parse the scenario {}", path.display()))
}

/// Input at `tick`: every covering segment OR-ed together.
pub fn input_at(script: &Script, tick: u64) -> Input {
    script
        .input
        .iter()
        .filter(|s| s.covers(tick))
        .fold(Input::default(), |acc, seg| acc.merge(seg.keys()))
}

/// Replay a scenario: deterministic ticks, optional trace, assertion checks.
///
/// No plugins and no hot reload (determinism first). A failed assertion does
/// NOT abort the replay:
/// listing every deviation in one run beats reporting only the first.
pub fn run_script(
    script: &Script,
    session: Session,
    paths: &Paths,
    map_name: &str,
    filmstrip: Option<crate::filmstrip::FilmstripOptions>,
) -> Result<ScenarioOutcome> {
    if let Some(expected) = &script.scenario.map {
        anyhow::ensure!(
            expected == map_name,
            "scenario expects map `{expected}` but `{map_name}` is loaded (pass --map or fix the script)"
        );
    }

    let mut trace = match &script.scenario.trace {
        Some(path) => Some(TraceWriter::create(path, map_name, paths.root())?),
        None => None,
    };
    // `--filmstrip`: compose PNG frames with the same draw calls the GPU would get.
    let mut recorder = match filmstrip {
        Some(opts) => Some(crate::filmstrip::Recorder::new(opts, &session)?),
        None => None,
    };

    // Determinism: no plugins, no watcher.
    let service = GameService::new(session, None, None, None);
    let mut kernel = Kernel::new().with(service);
    let mut outcome = ScenarioOutcome::default();
    // Reference point for displacement assertions (dx/dy): the tick 1 position.
    let mut base = (0.0f32, 0.0f32);

    for tick in 1..=script.scenario.ticks {
        let input = input_at(script, tick);
        if let Some(svc) = kernel.resolve_mut::<GameService>() {
            svc.input = input;
        }
        kernel.tick();

        let Some(svc) = kernel.resolve::<GameService>() else {
            anyhow::bail!("the game service vanished mid-scenario");
        };
        let room = svc.room_name();
        let snap = svc.player.snapshot(tick, room);
        if tick == 1 {
            base = (snap.x, snap.y);
        }
        if let Some(trace) = trace.as_mut() {
            trace.write(&snap)?;
        }
        if let Some(recorder) = recorder.as_mut() {
            recorder.record(&svc.session, &svc.player);
        }
        for expect in script.expect.iter().filter(|e| e.tick == tick) {
            check(&snap, base, expect, &mut outcome);
        }
        outcome.ticks = tick;
    }

    if let Some(trace) = trace {
        trace.finish()?;
    }
    if let Some(recorder) = recorder {
        let frames = recorder.len();
        let path = recorder.finish()?;
        println!(
            "ok: {frames} filmstrip frame(s) written to {}",
            path.display()
        );
    }
    kernel.shutdown();
    Ok(outcome)
}

/// Compare one assertion field by field, appending deviations to `outcome`.
///
/// `base` is the tick 1 position, used by the `dx`/`dy` displacement assertions.
fn check(snap: &Snapshot<'_>, base: (f32, f32), expect: &Expect, outcome: &mut ScenarioOutcome) {
    let tol = expect.tol.unwrap_or(DEFAULT_TOL);
    let tick = expect.tick;

    let mut float = |name: &str, got: f32, want: Option<f32>| {
        let Some(want) = want else { return };
        outcome.checks += 1;
        if (got - want).abs() > tol {
            outcome.failures.push(format!(
                "tick={tick} {name}: expected {want:.4}, got {got:.4} (delta {:.4} > tol {tol:.4})",
                (got - want).abs()
            ));
        }
    };
    float("x", snap.x, expect.x);
    float("y", snap.y, expect.y);
    float("dx", snap.x - base.0, expect.dx);
    float("dy", snap.y - base.1, expect.dy);
    float("vx", snap.vx, expect.vx);
    float("vy", snap.vy, expect.vy);
    float("stamina", snap.stamina, expect.stamina);

    let mut int = |name: &str, got: i64, want: Option<i64>| {
        let Some(want) = want else { return };
        outcome.checks += 1;
        if got != want {
            outcome
                .failures
                .push(format!("tick={tick} {name}: expected {want}, got {got}"));
        }
    };
    int("state", i64::from(snap.state), expect.state.map(i64::from));
    int(
        "dashes",
        i64::from(snap.dashes),
        expect.dashes.map(i64::from),
    );
    int(
        "on_ground",
        i64::from(snap.on_ground),
        expect.on_ground.map(i64::from),
    );

    if let Some(want) = &expect.anim {
        outcome.checks += 1;
        if snap.anim != want {
            outcome.failures.push(format!(
                "tick={tick} anim: expected {want:?}, got {:?}",
                snap.anim
            ));
        }
    }
    if let Some(want) = &expect.room {
        outcome.checks += 1;
        if snap.room != want {
            outcome.failures.push(format!(
                "tick={tick} room: expected {want:?}, got {:?}",
                snap.room
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Map snapshot
// ---------------------------------------------------------------------------

/// Write the session as the runtime sees it to JSON.
///
/// Complements `releste-map --example dump_rooms` (the raw file view): this
/// dumps what the engine **parsed** -- spawn point, atlas name, tile grid (hex
/// per row), so any deviation from the `.map` is visible at a glance.
pub fn dump_session_json(session: &Session, path: &Path) -> Result<()> {
    let doc: &MapDocument = &session.doc;
    let rooms: Vec<serde_json::Value> = doc
        .rooms
        .iter()
        .map(|room| {
            let rows: Vec<String> = (0..room.tiles.height)
                .map(|ty| {
                    (0..room.tiles.width)
                        .map(|tx| format!("{:02x}", room.tiles.get(tx, ty).unwrap_or(0)))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect();
            serde_json::json!({
                "name": room.name,
                "bounds": { "x": room.bounds.x, "y": room.bounds.y, "w": room.bounds.w, "h": room.bounds.h },
                "tiles": {
                    "atlas": room.tiles.atlas,
                    "width": room.tiles.width,
                    "height": room.tiles.height,
                    "rows": rows,
                },
                "entities": room.entities.iter().map(|e| serde_json::json!({
                    "id": e.id.raw(),
                    "kind": e.kind,
                    "x": e.x,
                    "y": e.y,
                    "width": e.width,
                    "height": e.height,
                    "origin_x": e.origin_x,
                    "origin_y": e.origin_y,
                    "properties": e.properties,
                })).collect::<Vec<_>>(),
                "triggers": room.triggers.iter().map(|t| serde_json::json!({
                    "kind": t.kind, "x": t.x, "y": t.y, "width": t.width, "height": t.height,
                })).collect::<Vec<_>>(),
                "script": room.script.as_ref().map(|p| p.display().to_string()),
            })
        })
        .collect();

    let value = serde_json::json!({
        "schema": "releste.mapdump.v1",
        "area": doc.area,
        "spawn": { "x": session.spawn.x.to_f32(), "y": session.spawn.y.to_f32() },
        "spawn_room": session.spawn_room,
        "atlas": session.atlas_name,
        "tileset_ids": session.tilesets.tilesets.keys().collect::<Vec<_>>(),
        "rooms": rooms,
        "sync_rules": doc.sync_rules.iter().map(|r| serde_json::json!({
            "kind": r.kind, "mode": format!("{:?}", r.mode), "flag": r.flag,
        })).collect::<Vec<_>>(),
    });

    let text = serde_json::to_string_pretty(&value).context("failed to encode the map dump")?;
    std::fs::write(path, text)
        .with_context(|| format!("failed to write the map dump {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = r#"
[scenario]
ticks = 8
map = "0-Intro.map"
note = "unit test"

[[input]]
from = 3
to = 5
right = true
jump = true

[[expect]]
tick = 5
dashes = 1
"#;

    #[test]
    fn script_parses_and_overlapping_segments_or_together() {
        let script: Script = toml::from_str(SCRIPT).expect("parse");
        assert_eq!(script.scenario.ticks, 8);
        assert_eq!(script.input.len(), 1);
        assert_eq!(script.expect.len(), 1);

        assert_eq!(input_at(&script, 1), Input::default());
        let held = input_at(&script, 4);
        assert!(held.right && held.jump && !held.dash);
        assert_eq!(input_at(&script, 6), Input::default());
    }

    /// Overlapping segments OR together: pressing left+right sets both keys
    /// (the engine's `axis_x` then reads 0, but the merge rule must be explicit).
    #[test]
    fn overlapping_segments_merge_with_or() {
        let text = r#"
[scenario]
ticks = 4

[[input]]
from = 1
to = 2
right = true

[[input]]
from = 2
to = 3
jump = true
"#;
        let script: Script = toml::from_str(text).expect("parse");
        let merged = input_at(&script, 2);
        assert!(merged.right, "tick 2 falls inside both segments");
        assert!(merged.jump);
        assert!(input_at(&script, 3).jump);
        assert!(!input_at(&script, 4).jump);
    }

    /// A misspelled key must be an error, never a silent default (AGENTS.md 15.9).
    #[test]
    fn unknown_keys_are_rejected() {
        let text = r#"
[scenario]
ticks = 4
jummmp = true
"#;
        let err = toml::from_str::<Script>(text).expect_err("must reject unknown keys");
        assert!(
            err.to_string().contains("jummmp"),
            "error should name the bad key: {err}"
        );
    }

    /// Float assertions use a tolerance; failures must carry delta and tol.
    #[test]
    fn float_expectations_report_deltas() {
        let snap = Snapshot {
            tick: 7,
            room: "lvl_-1",
            x: 10.0,
            y: 20.0,
            vx: 0.0,
            vy: 0.0,
            vx_ps: 0.0,
            vy_ps: 0.0,
            on_ground: true,
            facing: 1,
            state: 0,
            dashes: 1,
            stamina: 110.0,
            ducking: false,
            anim: "idle",
            anim_frame: 0,
            last_anim: "",
            move_x: 0.0,
            var_jump: 0,
            jump_buffer: 0,
            coyote: 6,
        };
        let good: Expect = toml::from_str("tick = 7\nx = 10.02").expect("parse");
        let mut outcome = ScenarioOutcome::default();
        check(&snap, (10.0, 20.0), &good, &mut outcome);
        assert_eq!(outcome.checks, 1);
        assert!(
            outcome.failures.is_empty(),
            "0.02 is inside the 0.05 tolerance"
        );

        let bad: Expect = toml::from_str("tick = 7\nx = 10.5\nanim = \"runSlow\"").expect("parse");
        check(&snap, (10.0, 20.0), &bad, &mut outcome);
        assert_eq!(outcome.checks, 3);
        // dx is displacement relative to tick 1: stable across spawn points.
        let rel: Expect = toml::from_str("tick = 7\ndx = 3.0").expect("parse");
        check(&snap, (7.0, 20.0), &rel, &mut outcome);
        assert_eq!(outcome.checks, 4);
        assert_eq!(outcome.failures.len(), 2);
        assert!(
            outcome.failures[0].contains("delta 0.5000 > tol 0.0500"),
            "got {:?}",
            outcome.failures[0]
        );
        assert!(outcome.failures[1].contains("anim"));
    }
}
