---
name: releste-vanilla-audit
description: Use when writing or reviewing Releste code that ports Celeste behavior (Player, camera, transitions, backdrops, tiles) — audits every behavior line against references/pureCeleste.cs with line citations, and runs the repo's full gate including `nix flake check` before any commit. Also use when the user says "审查", "对齐原版", "不许有偏差".
---

# Releste 原版对齐审查

`references/pureCeleste.cs`（131320 行，反编译）是**行为权威**；本仓库的规则是
「每一行行为代码都要能指回原版行号」。审查时逐条执行下列步骤。

## 1. 逐行对齐

1. 对每个被审查文件里的行为行（常量、公式、状态转移、绘制顺序），在
   `pureCeleste.cs` 找到出处，注释格式：`// `pureCeleste.cs:LINE`` 或
   `（`pureCeleste.cs:LINE-LINE`）`。**没有出处的行为行 = 违规**，要么补出处、
   要么在注释里写明"引擎自造 + 理由"。
2. **tick 换算**：原版是 60Hz、`Speed` 单位 px/s。本引擎固定步长 1 tick =
   1/60s、速度单位 px/tick —— 换算一律 `px/s ÷ 60`，且必须写成精确的
   `Fx::from_ratio`（如 105 px/s = `-7/4` px/tick）。抽查每个常量。
3. **tick 内顺序**：原版一个 tick 的顺序（如 Player.Update：计时递减 →
   NormalUpdate（跑动 → maxFall → 重力 → varJump 钳制 → 跳跃判定）→
   MoveH/MoveV → EnforceBounds）必须与引擎一致；顺序错=手感错。
4. **渲染/可见性规则**：原版 `X.Render` 的循环次序、层次（Depth 排序是
   降序=值小者后画）、每帧 vs 缓存，都要核对（如 backdrops 每帧重建、
   转场结束才卸载旧房间）。
5. 缺数据/未实现的部分必须 `warn_once` 或文档注明，禁止静默省略
   （AGENTS.md §15.8/15.9）。

## 2. 测试

- 纯逻辑（物理、几何、格式）必须有单测断言**原版数值**（不是"合理值"）；
  例子：`jump pins at JUMP_SPEED`、`v2_fixture_still_loads_and_migrates`。
- 纯渲染代码可免测试，但要在提交信息里写手动清单（AGENTS.md §9）。

## 3. 提交前门禁（缺一不可，顺序执行）

```bash
cd /home/w1ngd1nga5ter/Projects/Releste
export PRE_COMMIT_HOME="$PWD/.devtmp/pc"     # 本机 pre-commit 缓存只读，必须重定向
cargo fmt --all
cargo build --workspace --all-targets 2>&1 | grep -cE "^(warning|error)"   # 必须为 0
cargo test --workspace                        # 0 failed
# nix 门禁（新版 clippy + rustdoc + fmt + hooks），XDG/TMPDIR 必须重定向，
# 否则 fetcher-lock 只读 / TMPDIR 指向已消失的 shell 临时目录而误报：
env XDG_CACHE_HOME="$PWD/.devtmp/nix-cache" \
    XDG_STATE_HOME="$PWD/.devtmp/nix-state" \
    XDG_DATA_HOME="$PWD/.devtmp/nix-data" \
    TMPDIR="$PWD/.devtmp/tmp" \
    nix flake check
```

`nix flake check` **必须在 commit 之前**跑——它用的 clippy 比本地新
（会抓 chunks_exact_to_as_chunks、unnecessary_to_owned、too_many_arguments、
rustdoc intra-doc link（`node[0]` 这类方括号要转义）等本地看不见的问题），
且 fmt 规则一致。**没有 FLAKE-OK 不许 commit。**

## 4. 提交纪律

- Conventional Commits，type(scope): subject ≤72 字符（AGENTS.md §5.3）。
- 一个提交一个逻辑变更；提交信息里写原版行号出处与动机。
- 工作树必须干净；提交后 `git log -1 --format=%B` 复查信息没被 shell 吃掉
  字符（反引号、括号要用 heredoc `-F file` 传）。

## 5. 常见偏差清单（审查时重点盯）

- 速度单位忘了 ÷60；`Fx` 用 f32 常数代替精确 ratio（§15.7）。
- 松键砍速、 Coyote/缓冲帧数拍脑袋 —— 一律回 pureCeleste 查
  （VirtualButton.BufferTime 110376、jumpGraceTimer 119832）。
- 相机/转场/背景的"合理猜测"——原版全有出处（相机 120133-120146、
  转场 115282-115423、EnforceBounds 116517-116607、backdrops 112163-112906）。
- HashMap 用于需要字节确定性的结构（应 BTreeMap，E.8）。
