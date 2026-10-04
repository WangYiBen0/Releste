//! 音频请求。

use serde::{Deserialize, Serialize};

/// 音频总线。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bus {
    /// 总音量。
    Master,
    /// 音乐。
    Music,
    /// 音效。
    Sfx,
    /// UI。
    Ui,
}

impl Bus {
    /// 所有总线。
    pub const ALL: [Bus; 4] = [Bus::Master, Bus::Music, Bus::Sfx, Bus::Ui];

    /// 数组下标。
    pub const fn index(self) -> usize {
        match self {
            Bus::Master => 0,
            Bus::Music => 1,
            Bus::Sfx => 2,
            Bus::Ui => 3,
        }
    }
}

/// 音效优先级。
///
/// 高优先级可在通道不足时抢占低优先级（Celeste 风格的
/// "重要音效不被淹没"）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundPriority {
    /// 可被任意抢占。
    Low = 0,
    /// 默认。
    Normal = 1,
    /// 重要（如死亡、冲刺）。
    High = 2,
    /// 不允许被抢占。
    Critical = 3,
}

/// 播放句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SoundHandle(pub u64);

/// 音频请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AudioRequest {
    /// 播放一次性音效。
    PlaySfx {
        /// FMOD 事件路径（如 `event:/sfx/jump`）。
        event: String,
        /// 线性音量 `0.0..=1.0`。
        volume: f32,
        /// 优先级。
        priority: SoundPriority,
        /// 音高（1.0 = 原速）。
        pitch: f32,
    },
    /// 播放音乐（替换当前曲目）。
    PlayMusic {
        event: String,
        volume: f32,
        /// 交叉淡入帧数。
        fade_frames: u32,
    },
    /// 停止音乐。
    StopMusic { fade_frames: u32 },
    /// 播放环境音。
    PlayAmbience { event: String, volume: f32 },
    /// 停止环境音。
    StopAmbience { fade_frames: u32 },
    /// 设置总线音量。
    SetBusVolume { bus: Bus, volume: f32 },
    /// 停止所有声音。
    StopAll,
}

impl AudioRequest {
    /// 便捷构造：默认优先级的音效。
    pub fn play_sfx(event: impl Into<String>, volume: f32, priority: SoundPriority) -> Self {
        AudioRequest::PlaySfx {
            event: event.into(),
            volume,
            priority,
            pitch: 1.0,
        }
    }

    /// 便捷构造：音乐。
    pub fn play_music(event: impl Into<String>, volume: f32) -> Self {
        AudioRequest::PlayMusic {
            event: event.into(),
            volume,
            fade_frames: 0,
        }
    }

    /// 该请求是否参与同帧去重。
    ///
    /// 音乐 / 音量 / 停止类请求不去重（它们本身是幂等的或有状态）。
    pub fn participates_in_dedup(&self) -> bool {
        matches!(
            self,
            AudioRequest::PlaySfx { .. } | AudioRequest::PlayAmbience { .. }
        )
    }

    /// 去重键（仅对参与去重的请求有意义）。
    pub fn dedup_key(&self) -> Option<&str> {
        match self {
            AudioRequest::PlaySfx { event, .. } | AudioRequest::PlayAmbience { event, .. } => {
                Some(event)
            }
            _ => None,
        }
    }

    /// 优先级（非音效默认 `Normal`）。
    pub fn priority(&self) -> SoundPriority {
        match self {
            AudioRequest::PlaySfx { priority, .. } => *priority,
            _ => SoundPriority::Normal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_indices_are_unique() {
        let mut seen = [false; 4];
        for bus in Bus::ALL {
            assert!(!seen[bus.index()], "duplicate index for {bus:?}");
            seen[bus.index()] = true;
        }
    }

    #[test]
    fn priority_ordering() {
        assert!(SoundPriority::Critical > SoundPriority::High);
        assert!(SoundPriority::High > SoundPriority::Normal);
        assert!(SoundPriority::Normal > SoundPriority::Low);
    }

    #[test]
    fn sfx_participates_in_dedup_music_does_not() {
        assert!(AudioRequest::play_sfx("e", 1.0, SoundPriority::Normal).participates_in_dedup());
        assert!(!AudioRequest::play_music("e", 1.0).participates_in_dedup());
        assert!(!AudioRequest::StopAll.participates_in_dedup());
    }

    #[test]
    fn dedup_key_is_the_event() {
        let r = AudioRequest::play_sfx("event:/sfx/jump", 1.0, SoundPriority::Low);
        assert_eq!(r.dedup_key(), Some("event:/sfx/jump"));
        assert_eq!(AudioRequest::StopAll.dedup_key(), None);
    }
}
