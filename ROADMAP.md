# Releste Roadmap

Legend: ☐ not started · ◐ in progress · ☑ done

---

## Definition of Done

The rewrite is finished when all of the following hold:

1. **`nix run` and `cargo r` both launch the game directly**, no arguments needed, from a clean checkout.
2. **The running game is recognizably close to vanilla Celeste** — same rooms, same movement feel (dash, wall-jump, climb, grab), same visual language.
3. **Everything is a plugin.** Entities, rendering passes, audio backends, input maps, map formats, UI panels, CLI subcommands and log sinks all load from `./plugins` — the engine core ships none of them.
4. **Everything hot-plugs, and everything lazy-loads.** Drop a file into `./plugins` and it becomes available — `eager` at boot, `on-demand` on first use, `when-namespace:<ns>` the moment that area opens. Delete it and it unloads — **while the game is running, with state preserved, without stalling a frame**. No recompile, no restart, no Mixin-style runtime patching.
5. **The shipped directory looks like [Appendix C](#appendix-c--distribution-layout)** — `releste`, `plugins/`, `maps/`, `resources/`, `libs/`.
6. **Logs are readable by a human** without a manual: coloured, aligned, multi-line safe (spec in [Appendix A](#appendix-a--log-format-spec)).
7. **The CLI explains itself** — `--help` is enough to use it, flags are consistent across subcommands (spec in [Appendix B](#appendix-b--cli-spec)).

> **Guiding principle: 一切皆插件，万物热插拔** — everything is a plugin, everything hot-swappable. If adding a feature requires editing `crates/`, the design is wrong; the feature belongs in `plugins/`.

---

## Baseline (honest assessment)

Updated 2026-10-09 after verifying against the tree, not from memory. Every row
below was re-measured: `cargo build --workspace` clean, `cargo test --workspace`
**254 passed / 0 failed**, `cargo run --bin releste -- --headless --frames 600`
exits 0, and `nix develop --command cargo test --workspace --features
reles-script/lua,reles-input/gamepad` gives **268 passed / 0 failed**.

The engine's parts exist and are well-tested, and the keystone binary now exists
and runs. What is missing is *gameplay*: the vanilla feel of Phase 4.

| Area | State | Evidence |
|---|---|---|
| 14-crate workspace + tools | ☑ | `unit-tests` 254, full-feature 268, `nix flake check` green |
| Math / kernel / world / map / reload | ☑ | fixed-point `Fx`, blake3 stable IDs, patch + rollback verified |
| Content pipeline | ◐ | 22 atlases / 27 maps / 10 dialogs converted; packed `.data` pages decoded to PNG; **per-sprite `PackerNoAtlas` pages still need a multi-page packer + per-sprite runtime binding** |
| FMOD audio | ☑ | real `libfmodstudio.so.10` loaded, 527 events, bank played |
| Rendering | ☑ | `reles-render` consumed by `crates/game`; per-group textures, UV lookup, nearest sampling, autotiled tiles |
| **Playable game binary** | ◐ | **`releste` bin exists and boots a real room** (`crates/game`); vanilla gameplay (dash / wall-jump / climb / entities) still absent |
| **Windowed main loop** | ☑ | winit `ApplicationHandler` + `FIXED_DT` accumulator + interpolation, FPS/room title, `Esc` pause, graceful shutdown |
| **Plugin system** | ◐ | `.so` path works end to end: `plugin.toml` scan → `#[repr(C)]` vtable → `Fiber` states → frame-boundary unload → LIFO `Scope::effect`. **`.wasm` (the primary format, §3.2) is not implemented**; `plugins/` is not yet watched for hot-plug |
| **Entity factory registry** | ◐ | `Registry::register_kind` + `kind_id_for` exist and resolve `.map` kinds; host-side wiring of gameplay factories is Phase 4 |
| **Shipped directory layout** | ◐ | `plugins/` exists and is scanned; `Paths` already resolves `maps/<ns>/` and `resources/<ns>/atlas` with fallbacks; no `libs/` yet, assets still land in flat `assets/` |
| **Logging** | ◐ | `crates/game/src/logging.rs` implements Appendix A (right-aligned tags, `module>` in bright grey, continuation indent, `NO_COLOR`/non-TTY, `FATAL` panic hook); the other 3 bins still use default `fmt()` |
| **CLI** | ◐ | `releste` has `--root/--no-plugins/--headless/--frames/-v/--log/--no-color` and Appendix B error handling; the `play/edit/serve/convert` tree is still Phase 4 |
| **`cargo r`** | ◐ | `default-members = ["crates/game"]` makes bare `cargo run` launch the game; `.cargo/config.toml` adds the `r` alias |
| **`nix run`** | ◐ | `apps.<system>.default` + `meta.mainProgram` exist and point at the same `releste` bin; runtime assets (`assets/`, `libfmod.so`) still have to be provided next to it |
| §13 TODOs | ◐ 1/6 | see [Phase 6](#phase-6--leftovers-from-agentsmd-13-) |

**Biggest structural gap:** the crates are now linked together and the game
boots, but nothing in it is *vanilla Celeste* yet — Phase 4 is the keystone from
here.

---

## Phase 1 — Human-facing surface (logs + CLI)

Everything after this phase is easier to debug, so it goes first.

### 1.1 Custom log formatter ◐

Replace the default `tracing_subscriber::fmt()` with a layer implementing [Appendix A](#appendix-a--log-format-spec).

- Right-aligned level tag so `]` always lands at the same column.
- `module> ` prefix in grey; message coloured by level.
- Continuation lines of a multi-line message indented to the message start column.
- `RUST_LOG` still honoured via `EnvFilter`.
- Works when stdout is not a TTY (no ANSI escape codes — detect or honour `NO_COLOR`).

Deliverable: a `reles-log` module (or `crates/kernel/src/log.rs`) plus a snapshot test asserting the exact bytes of each level and a multi-line message.

**Done:** the layer lives in [`crates/game/src/logging.rs`](crates/game/src/logging.rs)
(`AppendixA` event formatter + `print_fatal` panic hook), wired into `releste`.
It is still game-only, so the remaining work is lifting it into a shared crate
(or `crates/kernel/src/log.rs`) and giving the other three bins
(`editor`, `net-server`, `content-pipeline`) the same formatter — and adding the
byte-exact snapshot test the deliverable asks for.

### 1.2 Purge raw output ◐

Convert all `println!`/`eprintln!` in `crates/` and `tools/` to the structured logger.

- Library crates: `tracing::{trace,debug,info,warn,error}` only. **No printing from libraries** — that is already a rule, it just is not enforced.
- Reserve `println!` for genuinely user-facing command output (CLI tables, `--verify` reports).

Acceptance: `grep -rn 'println!\|eprintln!' crates/` returns only documented, user-facing call sites.

**Current census (58 hits, all accounted for):** 22 are CLI/user-facing output
(`tools/content-pipeline/src/main.rs` 19, `crates/editor/src/main.rs` 3),
14 are the FMOD `build.rs` probe report (a build script has no `tracing`
subscriber), 18 sit inside `#[cfg(test)]` modules printing fixture diagnostics
(`fmod_studio.rs`, `fmod_backend.rs`, `data.rs`, `tilesets.rs`, `engine.rs`,
`autotiler.rs`), and 2 are the `--log` parse failure that exists precisely
because the subscriber is not up yet. **No library prints on its non-test
path.** What is left: document that census in-place, and add the two missing
user-facing reports (`convert` progress tally, `verify` summary — Appendix B
rule 4).

### 1.3 Unified CLI ◐

One clap command tree shared by all binaries — see [Appendix B](#appendix-b--cli-spec).

- Common flags (`--log`, `--verbose`, `--no-color`, `--config`) behave identically everywhere.
- Every error message ends with a suggested next action.
- Unknown flags print "did you mean …?" via clap's `suggestions` feature.

### 1.4 One-command entry points ☑

| Goal | Work | State |
|---|---|---|
| `cargo r` | add `[alias] r = "run"` to `.cargo/config.toml`; set `default-members = ["crates/game"]` so bare `cargo run` works from a virtual workspace | ☑ `[alias] r = "run"` + `default-members` both in place |
| `nix run` | add `apps.<system>.default = { type = "app"; program = … }` and set `meta.mainProgram` on `packages.default` | ☑ `apps.${system}.default` wraps `packages.default`, which carries `meta.mainProgram = "releste"`. Verified: `nix flake show` emits `apps` on all platforms and `nix run --impure . -- --root <tree> --headless` boots the real room |
| both launch the same binary | `nix run` must wrap the very same `releste` bin `cargo r` runs | ☑ both resolve to the `releste` bin built from `crates/game` |

Acceptance: from a fresh clone, `nix run .` and `cargo r` both open the game window. Document both in `README.md`.

**Still open for the acceptance line:** the nix store build ships the binary
only — `plugins/`, `assets/` and `libfmod.so` are gitignored by design
(Appendix C rule 6), so `nix run .` from a fresh clone opens a window with no
rooms until you point `--root` at a populated tree — which is why the verified
invocation above passes `--root`. The final README documentation still has to
spell that out, and a check should assert the two entry points cannot drift.

---

## Phase 2 — Playable runtime (`releste` binary) ◐

**This is the keystone.** The binary exists and boots a real room; the vanilla
game on top of it is Phase 4.

### 2.1 New crate `crates/game` ☑

Becomes the only consumer that wires the engine together:

```
reles-kernel  ← main loop, ServiceRegistry, EventBus
reles-input   ← 1kHz sampling, virtual buttons
reles-world   ← simulation
reles-map     ← load .map
reles-reload  ← FileWatcher + Patch apply
reles-render  ← wgpu (currently a leaf — first real consumer)
reles-audio   ← FMOD / NullBackend
reles-script  ← CutsceneBridge
```

`crates/game` exists with `main` / `app` / `service` / `session` / `player` /
`paths` / `sprites` / `logging`. It consumes `reles-kernel`, `reles-math`,
`reles-map`, `reles-reload`, `reles-render` and `reles-plugin-host`;
`reles-input`, `reles-audio` and `reles-script` are wired in Phase 4.

### 2.2 Windowed loop ◐

- `winit` `EventLoop` drives `kernel::tick()`; fixed `FIXED_DT = 1/60` accumulator with interpolation for rendering. ☑
- Frame pipeline: poll input → step sim → drain `SimCommand` → rebuild batches → submit to wgpu. ☐ (`SimCommand` draining lands with `reles-script` in Phase 4)
- Window title shows FPS and current room; `Esc` opens an in-game menu. ◐ title shows room / FPS / sprite count / plugin count; `Esc` toggles pause, but there is no menu UI yet.
- Graceful shutdown on window close and on `SIGINT` (flush audio, drop GPU resources). ◐ close + `Drop` teardown are in place; explicit `SIGINT` handling is not.

### 2.3 Headless mode `--headless` ◐

Runs the same loop with a null renderer and no window, for CI and determinism checks.

Acceptance: `cargo r -- --headless --frames 600` exits 0 and prints a frame report. Wired into `nix flake check` as `checks.runtime-smoke`.

**Measured:** `cargo run --bin releste -- --headless --frames 600` → exit 0,
`ok: 600 ticks in 470.30µs (0 us/tick); 7 rooms, 52 entities; spawn room lvl_-1; 1 plugins; 0 hot reloads`.
What is missing is only the `checks.runtime-smoke` wiring in the flake.

### 2.4 Map boot ◐

Load a converted map, place the player at a spawn point, render the tiles.

Acceptance: launching the game shows a real Celeste room with the player standing in it, at 60 FPS.

**Measured:** `0-Intro.map` loads (7 rooms, 52 entities), the player is placed
at a spawn, tiles render with autotiled per-tile texture coordinates. The FPS
half of the acceptance line has not been measured on this machine yet (that
needs a display, not a headless run).

---

## Phase 3 — Plugin system (`./plugins`) ◐

Per AGENTS.md §15 the engine must not hardcode vanilla gameplay. Taken to its conclusion: **the engine core ships no gameplay, no renderer specifics, no audio backend — it is a plugin host.**

Design target: **drop a file in, it works; delete it, it's gone — all while running.** No recompilation, no restart, no Mixin-style bytecode patching.

### 3.1 The core is a host, not a game

```
┌──────────── releste (static, rarely rebuilt) ────────────┐
│  plugin loader  ·  lifecycle  ·  service registry        │
│  event bus      ·  fixed-step loop  ·  ABI handshake     │
└───────────────────────────┬──────────────────────────────┘
                            │  stable capability interfaces
        ┌───────────────────┼───────────────────┐
        ▼                   ▼                   ▼
   entity kinds        render passes        audio backend
   map loaders         input maps           UI panels
   CLI subcommands     log sinks            resource namespaces
```

Everything below that line comes from `./plugins`. If a feature needs `crates/` edited, it belongs in `plugins/` instead.

### 3.2 Two formats, one contract

`plugins/` accepts both `.wasm` and `.so` (per [Appendix C](#appendix-c--distribution-layout)):

| | `.wasm` (primary) | `.so` / `.dll` / `.dylib` (escape hatch) |
|---|---|---|
| Safety | sandboxed, no host memory access | full process access |
| ABI | WIT/component-model, **versioned and stable** | `#[repr(C)]` vtable — **manually frozen** |
| Hot-plug | trivial (instantiation) | needs careful unload |
| Use for | gameplay logic, entities, UI, map rules | renderer backends, audio DSP, platform glue |
| Portability | one artifact everywhere | per-OS builds |

**Rule:** gameplay must be expressible in `.wasm`. Native `.so` exists only for what wasm cannot reach (GPU, audio device, filesystem). This is what keeps "no recompile" achievable for 99% of changes.

**State: `.so` only.** [`crates/plugin-host/src/abi.rs`](crates/plugin-host/src/abi.rs)
freezes the `#[repr(C)]` vtable (`HostApi`, `PluginV1`, `ABI_VERSION`,
`ENTRY_SYMBOL`), and [`plugins-src/clock`](plugins-src/clock) is a real
standalone cdylib built against it. There is **no wasm runner at all** — no
`wasmtime` in `[workspace.dependencies]`, no WIT surface, no instantiation path.
Per AGENTS.md §15.1 that import is a discussion first, so the `.wasm` column is
explicitly blocked on an issue rather than on code.

### 3.3 Hot-plug lifecycle ◐

```rust
pub trait Plugin: Send + Sync {
    /// ABI version the plugin was built against; host rejects mismatches.
    const ABI: u32;
    fn name(&self) -> &str;
    /// Called once on load, before any frame runs.
    fn load(&mut self, host: &mut Host) -> Result<(), PluginError>;
    /// Called every tick while active.
    fn tick(&mut self, ctx: &mut PluginCtx);
    /// Called before unload so the plugin can hand back owned state.
    fn unload(&mut self, host: &mut Host) -> Result<(), PluginError>;
}
```

- `Host` exposes capability registration: `register_entity_factory`, `register_render_pass`, `register_audio_backend`, `register_input_map`, `register_cli_command`, `register_log_sink`, `register_map_loader`, `register_resource_namespace`.
- **Load** = read dir → handshake → `load()` → capabilities live.
- **Unload** = `unload()` → revoke capabilities → migrate orphaned state (entities re-homed or despawned per policy) → release memory.
- **Reload** = unload + load in one step, preserving world state — the same machinery as map hot-reload (§4.3), so player/carry state survives a plugin swap too.
- Watch `./plugins` with the existing `FileWatcher` (100ms debounce) — the same diff→Patch→apply path already proven for maps.

**Decision (agreed): unload is *handed over* at a frame boundary and drains asynchronously.** The boundary revokes capabilities and marks the fiber `UNLOADING`; the `FnOnce` disposers then run LIFO on a teardown task so **the frame never stalls**. Releasing early or mid-tick is UB in Rust and a use-after-free in `.so` — the boundary is what makes ownership provable. Full rules in [Appendix D](#appendix-d--plugin-contract-cordis-inspired) (D.3 teardown, D.6 async, D.7 lazy).

### 3.4 Registration & resolution ◐

```rust
// What the registry needs today (see baseline): the kind → factory map.
// `Registry::register_kind` + `kind_id_for` exist; the remaining missing
// piece is `Session`/`world` consuming them on `.map` load (Phase 4).
registry.register_kind(EntityKind { .. });  // EntityKindId → EntityKind
```

- A `.map` references `EntityKindId`; the host resolves it against whatever plugin currently provides it.
- **Unknown kind → `tracing::warn`, skip, continue.** Never panic (§15.8/15.9). Removing a plugin mid-session degrades the world instead of crashing it.
- Stable IDs are unaffected: `blake3(area_id || room_name || kind_id || x || y)` does not care which plugin supplies the kind.

### 3.5 What is *not* a plugin (and why)

| Stays in core | Reason |
|---|---|
| Fixed-point `Fx` math | every plugin must agree on simulation arithmetic (§15.7) |
| `EventBus` / `ServiceRegistry` / `FrameId` | the substrate plugins register into (§4.1) |
| The `.map` **container** (magic + version + migration) | §4.2 "maps are data" needs one canonical envelope |
| Content-pipeline as a **build tool** | §4.6 — original formats only at build time; plugins may not parse `.xnb`/`.meta` |

Gameplay, presentation, I/O and tooling surface: **all plugins.**

### 3.6 Acceptance ◐

- `rm -rf plugins/` → engine boots to an empty world with warnings, **no crash**. ☑ (`boot_plugins` warns-and-continues; `--no-plugins` also exists)
- Drop `plugins/player.wasm` into a running game → player becomes controllable **without restart**. ☐ (wasm column is empty, §3.2)
- Delete it again → world despawns the player per unload policy, session survives. ☐
- Add a new entity kind in a plugin → a `.map` using it loads, **zero edits to `crates/`**. ◐ registry keys exist (`Registry::register_kind`, `kind_id_for`); no plugin registers gameplay kinds yet and `Session` does not resolve them (Phase 4)
- Swap the audio backend plugin while music plays → no audio glitch beyond the documented handover. ☐ (audio is a service, not yet a plugin)
- `cargo test` passes with `plugins/` empty — the host has no built-in gameplay to depend on. ☑

---

## Phase 4 — Vanilla parity (the game actually plays) ◐

Depends on Phase 3 — **except §4.1**, which is pure `crates/game` sim work and is
therefore in progress while the wasm half of Phase 3 waits on the AGENTS.md
§15.1 dependency discussion (§3.2).

### 4.1 Player ◐

The whole feel lives here — this is what "接近原版" means.

- ☑ **Run** (ramps to 1.5 px/tick at 1000 px/s² ground / 650 airborne; release & turn-around bleed off at 400 px/s²), **variable jump height** (release cuts rise ×0.4), **coyote time** (6 ticks), **jump buffering** (6 ticks). All in `crates/game/src/player.rs`, all `Fx`, each covered by a test that goes red when the feature is deleted. Constants are first-pass, not reference-matched.
- ☑ **Gravity / fall（2026-10-17，对照 `references/pureCeleste.cs` Player.Update）**：重力 900 px/s²；落速上限是**缓动状态** `maxFall`（160 ↔ 240 px/s，`FastMaxAccel = 300`，按住"下"且已在加速下落时爬升、松开回落）；顶点半重力（`|vy| < 40 px/s` 且按住跳跃 → 重力减半）。覆盖测试：`fall_speed_caps_at_vanilla_max_fall`、`holding_down_ramps_the_fall_cap_to_fast_fall_and_back`、`gravity_is_halved_near_the_apex_while_jump_is_held`。同一轮修掉了房间贴缝行走被夹取拽 ≈4px 的瞬移（兜底改为"中心不在任何房间才夹，回最近房间"）。
- ☐ **Jump 仍是一段式冲量**（-240 px/s + 松键 ×0.4 削减）—— 原版是 `JumpSpeed = -105` + varJump 钳制（按住跳跃 0.2s 内上升速度钉在 -105）+ `JumpHBoost = 40` 水平助推；这是手感对齐的下一项。
- ☐ Dash (8 directions, dash refresh on ground/wall, dash attacks)
- ☐ Wall jump, wall slide, wall climb, stamina
- ☐ Grab / hang, super dash, hyper dash
- ☐ Death, respawn, strawberry / winged strawberry collection
- Standing gap flagged in the baseline: `crates/game` still carries its own `Input` struct and does **not** use `reles-input`'s `InputManager`/`InputFrame`; wiring the real input crate in is what makes rebinding and 1 kHz sampling apply to gameplay.
- Commit step-by-step against reference recordings, not by eyeballing

### 4.2 Core entity families, one plugin each

- Solids: `crumble`, `moving-platform`, `ice`, `jump-thru`
- Hazards: `spikes`, `saw`, `falling-block`
- Collectibles: `strawberry`, `key`, `heart`, `cassette`
- Triggers: `intro`, `text`, `camera-stretch`, `flag`
- NPCs: `badeline`, `theo`, `granny`

### 4.3 Areas

Load all converted areas from `assets/maps/`, room transitions, checkpoints, and the overworld.

Acceptance: **playable from Area 1 start to a C-side completion**, no editor needed, no crash across a full session.

---

## Phase 5 — Content fidelity ◐

### 5.1 Pixel decode of the remaining `.data` payloads (AGENTS.md §13.1) ◐

- **What was already solved:** packed atlases (`AtlasData.Packer`) are decoded
  to PNG at build time by the pixel-RLE decoder
  ([`tools/content-pipeline/src/data.rs`](tools/content-pipeline/src/data.rs)),
  which also corrected the record: the `.data` files are **not Crunch** —
  Monocle's `Crunch*` branches actually read `.png`.
- **What is still open:** `PackerNoAtlas` pages (one `.data` per sprite) are
  still payload-copied. They need a multi-page packer plus per-sprite texture
  binding at runtime.
- Golden tests against known vanilla sprites.

### 5.2 MSAA / visual parity (AGENTS.md §13.6) ◐

- `render/gpu.rs` already has `msaa_samples` but hardcodes `multisampled: false` at line 233 — wire it through the pipeline.
- Note the existing comment: MSAA smears pixel art. Evaluate MSAA **and** integer-scaling/letterboxing before committing; parity may mean *no* MSAA plus correct upscale.
- `GpuRendererConfig` defaults `msaa_samples: 1` (MSAA off) and the nearest
  sampler is already in place; the open question is the integer-scale /
  letterbox path and a `multisampled` override wired through the pipeline.

### 5.3 Audio polish

- Ducking, snapshots (the 21 `snapshot:/` events found during FMOD testing), music transitions per room.

---

## Phase 6 — Leftovers from AGENTS.md §13 ◐

1/6 done (13.1 is effectively resolved — see Phase 5.1).

| # | Item | Phase | State |
|---|---|---|---|
| 13.1 | content-pipeline pixel decode (`Packer`) | 5.1 | ☑ packed pages decoded to PNG; `PackerNoAtlas` still payload-copied |
| 13.2 | Reload Lua chunk separately (today: whole map) | 6.1 | ☐ |
| 13.3 | Carry protocol disconnect handling | 6.2 | ☐ |
| 13.4 | Server heartbeat and timeout | 6.2 | ☐ |
| 13.5 | Editor schema panel perf on large rooms | 6.3 | ☐ |
| 13.6 | MSAA / visual parity | 5.2 | ◐ nearest + MSAA off default; letterbox path open |

- **6.1 Hot-reload Lua** — reload only the changed chunk, preserve `PendingCommands` and bridge state.
- **6.2 Multiplayer hardening** — application-level heartbeat (laminar's `SocketEvent::Timeout` is transport-only), reconnect, and carry-state cleanup when a peer drops.
- **6.3 Editor perf** — virtualize the entity panel; measure with a 500-entity room.

---

## Appendix A — Log format spec

### Layout

```
[TRACE] kernel> message
[DEBUG] render> batch flushed in 0.42ms
 [INFO] module_name> information
 [WARN] system> multi-line message
                continuation aligns to message start
[ERROR] wasm> plugin load error
[FATAL] kernel> panic!
```

**The alignment anchor is the tag's right bracket `]`.** Every tag is right-aligned into a fixed 7-column field, so `]` always lands at column 6 (0-indexed):

| Tag | Width | Leading spaces | `]` index |
|---|---|---|---|
| `[TRACE]` | 7 | 0 | 6 |
| `[DEBUG]` | 7 | 0 | 6 |
| `[INFO]` | 6 | 1 | 6 |
| `[WARN]` | 6 | 1 | 6 |
| `[ERROR]` | 7 | 0 | 6 |
| `[FATAL]` | 7 | 0 | 6 |

All six tags are **7 columns wide including padding**, so `]` is at index 6 for every one of them.

After the tag: **one space**, then `module>`, then **one space**, then the message.

```
column:  0123456789…
         [WARN] system> message
                 ^
                 message starts at 10 + len(module)
```

### Multi-line messages

Continuation lines are indented to the message start column of *that* line, so the text stays in one visual block:

```
 [WARN] system> first line
                second line
```

For `module = "system"` the message starts at `10 + 6 = 16`, hence 16 leading spaces.

### Colours

| Element | Colour |
|---|---|
| Level tag | level colour |
| Message | level colour (explicitly same as tag for `TRACE`) |
| `module>` | **bright grey** (ANSI bright-white, `\e[97m`) regardless of level |

Level colours (match `tracing`'s own palette so it stays familiar):

| Level | ANSI | RGB |
|---|---|---|
| `TRACE` | `\e[35m` | purple |
| `DEBUG` | `\e[34m` | blue |
| `INFO` | `\e[32m` | green |
| `WARN` | `\e[33m` | yellow |
| `ERROR` | `\e[31m` | red |
| `FATAL` | `\e[91m` | bright red |

Reset with `\e[0m` at the end of each coloured run.

### Non-TTY / `NO_COLOR`

When stdout is not a terminal, or `NO_COLOR` is set: emit the same layout **without** any escape codes. Alignment rules are unchanged — the padding that makes `]` line up must still be there.

### Levels

`FATAL` is above `ERROR` and is reserved for unrecoverable shutdown (panic, failed init). Map `tracing`'s five levels onto `TRACE/DEBUG/INFO/WARN/ERROR`; emit `FATAL` from the panic hook and from top-level error handling in `main`.

---

## Appendix B — CLI spec

### Entry points

```bash
cargo r                      # same binary as `nix run .`
nix run .                    # ditto
cargo r -- <subcommand>
nix run . -- <subcommand>
```

### Command tree

```text
releste [GLOBAL FLAGS] <COMMAND>

  play        Launch the game window (default when no COMMAND is given)
  edit        Open the map editor            (wraps reles-editor)
  serve       Run the relay server            (wraps reles-net-server)
  convert     Run the content pipeline        (wraps reles-content-pipeline)
  verify      Validate converted assets / maps
  probe-fmod  Report FMOD library generation and available symbols
  dev         Developer tools (subcommands: --inspect-plugins, watch, bench)
```

### Global flags — identical on every subcommand

| Flag | Meaning |
|---|---|
| `-v`, `--verbose` | raise verbosity (repeatable: `-vv` → debug, `-vvv` → trace) |
| `--log <LEVEL>` | explicit level; overrides `RUST_LOG` |
| `--log-file <PATH>` | also write logs to a file |
| `--no-color` | disable ANSI (also honours `NO_COLOR`) |
| `-c`, `--config <PATH>` | config file; default `./releste.toml` |
| `-h`, `--help` | help for this command **plus** the global flags |
| `-V`, `--version` | version |

### Human-friendliness rules

1. **`--help` must be sufficient.** No flag exists without a one-line explanation and, where non-obvious, an example.
2. **Every error names the fix.** Not `FMOD library not found.` but the full AGENTS.md §4.7 text with download URL and per-OS placement paths.
3. **Unknown flag → suggestion.** Enable clap's `suggestions` feature: ``error: unexpected argument '--verbse' found; tip: a similar argument exists: '--verbose'``.
4. **Long-running commands print progress, not silence.** `convert` shows a per-asset tally; `verify` ends with `OK: 27 maps, 824 rooms, 56440 entities`.
5. **Distinguish user error from bug.** User errors (missing file, bad flag) → exit 1 with a message; internal errors → exit 2 with a `RUST_BACKTRACE=1` hint.
6. **No emoji-only status.** Pair every ✓/✗ with a word, so logs survive terminals with poor glyph support.
7. **Dry-run where it matters.** `convert --dry-run` and `edit --read-only` must exist before Phase 4 ships.

---

## Appendix C — Distribution layout

The shipped tree. `cargo r` and `nix run .` must find everything relative to the executable — no absolute paths, no `$HOME` lookups.

```text
releste[.exe]              # single static host binary (Phase 2)

plugins/                   # EVERYTHING loadable lives here
  <name>/
    plugin.toml            # manifest: abi, load mode, provide/inject, config
    <name>.wasm            # gameplay, entities, UI, map rules
    <name>.so              # renderer/audio/platform escape hatch
    <name>.dll  /  <name>.dylib
  README.md                # how to write a plugin

maps/                      # one directory per namespace
  <namespace>/*.map        # namespace matches the atlas namespace
  <namespace>/…

resources/                 # material packs, one directory per namespace
  <namespace>/             # concrete pack contents
    atlas/*.atlas + *.png
    dialog/*.toml
    fonts/*.font
    audio/*.bank
    …

libs/                      # user-provided native libraries
  libfmod.so  ·  fmod.dll  ·  libfmod.dylib
  libfmodstudio.so  ·  …
```

### Plugin manifest

Each plugin carries a manifest beside its artifact. This is where `inject` / `provide` / load mode / config schema are declared (contract in [Appendix D](#appendix-d--plugin-contract-cordis-inspired)):

```toml
# plugins/player/plugin.toml
name    = "player"
version = "0.1.0"
abi     = 1                      # host rejects a mismatch
entry   = "player.wasm"          # or player.so
load    = "eager"                # eager | on-demand | when-namespace:<ns>

provide = ["player", "spawn"]    # services this plugin makes available
inject  = ["render", "audio"]    # services it needs (reactive — D.4)

[config]
schema = "player.schema.json"    # validated by the host BEFORE load
```

The manifest is **data, not code** (§4.2 applies to plugins too) — it can be read without instantiating anything, which is what makes lazy loading and `--inspect-plugins` possible before a plugin ever runs.

### Rules

1. **Namespace everywhere.** `maps/<namespace>/` and `resources/<namespace>/` use the *same* namespace string, so an atlas and the rooms that use it resolve together. A missing namespace is a warning, not a crash.
2. **`libs/` is searched first**, then the OS default paths (`LD_LIBRARY_PATH`, next to the exe). Satisfies AGENTS.md §4.7 without hardcoding `/usr/local/lib`.
3. **`plugins/` may be empty.** The host boots, warns, and runs (see Phase 3.6).
4. **Executable-relative resolution.** Derive the root from `std::env::current_exe()` so a relocated install keeps working. Ship a `--root <path>` override for packagers.
5. **`plugins/` and `resources/` are content-addressed at runtime**, not compiled in — that is what makes them hot-pluggable.
6. **Git ships none of it.** `.gitignore` already excludes `assets/`, `*.so`, `libfmod*`; extend it to `resources/`, `libs/` and any generated `plugins/**/*.so`. What stays in VCS: your authored `maps/<namespace>/*.map`, the `plugin.toml` manifests, and the plugin *sources*.
7. **A manifest is readable without loading anything** (see above) — `--inspect-plugins` and lazy-loading decisions are made from `plugin.toml` alone, before any wasm instantiation or `dlopen`. A manifest that requires executing code to be understood is a design failure.
8. **Unknown `abi` or malformed manifest → skip that plugin with a `[WARN]`, never abort the scan.** One broken plugin must not hide the other N.

### Migration from the current tree

| Today | Target |
|---|---|
| `assets/` (flat) | `resources/<namespace>/` |
| `assets/maps/*.map` | `maps/<namespace>/*.map` |
| repo-root `maps/example_*.lua` (examples) | `maps/<namespace>/` or `examples/` |
| FMOD lib on `$LD_LIBRARY_PATH` | `libs/` first, then fallback |
| *(no plugins)* | `plugins/` |

`content-pipeline` gains `--resources-root` and `--namespace` flags so its output lands directly in the target layout — this is a Phase 1 CLI change, not a late migration (cheaper to do before content accumulates).

---

## Appendix D — Plugin contract (Cordis-inspired)

**Yes, borrow from Cordis** — but its *shape*, not its code. Cordis (the TS meta-framework behind Koishi) solves the same problem: many independently-loaded units, hot-reloadable, that must tear down cleanly. Its `Fiber`/`effect`/`inject`/epoch model is the right design. What must change is everything about *ownership*, because Cordis runs on a GC and we run on Rust + wasm.

Verified against `cordisjs/cordis` `packages/core/src/{fiber,registry,service}.ts`.

### D.1 The four ideas worth stealing

| Cordis | Why it matters for us | Rust adaptation |
|---|---|---|
| **`Fiber`** — one *instance* scope per `ctx.plugin()`, with a state machine | gives hot-plug a well-defined begin/end instead of ad-hoc flags | `struct Fiber { state: FiberState, … }`, gated to frame boundary |
| **`effect(fn, label)`** — disposers registered against a scope, run in **reverse order**, each carrying a label and a child tree | you can ask *"what will be torn down, and in what order?"* before unloading | `scope.effect(|| …, "wgpu texture atlas")` returning a token |
| **`inject` + epoch** — declared deps; `_refresh()` folds provider uids into an `epoch` string; any change re-runs load/unload automatically | **this is the actual hot-plug engine**: delete `render.wasm`, dependents go *inactive* instead of dangling | `inject: &["render"]`, epoch recomputed on registry change |
| **Config via Standard Schema** — validated before `load`, errors carry a `path` | bad plugin config is a user error with a location, not a panic | serde schema + validation producing `path`-tagged issues |

### D.2 Fiber state machine

```
PENDING ──► LOADING ──► ACTIVE ──► UNLOADING ──► DISPOSED
                │          │            │
                └──────────┴────────► FAILED   (recover only via update())
```

Cordis uses exactly this set (`FiberState.{PENDING,LOADING,ACTIVE,FAILED,DISPOSED,UNLOADING}`). Adopt it verbatim — it already encodes the cases we care about:

- **`FAILED`** — a plugin that throws on load is *marked*, logged, and skipped; it does not take down the host, and it does not silently retry in a loop. Recovery is an explicit `update()`. Maps to §15.8 (no panic) and §15.9 (don't swallow — log it).
- **`UNLOADING` + `inertia`** — Cordis keeps a promise for in-flight teardown and makes `dispose()` await it. Ours is a *frame*: the unload is queued and executes at the next frame boundary; `Fiber::await_()` blocks until that frame completes.

### D.3 `effect()` — the teardown contract

```rust
impl Scope {
    /// Run `disposer` when this scope dies. Registration order is
    /// recorded; disposal happens in REVERSE order (LIFO).
    /// Returns a token; dropping the token early also disposes.
    fn effect<T>(&self, label: &str, disposer: impl FnOnce() -> T + 'static) -> EffectToken;
    /// Everything currently registered, for `releste dev --inspect-plugins`.
    fn effects(&self) -> Vec<EffectMeta>;   // { label, children }
}
```

- **LIFO, exactly as Cordis does** (`disposables.splice(0).reverse()`). Resource A allocated before B must be freed after B — textures before the device, listeners before the bus.
- **`label` is mandatory**, not optional: `EffectMeta { label, children }` gives you a printable teardown tree. Without it, hot-plug debugging is guesswork.
- **A failing disposer is logged and skipped**, not propagated — one bad plugin must not abort teardown of the rest (Cordis wraps each in `composeError` + `logger.error`).
- **Only `FnOnce`.** Rust ownership means a disposer consumes what it closes over; it can be run at most once, which makes double-dispose impossible by construction.
- `effect()` on a non-ACTIVE scope **errors** (`INACTIVE_EFFECT` in Cordis). Adopt: register during `load`, never during `tick`.

### D.4 `inject()` — reactive dependencies

```rust
static INJECT: &[&str] = &["render", "audio"];

fn epoch(&self, registry: &Registry) -> Epoch {
    // "render:3:audio:7"  — provider uid per dependency
    INJECT.iter().map(|n| registry.uid_of(n)).collect()
}
```

- If **every** dependency is present → epoch is stable → fiber stays `ACTIVE`.
- If a dependency **disappears** (its plugin was deleted) → epoch changes → fiber auto-**unloads**.
- If it **reappears** → epoch changes → fiber auto-**loads**.

This is what makes 万物热插拔 work without anyone writing wiring code: drop `plugins/render.wasm` in, the 12 plugins that injected `render` wake up; delete it, they unload cleanly. **No recompile, no restart, no Mixin.**

- Deps are declared as data, never resolved by `import` — that is the whole point.
- An inject list may carry a per-dep config override (Cordis: `Inject = name[] | { [name]: config }`); keep that, it lets two scopes use `render` differently.

### D.5 Config

```rust
#[derive(Deserialize, Schema)]
#[serde(deny_unknown_fields)]
struct Config { speed: Fx, tags: Vec<String> }
```

- Validated **before** `load`, on the host side, so a plugin never sees garbage.
- Errors are `path`-formatted (`- speed: expected Fx in range … (at speed)`) — same presentation Cordis' `ValidationError` produces.
- **Synchronous only.** Cordis explicitly rejects async validation (`"Async config validation is not supported"`). Adopt: config must validate without awaiting, or you cannot report it before load.
- Optional merge/intercept semantics (Cordis `Config.merge`, `intercept`) are deferred — we have no prototype chain to walk.

### D.6 Async teardown (decision: allowed)

**Decision: async disposers are allowed, including for `.so`.** The frame never waits on them.

```
frame N   sim step ──► boundary: revoke capabilities, mark UNLOADING,
                        hand disposers to the teardown task ──► render ──► frame N+1
                                                            (frame proceeds)
          …background…  disposers drain ──► DISPOSED
```

- **Frame boundary is the *handover*, not the completion.** Capabilities are revoked synchronously there, so nothing new can reach the plugin; the actual `FnOnce` disposers run on a teardown task.
- This is Cordis's `inertia` verbatim: `dispose()` returns immediately, `await_()` waits. Tests and shutdown call `await_()`; the frame loop does not.
- **`UNLOADING` is a first-class state** — consumers see a capability as gone the moment it is revoked, even though its memory is still being freed.

**Safety rule that makes async safe:** once capabilities are revoked, a disposer may touch only **its own** resources — buffers, GPU handles, sockets, memory it allocated. It may **not** re-enter the registry, spawn entities, or emit sim events. Rust enforces this by handing the disposer only what it owns, never `&mut Host`.

**Failure policy:**
- Disposer that never settles → after a deadline (default 5s), log `FATAL` with the `label`, mark the fiber `FAILED`, and leak deliberately rather than stall. A stuck plugin must not freeze the game.
- Every disposer is individually wrapped: one failure is logged and skipped, the rest still run (same as Cordis).
- **Shutdown is the exception** — on exit we *do* drain to completion before dropping the host, bounded by the same deadline.

### D.7 Lazy loading

Not every plugin should materialize at boot. Three triggers, declared in the plugin manifest:

| Mode | Loads when | Use for |
|---|---|---|
| `eager` | at startup (default) | `render`, `audio`, `input` — the game cannot run without them |
| `on-demand` | first time some fiber `inject`s it | optional backends, dev tools, editor panels |
| `when-namespace:<ns>` | first time `maps/<ns>/` or `resources/<ns>/` is opened | per-area entity packs — a 50-area game does not load 50 areas' plugins at boot |

**And the matching unload trigger — `unload = "on-world-switch"` (decision):** when the active world/area changes, plugins bound to the *previous* namespace go through the normal `UNLOADING` path (D.6) and the incoming namespace's plugins load. Same mechanism as eviction, just triggered by navigation instead of an idle timer.

- Loads and unloads are **canceled out** if you switch away and back within one frame — the epoch never changes, so nothing churns.
- In co-op (§4.4), the trigger is tied to the **local** world switch; other players' switches must never unload your plugins.
- Transient state handed across a switch follows the existing `Persistence` contract (§7.2) — `Persistent` kinds (player, camera) keep their owning plugin alive across the boundary.

**How it composes with the epoch (D.4):** an `on-demand` provider is *not* a missing dependency. On first `inject`, the registry instantiates it, the epoch settles, and the consumer becomes `ACTIVE` — laziness is invisible to the consumer. Only a dependency that is neither present nor loadable leaves a fiber `FAILED`.

- **First-use cost is bounded.** Lazy load happens on a frame boundary like everything else, with a budget (default 16ms). Exceeding it logs a `[WARN] plugins> lazy load took 42ms: <name>` — lazy must never become a hidden stutter.
- **`releste dev --preload <ns>`** forces materialization for benchmarks and for players who prefer to pay it once at startup.
- **Optional eviction:** `unload_after: 30s` unloads an `on-demand` plugin when nothing references it, going through the exact same `UNLOADING` path (D.6). Symmetric with load — that is what keeps hot-plug honest.
- Lazy is a **load-time** concern only. It never makes a *disposer* lazy: teardown order is always fully registered before any of it runs.

### D.8 Events worth keeping

| Cordis event | Releste use |
|---|---|
| `internal/plugin` (emitted on create **and** dispose) | drives the plugin list UI and the `--inspect-plugins` view |
| `internal/status` (on state change) | the `[INFO] plugins> render → ACTIVE` log line |
| `internal/update` (waterfall, **can veto**) | let a plugin refuse reload — e.g. mid-cutscene, unsaved editor state |

The veto is worth having: it is how "hot-reload must not lose session state" (§4.3) extends from maps to plugins.

### D.9 What we deliberately do **not** borrow

| Cordis | Why not |
|---|---|
| `Context` as a **JS prototype chain** (`extend` / `Object.getPrototypeOf`) | no prototype chains in Rust; use the existing `ServiceRegistry` (AGENTS.md §7.1) |
| Services as callable **Proxy/tracker** objects (`symbols.tracker`, `createCallable`) | a JS metaprogramming trick; Rust gets no benefit and pays compile time |
| GC-based lazy teardown | we must free deterministically → LIFO disposers + frame boundary |
| `Reflect`/`Symbol` keyed stores | replace with typed registry keys |
| Async effects that outlive a tick | an async disposer can still be running when the next frame starts; either await it before the boundary or reject it |

Also keep §4.1 intact: the **kernel** still only knows `Service` / `EventBus` / `FrameId`. Cordis's fat `Context` lives *above* the kernel as the plugin-facing `Host`, so the kernel's dependency set does not grow.

### D.10 Mapping table

| Cordis | Releste |
|---|---|
| `ctx.plugin(Plugin, config)` | `host.load(path, config) -> Fiber` |
| `Fiber` | `Fiber` (same name, same states) |
| `fiber.dispose()` | queued → runs at frame boundary |
| `fiber.restart()` | `reload()` = unload + load, world state preserved |
| `fiber.update(config)` | `update(config)` — the only path out of `FAILED` |
| `ctx.effect(fn, label)` | `scope.effect(label, disposer)` |
| `getEffects()` | `scope.effects()` → teardown tree |
| `plugin.inject` | `static INJECT` |
| `plugin.Config` (Standard Schema) | `#[derive(Schema)]` + host-side validation |
| `plugin.provide` | `static PROVIDE: &[&str]` → `Registry::provide` |
| `internal/update` veto | `ReloadPolicy::Reject(reason)` |
| `ctx.logger` | the Appendix A log layer |

### D.11 Acceptance for the contract

- Delete a plugin mid-session → capabilities revoked at the next frame boundary, disposers run **LIFO on the teardown task**, each logged with its `label`, world survives.
- **The frame does not stall:** a 200ms teardown does not drop the frame rate; `fiber.state == UNLOADING` while it drains, then `DISPOSED`.
- A disposer re-entering the registry after revocation is a compile/contract error, not a runtime surprise.
- A disposer that never settles hits the deadline, logs `FATAL` with its `label`, marks `FAILED`, and **the game keeps running**.
- Injecting a missing service leaves the fiber `FAILED` with a logged reason, not a panic.
- Restore the service → fibers flip back to `ACTIVE` without restart.
- **Lazy:** a `when-namespace:foo` plugin is absent until `maps/foo/` opens, then loads on a frame boundary; boot time with 50 namespaces ≈ boot time with 1.
- **Lazy is invisible:** an `on-demand` provider materializes on first `inject` and the consumer never observes the difference from `eager`; a lazy load over budget logs `[WARN] plugins> lazy load took 42ms: <name>`.
- **Eviction** (if enabled) reuses the identical `UNLOADING` path — no second teardown implementation.
- `releste dev --inspect-plugins` prints the full `EffectMeta` tree plus each fiber's `state` and load mode for every plugin.
- Config with a bad field reports `at <path>` and refuses to load, exit code 1 (Appendix B rule 5).

---

## Appendix E — `.map` format definition (v1)

Normative spec for the on-disk format. Verified against `bincode-1.3.3` source and `crates/map`, `crates/world` — not from memory.

### E.1 Constraints the format must satisfy

| # | Constraint | Source |
|---|---|---|
| 1 | **Roundtrip-exact** — parse → serialize → parse yields an equal document | AGENTS.md §9 (`map` row) |
| 2 | **Data, never code** — no logic, no script bodies | §4.2 |
| 3 | **Hot-reloadable** — readable within one 100ms debounce window | §10 |
| 4 | **Versioned with explicit migration** | §7.3 |
| 5 | **Cross-platform byte-identical** — same map, same bytes, any OS | §1 cross-platform |
| 6 | **Stable identity** — an entity keeps its ID across saves | §7.2 |

### E.2 Container

```
offset  size  field      value / notes
──────  ────  ─────────  ─────────────────────────────────────
0       8     magic      "RELMAP\x00\x00"  (0x52 45 4C 4D 41 50 00 00)
8       4     version    u32 LE, currently 1
12      …     document   MapDocument, bincode (see E.3)
```

Defined by `MapDisk { magic: [u8; 8], version: u32, document: MapDocument }` in `crates/map/src/loader.rs`.

The header is **not** self-delimiting: the only integrity check is the 8-byte magic. There is no length field and no checksum (see E.9).

### E.3 Encoding rules

`loader.rs` uses the free functions `bincode::serialize` / `bincode::deserialize`. In bincode 1.3.3 those resolve to:

```rust
DefaultOptions::new().with_fixint_encoding().allow_trailing_bytes()
```

| Property | Value | Consequence |
|---|---|---|
| Endianness | **little-endian** | fixed, never negotiated |
| Integer encoding | **fixint** (fixed width) | `usize` and all length prefixes are **u64** |
| Trailing bytes | **allowed** | garbage after the document is **silently ignored** |
| Byte limit | **none** | a corrupt length prefix can request an enormous allocation |

Primitive encodings used below:

| Type | Bytes |
|---|---|
| `u8` / `bool` | 1 (`bool`: 0 / 1) |
| `u32`, `i32`, `f32` | 4, LE (`f32` = IEEE-754) |
| `u64`, `usize`, `EntityId` | 8, LE |
| `String` | `u64` byte-length + UTF-8 bytes |
| `Vec<T>` | `u64` count + `T` × count |
| `Option<T>` | `u8` tag (0/1) + `T` if 1 |
| enum | `u32` variant index + payload |
| `HashMap<K,V>` | `u64` count + pairs — **iteration order unspecified** |

> **The three properties worth remembering:** LE, fixint with u64 lengths, trailing bytes ignored. Everything else follows.

### E.4 Field schema

**`MapDocument`**

| Field | Type | Encoding |
|---|---|---|
| `area` | `String` | u64 len + UTF-8 |
| `rooms` | `Vec<Room>` | u64 count + items |
| `sync_rules` | `Vec<SyncRule>` | u64 count + items |

**`Room`** — room order in the file is significant only for byte stability, **not** for identity (E.5).

| Field | Type | Encoding |
|---|---|---|
| `name` | `String` | **identity key** (E.5) |
| `bounds` | `RectDef {x,y,w,h: f32}` | 16 bytes |
| `tiles` | `Tileset` | |
| `bg` | `Tileset` | |
| `entities` | `Vec<EntityData>` | |
| `triggers` | `Vec<TriggerData>` | |
| `script` | `Option<PathBuf>` | **defect** — see E.9 |

**`Tileset`** — row-major tile-index grid; `0` = empty; index semantics belong to the renderer (the map layer must not interpret art).

| Field | Type | Notes |
|---|---|---|
| `atlas` | `String` | e.g. `"Gameplay"`; resolves via `resources/<namespace>/` |
| `width` | `u32` | tiles |
| `height` | `u32` | tiles |
| `tiles` | `Vec<u8>` | length must equal `width × height`; **`u8` caps a tileset at 255 indices** |

**`EntityData`**

| Field | Type | Notes |
|---|---|---|
| `id` | `EntityId` (u64) | **stored, and authoritative** (E.5) |
| `kind` | `String` | resolved against the plugin registry at load |
| `x`,`y` | `f32` | map coordinates |
| `width`,`height` | `Option<i32>` | |
| `origin_x`,`origin_y` | `f32` | |
| `properties` | `HashMap<String, FieldValue>` | **non-deterministic byte order** (E.9) |

**`FieldValue`** — variant index in parentheses:

| # | Variant | Payload bytes |
|---|---|---|
| 0 | `Float(f32)` | 4 |
| 1 | `Int(i32)` | 4 |
| 2 | `String(String)` | u64 + UTF-8 |
| 3 | `Bool(bool)` | 1 |
| 4 | `Enum(usize)` | **8** (usize → u64) |
| 5 | `Color([u8; 4])` | 4 |
| 6 | `EntityRef(u64)` | 8 |
| 7 | `Vec2{x,y: f32}` | 8 |

**`TriggerData`** — `kind: String`, `x,y,width,height: f32` (16), `properties: HashMap<String, String>`.

**`SyncRule`** — `kind: String`, `mode: SyncMode` (`0=flag`, `1=full`, `2=event`), `flag: Option<String>`.

### E.5 Identity rules

**Room — the name is the key.** `diff.rs` indexes rooms by `r.name` in a `HashMap<&str, _>`. Therefore:

- Room names must be **unique** within a map.
- **Renaming a room is a delete + add**, not an edit — every entity in it churns. The editor must treat a rename as a destructive operation and warn.

**Entity — the stored `id` is authoritative.** This resolves an ambiguity in AGENTS.md §7.2:

- `EntityId::generate(...)` is the **authoring-time** rule, used when an entity is *first created* (content-pipeline converting an original map, or the editor placing a new entity).
- The result is **persisted** into `EntityData.id` and never recomputed.
- Consequently `Patch::MoveEntity { id, pos }` works: moving changes `x`/`y` but not the stored `id`. If IDs were re-derived from position, every move would degrade into `Remove` + `Add` and destroy runtime state — exactly what §4.3 forbids.

**Generation layout.** `EntityId::generate(area_id: u32, room_name: &str, kind_id: u32, x: i32, y: i32)` hashes, in order:

```
area_id:u32 LE ‖ room_name:UTF-8 ‖ kind_id:u32 LE ‖ x:i32 LE ‖ y:i32 LE
```

This is **unambiguous without a separator**: `area` is the first fixed 4 bytes, `kind/x/y` are the last fixed 12 bytes, so `room_name` is exactly the middle `len − 16` bytes and can be recovered uniquely.

**Three gaps this rule does not cover — must be decided before Phase 4:**

1. **`area` is a `String` in the file but `generate` takes `u32`.** No `area_id_for()` exists anywhere (`crates/world/src/id.rs` has no counterpart to `apply.rs:244 kind_id_for`). Define it, mirroring `kind_id_for`: first 4 bytes of `blake3(area)`.
2. **`generate` takes `i32` coordinates while `EntityData` stores `f32`.** Truncating to integer makes two entities within 1 tile of each other **collide** — one silently replaces the other. Celeste rooms are full of sub-tile positions. Either hash the `f32` bits (after canonicalising `-0.0` → `0.0` and rejecting `NaN`) or widen the ID.
3. **Collision on creation.** Two entities of the same kind at the same position in the same room produce the *same* ID by construction. Authoring must either forbid that or disambiguate at creation time; the runtime cannot tell them apart later.

**Entity kind — `kind_id`.** There is no `kind` field in the map's identity space; `EntityKindId(u32)` is resolved from the `kind` string, with a fallback `kind_id_for(kind) = blake3(kind)[0..4]`. Registry lookup wins when a plugin provides the kind.

### E.6 What `#[serde(default)]` does **not** do here

`format.rs` sprinkles `#[serde(default)]` on `bg`, `entities`, `triggers`, `script`, `sync_rules`, `flag`, and the `Tileset` fields. **Under bincode these are inert.**

bincode is not self-describing: for a struct it unconditionally reads one field after another in declaration order, and EOF is an `Err`, never a "field absent". So `default` can never fire.

**Consequences:**

- **Adding a field to any struct breaks every existing `.map` file**, including files written by the immediately previous version. There is no "skip unknown fields" escape hatch.
- Backward compatibility therefore *requires* the explicit migration functions §7.3 promises — and none exist yet (E.7).
- Removing or reordering fields is likewise a hard break.

> Treat every schema change as a version bump + a migration function. Do not rely on `default` for tolerance.

### E.7 Versioning and migration

Current behaviour in `load_map_bytes`:

```rust
if disk.version != MAP_VERSION { return Err(MapError::VersionMismatch { .. }) }
```

**Exact equality — so a migratable older file is rejected before any migration could run.** The migration path §7.3 specifies is currently unreachable.

Required shape:

```rust
match version {
    MAP_VERSION => decode::<V1>(doc),       // current
    0 => migrate_v0_to_v1(...)?.and_then(decode::<V1>),
    v if v > MAP_VERSION => Err(MapError::FutureVersion { found: v }),
    v => Err(MapError::UnsupportedVersion { found: v }),
}
```

- **Reject forward versions loudly** — a file from a newer engine must not be half-read.
- **Migration is pure and chainable**: `v0 → v1 → v2 …`, each step a `fn(V_n) -> V_{n+1}`, so N versions back still loads without N special cases.
- `MAP_VERSION` bump requires a golden fixture per version under `tests/fixtures/` and a test asserting every fixture still loads.

### E.8 Determinism, atomicity, and hot reload

**Determinism.** `EntityData.properties` and `Props.fields` are `HashMap<String, _>`. bincode walks them in `RandomState` order, so **the same logical map serialises to different bytes on different runs.**

- The §9 roundtrip test passes anyway (it compares *structures*, and `HashMap` `PartialEq` ignores order), which is why this has gone unnoticed.
- Any content hash of a `.map` is therefore unusable — for change detection, build caching, or VCS-friendly diffs.
- Editor "save without changes" produces different bytes and triggers `FileWatcher` unnecessarily.

**Fix: `BTreeMap`.** Requires a version bump and regeneration of the 27 converted maps (56,440 entities).

**Atomicity.** `save_map` is a bare `fs::write`. A crash mid-write leaves a truncated file; `FileWatcher` then fires and the parse fails. Required:

```
write  <path>.tmp  →  fsync  →  rename  <path>.tmp  →  <path>
```

`rename` is atomic on POSIX and on NTFS within a volume, so the watcher only ever observes a complete file.

**Partial reads.** With no checksum (E.2) a stale or torn file is indistinguishable from a valid one. Options: add a `u32 CRC32` over the document (version bump), or rely solely on atomic rename — **atomic rename alone is sufficient** and cheaper; prefer it.

### E.9 Known defects

| # | Defect | Severity | Status | Where |
|---|---|---|---|---|
| 1 | `diff()` room add/remove branches were **empty** — adding a room emitted nothing, deleting a room orphaned its entities | **high** | **fixed** | `crates/map/src/diff.rs` |
| 2 | `Patch::AddEntity` hardcoded `kind: String::new()`, and `EntityRef` also dropped `width`/`height`/`origin_*` — hot-reload-added entities lost their kind *and* their size | **high** | **fixed** | `crates/map/src/diff.rs` |
| 3 | `HashMap` → non-deterministic bytes; content hashing impossible | **high** | open | `format.rs` `properties`, `Props.fields` |
| 4 | `version != MAP_VERSION` hard-rejects; migration path unreachable | **high** | open | `crates/map/src/loader.rs:32` |
| 5 | `#[serde(default)]` inert under bincode — false sense of forward compat | **medium** | open | `format.rs` (7 sites) |
| 6 | Non-atomic save; torn files reach the watcher | **medium** | open | `crates/map/src/loader.rs:50` |
| 7 | `EntityId::generate` takes `i32` coords, `EntityData` has `f32` → sub-tile ID collisions | **medium** | open | `crates/world/src/id.rs:22` |
| 8 | `area: String` vs `area_id: u32` — mapping undefined | **medium** | open | `format.rs:20` / `id.rs:22` |
| 9 | `script: Option<PathBuf>` — platform-dependent encoding; not byte-identical across OS | **medium** | open | `format.rs:39` |
| 10 | No size limit / no checksum on read | **low** | open | `loader.rs:27` |
| 11 | `Tileset.tiles: Vec<u8>` caps a tileset at 255 indices | **low** | open | `format.rs:77` |
| 12 | `f32::EPSILON` used for coordinate equality; `NaN` never compares as changed | **low** | open | `diff.rs` `diff_entities` |
| 13 | Patch **sequence** was non-deterministic — rooms and entities both iterated `HashSet` | **medium** | **fixed** | `crates/map/src/diff.rs` |

**Fixed in one pass** (1, 2, 13 — all in `crates/map/src/diff.rs`, no format change, no regeneration needed):

- Room add now emits `ReplaceTiles` + one `AddEntity` per entity; room removal emits `RemoveEntity` per entity.
- `EntityRef` carries the full entity signature (`kind`, `width`, `height`, `origin_x/y`) so `AddEntity` is lossless instead of reconstructing a stub.
- Room iteration follows `old.rooms` then `new.rooms` order; entity iteration follows slice order. Same input now always yields the same patch sequence.
- Four regression tests added, including one that diffs 16× and asserts byte-identical patch sequences.

**Remaining, ordered by what blocks Phase 4:** #3 and #4 are both fixed by the same change (a `MAP_VERSION` bump), and #4's migration hook also lets #5's tolerance gap be handled honestly instead of by the inert `#[serde(default)]`. Doing 3 + 4 + 5 together is one regeneration pass over the 27 converted maps. #6 is a four-line fix that should ride along. #7 and #8 need a decision, not code — see E.5.

### E.10 Evolution (v2 — proposal, not scheduled)

Deserialisation is strictly sequential: **loading one room requires parsing every preceding room.** With 824 rooms / 56,440 entities that fights §10 (100ms debounce) and §13's "reload Lua chunk separately" TODO.

A v2 container would add a room index:

```
header (12) ── room count ── [name, byte_offset, byte_len] × N ── room payloads …
```

- Random access to a single room → **parse only what changed**, reusing `FileWatcher`'s diff at byte level.
- Each room payload independently decodable → per-room `script` reload resolves §13 TODO #2 without touching the whole map context.
- Requires the schema changes in E.9 anyway (version bump), so this is the natural moment.

Explicitly **not scheduled** — decide when Phase 3 lands and the hot-reload path is measurable.

### E.11 Acceptance

- `to_bytes(load(bytes)) == bytes` for every fixture (byte-exact roundtrip, not just structural).
- Two independent serialisations of the same `MapDocument` produce identical bytes in the same process **and across processes**.
- Loading `maps/<ns>/*.map` for all 27 converted areas completes within one 100ms debounce window.
- Adding a room and deleting a room each produce the expected `Patch` list (E.9 #1).
- A `v0` and a `v1` fixture both load; a `v99` fixture fails with `FutureVersion`, not a parse error.
- A file written on Linux loads byte-identically on Windows (E.9 #9).
- Killing the editor mid-save leaves either the old file or the new file, never a truncated one.

---

## Sequencing & risk

```text
Phase 1 ──► Phase 2 ──► Phase 3 ──► Phase 4 ──► Phase 5
  logs/CLI      game bin    plugins    parity      fidelity
                (keystone)     │                    ▲
                               └──► Phase 6 ────────┘
                                    §13 leftovers
```

| Risk | Mitigation |
|---|---|
| ~~No binary exists — work so far is unobservable~~ | Phase 2 shipped: `releste` boots a real room headless and in a window; the keystone moves to Phase 4 (vanilla feel) |
| Plugin ABI churn is where rewrites die | Freeze the `.wasm` WIT surface first; keep `.so` behind a tiny `#[repr(C)]` vtable (Phase 3.2). Gameplay must stay expressible in wasm so 99% of changes need no native build. **Currently the `.so` vtable is frozen but there is no `.wasm` at all (§3.2) — the wasm side is blocked on the `wasmtime` dependency discussion (AGENTS.md §15.1)** |
| Vanilla movement feel is the whole game | Phase 4.1 needs reference recordings and step-by-step assertions, not eyeballing. **Started:** coyote time / jump buffering / variable jump height landed with tests that fail when the feature is deleted — the constants are still first-pass, so the reference-matched pass is what remains |
| ~~Crunch decoder scope creep~~ | Kept in `tools/content-pipeline` (§4.6); the packed-page decoder shipped there. The remaining `PackerNoAtlas` gap is the same rule applied to new work |
| `nix run` and `cargo r` drifting apart | Both must resolve to the same bin; assert it in a check |
| Async teardown hides use-after-free until it bites | Capabilities revoked **synchronously** at the boundary; disposer owns only its own resources (D.6). `UNLOADING` is observable, `await_()` exists for tests — assert it in `cargo test` |
| A stuck disposer freezes the game | Deadline → `FATAL` + `FAILED` + deliberate leak; never block the frame (D.6) |
| Lazy loading becomes hidden stutter | 16ms budget with `[WARN]` on overrun, `--preload` to opt out, eviction shares the single teardown path (D.7) |
| Cordis's shape does not survive the Rust ownership model | Appendix D.9 lists exactly what is *not* borrowed; prototype chain / Proxy / GC assumptions are dropped up front, not halfway through |

**Rule that never changes:** if a phase requires violating an AGENTS.md invariant (§4 core invariants, §15 hard constraints), stop and open an issue to amend AGENTS.md first. Do not route around it.
