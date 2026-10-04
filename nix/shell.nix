# 开发 shell（flake-parts 模块）。
{ ... }:
{
  perSystem =
    {
      pkgs,
      config,
      ...
    }:
    let
      # 复用 rust.nix 里定义的构建参数。
      inherit (config._module.args) commonArgs;
      # 模块把钩子配置挂在 pre-commit.settings 下；
      # shellHook 与 enabledPackages 由它派生（readOnly）。
      hooks = config.pre-commit.settings;
      shellHookText = config.pre-commit.shellHook;
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
          echo ""
          echo "Releste dev shell"
          echo ""
          echo "构建 / 测试（与 CI 完全一致）："
          echo "  nix build .#checks.\$system.fmt          # rustfmt + nixfmt"
          echo "  nix build .#checks.\$system.clippy       # -D warnings"
          echo "  nix build .#checks.\$system.unit-tests   # 182 个测试"
          echo "  nix build .#checks.\$system.pre-commit   # 钩子"
          echo "  nix build                               # 全部二进制"
          echo ""
          echo "直接用 cargo（快速迭代）："
          echo "  cargo test --workspace"
          echo "  cargo run -p reles-editor"
          echo ""
          echo "可选 feature（本 shell 已提供 make / udev / pkg-config）："
          echo "  cargo test -p reles-script --features lua"
          echo "  cargo test -p reles-input  --features gamepad"
          echo ""
          echo "FMOD：本仓库不含 libfmod.so，需自备并指向它："
          echo "  RELESTE_FMOD_ALLOW_MISSING=1 RELESTE_FMOD_LIB=/path/to/libfmod.so \\"
          echo "    cargo test -p reles-audio --features fmod -- --nocapture"
        '';
      };
    };
}
