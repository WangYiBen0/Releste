# Development shell (flake-parts module).
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
      # Reuse the build arguments defined in rust.nix.
      inherit (config._module.args) commonArgs rustToolchain;
      # The module hangs the hook configuration under pre-commit.settings;
      # shellHook and enabledPackages are derived from it (read-only).
      hooks = config.pre-commit.settings;
      shellHookText = config.pre-commit.shellHook;

      # `buildInputs` are visible at link time only. Runtime dlopen looks at
      # `LD_LIBRARY_PATH`, and NixOS has no /usr/lib - without exporting it,
      # none of winit's wayland / X11 / xkbcommon libraries can be found and
      # the window never opens. So make them all part of `LD_LIBRARY_PATH`.
      guiLibDirs = lib.makeLibraryPath commonArgs.buildInputs;
    in
    {
      devShells.default = pkgs.mkShell {
        name = "releste";

        inputsFrom = [ config.packages.default ];

        packages =
          # The very toolchain the builds use (fenix, from rust-toolchain.toml) -
          # not nixpkgs' separate cargo/rustc/rustfmt/clippy, which would drift
          # from `nix build`.
          [ rustToolchain ]
          ++ (with pkgs; [
            # Image tooling for the content pipeline (manual atlas checks).
            imagemagick

            # Debugging / benchmarking.
            cargo-flamegraph
            hyperfine
            jq
          ])
          # Tools provided by git-hooks (pre-commit itself, ...).
          ++ hooks.enabledPackages;

        # Build tools for vendored LuaJIT and gilrs.
        nativeBuildInputs = commonArgs.nativeBuildInputs;
        buildInputs = commonArgs.buildInputs;

        RELESTE_DEV_SHELL = "1";

        shellHook = shellHookText + ''
          # Runtime dlopen needs LD_LIBRARY_PATH (see guiLibDirs above);
          # append to an existing value instead of clobbering it.
          export LD_LIBRARY_PATH=${guiLibDirs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
          echo ""
          echo "Releste dev shell"
          echo ""
          echo "Build / test (exactly what CI runs):"
          echo "  nix build .#checks.\$system.fmt          # rustfmt + nixfmt"
          echo "  nix build .#checks.\$system.clippy       # -D warnings"
          echo "  nix build .#checks.\$system.unit-tests   # whole workspace suite"
          echo "  nix build .#checks.\$system.pre-commit   # git hooks"
          echo "  nix build                               # every binary"
          echo ""
          echo "Run the game (both entry points are the same binary):"
          echo "  cargo r                                  # fast iteration"
          echo "  nix run .                                # flake entry point"
          echo ""
          echo "Plain cargo (fast iteration):"
          echo "  cargo test --workspace"
          echo "  cargo run -p releste-editor"
          echo ""
          echo "Optional features (this shell provides make / udev / pkg-config):"
          echo "  cargo test -p releste-script --features lua"
          echo "  cargo test -p releste-input  --features gamepad"
          echo ""
          echo "GUI: wayland / libX11 / libxkbcommon / vulkan-loader are on LD_LIBRARY_PATH,"
          echo "  so \`cargo run -p releste-game\` opens a window as-is."
          echo ""
          echo "FMOD: this repository ships no libfmod.so - provide your own:"
          echo "  RELESTE_FMOD_ALLOW_MISSING=1 RELESTE_FMOD_LIB=/path/to/libfmod.so \\"
          echo "    cargo test -p releste-audio --features fmod -- --nocapture"
        '';
      };
    };
}
