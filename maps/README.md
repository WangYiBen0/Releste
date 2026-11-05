# Sample maps

`.map` is an **engine-native binary format** (see
[`AGENTS.md` 7.3](../AGENTS.md)) and is not kept under version control - the
repository only carries text sources.

## Generating from the vanilla assets

```bash
# The vanilla Celeste assets must be provided under assets-src/
# (see references/README.md)
cargo run -p releste-content-pipeline -- \
    --source assets-src \
    --output assets \
    --only maps \
    --verify
```

The converted maps land in `assets/maps/`.

## Authoring one yourself

```bash
# Create a new empty map
cargo run -p releste-editor

# Open an existing map
cargo run -p releste-editor -- assets/maps/1-ForsakenCity.map

# Only check that it loads (no GPU / display needed)
cargo run -p releste-editor -- --check assets/maps/1-ForsakenCity.map
```

After the editor saves, `releste-reload`'s `FileWatcher` fires once the debounce
window (100ms by default) elapses, and **the player's position, velocity, state
and carried objects are all preserved** (AGENTS.md 10).

## Per-room scripts

A room may be associated with a Lua script (`.lua`) for cutscenes. A script
**cannot change physics state directly** - every side effect is queued as a
`SimCommand` on the `CutsceneBridge` and consumed by the simulation on the next
frame (AGENTS.md 4.5).

See [`example_cutscene.lua`](example_cutscene.lua).
