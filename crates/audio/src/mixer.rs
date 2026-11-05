//! Mixing layer: bus volumes, same-frame deduplication, priority preemption.
//!
//! This layer is **pure logic** and does not depend on FMOD, so it can be tested in any environment.
//! Real playback is the responsibility of the [`AudioBackend`] implementation.

use std::collections::{HashMap, HashSet};

use crate::command::{AudioRequest, Bus, SoundHandle, SoundPriority};
use crate::AudioError;

/// Default cap on simultaneously playing sound effects (beyond it, preemption by priority kicks in).
pub const DEFAULT_SFX_VOICES: usize = 32;

/// Audio backend.
///
/// Isolates FMOD behind a trait so the mixing logic can be tested independently.
pub trait AudioBackend: Send {
    /// Plays an event and returns a handle.
    fn play(&mut self, event: &str, volume: f32, pitch: f32) -> Result<u64, AudioError>;

    /// Stops one handle.
    fn stop(&mut self, handle: u64);

    /// Stops all sounds.
    fn stop_all(&mut self);

    /// Sets a bus volume.
    fn set_bus_volume(&mut self, bus: Bus, volume: f32);

    /// Backend name.
    fn name(&self) -> &'static str;
}

/// Observable state of the silent backend.
#[derive(Debug, Default)]
pub struct NullState {
    /// Play requests received (for diagnostics / test assertions).
    pub played: Vec<String>,
    /// Number of times `stop_all` was received.
    pub stops: usize,
    /// Record of bus volume settings.
    pub volumes: HashMap<Bus, f32>,
    /// Next handle.
    next: u64,
}

/// Silent backend: records calls but makes no sound.
///
/// For headless tests and environments without FMOD. State is shared through
/// [`NullBackend::state`] to make assertions easy.
#[derive(Debug, Clone, Default)]
pub struct NullBackend {
    state: std::sync::Arc<std::sync::Mutex<NullState>>,
}

impl NullBackend {
    /// Creates a new one.
    pub fn new() -> Self {
        Self::default()
    }

    /// Shared state handle (tests can keep observing after the `Mixer` takes ownership of the backend).
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

/// A sound effect that is currently playing.
#[derive(Debug, Clone, Copy)]
struct Voice {
    handle: u64,
    priority: SoundPriority,
    /// Frame number when it was submitted (low priorities can be preempted).
    started_frame: u64,
}

/// Mixing statistics.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MixerStats {
    /// Number of times playback actually happened.
    pub played: usize,
    /// Number of times it was blocked by same-frame deduplication.
    pub deduped: usize,
    /// Number of times a voice was preempted by priority.
    pub preempted: usize,
    /// Number of times a request was dropped because channels ran out.
    pub dropped: usize,
    /// Number of currently active sound effects.
    pub active: usize,
}

/// Mixer.
pub struct Mixer {
    backend: Box<dyn AudioBackend>,
    /// Per-bus volumes (already applied).
    volumes: [f32; 4],
    /// Current frame number.
    frame: u64,
    /// Events already played this frame (same-frame deduplication).
    played_this_frame: HashSet<String>,
    /// Active sound effects.
    voices: Vec<Voice>,
    /// Sound effect channel cap.
    max_voices: usize,
    /// Music handle to stop.
    music: Option<u64>,
    /// Ambience handle.
    ambience: Option<u64>,
    stats: MixerStats,
}

impl Mixer {
    /// Creates with the given backend.
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
        // Push the initial volumes to the backend.
        for bus in Bus::ALL {
            mixer.backend.set_bus_volume(bus, 1.0);
        }
        mixer
    }

    /// Sets the sound effect channel cap.
    pub fn with_max_voices(mut self, max: usize) -> Self {
        self.max_voices = max.max(1);
        self
    }

    /// The backend (mutable).
    pub fn backend_mut(&mut self) -> &mut dyn AudioBackend {
        self.backend.as_mut()
    }

    /// Backend name.
    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    /// Statistics.
    pub fn stats(&self) -> &MixerStats {
        &self.stats
    }

    /// Current volume of a bus.
    pub fn bus_volume(&self, bus: Bus) -> f32 {
        self.volumes[bus.index()]
    }

    /// Submits one request.
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

    /// Plays a sound effect (including deduplication and preemption).
    fn play_sfx(
        &mut self,
        event: &str,
        volume: f32,
        priority: SoundPriority,
        pitch: f32,
    ) -> Result<SoundHandle, AudioError> {
        // Same-frame deduplication: one event only sounds once per frame (Celeste behavior).
        if !self.played_this_frame.insert(event.to_string()) {
            self.stats.deduped += 1;
            return Ok(SoundHandle(0));
        }

        // Not enough channels -> try to preempt the lowest priority.
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

    /// Applies the bus volumes (master volume x per-bus volume).
    fn scaled(&self, bus: Bus, volume: f32) -> f32 {
        (volume.clamp(0.0, 1.0) * self.volumes[Bus::Master.index()] * self.volumes[bus.index()])
            .clamp(0.0, 1.0)
    }

    /// Advances one frame: resets the dedup window and cleans up finished voices.
    pub fn tick(&mut self) {
        self.frame += 1;
        self.played_this_frame.clear();

        // Simplification: assume a sound effect lives for only one frame (a real backend
        // would report when playback finishes). Keep voices from the last 8 frames to
        // simulate channel occupancy.
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

    /// Test fixture: a Mixer plus observable backend state.
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
        // Channels are full, so the high priority should preempt
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
