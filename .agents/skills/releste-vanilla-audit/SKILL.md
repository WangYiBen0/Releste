---
name: releste-vanilla-audit
description: Use when writing or reviewing Releste code that ports Celeste behavior (Player, camera, transitions, backdrops, tiles) - audits every behavior line against references/pureCeleste.cs with line citations, and runs the repo's full gate including `nix flake check` before any commit. Also use when the user asks to audit, align with vanilla, or for zero deviation.
---

# Releste vanilla-parity audit

`references/pureCeleste.cs` (131,320 lines, decompiled) is the **behavioural
authority**. The rule in this repository: every behavioural line must point
back to a vanilla line number. Work through the steps below in order.

## 1. Line-by-line alignment

1. For every behavioural line in the files under review (constants, formulas,
   state transitions, draw order), find its source in `pureCeleste.cs` and note
   it as `// pureCeleste.cs:LINE` or `(pureCeleste.cs:LINE-LINE)`.
   **A behavioural line with no source is a violation**: either add the source
   or write "engine-specific + reason" in the comment.
2. **Tick conversion**: vanilla runs at 60Hz with `Speed` in px/s. This engine
   uses a fixed 1 tick = 1/60s and px/tick - always divide px/s by 60 and spell
   it as an exact `Fx::from_ratio` (105 px/s = `-7/4` px/tick). Spot-check every
   constant.
3. **Within-tick order**: vanilla's order (e.g. `Player.Update`: timer decay ->
   NormalUpdate (run -> maxFall -> gravity -> varJump pin -> jump decision) ->
   MoveH/MoveV -> EnforceBounds) must match the engine. Wrong order = wrong feel.
4. **Render/visibility rules**: check vanilla `X.Render` iteration order, layer
   order (Depth sorts descending: smaller values draw last) and per-frame vs
   cached work (backdrops rebuild every frame; the old room only unloads when
   the transition finishes).
5. Missing data or unimplemented paths must `warn_once` or be documented -
   silent omission is forbidden (AGENTS.md 15.8/15.9).

## 2. Tests

- Pure logic (physics, geometry, formats) needs unit tests asserting the
  **vanilla numbers**, not "reasonable" ones - e.g. `jump pins at JUMP_SPEED`,
  `v2_fixture_still_loads_and_migrates`.
- Pure rendering code may skip tests, but then the commit message must carry a
  manual checklist (AGENTS.md 9).

## 3. Pre-commit gate (all of it, in order)

```bash
cd /home/w1ngd1nga5ter/Projects/Releste
export PRE_COMMIT_HOME="$PWD/.devtmp/pc"     # the machine's pre-commit cache is read-only
cargo fmt --all
cargo build --workspace --all-targets 2>&1 | grep -cE "^(warning|error)"   # must be 0
cargo test --workspace                        # 0 failed
# nix gate (newer clippy + rustdoc + fmt + hooks). XDG/TMPDIR must be
# redirected, otherwise the fetcher cache is read-only / TMPDIR points at a
# temporary directory that no longer exists and you get bogus failures:
env XDG_CACHE_HOME="$PWD/.devtmp/nix-cache" \
    XDG_STATE_HOME="$PWD/.devtmp/nix-state" \
    XDG_DATA_HOME="$PWD/.devtmp/nix-data" \
    TMPDIR="$PWD/.devtmp/tmp" \
    nix flake check
```

`nix flake check` **must run before the commit**: its clippy is newer than a
local one and catches things you cannot see otherwise (including
`chunks_exact_to_as_chunks`, `unnecessary_to_owned`, `too_many_arguments`, and
rustdoc intra-doc links - square brackets such as `node[0]` must be escaped).
**No FLAKE-OK, no commit.**

Two gotchas that cost real time:

- nix only sees **git-tracked** files, so `git add` new files before running the
  check or it fails with `failed to resolve mod <name>`.
- The dev shell, the builds and the hooks all use the toolchain pinned by
  `rust-toolchain.toml` through fenix; if you change that file, update the
  `sha256` in `nix/rust.nix` (nix prints the correct value on mismatch).

## 4. Commit discipline

- Conventional Commits, `type(scope): subject` <=72 chars (AGENTS.md 5.3).
- One logical change per commit; cite the vanilla lines and the motivation in
  the message body.
- Keep the working tree clean. After committing, re-read the message
  (`git log -1 --format=%B`) to make sure the shell did not eat characters -
  pass it with `-F file` when it contains backticks or parentheses.
- Write commit messages and in-repo prose in English.

## 5. Recurring deviations to watch for

- px/s not divided by 60; `Fx` built from f32 constants instead of exact ratios
  (15.7).
- Release-speed cut, coyote/buffer frame counts made up - always look them up in
  pureCeleste (`VirtualButton.BufferTime` 110376, `jumpGraceTimer` 119832).
- Camera/transition/backdrop "reasonable guesses" - vanilla has a source for all
  of them (camera 120133-120146; transitions 115282-115423; EnforceBounds
  116517-116607; backdrops 112163-112906).
- `HashMap` in structures that need byte determinism (use `BTreeMap`, E.8).

## 6. Debugging tools (LLM-friendly, machine-readable)

Four tools, all **version-controlled** - do not write throw-away scripts.

### 6.1 Citation audit `citation_audit`

```bash
# report problems only (out_of_range / snippet_mismatch); exit 0 when clean
cargo run -p releste-game --example citation_audit
# machine-readable (every citation + the vanilla excerpt + status)
cargo run -p releste-game --example citation_audit -- --json
# every entry + per-file verified ratio
cargo run -p releste-game --example citation_audit -- --all --coverage
# look up vanilla line numbers (do this instead of trusting memory) + method table
cargo run -p releste-game --example citation_audit -- --find "private void ClimbHop"
cargo run -p releste-game --example citation_audit -- --methods
```

**Verifiable citation style**: put a backtick-quoted snippet right after the
citation; the tool checks that the snippet really occurs inside the cited line
range (whitespace-normalised):

```rust
// pureCeleste.cs:120402 `Math.Abs(Speed.X) <= 25f && moveX == 0`
```

A citation without a snippet is reported as `unchecked` (listed, but unable to
catch drift). Bare line numbers (`(122042-122048)` without the file name) are
invisible to the tool - write new and edited citations as the full
`pureCeleste.cs:LINE`, with a snippet whenever possible.

### 6.2 Per-tick trace `--trace`

```bash
cargo run --bin releste -- --headless --frames 600 --trace .devtmp/run.jsonl
```

The first line is a self-describing banner (`releste.trace.v1`), then one JSON
object per tick: `tick/room/x/y/vx/vy/vx_ps/vy_ps/on_ground/facing/state/
dashes/stamina/ducking/anim/anim_frame/last_anim/move_x/var_jump/jump_buffer/
coyote`. The `*_ps` fields are vanilla units (px/s) - read those when comparing
against `pureCeleste.cs`. Keep plugins off (`--no-plugins`) for reproducibility.

### 6.3 Scenario replay `--script`

```bash
cargo run --bin releste -- --script crates/game/tests/scenarios/walk_and_stop.toml
```

Deterministic (no plugins, no hot reload). A failed assertion prints
`tick=... field: expected ... got ... (delta... > tol ...)` and exits 1. Fields and syntax:
`crates/game/tests/scenarios/README.md`. **Expectations must be derivable from
vanilla**, with the source line in the comment.

### 6.4 Runtime map snapshot `--dump-map`

```bash
cargo run --bin releste -- --dump-map .devtmp/map.json
```

What the engine **parsed** (spawn, atlas name, per-room tile rows in hex,
entities and properties, sync rules) - the complement of
`releste-map --example dump_rooms` (raw file view). When behaviour and map data
disagree, compare both views.

### 6.5 Visual cross-check `--example walk_cycle_sheet`

```bash
cargo run -p releste-render --example walk_cycle_sheet -- .devtmp/walk-cycle.png
```

Assembles the atlas' real frames into one magnified contact sheet - the fastest
way to see whether an animation uses the vanilla frames. The verdict still
belongs to `Sprites.xml` / `pureCeleste.cs`.
