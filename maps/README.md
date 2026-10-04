# 示例地图

`.map` 是**自研二进制格式**（见 [`AGENTS.md` §7.3](../AGENTS.md)），
不纳入版本控制——仓库只包含文本源码。

## 从原版资源生成

```bash
# 需要自备原版 Celeste 资源到 assets-src/（见 references/README.md）
cargo run -p reles-content-pipeline -- \
    --source assets-src \
    --output assets \
    --only maps \
    --verify
```

产出的地图位于 `assets/maps/`。

## 自己画一张

```bash
# 新建空地图
cargo run -p reles-editor

# 打开已有地图
cargo run -p reles-editor -- assets/maps/1-ForsakenCity.map

# 只校验能否加载（不需要 GPU / 显示器）
cargo run -p reles-editor -- --check assets/maps/1-ForsakenCity.map
```

编辑器保存后，`reles-reload` 的 `FileWatcher` 会在
debounce 窗口（默认 100ms）后触发重载，
**玩家位置、速度、状态与携带物全部保留**（AGENTS.md §10）。

## 单图脚本

每个房间可以关联一个 Lua 脚本（`.lua`），用于过场演出。
脚本**不能直接改物理状态**——所有副作用通过
`CutsceneBridge` 的 `SimCommand` 队列排队，下一帧由 sim 消费
（AGENTS.md §4.5）。

见 [`example_cutscene.lua`](example_cutscene.lua)。
