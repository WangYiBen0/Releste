# Rust build definitions (flake-parts module).
#
# crane splits "compile the dependencies" from "compile this repository":
#   buildDepsOnly  -> third-party dependencies (survive source changes)
#   buildPackage   -> this workspace
# so `nix build` stays quick when only application code changed.
{ inputs, ... }:
{
  perSystem =
    {
      config,
      pkgs,
      lib,
      system,
      ...
    }:
    let
      # Rust toolchain, read from the repository's rust-toolchain.toml via
      # fenix. cargo / rustc / rustfmt / clippy therefore come from the same
      # toolchain rustup uses locally - no "fmt passes locally, fails in CI"
      # drift, and no dependency on whatever nixpkgs happens to package.
      #
      # `sha256` is the hash of the toolchain file; bump it whenever
      # rust-toolchain.toml changes (nix prints the right value on mismatch).
      rustToolchain = inputs.fenix.packages.${system}.fromToolchainFile {
        file = ../rust-toolchain.toml;
        sha256 = "sha256-p8h3Sl/YRByZfZTAKXdsvF6xEenXKrXSVvpphmZENH4=";
      };

      craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchain;

      inherit (pkgs) stdenv;

      # Build source.
      #
      # In a git repository `inputs.self` already contains only git-tracked
      # files (Nix copies the flake source following .gitignore), so these
      # never reach the store:
      #   - references/  (~13GB of third-party material)
      #   - assets/      (derived artefacts)
      #   - libfmod.so   (proprietary library the user provides)
      #   - target/, ...
      #
      # Do NOT re-filter with `lib.fileset` here: `inputs.self` is a string
      # (a store path) and fileset wants a real path, which pure evaluation
      # gives no legal way to obtain. Relying on git filtering also means
      # the flake MUST be used from a git checkout.
      src = inputs.self;

      # System libraries wgpu / winit need, per platform.
      linuxLibs = lib.optionals stdenv.hostPlatform.isLinux (
        with pkgs;
        [
          vulkan-loader
          libGL
          libxkbcommon
          wayland
          libX11
          libXcursor
          libXrandr
          libXi
          libXext
          udev
          alsa-lib
        ]
      );

      darwinLibs = lib.optionals stdenv.hostPlatform.isDarwin (
        with pkgs;
        [
          darwin.apple_sdk.frameworks.AppKit
          darwin.apple_sdk.frameworks.QuartzCore
          darwin.apple_sdk.frameworks.Metal
          darwin.apple_sdk.frameworks.Carbon
          darwin.apple_sdk.frameworks.CoreVideo
          darwin.apple_sdk.frameworks.CoreGraphics
          darwin.apple_sdk.frameworks.Foundation
        ]
      );

      # Arguments shared by every build.
      commonArgs = {
        inherit src;
        pname = "releste";
        version = "0.1.0";

        strictDeps = true;
        doCheck = false;

        nativeBuildInputs = with pkgs; [
          pkg-config
          # The `lua` / `gamepad` features build native dependencies.
          gnumake
          perl
        ];

        buildInputs = linuxLibs ++ darwinLibs;
      };

      # Third-party dependencies (the cacheable half).
      #
      # crane's buildDepsOnly appends a `-deps` suffix to pname on its own
      # (lib/buildDepsOnly.nix: pnameSuffix = "-deps"), so pname stays
      # "releste" here - spelling out "releste-deps" yields the silly
      # `releste-deps-deps-0.1.0`.
      cargoArtifacts = craneLib.buildDepsOnly (
        commonArgs
        // {
          pname = "releste";
        }
      );

      # Full workspace build (all binaries).
      #
      # This is what `nix run` executes. The binaries live in the store and
      # the windowed path dlopens wayland / X11 / vulkan-loader at runtime
      # (NixOS has no /usr/lib), so makeWrapper bakes the library path into
      # every binary - headless, editor and pipeline benefit alike.
      releste = craneLib.buildPackage (
        commonArgs
        // {
          inherit cargoArtifacts;
          # Collect every binary in the workspace.
          cargoExtraArgs = "--workspace";
          # Lets `nix run` work on this package (apps.default points at the
          # very same `releste` bin; ROADMAP Phase 1.4).
          meta.mainProgram = "releste";

          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.makeWrapper ];
          postInstall = ''
            for bin in "$out"/bin/*; do
              wrapProgram "$bin" \
                --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath commonArgs.buildInputs}"
            done
          '';
        }
      );

      # Build with the optional features: Nix provides make / udev /
      # pkg-config, so `lua` (vendored LuaJIT) and `gamepad` (gilrs) both work.
      #
      # `lua` needs `make` for vendored LuaJIT, `gamepad` needs udev +
      # pkg-config - both come from commonArgs.
      fullFeatures = "--workspace --features releste-script/lua,releste-input/gamepad";

      # The full variant gets its own dependency cache: its dependency graph
      # differs from the default one, and reusing those artifacts would force
      # a rebuild of the very crates the cache is meant to avoid.
      # (crane appends `-deps` -> `releste-full-deps`.)
      cargoArtifactsFull = craneLib.buildDepsOnly (
        commonArgs
        // {
          pname = "releste-full";
          cargoExtraArgs = fullFeatures;
        }
      );

      relesteFull = craneLib.buildPackage (
        commonArgs
        // {
          pname = "releste-full";
          cargoArtifacts = cargoArtifactsFull;
          cargoExtraArgs = fullFeatures;
        }
      );

      # -- Checks ------------------------------------------------
      # These become the targets of `nix flake check` /
      # `nix build .#checks.<system>.<name>`.

      clippy = craneLib.cargoClippy (
        commonArgs
        // {
          inherit cargoArtifacts;
          cargoClippyExtraArgs = "--workspace --all-targets -- --deny warnings";
        }
      );

      fmt = craneLib.cargoFmt {
        inherit src;
        pname = "releste-fmt";
      };

      doc = craneLib.cargoDoc (
        commonArgs
        // {
          inherit cargoArtifacts;
          # Broken intra-doc links in the API docs are failures.
          RUSTDOCFLAGS = "-D warnings";
        }
      );

      nextest = craneLib.cargoNextest (
        commonArgs
        // {
          inherit cargoArtifacts;
          pname = "releste-nextest";
          # As above: without overriding doCheck, no test runs at all.
          doCheck = true;
          partitions = 1;
          partitionType = "count";
        }
      );

      # Plain cargo test, independent of nextest (safer for CI).
      #
      # `doCheck = true` is mandatory: crane's cargoTest honours doCheck and
      # commonArgs sets it to false so `buildPackage` does not run the suite
      # twice. Forget the override and this check "passes" while running
      # NOTHING (the log only says doCheck is not set).
      unit-tests = craneLib.cargoTest (
        commonArgs
        // {
          inherit cargoArtifacts;
          pname = "releste-unit-tests";
          doCheck = true;
        }
      );

      # Tests for the optional features.
      #
      # The `lua` scripting engine is not compiled in the default build (it
      # needs make for vendored LuaJIT), so this check is the only place it
      # is actually verified.
      lua-tests = craneLib.cargoTest (
        commonArgs
        // {
          pname = "releste-lua-tests";
          cargoArtifacts = cargoArtifactsFull;
          cargoExtraArgs = fullFeatures;
          doCheck = true;
        }
      );

      # Fake 64-bit shared libraries exporting FMOD symbols.
      #
      # They exist to test fmod_backend's generation *probing*: the real
      # libfmod.so is user-provided and never committed, so it cannot go into
      # CI - but the "identify the generation by its symbols" logic must be
      # testable. Three stubs, one per API generation.
      fmodStubs =
        let
          mkStub =
            name: header: symbols:
            pkgs.runCommand "releste-fmod-stub-${name}"
              {
                nativeBuildInputs = [ pkgs.gcc ];
              }
              ''
                mkdir -p $out/lib
                cat >stub.c <<'STUB_EOF'
                ${header}
                ${symbols}
                STUB_EOF
                gcc -shared -fPIC -o $out/lib/libfmod.so stub.c
                test -f $out/lib/libfmod.so
              '';
        in
        {
          # FMOD Studio 5.x+
          studio = mkStub "studio" "/* FMOD Studio */" ''
            void *FMOD_Studio_System_Create(void) { return 0; }
            int FMOD_Studio_System_Initialize(void *s) { (void)s; return 0; }
            int FMOD_Studio_System_GetCoreSystem(void *s) { (void)s; return 0; }
            int FMOD_Studio_System_LoadBankFile(void *s) { (void)s; return 0; }
          '';
          # FMOD Ex 4.x (deliberately exports no Studio symbols)
          ex = mkStub "ex" "/* FMOD Ex */" ''
            int FMOD_System_Create(void **s) { (void)s; return 0; }
            int FMOD_System_Init(void *s) { (void)s; return 0; }
            int FMOD_System_CreateSound(void *s) { (void)s; return 0; }
          '';
          # FMOD 3.x (deliberately exports no Ex / Studio symbols)
          legacy3 = mkStub "legacy3" "/* FMOD 3.x */" ''
            int FSOUND_Init(int a, int b, int c) { (void)a; (void)b; (void)c; return 1; }
            int FSOUND_PlaySound(int ch, int snd) { (void)ch; (void)snd; return 1; }
            int FSOUND_GetVersion(void) { return 0x0306; }
            int FMUSIC_LoadSong(const char *n) { (void)n; return 1; }
          '';
        };

      # FMOD generation probe: hand each stub to the backend through
      # RELESTE_FMOD_LIB and assert the detected generation with
      # RELESTE_FMOD_EXPECT.
      #
      # RELESTE_FMOD_ALLOW_MISSING skips build.rs's hard failure at build
      # time: the stubs are not on the standard search path, they are only
      # pointed at at run time.
      fmod-probe = craneLib.mkCargoDerivation (
        commonArgs
        // {
          inherit cargoArtifacts;
          pname = "releste-fmod-probe";
          doCheck = false;
          doInstallCargoArtifacts = false;
          RELESTE_FMOD_ALLOW_MISSING = "1";
          buildPhaseCargoCommand = ''
            for pair in "studio:${fmodStubs.studio}" "ex:${fmodStubs.ex}" "legacy3:${fmodStubs.legacy3}"; do
              expect="''${pair%%:*}"
              lib="''${pair#*:}/lib/libfmod.so"
              echo "--- expecting generation: $expect"
              RELESTE_FMOD_LIB="$lib" RELESTE_FMOD_EXPECT="$expect" \
                cargo test -p releste-audio --features fmod fmod_backend -- --nocapture
            done
          '';
        }
      );

      # End to end: content-pipeline conversion + read-back through the
      # runtime loader.
      content-pipeline-smoke = craneLib.mkCargoDerivation (
        commonArgs
        // {
          inherit cargoArtifacts;
          pname = "releste-content-pipeline-smoke";
          # Exercise the parse/serialise path over a minimal .map.
          buildPhaseCargoCommand = ''
            cargo run --release -p releste-content-pipeline -- --help >/dev/null
            cargo run --release -p releste-net-server -- --selftest
          '';
          doInstallCargoArtifacts = false;
        }
      );
    in
    {
      _module.args.craneLib = craneLib;
      # Reused by hooks.nix so pre-commit's rustfmt is the same binary as
      # `nix build .#checks.*.fmt`.
      _module.args.rustToolchain = rustToolchain;
      _module.args.commonArgs = commonArgs;
      _module.args.cargoArtifacts = cargoArtifacts;

      # For `nix fmt`. nixfmt-tree walks the repository's .nix files and
      # shares its version with the `nixfmt` pre-commit hook, so formatting
      # cannot drift between the two.
      formatter = pkgs.nixfmt-tree;

      packages = {
        inherit releste;
        default = releste;

        full = relesteFull;

        # Expose two more artefacts for quick checks.
        content-pipeline = craneLib.buildPackage (
          commonArgs
          // {
            pname = "releste-content-pipeline";
            inherit cargoArtifacts;
            cargoExtraArgs = "-p releste-content-pipeline";
          }
        );

        editor = craneLib.buildPackage (
          commonArgs
          // {
            pname = "releste-editor";
            inherit cargoArtifacts;
            cargoExtraArgs = "-p releste-editor";
          }
        );
      };

      checks = {
        inherit
          releste
          relesteFull
          clippy
          fmt
          doc
          unit-tests
          lua-tests
          fmod-probe
          content-pipeline-smoke
          ;
      }
      // lib.optionalAttrs (!stdenv.hostPlatform.isDarwin) {
        # nextest occasionally trips over the darwin sandbox; Linux only.
        inherit nextest;
      };
    };
}
