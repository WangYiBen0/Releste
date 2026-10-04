//! 混音层：总线音量、同帧去重、优先级抢占。
//!
//! 这一层是**纯逻辑**，不依赖 FMOD，因此可以在任何环境测试。
//! 真实播放由 [`AudioBackend`] 实现负责。

use std::collections::{HashMap, HashSet};

use crate::command::{AudioRequest, Bus, SoundHandle, SoundPriority};
use crate::AudioError;

/// 默认同时播放的音效上限（超过则按优先级抢占）。
pub const DEFAULT_SFX_VOICES: usize = 32;

/// 音频后端。
///
/// 用 trait 隔离 FMOD，使混音逻辑可独立测试。
pub trait AudioBackend: Send {
    /// 播放一个事件，返回句柄。
    fn play(&mut self, event: &str, volume: f32, pitch: f32) -> Result<u64, AudioError>;

    /// 停止一个句柄。
    fn stop(&mut self, handle: u64);

    /// 停止所有声音。
    fn stop_all(&mut self);

    /// 设置总线音量。
    fn set_bus_volume(&mut self, bus: Bus, volume: f32);

    /// 后端名称。
    fn name(&self) -> &'static str;
}

/// 静音后端的可观测状态。
#[derive(Debug, Default)]
pub struct NullState {
    /// 收到的播放请求（诊断 / 测试断言用）。
    pub played: Vec<String>,
    /// 收到 `stop_all` 的次数。
    pub stops: usize,
    /// 总线音量设置记录。
    pub volumes: HashMap<Bus, f32>,
    /// 下一个句柄。
    next: u64,
}

/// 静音后端：记录调用但不出声。
///
/// 用于 headless 测试与缺少 FMOD 的环境。状态通过
/// [`NullBackend::state`] 共享，便于断言。
#[derive(Debug, Clone, Default)]
pub struct NullBackend {
    state: std::sync::Arc<std::sync::Mutex<NullState>>,
}

impl NullBackend {
    /// 新建。
    pub fn new() -> Self {
        Self::default()
    }

    /// 共享状态句柄（测试可在 `Mixer` 持有后端后继续观测）。
    pub fn state(&self) -> std::sync::Arc<std::sync::Mutex<NullState>> {
        std::sync::Arc::clone(&self.state)
    }
}

impl AudioBackend for NullBackend {
    fn play(&mut self, event: &str, _volume: f32, _pitch: f32) -> Result<u64, AudioError> {
        let mut s = self.state.lock().expect("null backend state poisoned");
        s.next += 1;
        s.played.push(event.to_string());
        Ok(s.next)
    }

    fn stop(&mut self, _handle: u64) {}

    fn stop_all(&mut self) {
        let mut s = self.state.lock().expect("null backend state poisoned");
        s.stops += 1;
        s.played.clear();
    }

    fn set_bus_volume(&mut self, bus: Bus, volume: f32) {
        let mut s = self.state.lock().expect("null backend state poisoned");
        s.volumes.insert(bus, volume);
    }

    fn name(&self) -> &'static str {
        "null"
    }
}

/// 正在播放的音效。
#[derive(Debug, Clone, Copy)]
struct Voice {
    handle: u64,
    priority: SoundPriority,
    /// 提交时的帧号（低优先级可被抢占）。
    started_frame: u64,
}

/// 混音统计。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MixerStats {
    /// 实际播放的次数。
    pub played: usize,
    /// 被同帧去重挡掉的次数。
    pub deduped: usize,
    /// 被优先级抢占的次数。
    pub preempted: usize,
    /// 因通道耗尽而丢弃的次数。
    pub dropped: usize,
    /// 当前活跃音效数。
    pub active: usize,
}

/// 混音器。
pub struct Mixer {
    backend: Box<dyn AudioBackend>,
    /// 各总线音量（已应用）。
    volumes: [f32; 4],
    /// 当前帧号。
    frame: u64,
    /// 本帧已播放的事件（同帧去重）。
    played_this_frame: HashSet<String>,
    /// 活跃音效。
    voices: Vec<Voice>,
    /// 音效通道上限。
    max_voices: usize,
    /// 待停止的音乐句柄。
    music: Option<u64>,
    /// 环境音句柄。
    ambience: Option<u64>,
    stats: MixerStats,
}

impl Mixer {
    /// 使用指定后端创建。
    pub fn new(backend: Box<dyn AudioBackend>) -> Self {
        let mut mixer = Mixer {
            backend,
            volumes: [1.0; 4],
            frame: 0,
            played_this_frame: HashSet::new(),
            voices: Vec::new(),
            max_voices: DEFAULT_SFX_VOICES,
            music: None,
            ambience: None,
            stats: MixerStats::default(),
        };
        // 初始音量推给后端。
        for bus in Bus::ALL {
            mixer.backend.set_bus_volume(bus, 1.0);
        }
        mixer
    }

    /// 设置音效通道上限。
    pub fn with_max_voices(mut self, max: usize) -> Self {
        self.max_voices = max.max(1);
        self
    }

    /// 后端（可变）。
    pub fn backend_mut(&mut self) -> &mut dyn AudioBackend {
        self.backend.as_mut()
    }

    /// 后端名。
    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// 统计。
    pub fn stats(&self) -> &MixerStats {
        &self.stats
    }

    /// 某总线的当前音量。
    pub fn bus_volume(&self, bus: Bus) -> f32 {
        self.volumes[bus.index()]
    }

    /// 提交一条请求。
    pub fn submit(&mut self, req: AudioRequest) -> Result<SoundHandle, AudioError> {
        match req {
            AudioRequest::PlaySfx {
                event,
                volume,
                priority,
                pitch,
            } => self.play_sfx(&event, volume, priority, pitch),

            AudioRequest::PlayMusic {
                event,
                volume,
                fade_frames: _,
            } => {
                if let Some(old) = self.music.take() {
                    self.backend.stop(old);
                }
                let v = self.scaled(Bus::Music, volume);
                let handle = self.backend.play(&event, v, 1.0)?;
                self.music = Some(handle);
                self.stats.played += 1;
                Ok(SoundHandle(handle))
            }

            AudioRequest::StopMusic { fade_frames: _ } => {
                if let Some(h) = self.music.take() {
                    self.backend.stop(h);
                }
                Ok(SoundHandle(0))
            }

            AudioRequest::PlayAmbience { event, volume } => {
                if let Some(old) = self.ambience.take() {
                    self.backend.stop(old);
                }
                let v = self.scaled(Bus::Sfx, volume);
                let handle = self.backend.play(&event, v, 1.0)?;
                self.ambience = Some(handle);
                self.stats.played += 1;
                Ok(SoundHandle(handle))
            }

            AudioRequest::StopAmbience { fade_frames: _ } => {
                if let Some(h) = self.ambience.take() {
                    self.backend.stop(h);
                }
                Ok(SoundHandle(0))
            }

            AudioRequest::SetBusVolume { bus, volume } => {
                let v = volume.clamp(0.0, 1.0);
                self.volumes[bus.index()] = v;
                self.backend.set_bus_volume(bus, v);
                Ok(SoundHandle(0))
            }

            AudioRequest::StopAll => {
                self.backend.stop_all();
                self.voices.clear();
                self.music = None;
                self.ambience = None;
                Ok(SoundHandle(0))
            }
        }
    }

    /// 播放音效（含去重与抢占）。
    fn play_sfx(
        &mut self,
        event: &str,
        volume: f32,
        priority: SoundPriority,
        pitch: f32,
    ) -> Result<SoundHandle, AudioError> {
        // 同帧去重：同一事件一帧内只响一次（Celeste 行为）。
        if !self.played_this_frame.insert(event.to_string()) {
            self.stats.deduped += 1;
            return Ok(SoundHandle(0));
        }

        // 通道不足 → 尝试抢占最低优先级。
        if self.voices.len() >= self.max_voices {
            let weakest = self
                .voices
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| (v.priority, v.started_frame))
                .map(|(i, v)| (i, v.priority, v.handle));

            match weakest {
                Some((idx, weakest_priority, handle)) if priority > weakest_priority => {
                    self.backend.stop(handle);
                    self.voices.swap_remove(idx);
                    self.stats.preempted += 1;
                }
                _ => {
                    self.stats.dropped += 1;
                    return Ok(SoundHandle(0));
                }
            }
        }

        let v = self.scaled(Bus::Sfx, volume);
        let handle = self.backend.play(event, v, pitch)?;
        self.voices.push(Voice {
            handle,
            priority,
            started_frame: self.frame,
        });
        self.stats.played += 1;
        Ok(SoundHandle(handle))
    }

    /// 应用总线音量（主音量 × 分总线）。
    fn scaled(&self, bus: Bus, volume: f32) -> f32 {
        (volume.clamp(0.0, 1.0) * self.volumes[Bus::Master.index()] * self.volumes[bus.index()])
            .clamp(0.0, 1.0)
    }

    /// 每帧推进：重置去重窗口，清理已播完的语音。
    pub fn tick(&mut self) {
        self.frame += 1;
        self.played_this_frame.clear();

        // 简化：假设音效只在一帧内有效（真实后端会回报播放完毕）。
        // 保留最近 8 帧的语音以模拟通道占用。
        const VOICE_LIFETIME_FRAMES: u64 = 8;
        let now = self.frame;
        self.voices
            .retain(|v| now.saturating_sub(v.started_frame) < VOICE_LIFETIME_FRAMES);

        self.stats.active = self.voices.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// 测试夹具：Mixer + 可观测的后端状态。
    struct Fixture {
        mixer: Mixer,
        state: Arc<Mutex<NullState>>,
    }

    impl Fixture {
        fn new(max_voices: usize) -> Self {
            let backend = NullBackend::new();
            let state = backend.state();
            Fixture {
                mixer: Mixer::new(Box::new(backend)).with_max_voices(max_voices),
                state,
            }
        }

        fn played(&self) -> Vec<String> {
            self.state.lock().unwrap().played.clone()
        }

        fn stops(&self) -> usize {
            self.state.lock().unwrap().stops
        }

        fn volumes(&self) -> HashMap<Bus, f32> {
            self.state.lock().unwrap().volumes.clone()
        }
    }

    fn fixture() -> Fixture {
        Fixture::new(DEFAULT_SFX_VOICES)
    }

    #[test]
    fn plays_sfx() {
        let mut f = fixture();
        f.mixer
            .submit(AudioRequest::play_sfx(
                "event:/sfx/jump",
                1.0,
                SoundPriority::Normal,
            ))
            .unwrap();
        assert_eq!(f.mixer.stats().played, 1);
        assert_eq!(f.played(), vec!["event:/sfx/jump"]);
    }

    #[test]
    fn dedups_same_event_within_a_frame() {
        let mut f = fixture();
        let req = AudioRequest::play_sfx("event:/sfx/step", 1.0, SoundPriority::Low);
        f.mixer.submit(req.clone()).unwrap();
        f.mixer.submit(req.clone()).unwrap();
        f.mixer.submit(req).unwrap();
        assert_eq!(f.mixer.stats().played, 1);
        assert_eq!(f.mixer.stats().deduped, 2);
        assert_eq!(f.played().len(), 1);
    }

    #[test]
    fn dedup_window_resets_each_frame() {
        let mut f = fixture();
        let req = AudioRequest::play_sfx("event:/sfx/step", 1.0, SoundPriority::Low);
        f.mixer.submit(req.clone()).unwrap();
        f.mixer.tick();
        f.mixer.submit(req).unwrap();
        assert_eq!(f.mixer.stats().played, 2, "new frame should allow replay");
    }

    #[test]
    fn different_events_are_not_deduped() {
        let mut f = fixture();
        f.mixer
            .submit(AudioRequest::play_sfx("a", 1.0, SoundPriority::Normal))
            .unwrap();
        f.mixer
            .submit(AudioRequest::play_sfx("b", 1.0, SoundPriority::Normal))
            .unwrap();
        assert_eq!(f.mixer.stats().played, 2);
    }

    #[test]
    fn priority_preempts_weakest_voice() {
        let mut f = Fixture::new(2);
        f.mixer
            .submit(AudioRequest::play_sfx("low1", 1.0, SoundPriority::Low))
            .unwrap();
        f.mixer
            .submit(AudioRequest::play_sfx("low2", 1.0, SoundPriority::Low))
            .unwrap();
        // 通道已满，高优先级应该抢占
        f.mixer
            .submit(AudioRequest::play_sfx("crit", 1.0, SoundPriority::Critical))
            .unwrap();
        assert_eq!(f.mixer.stats().preempted, 1);
        assert!(f.played().contains(&"crit".to_string()));
    }

    #[test]
    fn low_priority_is_dropped_when_full() {
        let mut f = Fixture::new(1);
        f.mixer
            .submit(AudioRequest::play_sfx(
                "critical",
                1.0,
                SoundPriority::Critical,
            ))
            .unwrap();
        f.mixer
            .submit(AudioRequest::play_sfx("weak", 1.0, SoundPriority::Low))
            .unwrap();
        assert_eq!(f.mixer.stats().dropped, 1);
        assert!(!f.played().contains(&"weak".to_string()));
    }

    #[test]
    fn bus_volume_scales_output() {
        let mut f = fixture();
        f.mixer
            .submit(AudioRequest::SetBusVolume {
                bus: Bus::Master,
                volume: 0.5,
            })
            .unwrap();
        assert_eq!(f.mixer.bus_volume(Bus::Master), 0.5);
        assert_eq!(f.volumes().get(&Bus::Master), Some(&0.5));
    }

    #[test]
    fn music_replaces_previous() {
        let mut f = fixture();
        f.mixer
            .submit(AudioRequest::play_music("track_a", 1.0))
            .unwrap();
        f.mixer
            .submit(AudioRequest::play_music("track_b", 1.0))
            .unwrap();
        assert_eq!(f.mixer.stats().played, 2);
        assert_eq!(f.played(), vec!["track_a", "track_b"]);
    }

    #[test]
    fn stop_all_clears_everything() {
        let mut f = fixture();
        f.mixer
            .submit(AudioRequest::play_sfx("a", 1.0, SoundPriority::Normal))
            .unwrap();
        f.mixer.submit(AudioRequest::play_music("m", 1.0)).unwrap();
        f.mixer.submit(AudioRequest::StopAll).unwrap();
        assert_eq!(f.stops(), 1);
        assert!(f.played().is_empty());
    }

    #[test]
    fn volume_is_clamped() {
        let mut f = fixture();
        f.mixer
            .submit(AudioRequest::SetBusVolume {
                bus: Bus::Sfx,
                volume: 5.0,
            })
            .unwrap();
        assert_eq!(f.mixer.bus_volume(Bus::Sfx), 1.0);
        f.mixer
            .submit(AudioRequest::SetBusVolume {
                bus: Bus::Sfx,
                volume: -3.0,
            })
            .unwrap();
        assert_eq!(f.mixer.bus_volume(Bus::Sfx), 0.0);
    }

    #[test]
    fn voices_are_released_after_lifetime() {
        let mut f = fixture();
        f.mixer
            .submit(AudioRequest::play_sfx("a", 1.0, SoundPriority::Normal))
            .unwrap();
        f.mixer.tick();
        assert_eq!(f.mixer.stats().active, 1);
        for _ in 0..10 {
            f.mixer.tick();
        }
        assert_eq!(f.mixer.stats().active, 0);
    }

    #[test]
    fn master_volume_multiplies_bus_volume() {
        let mut f = fixture();
        f.mixer
            .submit(AudioRequest::SetBusVolume {
                bus: Bus::Master,
                volume: 0.5,
            })
            .unwrap();
        f.mixer
            .submit(AudioRequest::SetBusVolume {
                bus: Bus::Sfx,
                volume: 0.5,
            })
            .unwrap();
        // 0.8 * 0.5 * 0.5 = 0.2
        assert!((f.mixer.scaled(Bus::Sfx, 0.8) - 0.2).abs() < 1e-6);
    }
}
