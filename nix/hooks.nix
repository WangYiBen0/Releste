# pre-commit / pre-push hooks (flake-parts module, backed by
# cachix/git-hooks.nix).
#
# Usage:
#   nix build .#checks.<system>.pre-commit   # run over all files, sandboxed
#   nix develop                              # hooks install on shell entry
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
      # Shares the pinned toolchain with the flake's fmt check.
      inherit (config._module.args) rustToolchain;

      # Wrap the lint script into an executable and generate one hook per
      # check.
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
        # Only inspect the files about to be committed.
        default_stages = [ "pre-commit" ];

        hooks = {
          # ── Restricted content guard ────────────────────────
          #
          # .gitignore is passive (it ignores silently); this hook is active:
          # if someone forces a restricted file in with `git add -f`, the
          # commit is rejected.
          #
          # AGENTS.md and the distribution licence forbid:
          #   - references/  vanilla Celeste executables / DLLs / decompiled source
          #   - assets/      art, audio and maps derived from vanilla assets
          #   - any shared library (libfmod.so and friends are user-provided)
          #   - engine-native binary formats such as .map
          restricted-content = lint "restricted" "";

          # Backstop: no single file may exceed 512 KiB.
          # The largest tracked file today is Cargo.lock (~113 KiB).
          added-large-files = lint "large" "--maxkb=512";

          # ── Repository hygiene ──────────────────────────────
          trailing-whitespace = lint "whitespace" "";
          end-of-file = lint "eof" "";
          line-ending = lint "line-ending" "";
          merge-conflict = lint "conflict" "";
          private-key = lint "private-key" "";
          case-conflict = lint "case-conflict" "";

          # ── Rust ────────────────────────────────────────────
          # Point the hook's cargo/rustfmt at the flake's pinned toolchain,
          # never at nixpkgs' own rustfmt: a version mismatch makes the hook
          # and `nix build .#checks.*.fmt` fight each other.
          rustfmt = {
            enable = true;
            files = "\\.rs$";
            packageOverrides = {
              cargo = rustToolchain;
              rustfmt = rustToolchain;
            };
          };

          # ── Nix ─────────────────────────────────────────────
          # git-hooks.nix dropped `nixfmt-rfc-style` / `nixpkgs-fmt`;
          # `nixfmt` is the single formatter now.
          nixfmt = {
            enable = true;
            files = "\\.nix$";
          };
        };
      };

      # `check.enable` defaults to true and would register checks.pre-commit
      # anyway; spelling it out makes it visible in `nix flake show`.
      pre-commit.check.enable = true;
    };
}
