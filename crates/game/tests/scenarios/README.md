# 场景脚本（LLM 友好的行为回归）

每个 `.toml` 是一份**确定性**回放：显式输入 + 断言。没有插件、没有热重载、
没有墙钟依赖 —— 同一份脚本永远得到同一条轨迹。

```bash
cargo run --bin releste -- --script crates/game/tests/scenarios/walk_and_stop.toml
```

断言失败会逐条打印（并返回退出码 1）：

```text
FAIL tick=40 vx: expected 1.6000, got 1.5000 (Δ0.1000 > tol 0.0500)
scenario: 60 ticks, 10 checks, 1 failure(s)
```

## 为什么这么设计

- **写原版数值，不写"跑出来的数值"**：期望值应当能从
  `references/pureCeleste.cs` 推出来（如 `MaxRun = 90f` → 1.5 px/tick），
  脚本注释里写下出处行号。抄一遍当前实现只能锁住现状，锁不住正确性。
- **位移用 `dx`/`dy`**（相对 tick 1），别用绝对 `x`/`y`：地图转换产物
  （`assets/`，gitignored）换了出生点也不会误报。
- **拼错的键直接报错**（`deny_unknown_fields`），不会静默当默认值。

## 字段

```toml
[scenario]
ticks = 60                 # 跑几次 kernel.tick()（1-based 计）
map = "0-Intro.map"        # 期望的地图；不符即报错（防止 --map 写错）
trace = ".devtmp/out.jsonl" # 可选：逐 tick 轨迹（JSONL）
note = "一句话说明场景"

[[input]]                  # 可多段；重叠处按位或
from = 5
to = 40                    # 含两端
right = true               # left/right/up/down/jump/dash/grab

[[expect]]                 # 可多条；只检查写了的字段
tick = 40
vx = 1.5                   # 浮点比较用 tol（默认 0.05）
dx = 50.667
anim = "runFast"
state = 0                  # 0 = Normal、1 = Climb、2 = Dash
dashes = 1
on_ground = true
```

需要新数值时，先看轨迹再写断言：

```bash
cargo run --bin releste -- --script <scenario>.toml   # 用 [scenario] trace 落盘
# 再逐行读 JSONL：字段名即断言名，单位写在名字里（vx = px/tick，vx_ps = px/s）
```
