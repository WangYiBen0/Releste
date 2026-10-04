# `clock` — sample Releste plugin

A minimal `.so` plugin. It logs one line per second of game time through the
host's `log` callback, which exercises the whole chain: scan → `dlopen` →
`load()` → `tick()` → `unload()`.

## Build

```bash
cargo build --release --manifest-path plugins-src/clock/Cargo.toml
mkdir -p plugins/clock
cp plugins-src/clock/target/release/libclock.so plugins/clock/clock.so
```

The manifest `plugins/clock/plugin.toml` is already in the repo, so after the
copy `releste` picks the plugin up on the next boot:

```text
 [INFO] plugins> loaded `clock`
 [INFO] plugins> clock: 1s of game time
```

## Layout

- `plugins-src/<name>/` — plugin **sources** (standalone cdylib crates; not in
  the main workspace, zero dependencies, AGENTS.md §15.1).
- `plugins/<name>/` — what the game actually loads: `plugin.toml` + the
  artifact. `*.so` is gitignored; the manifest is committed.

## Contract

One symbol: `releste_plugin_v1() -> *const PluginV1`. The host reads `abi`
first and rejects a mismatch before calling anything. Everything crossing the
boundary is `#[repr(C)]` — integers, `CStr`, and JSON; no Rust types.
