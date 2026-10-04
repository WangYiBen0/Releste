# Rust 构建定义（flake-parts 模块）。
#
# 用 crane 把"依赖编译"与"本仓库编译"分离：
#   buildDepsOnly  → 第三方依赖，源码改动不会使其失效
#   buildPackage   → 本 workspace
# 这样 `nix build` 在只改业务代码时是秒级的。
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
      # 用 rust-overlay 提供的、由 rust-toolchain.toml 固定的工具链。
      #
      # `fromRustupToolchainFile` 读取仓库根的 rust-toolchain.toml，
      # 因此 Cargo 与 Nix 用的是**同一个** rustc/rustfmt/clippy，
      # 不会出现格式化版本漂移。
      rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml;

      craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchain;

      inherit (pkgs) stdenv;

      # 构建源。
      #
      # `inputs.self` 在 git 仓库里**已经**只包含被 git 跟踪的文件
      # （Nix 复制 flake 源时遵循 .gitignore），因此：
      #   - references/  （~13GB 第三方资料）不会进入 store
      #   - assets/      （派生产物）
      #   - libfmod.so   （用户自备的专有动态库）
      #   - target/ 等
      # 全部被排除。
      #
      # 注意：不要用 `lib.fileset` 在这里再筛一遍——`inputs.self`
      # 是字符串（store 路径），而 fileset 要求真正的 path，
      # 纯求值模式下没有合法的转换方式。保持依赖 git 过滤，
      # 也意味着**必须从 git 仓库使用本 flake**。
      src = inputs.self;

      # 各平台上构建 wgpu / winit 所需的系统库。
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

      # 所有构建共享的参数。
      commonArgs = {
        inherit src;
        pname = "releste";
        version = "0.1.0";

        strictDeps = true;
        doCheck = false;

        nativeBuildInputs = with pkgs; [
          pkg-config
          # `lua` / `gamepad` feature 需要构建 32 位友好的原生依赖。
          gnumake
          perl
        ];

        buildInputs = linuxLibs ++ darwinLibs;
      };

      # 第三方依赖（缓存友好）。
      cargoArtifacts = craneLib.buildDepsOnly (
        commonArgs
        // {
          pname = "releste-deps";
        }
      );

      # 完整 workspace 构建（含所有二进制）。
      releste = craneLib.buildPackage (
        commonArgs
        // {
          inherit cargoArtifacts;
          # 把两个二进制的产物都收集起来。
          cargoExtraArgs = "--workspace";
        }
      );

      # 带可选 feature 的构建：Nix 提供 make / udev / pkg-config，
      # 因此 `lua`（vendored LuaJIT）与 `gamepad`（gilrs）都能启用。
      # 可选 feature 的构建参数。
      #
      # `lua` 需要 vendored LuaJIT（构建期要 make），`gamepad` 需要
      # udev + pkg-config —— 二者都由 Nix 提供（见 commonArgs）。
      fullFeatures = "--workspace --features reles-script/lua,reles-input/gamepad";

      # full 变体有自己的依赖缓存：与默认变体的依赖图不同，
      # 复用同一份 artifacts 会让这些依赖在构建阶段重编。
      cargoArtifactsFull = craneLib.buildDepsOnly (
        commonArgs
        // {
          pname = "releste-deps-full";
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

      # ── 检查项 ────────────────────────────────────────────────
      # 这些会成为 `nix flake check` / `nix build .#checks.<system>.<name>`
      # 的目标。

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
          # 文档中的 broken intra-doc link 视为失败。
          RUSTDOCFLAGS = "-D warnings";
        }
      );

      nextest = craneLib.cargoNextest (
        commonArgs
        // {
          inherit cargoArtifacts;
          pname = "releste-nextest";
          # 同上：不覆盖 doCheck 就一个测试都不会跑。
          doCheck = true;
          partitions = 1;
          partitionType = "count";
        }
      );

      # 不依赖 nextest 的纯 cargo test（CI 里更保险）。
      #
      # 注意 `doCheck = true` 必不可少：crane 的 cargoTest 会遵循
      # doCheck，而 commonArgs 里为了 `buildPackage` 不重复跑测试
      # 把它设成了 false。忘了覆盖的话这个 check 会"通过"但
      # **一个测试都不执行**（日志里只有一行 doCheck is not set）。
      unit-tests = craneLib.cargoTest (
        commonArgs
        // {
          inherit cargoArtifacts;
          pname = "releste-unit-tests";
          doCheck = true;
        }
      );

      # 可选 feature 的测试。
      #
      # `lua` 的脚本引擎在默认构建下不参与编译（需要 make 构建
      # vendored LuaJIT），因此必须有这个 check 才能真正验证它。
      lua-tests = craneLib.cargoTest (
        commonArgs
        // {
          pname = "releste-lua-tests";
          cargoArtifacts = cargoArtifactsFull;
          cargoExtraArgs = fullFeatures;
          doCheck = true;
        }
      );

      # 伪造一个导出 FMOD 3.x 符号的 64 位动态库。
      #
      # 用来验证 fmod_backend 的**代际探测**逻辑：
      # 真实 libfmod.so 由用户自备且不入库，无法进 CI；
      # 但探测逻辑本身必须可测。
      # 三个伪造的 FMOD 动态库，分别导出三代 API 的关键符号。
      #
      # 真实 libfmod.so 由用户自备且不入库，无法进 CI；
      # 但"按符号识别代际"的逻辑本身必须可测。
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
          # FMOD Ex 4.x（刻意不导出 Studio 符号）
          ex = mkStub "ex" "/* FMOD Ex */" ''
            int FMOD_System_Create(void **s) { (void)s; return 0; }
            int FMOD_System_Init(void *s) { (void)s; return 0; }
            int FMOD_System_CreateSound(void *s) { (void)s; return 0; }
          '';
          # FMOD 3.x（刻意不导出 Ex / Studio 符号）
          legacy3 = mkStub "legacy3" "/* FMOD 3.x */" ''
            int FSOUND_Init(int a, int b, int c) { (void)a; (void)b; (void)c; return 1; }
            int FSOUND_PlaySound(int ch, int snd) { (void)ch; (void)snd; return 1; }
            int FSOUND_GetVersion(void) { return 0x0306; }
            int FMUSIC_LoadSong(const char *n) { (void)n; return 1; }
          '';
        };

      # FMOD 代际探测测试：把各桩库通过 RELESTE_FMOD_LIB 喂给后端，
      # 用 RELESTE_FMOD_EXPECT 断言识别结果。
      #
      # 构建期用 RELESTE_FMOD_ALLOW_MISSING 跳过 build.rs 的硬失败
      # （桩库不在标准搜索路径里，运行期才通过环境变量指定）。
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
                cargo test -p reles-audio --features fmod fmod_backend -- --nocapture
            done
          '';
        }
      );

      # 端到端：内容管线转换 + 用运行时加载器回读校验。
      content-pipeline-smoke = craneLib.mkCargoDerivation (
        commonArgs
        // {
          inherit cargoArtifacts;
          pname = "releste-content-pipeline-smoke";
          # 造一个最小 .map，过一遍管线的解析/序列化链路。
          buildPhaseCargoCommand = ''
            cargo run --release -p reles-content-pipeline -- --help >/dev/null
            cargo run --release -p reles-net-server -- --selftest
          '';
          doInstallCargoArtifacts = false;
        }
      );
    in
    {
      _module.args.craneLib = craneLib;
      # 供 hooks.nix 复用，保证 pre-commit 的 rustfmt 与
      # `nix build .#checks.*.fmt` 用的是同一个二进制。
      _module.args.rustToolchain = rustToolchain;
      _module.args.commonArgs = commonArgs;
      _module.args.cargoArtifacts = cargoArtifacts;

      # `nix fmt` 用。nixfmt-tree 遍历仓库里的 .nix 文件，
      # 与 pre-commit 的 `nixfmt` 钩子共用同一版本，避免格式漂移。
      formatter = pkgs.nixfmt-tree;

      packages = {
        inherit releste;
        default = releste;

        full = relesteFull;

        # 单独暴露两个便于快速验证的产物。
        content-pipeline = craneLib.buildPackage (
          commonArgs
          // {
            pname = "reles-content-pipeline";
            inherit cargoArtifacts;
            cargoExtraArgs = "-p reles-content-pipeline";
          }
        );

        editor = craneLib.buildPackage (
          commonArgs
          // {
            pname = "reles-editor";
            inherit cargoArtifacts;
            cargoExtraArgs = "-p reles-editor";
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
        # nextest 在 darwin 上偶有沙箱问题，Linux 才跑。
        inherit nextest;
      };
    };
}
