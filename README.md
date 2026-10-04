# Releste

A from-scratch rewrite of a 2D precision platformer engine (Rust).

- **Hot-reloadable maps are a first-class citizen**: edit a `.map` while the game runs, see it take effect within a second, no process restart
- **Reuses vanilla Celeste art / audio / map data**: converted offline once, read at runtime only in our own formats
- **Client-authoritative networking**: the server is a pure relay and world state is not synced by default
- **Cross-platform**: Windows / Linux / macOS (aarch64)

See [`AGENTS.md`](AGENTS.md) for the full set of design constraints.

## Building with Nix (recommended)

Every build and check goes through a flake output and is **identical to CI**.

```bash
# Everything (format / clippy / docs / tests / hooks / binaries)
nix flake check

# Individual checks
nix build .#checks.x86_64-linux.fmt            # rustfmt + nixfmt --check
nix build .#checks.x86_64-linux.clippy         # -D warnings
nix build .#checks.x86_64-linux.doc            # rustdoc -D warnings
nix build .#checks.x86_64-linux.unit-tests     # 183 tests
nix build .#checks.x86_64-linux.lua-tests      # script engine tests with the lua feature on
nix build .#checks.x86_64-linux.fmod-probe     # FMOD generation detection
nix build .#checks.x86_64-linux.pre-commit     # git hooks
nix build .#checks.x86_64-linux.nextest        # cargo-nextest

# Executables
nix build                                # all binaries -> result/bin/{editor,net-server,content-pipeline}
nix build .#packages.x86_64-linux.full   # additionally enables the lua + gamepad features

# Development shell (installs the git hooks)
nix develop

# Formatting
nix fmt
```

**Why Nix**: the `lua` feature needs `make` to build vendored LuaJIT, and
`gamepad` needs `udev` + `pkg-config`. The flake supplies all of it, so a
complete build reproduces on any machine instead of depending on whatever the
host happens to have installed.

### Pinned toolchain

`rust-toolchain.toml` is the **single source of truth shared by Cargo and Nix**:

```toml
[toolchain]
channel = "1.98.1"
components = ["rustfmt", "clippy", "rust-analyzer", "rust-src"]
```

The flake reads it through rust-overlay's `fromRustupToolchainFile`, so
`cargo fmt` (local) and `nix build .#checks.*.fmt` (CI) use the **same rustfmt
binary**. The pre-commit `rustfmt` hook points at that same toolchain through
`packageOverrides`.

This exists to avoid a trap we already hit: nixpkgs ships one rustfmt while
rust-overlay provides another, the two rewrite each other's output, and you get
"green locally, red in CI".

## Building with Cargo (fast iteration)

```bash
cargo build --workspace
cargo test --workspace
cargo run -p reles-editor
cargo run -p reles-net-server -- --port 7777
cargo run -p reles-content-pipeline -- --source references/Celeste/Content --output assets --verify
```

## Crate layout

| Crate                    | Responsibility                                       | Status              |
| ------------------------ | ---------------------------------------------------- | ------------------- |
| `crates/math`            | fixed-point `Fx` (16.16), `Vec2`, `Rect`             | ✅                  |
| `crates/kernel`          | `Service`, `EventBus`, `FrameId`, main loop          | ✅                  |
| `crates/world`           | ECS, stable entity IDs, Schema, overlap resolution   | ✅                  |
| `crates/map`             | `.map` binary format (version header), diff          | ✅                  |
| `crates/reload`          | FileWatcher (debounce), Patch, rollback              | ✅                  |
| `crates/input`           | virtual buttons, edge detection, input buffering     | ✅                  |
| `crates/script`          | `CutsceneBridge`, `SimCommand` queue                 | ✅                  |
| `crates/render`          | camera, atlases, batching, wgpu backend              | ✅                  |
| `crates/audio`           | bus mixing, same-frame dedup, priority preemption, FMOD backend | ✅       |
| `crates/net-client`      | collaboration protocol, carry state machine, session interpolation | ✅ |
| `crates/net-server`      | pure relay (does not understand map content)         | ✅                  |
| `crates/editor`          | egui editor, auto-generated Schema panels, undo stack | ✅                 |
| `tools/content-pipeline` | vanilla assets -> engine formats                     | ✅                  |

## Measured results

`content-pipeline` runs end to end over **every vanilla Celeste asset**, and the
output is read back and validated by the runtime loaders:

```
atlases           : 22
sprites           : 8371
dialogs           : 10
dialog entries    : 5450
maps              : 27
rooms             : 824
entities          : 56440
FMOD banks copied : 7
verified          : 27 maps, 824 rooms, 56440 entities reload OK
```

## What the repository does not contain

Because of distribution licensing, the following is **not committed** (passively
ignored by `.gitignore` and actively blocked by the pre-commit
`restricted-content` hook):

- `references/` (~13 GB): vanilla Celeste executables / DLLs / decompiled source
- `assets/`, `assets-src/`: art / audio / maps derived from vanilla assets
- `libfmod.so` and other shared libraries: FMOD is proprietary, supply your own
- our own binary formats such as `.map`
- any build output

Get `libfmod.so` yourself from <https://www.fmod.com/download> and point
`RELESTE_FMOD_LIB` at it.

## FMOD support

No FMOD binary is vendored (AGENTS.md §4.7). With the `fmod` feature enabled:

- **At build time**: `crates/audio/build.rs` probes for the system library and,
  if it is missing, fails with the message format AGENTS.md specifies. Set
  `RELESTE_FMOD_ALLOW_MISSING=1` to skip the probe.
- **At run time**: `FmodBackend::new` opens the library with `libloading` and
  **identifies the API generation from its symbols** — the three generations
  have mutually incompatible ABIs, and getting it wrong fails silently:

  | Generation      | Key symbol                  |
  | --------------- | --------------------------- |
  | FMOD Studio     | `FMOD_Studio_System_Create` |
  | FMOD Ex (4.x)   | `FMOD_System_Create`        |
  | FMOD 3.x        | `FSOUND_Init`               |

  Only the Studio generation is directly supported by the engine; the others are
  detected and reported explicitly.

- **Bitness must match**: a 32-bit `.so` cannot be loaded by a 64-bit process and
  vice versa. That case reports `FmodUnusable` with the original `dlopen` error.

### Three traps when using an older FMOD

Older FMOD shared libraries found in the wild frequently hit several of these at
once. Check before launching:

```bash
# 1) Bitness must match the process
file libfmod.so            # ELF 32-bit -> only a 32-bit process can load it
readelf -h libfmod.so | grep -E "Class|Machine"

# 2) Confirm the API generation (this decides whether it can be used at all)
readelf --dyn-syms -W libfmod.so | grep -oE "FMOD_Studio_System_Create|FMOD_System_Create|FSOUND_Init" | sort -u

# 3) Whether it demands an executable stack (modern glibc/kernel refuse, with an obscure error)
readelf -lW libfmod.so | grep GNU_STACK   # RWE means an executable stack is required
```

The symptom of the third is `dlopen` reporting
`cannot enable executable stack as shared object requires: Invalid argument`.
It is the result of **security hardening**, not a corrupt file: you can clear the
flag with `execstack -c libfmod.so` (which modifies the proprietary binary) or
use an older environment that permits executable stacks. The engine does **not**
make that modification on the user's behalf.

Without the `fmod` feature the engine uses `NullBackend`, and the **mixing logic
(dedup / preemption / bus volume) remains fully functional and test-covered**.

### The full pipeline has been verified against vanilla Celeste libraries

`crates/audio/src/fmod_studio.rs` is a hand-written FMOD Studio binding built on
`libloading` (symbols resolved at run time, no link-time dependency). Measured
against `references/Celeste/lib64/`:

| Library                     | Architecture | Generation  | Usable           |
| --------------------------- | ------------ | ----------- | ---------------- |
| `lib64/libfmodstudio.so.10` | x86-64       | Studio      | ✅               |
| `lib64/libfmod.so.10`       | x86-64       | Ex / low-level | ✅            |
| `lib/libfmod.so.10` (32-bit)| i386         | 3.x         | ❌ see above     |

The complete sequence exercised with `libfmodstudio.so.10`:

```
Studio_System_Create(headerversion=0x00011014) -> 0
Studio_System_Initialize                        -> 0
LoadBankFile(Master Bank.bank)                  -> 0
LoadBankFile(Master Bank.strings.bank)          -> 0
LoadBankFile(sfx.bank)                          -> 0   (527 entries)
GetEvent(event:/char/badeline/boss_prefight_getup) -> 0
EventDescription_CreateInstance                 -> 0
EventInstance_Start                             -> 0
System_Update                                   -> 0
```

In other words: **banks produced by the content pipeline load and play in real
FMOD Studio**. (21 of those 527 entries are `snapshot:/...`, which are FMOD
mixing snapshots rather than events.)

The headerversion is not hardcoded: different FMOD releases number it
differently, so `KNOWN_HEADER_VERSIONS` is tried newest to oldest and the one
actually accepted is recorded.

To reproduce (the library is not committed, copy it out of `references/`):

```bash
mkdir -p .fmod && cp references/Celeste/lib64/libfmod*.so.10 .fmod/
cd .fmod && ln -sf libfmod.so.10 libfmod.so && cd ..

RELESTE_FMOD_ALLOW_MISSING=1 \
RELESTE_FMOD_STUDIO_LIB="$PWD/.fmod/libfmodstudio.so" \
RELESTE_FMOD_BANKS="$PWD/assets/audio" \
LD_LIBRARY_PATH="$PWD/.fmod" \
  cargo test -p reles-audio --features fmod full_studio_pipeline -- --nocapture
```

## Lua cutscene scripts

The `lua` feature enables mlua + vendored LuaJIT (which needs `make` at build
time).

Scripts **cannot touch physics state directly** (AGENTS.md §4.5): `world.*` /
`camera.*` / `audio.*` / `spawn` / `set_player_state` only push `SimCommand`
values onto a queue, which is handed to `CutsceneBridge` once the chunk finishes
and consumed by the simulation on the next frame.

See [`maps/example_cutscene.lua`](maps/example_cutscene.lua).

## Crunch textures (AGENTS.md §13 TODO)

The `.data` payloads of vanilla art are Crunch-compressed and pixel decoding is
not implemented yet. The pipeline parses the atlas descriptors correctly, copies
the raw payloads through, and reports exactly which textures still need decoding.
PNG atlases render directly.

## Core invariants

- **Maps are data, not code**: there is no logic inside a `.map`
- **Hot reload never restarts or drops the session**: player position / velocity /
  state / carried objects are all preserved
- **The world is not synced by default**: syncing is declared explicitly with a
  map's `[[sync_rule]]`
- **Lua never touches the simulation**: every side effect is queued as a `SimCommand`
- **Vanilla formats only appear at build time**: the runtime reads only our own formats
- **The simulation uses fixed point**: `dt` is always `Fx` and always 1/60

## Tests

```bash
nix flake check                    # everything (recommended)
cargo test --workspace             # 183 tests
cargo test -p reles-reload         # includes hot reload end-to-end (real file watching)
```
