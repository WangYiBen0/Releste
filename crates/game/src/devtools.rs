//! LLM 友好的调试工具：确定性回放、逐 tick 轨迹、运行时地图快照。
//!
//! 设计原则（为什么这么写）：
//!
//! 1. **确定性**：场景回放不装插件、不监听热重载，输入由脚本显式给出，
//!    同一输入必然得到同一轨迹 —— 否则 LLM（和人）无法复现问题。
//! 2. **机器可读**：轨迹是 JSONL（每 tick 一行对象），首行是自描述的
//!    schema banner；地图快照是 JSON。字段名稳定、单位写在字段名里。
//! 3. **失败要说清楚**：断言失败打印 `tick=… 字段: expected … got … (Δ… > tol …)`
//!    并以退出码 1 结束 —— LLM 可以直接读这一行定位。
//! 4. **配置写错要炸**：TOML 一律 `deny_unknown_fields`，拼错的键会报错，
//!    不会静默当作默认值（AGENTS.md §15.9）。
//!
//! 三个入口都在 `releste --help` 里：
//!
//! ```text
//! releste --headless --trace  out.jsonl   [--frames N]      # 逐 tick 轨迹
//! releste --script            scenario.toml                 # 脚本化回放 + 断言
//! releste --dump-map          map.json                      # 运行时地图快照
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

/// 轨迹格式版本。字段一变就升版本 —— LLM 拿到的 JSONL 自带这一行。
pub const TRACE_SCHEMA: &str = "releste.trace.v1";

/// 一 tick 的玩家状态快照。
///
/// 位置单位 px，速度同时给 px/tick（引擎内部单位）与 px/s（原版
/// `pureCeleste.cs` 的单位，方便逐行对照）。
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot<'a> {
    /// 第几次 `kernel.tick()` 之后的状态（1-based，与脚本断言同轴）。
    pub tick: u64,
    /// 该 tick 玩家中心所在的房间名。
    pub room: &'a str,
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    /// 原版单位（px/s）= px/tick × 60。
    pub vx_ps: f32,
    /// 原版单位（px/s）。
    pub vy_ps: f32,
    pub on_ground: bool,
    pub facing: i32,
    /// 原版 `StateMachine.State`：0 = Normal，2 = Dash。
    pub state: u8,
    pub dashes: u8,
    pub stamina: f32,
    pub ducking: bool,
    pub anim: &'a str,
    pub anim_frame: usize,
    /// 原版 `Sprite.LastAnimationID`。
    pub last_anim: &'a str,
    /// 本 tick 生效的水平输入（`moveX`）。
    pub move_x: f32,
    /// `varJumpTimer` 剩余 tick。
    pub var_jump: u32,
    /// 跳跃缓冲剩余 tick。
    pub jump_buffer: u32,
    /// 土狼时间剩余 tick。
    pub coyote: u32,
}

impl Snapshot<'_> {
    /// 人读一行（`--trace` 之外的手动排查用）。
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

/// 逐 tick 轨迹写入器（JSONL）。
pub struct TraceWriter {
    out: BufWriter<File>,
    wrote: u64,
}

impl TraceWriter {
    /// 建文件并写下 schema banner。
    ///
    /// banner 记录地图与步长 —— 光看轨迹也能知道它是怎么跑出来的。
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

    /// 写一 tick。
    pub fn write(&mut self, snap: &Snapshot<'_>) -> Result<()> {
        let line = serde_json::to_string(snap).context("failed to encode a trace snapshot")?;
        writeln!(self.out, "{line}").context("failed to write a trace snapshot")?;
        self.wrote += 1;
        Ok(())
    }

    /// 已写 tick 数。
    pub fn ticks(&self) -> u64 {
        self.wrote
    }

    /// 落盘。
    pub fn finish(mut self) -> Result<()> {
        self.out.flush().context("failed to flush the trace")?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 场景脚本
// ---------------------------------------------------------------------------

/// 一份场景脚本（TOML）。
///
/// 例子见 `tests/scenarios/`；字段拼错会直接报错，不会静默忽略。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Script {
    /// 场景元信息。
    pub scenario: ScenarioMeta,
    /// 输入段（可重叠，重叠处按位或）。
    #[serde(default)]
    pub input: Vec<InputSegment>,
    /// 断言。
    #[serde(default)]
    pub expect: Vec<Expect>,
}

/// 场景元信息。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioMeta {
    /// 总 tick 数（1-based：跑 `ticks` 次 `kernel.tick()`）。
    pub ticks: u64,
    /// 期望加载的地图名（写错说明 `--map`/默认值不对，属于配置错误）。
    #[serde(default)]
    pub map: Option<String>,
    /// 把逐 tick 轨迹写到这里（JSONL）。
    #[serde(default)]
    pub trace: Option<PathBuf>,
    /// 给轨迹/断言写一句人读的说明。
    #[serde(default)]
    pub note: Option<String>,
}

/// 一段输入：`from..=to`（1-based tick，含两端）期间按住这些键。
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
    /// 该段的按键状态。
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

    /// 是否覆盖 `tick`（1-based，含两端）。
    pub fn covers(&self, tick: u64) -> bool {
        self.from <= tick && tick <= self.to
    }
}

/// 一条断言：在 `tick`（1-based，与轨迹同轴）检查给到的字段。
///
/// 只检查**写了**的字段；浮点比较用 `tol`（默认 [`DEFAULT_TOL`]）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    pub tick: u64,
    /// 浮点容差（px）。默认 0.05。
    #[serde(default)]
    pub tol: Option<f32>,
    #[serde(default)]
    pub x: Option<f32>,
    #[serde(default)]
    pub y: Option<f32>,
    /// 相对 tick 1 的位移（px）：跨地图/出生点变化都稳定，行为回归首选。
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

/// 浮点断言默认容差（px）。
pub const DEFAULT_TOL: f32 = 0.05;

/// 场景回放结果。
#[derive(Debug, Default)]
pub struct ScenarioOutcome {
    /// 跑的 tick 数。
    pub ticks: u64,
    /// 检查过的字段数。
    pub checks: usize,
    /// 失败的检查（人读一行一条）。
    pub failures: Vec<String>,
}

impl ScenarioOutcome {
    /// 一行总结（成功路径也会打印）。
    pub fn summary(&self) -> String {
        format!(
            "scenario: {} ticks, {} checks, {} failure(s)",
            self.ticks,
            self.checks,
            self.failures.len()
        )
    }
}

/// 读一份场景脚本。
pub fn load_script(path: &Path) -> Result<Script> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read the scenario {}", path.display()))?;
    toml::from_str(&text)
        .with_context(|| format!("failed to parse the scenario {}", path.display()))
}

/// 计算 `tick` 时刻的输入：所有覆盖它的段按位或。
pub fn input_at(script: &Script, tick: u64) -> Input {
    script
        .input
        .iter()
        .filter(|s| s.covers(tick))
        .fold(Input::default(), |acc, seg| acc.merge(seg.keys()))
}

/// 回放一份场景：确定性 tick、可选轨迹、断言检查。
///
/// 不装插件、不监听热重载（确定性优先）。断言失败**不中断**回放 ——
/// 一次跑完把所有偏差都列出来，比只报第一条更有用。
pub fn run_script(
    script: &Script,
    session: Session,
    paths: &Paths,
    map_name: &str,
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

    // 确定性：无插件、无 watcher。
    let service = GameService::new(session, None, None, None);
    let mut kernel = Kernel::new().with(service);
    let mut outcome = ScenarioOutcome::default();
    // 位移断言（dx/dy）的基准：tick 1 的位置（出生点站稳的位置）。
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
        for expect in script.expect.iter().filter(|e| e.tick == tick) {
            check(&snap, base, expect, &mut outcome);
        }
        outcome.ticks = tick;
    }

    if let Some(trace) = trace {
        trace.finish()?;
    }
    kernel.shutdown();
    Ok(outcome)
}

/// 逐字段比对一条断言，把偏差写进 `outcome`。
///
/// `base` 是 tick 1 的位置，供 `dx`/`dy` 相对位移断言用。
fn check(snap: &Snapshot<'_>, base: (f32, f32), expect: &Expect, outcome: &mut ScenarioOutcome) {
    let tol = expect.tol.unwrap_or(DEFAULT_TOL);
    let tick = expect.tick;

    let mut float = |name: &str, got: f32, want: Option<f32>| {
        let Some(want) = want else { return };
        outcome.checks += 1;
        if (got - want).abs() > tol {
            outcome.failures.push(format!(
                "tick={tick} {name}: expected {want:.4}, got {got:.4} (Δ{:.4} > tol {tol:.4})",
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
// 地图快照
// ---------------------------------------------------------------------------

/// 把运行时看到的会话落成 JSON。
///
/// 与 `releste-map --example dump_rooms`（原始文件视图）互补：这里dump 的是
/// **运行时解析后**的东西 —— 出生点、图集名、tile 网格（按行十六进制），
/// 以及与 `.map` 有偏差的地方一眼可见。
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

    /// 重叠段按位或：左+右同时按住时输入两个键都置位（引擎里
    /// `axis_x` 会判 0，但脚本层的合并规则必须明确）。
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
        assert!(merged.right, "tick 2 同时落在两段里");
        assert!(merged.jump);
        assert!(input_at(&script, 3).jump);
        assert!(!input_at(&script, 4).jump);
    }

    /// 拼错的键必须报错，不能静默当默认值（§15.9）。
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

    /// 浮点断言用容差，失败信息要带 Δ 与 tol。
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
        assert!(outcome.failures.is_empty(), "0.02 < 0.05 容差");

        let bad: Expect = toml::from_str("tick = 7\nx = 10.5\nanim = \"runSlow\"").expect("parse");
        check(&snap, (10.0, 20.0), &bad, &mut outcome);
        assert_eq!(outcome.checks, 3);
        // dx 是相对 tick 1 的位移：跨出生点稳定。
        let rel: Expect = toml::from_str("tick = 7\ndx = 3.0").expect("parse");
        check(&snap, (7.0, 20.0), &rel, &mut outcome);
        assert_eq!(outcome.checks, 4);
        assert_eq!(outcome.failures.len(), 2);
        assert!(
            outcome.failures[0].contains("Δ0.5000 > tol 0.0500"),
            "got {:?}",
            outcome.failures[0]
        );
        assert!(outcome.failures[1].contains("anim"));
    }
}
