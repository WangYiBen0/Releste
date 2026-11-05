# Scenario scripts (LLM-friendly behaviour regressions)

Each `.toml` is a **deterministic** replay: explicit input plus assertions.
No plugins, no hot reload, no wall-clock dependency -- the same script always
produces the same trace.

```bash
cargo run --bin releste -- --script crates/game/tests/scenarios/walk_and_stop.toml
```

A failed assertion is printed (and the process exits 1):

```text
FAIL tick=40 vx: expected 1.6000, got 1.5000 (delta 0.1000 > tol 0.0500)
scenario: 60 ticks, 10 checks, 1 failure(s)
```

## Why it is shaped this way

- **Write vanilla values, not "whatever it prints"**: expectations should be
  derivable from `references/pureCeleste.cs` (e.g. `MaxRun = 90f` -> 1.5
  px/tick), with the source line in the comment. Copying today's output only
  freezes the status quo, not correctness.
- **Use `dx`/`dy` for displacement** (relative to tick 1) rather than absolute
  `x`/`y`: the converted assets under `assets/` are gitignored and their spawn
  point can change without invalidating the scenario.
- **A typo in a key is an error** (`deny_unknown_fields`), never a silent
  default.

## Fields

```toml
[scenario]
ticks = 60                  # how many kernel.tick() calls (counted from 1)
map = "0-Intro.map"         # expected map; a mismatch is an error
trace = ".devtmp/out.jsonl" # optional: per-tick trace (JSONL)
note = "one line describing the scenario"

[[input]]                   # any number; overlaps are OR-ed
from = 5
to = 40                     # inclusive
right = true                # left/right/up/down/jump/dash/grab

[[expect]]                  # any number; only the fields you write are checked
tick = 40
vx = 1.5                    # floats compare with tol (default 0.05)
dx = 50.667
anim = "runFast"
state = 0                   # 0 = Normal, 1 = Climb, 2 = Dash
dashes = 1
on_ground = true
```

To find new numbers, trace first and then write the assertion:

```bash
cargo run --bin releste -- --script <scenario>.toml   # writes [scenario] trace
# then read the JSONL: field names are assertion names, and units are in the
# names (vx = px/tick, vx_ps = px/s)
```

These scenarios need the converted assets (`assets/maps/`, `assets/atlas/`),
which are gitignored: run them on a dev machine, not in the nix sandbox.
