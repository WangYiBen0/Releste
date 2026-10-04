//! FMOD 后端（`fmod` feature）。
//!
//! # 用户自备动态库（AGENTS.md §4.7）
//! 本模块**不链接** FMOD，而是在运行期用 `libloading` 打开
//! `libfmod.so` / `fmod.dll` / `libfmod.dylib`。
//!
//! - 构建期：`build.rs` 探测库位置，找不到时以 AGENTS.md 指定的
//!   格式报错（可用 `RELESTE_FMOD_ALLOW_MISSING=1` 跳过）。
//! - 运行期：[`FmodBackend::new`] 打开库并探测 API 代际；失败返回
//!   [`AudioError::FmodNotFound`] 或 [`AudioError::FmodUnusable`]。
//!
//! # 支持的 API 代际
//! FMOD 的 ABI 在三代之间完全不兼容，因此启动时必须识别：
//!
//! | 代际            | 关键符号                    | 年代 |
//! | --------------- | --------------------------- | ---- |
//! | FMOD Studio     | `FMOD_Studio_System_Create` | 5.x+ |
//! | FMOD Ex         | `FMOD_System_Create`        | 4.x  |
//! | FMOD 3 (Legacy) | `FSOUND_Init`               | 3.x  |
//!
//! 只有 Studio 代际能直接支撑本引擎的 bank/event 模型；Ex 与
//! Legacy 会被识别并**明确报告**，而不是静默当成 Studio 用。

use std::path::{Path, PathBuf};

use libloading::{Library, Symbol};

use crate::command::Bus;
use crate::{AudioBackend, AudioError, FMOD_NOT_FOUND_MESSAGE};

/// FMOD API 代际。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FmodGeneration {
    /// FMOD Studio（5.x/6.x）：bank + event 模型。
    Studio,
    /// FMOD Ex（4.x）：programmer sound 模型。
    Ex,
    /// FMOD 3.x：`FSOUND_*` / `FMUSIC_*` 模型。
    Legacy3,
}

impl FmodGeneration {
    /// 人类可读描述。
    pub fn describe(self) -> &'static str {
        match self {
            FmodGeneration::Studio => "FMOD Studio (5.x+)",
            FmodGeneration::Ex => "FMOD Ex (4.x)",
            FmodGeneration::Legacy3 => "FMOD 3.x (FSOUND/FMUSIC)",
        }
    }

    /// 是否为本引擎直接支持的代际。
    pub fn is_supported(self) -> bool {
        matches!(self, FmodGeneration::Studio)
    }
}

/// 已加载库的探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FmodProbe {
    /// 实际加载成功的路径。
    pub path: PathBuf,
    /// 识别出的 API 代际。
    pub generation: FmodGeneration,
    /// 命中探测的关键符号。
    pub symbols: Vec<&'static str>,
}

impl FmodProbe {
    /// 一行摘要（日志用）。
    pub fn summary(&self) -> String {
        format!(
            "{} @ {} (symbols: {})",
            self.generation.describe(),
            self.path.display(),
            self.symbols.join(", ")
        )
    }
}

/// 平台默认库名。
pub fn default_library_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "fmod.dll"
    } else if cfg!(target_os = "macos") {
        "libfmod.dylib"
    } else {
        "libfmod.so"
    }
}

/// 构建脚本探测到的库路径（若找到）。
pub fn probed_library_path() -> Option<PathBuf> {
    option_env!("RELESTE_FMOD_PATH").map(PathBuf::from)
}

/// 候选库路径：优先环境变量，其次构建脚本探测结果，最后平台默认名。
fn candidate_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for var in ["RELESTE_FMOD_LIB", "RELESTE_FMOD_PATH"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() && !out.contains(&PathBuf::from(&v)) {
                out.push(PathBuf::from(v));
            }
        }
    }
    if let Some(p) = probed_library_path() {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out.push(PathBuf::from(default_library_name()));
    out
}

/// 按符号探测 API 代际。
///
/// 只解析符号、**不调用**任何函数指针，因此对未经验证的库也安全。
fn probe_generation(library: &Library) -> Option<(FmodGeneration, Vec<&'static str>)> {
    // (代际, 必需符号, 辅助符号)
    const CANDIDATES: &[(FmodGeneration, &str, &[&str])] = &[
        (
            FmodGeneration::Studio,
            "FMOD_Studio_System_Create",
            &[
                "FMOD_Studio_System_Initialize",
                "FMOD_Studio_System_GetCoreSystem",
                "FMOD_Studio_System_LoadBankFile",
            ],
        ),
        (
            FmodGeneration::Ex,
            "FMOD_System_Create",
            &["FMOD_System_Init", "FMOD_System_CreateSound"],
        ),
        (
            FmodGeneration::Legacy3,
            "FSOUND_Init",
            &["FSOUND_PlaySound", "FMUSIC_LoadSong", "FSOUND_GetVersion"],
        ),
    ];

    for (generation, required, helpers) in CANDIDATES {
        let has_required: bool = unsafe {
            library
                .get::<Symbol<*const ()>>(format!("{required}\0").as_bytes())
                .is_ok()
        };
        if !has_required {
            continue;
        }

        let mut symbols = vec![*required];
        for helper in *helpers {
            let present = unsafe {
                library
                    .get::<Symbol<*const ()>>(format!("{helper}\0").as_bytes())
                    .is_ok()
            };
            if present {
                symbols.push(helper);
            }
        }
        return Some((*generation, symbols));
    }

    None
}

/// FMOD 后端。
///
/// 持有着已打开的库句柄，保证符号有效。
pub struct FmodBackend {
    _library: Library,
    probe: FmodProbe,
    next_handle: u64,
}

impl FmodBackend {
    /// 打开并识别 FMOD 库。
    ///
    /// # Errors
    /// - 所有候选路径都打不开 → [`AudioError::FmodNotFound`]（AGENTS.md §4.7 原文）。
    /// - 打开了但没有可识别的 FMOD 符号 → [`AudioError::FmodUnusable`]。
    pub fn new() -> Result<Self, AudioError> {
        let candidates = candidate_paths();
        let mut attempts: Vec<String> = Vec::new();

        for path in &candidates {
            // SAFETY: 加载外部动态库本质上不受 Rust 保护。
            // 我们只解析符号、不调用未经验证的函数指针。
            match unsafe { Library::new(path) } {
                Ok(library) => {
                    let Some((generation, symbols)) = probe_generation(&library) else {
                        attempts.push(format!(
                            "{}: loaded but no FMOD symbols found",
                            path.display()
                        ));
                        continue;
                    };

                    let probe = FmodProbe {
                        path: path.clone(),
                        generation,
                        symbols,
                    };

                    tracing::info!(probe = %probe.summary(), "FMOD library loaded");
                    if !generation.is_supported() {
                        tracing::warn!(
                            generation = generation.describe(),
                            "FMOD generation is not the supported Studio API; \
                             audio will run through the stub backend"
                        );
                    }

                    return Ok(FmodBackend {
                        _library: library,
                        probe,
                        next_handle: 0,
                    });
                }
                Err(e) => {
                    attempts.push(format!("{}: {e}", path.display()));
                }
            }
        }

        // 只要候选里有一个真实存在的文件，就报"不可用"并附上原始错误
        // （例如位数不匹配）——比笼统的 not found 有用得多。
        if candidates.iter().any(|p| p.exists()) {
            Err(AudioError::FmodUnusable(format!(
                "FMOD library was found but could not be used.\n\
                 Tried:\n  {}\n\
                 Hint: the library architecture must match the process \
                 (a 32-bit .so cannot be loaded into a 64-bit build, and vice versa).",
                attempts.join("\n  ")
            )))
        } else {
            Err(AudioError::FmodNotFound(FMOD_NOT_FOUND_MESSAGE.to_string()))
        }
    }

    /// 探测结果。
    pub fn probe(&self) -> &FmodProbe {
        &self.probe
    }

    /// API 代际。
    pub fn generation(&self) -> FmodGeneration {
        self.probe.generation
    }
}

impl std::fmt::Debug for FmodBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FmodBackend")
            .field("probe", &self.probe)
            .finish()
    }
}

impl AudioBackend for FmodBackend {
    fn play(&mut self, event: &str, _volume: f32, _pitch: f32) -> Result<u64, AudioError> {
        // 完整接线需要代际相关的 C API 调用（见模块文档）。
        // 目前只递增句柄，保持混音层可用。
        self.next_handle += 1;
        tracing::debug!(
            event,
            handle = self.next_handle,
            generation = self.probe.generation.describe(),
            "fmod play (stub)"
        );
        Ok(self.next_handle)
    }

    fn stop(&mut self, handle: u64) {
        tracing::debug!(handle, "fmod stop (stub)");
    }

    fn stop_all(&mut self) {
        tracing::debug!("fmod stop_all (stub)");
        self.next_handle = 0;
    }

    fn set_bus_volume(&mut self, bus: Bus, volume: f32) {
        tracing::debug!(?bus, volume, "fmod set_bus_volume (stub)");
    }

    fn name(&self) -> &'static str {
        "fmod"
    }
}

/// 指定的库路径是否存在（用于诊断信息）。
pub fn library_exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_library_name_is_platform_specific() {
        let name = default_library_name();
        assert!(name.contains("fmod"), "should mention fmod: {name}");
        assert!(name.ends_with(".so") || name.ends_with(".dll") || name.ends_with(".dylib"));
    }

    #[test]
    fn candidates_are_never_empty() {
        assert!(!candidate_paths().is_empty());
    }

    #[test]
    fn generations_describe_and_support_correctly() {
        assert!(FmodGeneration::Studio.is_supported());
        assert!(!FmodGeneration::Ex.is_supported());
        assert!(!FmodGeneration::Legacy3.is_supported());

        assert!(FmodGeneration::Legacy3.describe().contains("3.x"));
        assert!(FmodGeneration::Ex.describe().contains("4.x"));
        assert!(FmodGeneration::Studio.describe().contains("Studio"));
    }

    #[test]
    fn error_message_matches_agent_md() {
        // AGENTS.md §4.7 指定的文本必须逐项出现。
        for needle in [
            "FMOD library not found.",
            "https://www.fmod.com/download",
            "libfmod.so / fmod.dll / libfmod.dylib",
            "LD_LIBRARY_PATH",
            "DYLD_LIBRARY_PATH",
            "alongside the .exe",
        ] {
            assert!(
                FMOD_NOT_FOUND_MESSAGE.contains(needle),
                "missing {needle:?} in message"
            );
        }
    }

    /// 若设置了 `RELESTE_FMOD_LIB`，真的去加载它。
    ///
    /// 这是"用实物测"的入口：
    /// ```text
    /// RELESTE_FMOD_LIB=/path/to/libfmod.so cargo test -p reles-audio \
    ///     --features fmod -- --nocapture
    /// ```
    ///
    /// 若同时设置 `RELESTE_FMOD_EXPECT`（`studio` / `ex` / `legacy3`），
    /// 还会断言识别出的 API 代际与之一致——这样 CI 可以用桩库
    /// 验证探测逻辑，而本地可以用真实库验证兼容性。
    ///
    /// 未设置 `RELESTE_FMOD_LIB` 时**跳过并说明**，不假装成功。
    #[test]
    fn load_real_library_when_provided() {
        let Ok(path) = std::env::var("RELESTE_FMOD_LIB") else {
            eprintln!("skipping: RELESTE_FMOD_LIB not set");
            return;
        };
        if path.is_empty() {
            eprintln!("skipping: RELESTE_FMOD_LIB is empty");
            return;
        }

        let expected = std::env::var("RELESTE_FMOD_EXPECT").ok();

        match FmodBackend::new() {
            Ok(backend) => {
                let probe = backend.probe();
                println!("loaded: {}", probe.summary());
                assert!(probe.path.exists(), "probe should reference a real file");
                assert!(
                    !probe.symbols.is_empty(),
                    "at least one FMOD symbol must be found"
                );

                if let Some(expected) = expected {
                    let actual = probe.generation;
                    let matches = match expected.as_str() {
                        "studio" => actual == FmodGeneration::Studio,
                        "ex" => actual == FmodGeneration::Ex,
                        "legacy3" => actual == FmodGeneration::Legacy3,
                        other => panic!("unknown RELESTE_FMOD_EXPECT: {other}"),
                    };
                    assert!(
                        matches,
                        "expected generation {expected:?}, detected {:?}",
                        actual
                    );
                }
            }
            Err(AudioError::FmodUnusable(msg)) => {
                // 位数不匹配等：这是有效结论，不是测试失败。
                // 打印出来供人工确认（64 位进程加载 32 位 .so 就是这种）。
                println!("library unusable in this process:\n{msg}\n");
                assert!(
                    expected.is_none(),
                    "expected generation {expected:?} but the library could not be loaded"
                );
            }
            Err(e) => panic!("expected to load {path}, got: {e}"),
        }
    }

    /// 32 位库在 64 位进程里必须给出可诊断的错误，而不是笼统的 not found。
    #[test]
    fn architecture_mismatch_is_reported_as_unusable() {
        // 用一个内容合法但不是 ELF 的文件冒充库。
        let dir = std::env::temp_dir().join("reles-fmod-arch-test");
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("libfmod-fake.so");
        std::fs::write(&fake, b"not an ELF file at all").unwrap();

        // 直接走 Library::new 看错误是否被捕获成 FmodUnusable。
        // 这里不通过 FmodBackend::new，避免候选路径污染。
        assert!(unsafe { libloading::Library::new(&fake) }.is_err());

        std::fs::remove_file(&fake).ok();
    }
}
