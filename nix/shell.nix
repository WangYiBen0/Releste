# 开发 shell（flake-parts 模块）。
{ ... }:
{
  perSystem =
    {
      pkgs,
      config,
      lib,
      ...
    }:
    let
      # 复用 rust.nix 里定义的构建参数。
      inherit (config._module.args) commonArgs;
      # 模块把钩子配置挂在 pre-commit.settings 下；
      # shellHook 与 enabledPackages 由它派生（readOnly）。
      hooks = config.pre-commit.settings;
      shellHookText = config.pre-commit.shellHook;

      # `buildInputs` 只在**链接期**可见；运行期的 dlopen 只认
      # `LD_LIBRARY_PATH`，而 NixOS 上没有 /usr/lib —— 不导出它的话
      # winit 的 wayland / x11 / xkbcommon 库一个都找不到，窗口起不来。
      # 这里把它们全部变成 `LD_LIBRARY_PATH`。
      guiLibDirs = lib.makeLibraryPath commonArgs.buildInputs;
    in
    {
      devShells.default = pkgs.mkShell {
        name = "releste";

        inputsFrom = [ config.packages.default ];

        packages =
          (with pkgs; [
            # Rust 工具链（含 rustfmt / clippy / rust-analyzer）
            cargo
            rustc
            rustfmt
            clippy
            rust-analyzer

            # 内容管线需要的图像工具（手工核对图集时用）
            imagemagick

            # 调试 / 性能
            cargo-flamegraph
            hyperfine
            jq
          ])
          # git-hooks 提供的工具（pre-commit 本体等）
          ++ hooks.enabledPackages;

        # vendored LuaJIT 与 gilrs 需要的构建工具。
        nativeBuildInputs = commonArgs.nativeBuildInputs;
        buildInputs = commonArgs.buildInputs;

        RELESTE_DEV_SHELL = "1";

        shellHook = shellHookText + ''
          # 运行期 dlopen 需要 LD_LIBRARY_PATH（见上面的 guiLibDirs）；
          # 已有值时追加，不覆盖。
          export LD_LIBRARY_PATH=${guiLibDirs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
          echo ""
          echo "Releste dev shell"
          echo ""
          echo "构建 / 测试（与 CI 完全一致）："
          echo "  nix build .#checks.\$system.fmt          # rustfmt + nixfmt"
          echo "  nix build .#checks.\$system.clippy       # -D warnings"
          echo "  nix build .#checks.\$system.unit-tests   # 254 个测试"
          echo "  nix build .#checks.\$system.pre-commit   # 钩子"
          echo "  nix build                               # 全部二进制"
          echo ""
          echo "启动游戏（两个入口点是同一个二进制）："
          echo "  cargo r                                  # 快速迭代"
          echo "  nix run .                                # flake 入口"
          echo ""
          echo "直接用 cargo（快速迭代）："
          echo "  cargo test --workspace"
          echo "  cargo run -p reles-editor"
          echo ""
          echo "可选 feature（本 shell 已提供 make / udev / pkg-config）："
          echo "  cargo test -p reles-script --features lua"
          echo "  cargo test -p reles-input  --features gamepad"
          echo ""
          echo "GUI：wayland / libX11 / libxkbcommon / vulkan-loader 已在 LD_LIBRARY_PATH，"
          echo "  窗口直接 cargo run -p reles-game 就能起。"
          echo ""
          echo "FMOD：本仓库不含 libfmod.so，需自备并指向它："
          echo "  RELESTE_FMOD_ALLOW_MISSING=1 RELESTE_FMOD_LIB=/path/to/libfmod.so \\"
          echo "    cargo test -p reles-audio --features fmod -- --nocapture"
        '';
      };
    };
}
