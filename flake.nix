{
  description = "Releste - a from-scratch rewrite of a 2D precision platformer engine";

  # Inputs are pinned by flake.lock, which is committed. Do NOT put revisions
  # in these URLs: that duplicates what the lock already guarantees and turns
  # every update into a manual edit of this file.
  #
  #   update one input:  nix flake update nixpkgs
  #   update everything: nix flake update
  #   (add --commit-lock-file to commit the new lock in one step)
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };

    # crane takes no nixpkgs input (the caller passes pkgs), so no follows.
    crane.url = "github:ipetkov/crane";

    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    # Rust toolchain: fenix mirrors the toolchain declared in
    # rust-toolchain.toml, so cargo / rustc / rustfmt / clippy are the same
    # binaries locally (rustup) and in the flake (CI) and cannot drift.
    fenix = {
      url = "github:nix-community/fenix";
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

      # x86_64-darwin is deliberately absent: the pinned nixpkgs (26.11)
      # dropped it. For Intel Mac support, follow nixpkgs-26.05-darwin instead.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      perSystem =
        { system, config, ... }:
        {
          # Plain nixpkgs for this system. The Rust toolchain is NOT an
          # overlay any more: it comes from fenix as an explicit derivation
          # (see nix/rust.nix), which keeps cross-compilation and the
          # toolchain file as the single source of truth.
          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
          };

          # `nix run .` runs the very same `releste` binary as bare `cargo r`
          # (ROADMAP Phase 1.4 "one-command entry points").
          apps = {
            default = {
              type = "app";
              program = "${config.packages.default}/bin/releste";
            };
          };
        };

      # Repository-wide metadata (does not vary per system).
      flake = {
        # Build arguments other flakes may reuse.
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
