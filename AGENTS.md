# Releste Project Agent Constraints

## 1. Project Goals

Releste is a **from-scratch rewrite** of a 2D precision platformer engine with the following objectives:

- **Hot-reloadable maps as a first-class citizen** — change `.map` files while running and they take effect in seconds without restarting the process.
- **Reuse original Celeste art, audio, and map data** — convert once offline, load only the engine-native format at runtime.
- **Client-authoritative networking** — the server is a pure relay; world state is not synchronized by default. "Unsynchronized worlds" are a feature, not a bug.
- **Cross-platform** (Windows, Linux, macOS, and potentially Wasm).
- **Full control** — not built on Everest, FNA, or MonoGame.

**This is not a Celeste remake.** Celeste is a reference. The entity system, map format, scripting layer, and networking layer are all redesigned.

## 2. Locked Technology Stack

| Layer | Technology | Notes |
|---|---|---|
| Language | **Rust** (stable, edition 2021) | Unified across the project |
| Rendering | **wgpu** + **winit** | No OpenGL, no SDL rendering layer |
| Audio | **fmod-oxide** | User must provide `libfmod.so` / `fmod.dll` / `libfmod.dylib` |
| Scripting | **mlua** (feature = `luajit`, `vendored`) | Cutscenes + per-room logic |
| Networking (transport) | **laminar** (default) / **quinn** (optional) | Semi-reliable UDP / QUIC |
| Server | **tokio** + **laminar** | Pure relay |
| Serialization | **serde** + **bincode** (runtime) / **toml** (config) | No JSON for binary data |
| Editor UI | **egui** | Auto-generate panels from entity schemas |
| Logging | **tracing** + **tracing-subscriber** | Structured with spans |
| Errors | **thiserror** (libraries) / **anyhow** (applications) | Libraries must not expose anyhow |
| Testing | **cargo test** + **proptest** | Use proptest for critical algorithms |
| CLI | **clap** | — |

**Prohibited:** `bevy_ecs`, `godot`, `macroquad`, `piston`, or any library binding to MonoGame/XNA/FNA.

## 3. Repository Structure

```text
releste/
  Cargo.toml            # workspace
  AGENTS.md             # this file
  README.md

  crates/
    kernel/             # main loop, ServiceRegistry, EventBus, FrameId
    world/              # ECS, EntityRegistry, Schema, components
    map/                # .map parsing/serialization/diff
    reload/             # FileWatcher, Patch, conflict resolution
    render/             # wgpu renderer, camera, batching
    audio/              # fmod-oxide wrapper
    input/              # input sampling (1kHz), virtual buttons
    script/             # mlua integration, CutsceneBridge
    net-client/         # client networking (avatar, coop protocol)
    net-server/         # server (binary crate)
    editor/             # egui map editor (binary crate)

  tools/
    content-pipeline/   # original assets -> engine format (binary crate)

  assets/               # engine runtime assets (build artifacts, gitignored)
  assets-src/           # original Celeste assets (user-provided, gitignored)
  maps/                 # example maps (.map + .lua)
```

**Cargo workspace.** All crates share one `Cargo.lock`; use `[workspace.dependencies]` to unify versions.

## 4. Core Invariants

Violating any of the following is an automatic rejection in code review.

### 4.1 Kernel Must Not Know Game Logic

`crates/kernel/` **must not** depend on `world`, `render`, `audio`, `script`, or any business crate. The kernel only knows the `Service` trait, `EventBus`, and `FrameId`.

### 4.2 Maps Are Data, Not Code

`.map` files must **never** contain logic. Entities only reference the engine registry via `EntityKindId`. Lua scripts in maps may only interact with high-level APIs such as `world`, `camera`, and `audio`.

### 4.3 Hot Reload Must Not Restart or Lose Session State

After modifying a `.map` file:

- Player position, velocity, and state **must be preserved**.
- Carried objects **must be preserved**.
- Static entities are diffed by ID.
- The camera is recalculated but not reset.

If any map change requires a restart to take effect, discuss it before implementing.

### 4.4 Worlds Are Not Synchronized by Default

The networking layer must **not** assume that any entity needs to be synchronized. Synchronization must be explicitly declared in the map:

```toml
[[sync_rule]]
kind = "SharedDoor"
mode = "flag"
flag = "door_open"
```

The server **does not understand** map content. It only relays: player avatars, cooperative protocol messages, and explicitly synchronized events.

### 4.5 Cutscenes Must Not Touch Simulation

Lua scripts drive the simulation via the `CutsceneBridge` command queue. **Lua must never directly modify physics state.**

### 4.6 Original Formats Only Appear at Build Time

Runtime crates (`crates/`) must **not** contain parsing code for `.bin`, `.xnb`, or `.meta` files. These only appear in `tools/content-pipeline/`.

### 4.7 FMOD Library Is Provided by the User

Do not vendor FMOD binaries. The build system must detect the system library and report a clear error if not found:

```
error: FMOD library not found.
Download from https://www.fmod.com/download
Place libfmod.so / fmod.dll / libfmod.dylib in:
  - Linux: /usr/local/lib/ or $LD_LIBRARY_PATH
  - Windows: alongside the .exe
  - macOS: /usr/local/lib/ or DYLD_LIBRARY_PATH
```

## 5. Code Style

### 5.1 Rust

- `#![deny(warnings)]` in CI; if locally allowing warnings with `#[allow]`, include a clear reason.
- All public APIs must have doc comments.
- Use `thiserror` for error types; **libraries must not panic** — propagate errors with `Result`.
- Applications (`bin`) may use `anyhow`, but must include context.
- Prefer `crossbeam` or `flume` channels for concurrency; **avoid shared mutable state**.
- Avoid allocations on hot paths (pre-allocate `Vec`s, use `SmallVec`, use object pools).
- Only use SIMD if profiling proves it necessary.

### 5.2 Naming

- Types: `PascalCase`; functions/variables: `snake_case`.
- Constants: `SCREAMING_SNAKE_CASE`.
- Modules: `snake_case`.
- **No Hungarian notation**; do not use `_t` or `_p` suffixes.

### 5.3 Commits

- One logical change per commit.
- Message format: `<scope>: <imperative summary>`
  - `map: add stable entity IDs for hot reload`
  - `render: fix batch flush order`
- For commits involving original asset conversion, include the source asset path(s).

## 6. Common Commands

```bash
# Build
cargo build --workspace
cargo build -p editor --release

# Run
cargo run -p editor                       # map editor
cargo run -p net-server -- --port 7777    # server
cargo run --bin releste                   # game

# Content pipeline (run once)
cargo run -p content-pipeline -- \
  --source ../assets-src \
  --output ../assets \
  --atlas-format packer

# Test
cargo test --workspace
cargo test -p world -- --ignored          # run slow tests

# Profiling
cargo flamegraph --bin releste
cargo bench -p world
```

## 7. Key Module Contracts

### 7.1 `kernel`

```rust
pub trait Service: 'static {
    fn id(&self) -> ServiceId;
    fn attach(&mut self, ctx: &mut Context);
    fn update(&mut self, ctx: &mut Context, frame: FrameId, dt: Fx);
    fn dispose(&mut self) {}
}

pub struct Kernel { /* ... */ }

impl Kernel {
    pub fn new() -> Self;
    pub fn with<S: Service>(self, svc: S) -> Self;
    pub fn tick(&mut self);
    pub fn run(&mut self) -> !;
}
```

**Rules:** `dt` is always `Fx` (fixed-point) and always `1/60`. Do not accept variable `dt`.

### 7.2 `world`

```rust
pub trait EntityFactory: Send + Sync {
    fn id(&self) -> EntityKindId;
    fn build(&self, data: &EntityData, ctx: &mut BuildCtx) -> Entity;
    fn schema(&self) -> &Schema;
    fn persistence(&self) -> Persistence;
}

pub enum Persistence {
    Rebuild,    // Rebuild on hot reload (walls, decorations)
    Preserve,   // Preserve state, only update properties (pickups)
    Persistent, // Never unload (player, camera)
}

pub struct World {
    entities: SlotMap<EntityId, Entity>,
    kind_index: HashMap<EntityKindId, Vec<EntityId>>,
    // ...
}
```

**All map entities must have stable IDs.** Rule: `blake3(area_id || room_name || kind_id || x || y)` take first 8 bytes.

### 7.3 `map`

```rust
pub struct Map {
    pub area: String,
    pub rooms: Vec<Room>,
    pub sync_rules: Vec<SyncRule>,
}

pub struct Room {
    pub name: String,
    pub bounds: Rect,
    pub tiles: Tileset,
    pub bg: Tileset,
    pub entities: Vec<EntityData>,
    pub triggers: Vec<TriggerData>,
    pub script: Option<PathBuf>,
}
```

`.map` format: custom binary (`bincode`), with a version header. Backward compatibility must be handled via explicit migration functions.

### 7.4 `reload`

```rust
pub enum Patch {
    AddEntity(EntityData),
    RemoveEntity(EntityId),
    UpdateEntity { id: EntityId, props: Props },
    MoveEntity { id: EntityId, pos: Vec2 },
    ReplaceTiles(Tileset),
    UpdateSync(Vec<SyncRule>),
}

impl Reload {
    pub fn apply(&mut self, world: &mut World, patch: Vec<Patch>) -> Result<(), ReloadError>;
}
```

**Conflict resolution strategy (in order):**

1. Player overlaps with newly added entity → push player out (`world.resolve_overlap`).
2. Referenced ID no longer exists → log warning and skip that patch entry.
3. Room bounds shrink and player is now outside → log error and roll back the entire patch.

### 7.5 `script`

```rust
pub struct CutsceneBridge {
    pending: VecDeque<SimCommand>,
    active: Vec<ActiveCommand>,
}

pub enum SimCommand {
    DummyWalkTo { actor: EntityId, target: Vec2, duration: f32 },
    SetPlayerState(PlayerState),
    CameraZoom { target: Vec2, zoom: f32, duration: f32 },
    SpawnActor { kind: ActorKind, pos: Vec2 },
}
```

**Lua must never directly manipulate `World`.** All side effects go through `SimCommand` queued to the bridge and consumed by the simulation on the next frame.

## 8. Performance Budget

Target frame time: 16.67ms (60 FPS). Budgets:

| Stage | Budget | Monitoring |
|---|---|---|
| Input sampling | < 0.1ms | `tracing::span` |
| Simulation tick | < 2ms | `cargo bench -p world` |
| Lua cutscene | < 0.5ms | `mlua` internal timing |
| Render submission | < 4ms | `wgpu` timestamp query |
| Audio | < 0.5ms | fmod-oxide timing |
| Networking | < 0.5ms | custom counters |

**If any stage exceeds its budget by 2×, open an issue to discuss.** Use `tracing` spans in production with 1% sampling.

## 9. Testing Strategy

| Layer | Strategy |
|---|---|
| `kernel` | Unit tests + property tests (monotonic frame IDs, event order) |
| `world` | Unit tests + snapshot roundtrip (`snapshot` → `restore` must be idempotent) |
| `map` | Roundtrip (parse → serialize → parse, results equal) |
| `reload` | Property tests (random patch sequences, world invariants hold) |
| `content-pipeline` | Golden tests (original samples → expected output) |
| End-to-end | Headless client × 2, local network, run cooperative scenarios |

**Exception for not writing tests:** pure rendering code, pure UI code. In those cases, maintain a manual checklist.

## 10. Hot Reload Workflow

This is the core capability of the project. **No PR may regress it.**

1. User edits `.map` in the editor and saves.
2. `reload` crate's `FileWatcher` triggers (100ms debounce).
3. `map` crate parses the new file.
4. `reload::diff(old, new)` generates `Vec<Patch>`.
5. `reload::apply(world, patch)` applies the patches:
   - Player/carried state preserved.
   - Static entities diffed by ID.
   - Conflicts handled per §7.4.
6. `render` receives `WorldChanged` event and rebuilds affected GPU resources.
7. User sees changes; **session is uninterrupted.**

**Acceptance:** move a wall corner, save, see change, player not ejected, no frame drops.

## 11. Cooperative Networking Protocol

Client-authoritative, server relay. Message types:

```rust
pub enum Up {
    Avatar(AvatarState),                 // high frequency, unreliable
    SyncEvent(SyncEvent),                // low frequency, reliable
    CarryRequest { target: PlayerId, mode: CarryMode },
    CarryAccept { initiator: PlayerId },
    CarryEnd { initiator: PlayerId },
}

pub enum Down {
    Avatar(PlayerId, AvatarState),
    SyncEvent(PlayerId, SyncEvent),
    CarryRequest(PlayerId, CarryMode),
    // ...
}

pub enum CarryMode {
    Grab,    // hold and follow
    Tether,  // elastic rope
    Ride,    // ride on top
    Launch,  // throw
}
```

**Carried entity disables local collision** — this is the technical key to "carry-through-wall" behavior. See `crates/net-client/src/carry.rs`.

## 12. Asset Pipeline

**Convert once offline; do not do this at runtime.**

```
assets-src/                   assets/
  Graphics/Atlases/    →        atlas/*.atlas
  Maps/*.bin           →        maps/*.map
  Dialog/*.txt         →        dialog/*.toml
  Fonts/*.fnt          →        fonts/*.font
  FMOD/Desktop/*.bank  →        audio/*.bank (copied as-is)
```

`.atlas` format: one PNG + one TOML (sprite list, rect, offset, origin).

**Do not** parse `.meta`, `.xnb`, or `.bin` at runtime. These only appear in `content-pipeline`.

## 13. TODOs / Known Issues

- [ ] `content-pipeline`: Crunch format support
- [ ] Hot reload: reload Lua chunk separately (currently reloads whole room/map context)
- [ ] Carry protocol: disconnect handling
- [ ] Server: heartbeat and timeout
- [ ] Editor: schema auto-panel performance for large rooms (hundreds of entities)
- [ ] wgpu: MSAA and visual parity with original Celeste

## 14. References

- Original Celeste decompiled source (`src.cs`) — **behavioral reference only**, not structural reference for code.
- CelesteNet / MiaoNet protocol design — **networking reference only**; do not adopt their "world sync" assumptions.
- GGPO rollback paper — **not adopted, but worth reading**.

## 15. Hard Constraints for AI Agents

1. **Do not add new crates** unless already listed in `[workspace.dependencies]`, or first open an issue to discuss.
2. **Do not change** `Cargo.toml` `edition` or `rust-version`.
3. **Do not import any business crate** in `crates/kernel/`.
4. **Do not put logic in maps** — only data.
5. **Do not write hot-reload code** that requires restart to take effect.
6. **Do not assume world synchronization** — sync is opt-in.
7. **Do not use `f32` for simulation** — use `Fx` (fixed-point).
8. **Do not panic** — use `Result`.
9. **Do not silently swallow errors** — at minimum use `tracing::warn`.
10. **Do not commit unformatted code** — run `cargo fmt` and `cargo clippy` first.

Violating any of these results in PR rejection and required fixes.

---

**Final note:** This file evolves with the project. If any rule blocks the right thing, **open an issue to discuss modifying this file** rather than bypassing it.
