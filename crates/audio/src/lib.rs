//! `reles-audio`: FMOD 封装与音频混音。
//!
//! # FMOD 动态库由用户提供（AGENTS.md §4.7）
//! 不 vendor FMOD 二进制。`fmod` feature 关闭时，混音层仍可用
//! （headless / 测试 / 无音频设备环境），并给出清晰错误。
//!
//! # 分层
//! - [`command`]：音频请求（播放 / 停止 / 音量）。
//! - [`mixer`]：总线音量、同帧去重、优先级抢占（纯逻辑，有测试）。
//! - `fmod_backend`：真实 FMOD 调用（`fmod` feature，默认关闭）。
//! - [`AudioManager`]：Service 包装。

#![deny(warnings)]

pub mod command;
pub mod mixer;

#[cfg(feature = "fmod")]
pub mod fmod_backend;

#[cfg(feature = "fmod")]
pub mod fmod_studio;

#[cfg(feature = "fmod")]
pub use fmod_backend::{FmodBackend, FmodGeneration, FmodProbe};
#[cfg(feature = "fmod")]
pub use fmod_studio::FmodStudio;

pub use command::{AudioRequest, Bus, SoundHandle, SoundPriority};
pub use mixer::{AudioBackend, Mixer, MixerStats, NullBackend};

/// Audio 的 ServiceId。
pub const SERVICE_ID: &str = "audio";

/// FMOD 库缺失时的说明（AGENTS.md §4.7 指定的格式）。
pub const FMOD_NOT_FOUND_MESSAGE: &str = "\
error: FMOD library not found.
Download from https://www.fmod.com/download
Place libfmod.so / fmod.dll / libfmod.dylib in:
  - Linux: /usr/local/lib/ or $LD_LIBRARY_PATH
  - Windows: alongside the .exe
  - macOS: /usr/local/lib/ or DYLD_LIBRARY_PATH";

/// 音频错误。
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("{0}")]
    FmodNotFound(String),
    /// 找到了库但无法使用（位数不匹配、无可识别符号等）。
    #[error("{0}")]
    FmodUnusable(String),
    #[error("failed to initialise FMOD: {0}")]
    Init(String),
    #[error("failed to load bank {path}: {message}")]
    BankLoad { path: String, message: String },
    #[error("failed to play event {event}: {message}")]
    Play { event: String, message: String },
    #[error("audio backend is not available (built without the `fmod` feature)")]
    BackendUnavailable,
}

/// 音频服务。
///
/// 所有游戏侧的音频请求都经过 [`AudioManager::request`]，
/// 由 [`Mixer`] 做去重 / 优先级处理后再交给后端。
pub struct AudioManager {
    mixer: Mixer,
}

impl AudioManager {
    /// 使用指定后端创建。
    pub fn new(backend: Box<dyn AudioBackend>) -> Self {
        AudioManager {
            mixer: Mixer::new(backend),
        }
    }

    /// 无后端（静音）创建。
    ///
    /// 用于 headless 测试与缺少 FMOD 的环境。
    pub fn silent() -> Self {
        Self::new(Box::new(NullBackend::new()))
    }

    /// 提交一条音频请求。
    pub fn request(&mut self, req: AudioRequest) -> Result<SoundHandle, AudioError> {
        self.mixer.submit(req)
    }

    /// 每帧推进（处理延迟释放、去重窗口重置）。
    pub fn tick(&mut self) {
        self.mixer.tick();
    }

    /// 混音统计。
    pub fn stats(&self) -> &MixerStats {
        self.mixer.stats()
    }

    /// 后端（可变）。
    pub fn backend_mut(&mut self) -> &mut dyn AudioBackend {
        self.mixer.backend_mut()
    }
}

impl Default for AudioManager {
    fn default() -> Self {
        Self::silent()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmod_message_mentions_all_platforms() {
        for needle in [
            "libfmod.so",
            "fmod.dll",
            "libfmod.dylib",
            "LD_LIBRARY_PATH",
            "DYLD_LIBRARY_PATH",
        ] {
            assert!(
                FMOD_NOT_FOUND_MESSAGE.contains(needle),
                "message should mention {needle}"
            );
        }
    }

    #[test]
    fn silent_manager_works() {
        let mut mgr = AudioManager::silent();
        mgr.request(AudioRequest::play_sfx(
            "event:/sfx/jump",
            1.0,
            SoundPriority::Normal,
        ))
        .unwrap();
        mgr.tick();
        assert_eq!(mgr.stats().played, 1);
    }
}
