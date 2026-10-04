# pre-commit / pre-push 钩子（flake-parts 模块，基于 cachix/git-hooks.nix）。
#
# 用法：
#   nix build .#checks.<system>.pre-commit   # 在沙箱里对全部文件跑一遍
#   nix develop                              # 进 shell 时自动装钩子
{ inputs, ... }:
{
  perSystem =
    {
      config,
      pkgs,
      system,
      ...
    }:
    let
      # 与 flakes 的 fmt check 共用同一个被固定的 toolchain。
      inherit (config._module.args) rustToolchain;

      # 把 lint 脚本包成可执行文件，并为每个检查项生成一个钩子。
      lint = mode: args: {
        enable = true;
        name = "releste-${mode}";
        entry = "${
          pkgs.writeShellApplication {
            name = "releste-lint-files";
            runtimeInputs = with pkgs; [
              coreutils
              gnugrep
              file
            ];
            text = builtins.readFile ./scripts/releste-lint-files.sh;
          }
        }/bin/releste-lint-files ${mode} ${args}";
        language = "system";
        pass_filenames = true;
      };
    in
    {
      pre-commit.settings = {
        # 只检查将要提交的文件。
        default_stages = [ "pre-commit" ];

        hooks = {
          # ── 受限内容守卫 ────────────────────────────────────
          #
          # .gitignore 是被动的（静默忽略），本钩子是主动的：
          # 一旦有人 `git add -f` 强行加入受限文件，提交会被拒绝。
          #
          # 受 AGENTS.md 与分发许可约束，仓库不得包含：
          #   - references/  原版 Celeste 可执行文件 / DLL / 反编译源码
          #   - assets/      由原版资源派生的美术 / 音频 / 地图
          #   - 任何动态库（libfmod.so 等由使用者自备）
          #   - .map 等自研二进制格式
          restricted-content = lint "restricted" "";

          # 兜底：单个文件不得超过 512 KiB。
          # 当前最大的已跟踪文件是 Cargo.lock（约 113 KiB）。
          added-large-files = lint "large" "--maxkb=512";

          # ── 仓库卫生 ────────────────────────────────────────
          trailing-whitespace = lint "whitespace" "";
          end-of-file = lint "eof" "";
          line-ending = lint "line-ending" "";
          merge-conflict = lint "conflict" "";
          private-key = lint "private-key" "";
          case-conflict = lint "case-conflict" "";

          # ── Rust ────────────────────────────────────────────
          # 用 nixpkgs 的 rustfmt，保证与 `nix build .#checks.*.fmt`
          # 完全一致（版本不同会导致"本地过了 CI 挂"）。
          #
          # 关键：把钩子的 cargo/rustfmt 指向 flake 里那个被固定的
          # toolchain。默认它用 nixpkgs 的 rustfmt，版本可能与
          # rust-overlay 的不同，导致 pre-commit 与 nix build 互相打架。
          rustfmt = {
            enable = true;
            files = "\\.rs$";
            packageOverrides = {
              cargo = rustToolchain;
              rustfmt = rustToolchain;
            };
          };

          # ── Nix ─────────────────────────────────────────────
          # 注意：`nixfmt-rfc-style` / `nixpkgs-fmt` 已被 git-hooks.nix
          # 移除，统一用 `nixfmt`。
          nixfmt = {
            enable = true;
            files = "\\.nix$";
          };
        };
      };

      # `check.enable` 默认为 true，会自动注册 checks.pre-commit。
      # 这里显式写出，便于 `nix flake show` 呈现。
      pre-commit.check.enable = true;
    };
}
