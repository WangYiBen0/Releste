{
  description = "Releste — 从零重写的 2D 精确平台跳跃引擎";

  # 输入按 rev 固定，保证跨机器可复现。
  # 升级：nix flake update --commit-lock-file
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/c2313a03e6edc6232c6c33da23414b1e9a7e74a1";

    flake-parts = {
      url = "github:hercules-ci/flake-parts/024633cd702b10285db5cb19b40ad48d2399ba60";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };

    # crane 不带 nixpkgs 输入（用调用方传入的 pkgs），因此无需 follows。
    crane.url = "github:ipetkov/crane/47b6b27ed9a3a9181415e4367d0c30ab2a0e0250";

    git-hooks = {
      url = "github:cachix/git-hooks.nix/a0e4241b51206fbcbf52fd322eb5f0cd80f153c4";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # Rust 工具链。
    #
    # rust-overlay 让 rustc / rustfmt / clippy 来自同一个被固定的
    # toolchain（由 rust-toolchain.toml 声明），而不是靠 nixpkgs
    # 恰好打包的那一套——否则本机与 CI 的 rustfmt 版本可能不同，
    # 出现"本地格式化通过、nix build 的 fmt 失败"。
    rust-overlay = {
      url = "github:oxalica/rust-overlay/7470661ba2b51156048af08464b82020223dc942";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.git-hooks.flakeModule
        ./nix/rust.nix
        ./nix/hooks.nix
        ./nix/shell.nix
      ];

      # 注意：不列 x86_64-darwin —— 当前固定的 nixpkgs(26.11) 已弃用该平台。
      # 需要 Intel Mac 支持时改用 nixpkgs-26.05-darwin 分支。
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      # 让所有模块看到的是"叠加了 rust-overlay"的 pkgs，
      # 这样 pkgs.rust-bin 才可用。
      perSystem =
        { system, ... }:
        {
          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ inputs.rust-overlay.overlays.default ];
          };
        };

      # 仓库级元信息（不随 system 变化）。
      flake = {
        # 供其它 flake 复用的构建参数。
        lib = {
          supportedSystems = [
            "x86_64-linux"
            "aarch64-linux"
            "aarch64-darwin"
          ];
        };
      };
    };
}
