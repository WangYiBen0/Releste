//! Audio requests.

use serde::{Deserialize, Serialize};

/// Audio bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bus {
    /// Master volume.
    Master,
    /// Music.
    Music,
    /// Sound effects.
    Sfx,
    /// UI.
    Ui,
}

impl Bus {
    /// All buses.
    pub const ALL: [Bus; 4] = [Bus::Master, Bus::Music, Bus::Sfx, Bus::Ui];

    /// Array index.
    pub const fn index(self) -> usize {
        match self {
            Bus::Master => 0,
            Bus::Music => 1,
            Bus::Sfx => 2,
            Bus::Ui => 3,
        }
    }
}

/// Sound effect priority.
///
/// A high priority can preempt a lower one when channels run short (the Celeste
/// style of "important sound effects must not be drowned out").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundPriority {
    /// Can be preempted by anything.
    Low = 0,
    /// Default.
    Normal = 1,
    /// Important (such as death or dash).
    High = 2,
    /// Cannot be preempted.
    Critical = 3,
}

/// Playback handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SoundHandle(pub u64);

/// Audio request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AudioRequest {
    /// Plays a one-shot sound effect.
    PlaySfx {
        /// FMOD event path (such as `event:/sfx/jump`).
        event: String,
        /// Linear volume `0.0..=1.0`.
        volume: f32,
        /// Priority.
        priority: SoundPriority,
        /// Pitch (1.0 = original speed).
        pitch: f32,
    },
    /// Plays music (replacing the current track).
    PlayMusic {
        event: String,
        volume: f32,
        /// Crossfade frame count.
        fade_frames: u32,
    },
    /// Stops the music.
    StopMusic { fade_frames: u32 },
    /// Plays ambience.
    PlayAmbience { event: String, volume: f32 },
    /// Stops the ambience.
    StopAmbience { fade_frames: u32 },
    /// Sets a bus volume.
    SetBusVolume { bus: Bus, volume: f32 },
    /// Stops all sounds.
    StopAll,
}

impl AudioRequest {
    /// Convenience constructor: a default-priority sound effect.
    pub fn play_sfx(event: impl Into<String>, volume: f32, priority: SoundPriority) -> Self {
        AudioRequest::PlaySfx {
            event: event.into(),
            volume,
            priority,
            pitch: 1.0,
        }
    }

    /// Convenience constructor: music.
    pub fn play_music(event: impl Into<String>, volume: f32) -> Self {
        AudioRequest::PlayMusic {
            event: event.into(),
            volume,
            fade_frames: 0,
        }
    }

    /// Whether this request takes part in same-frame deduplication.
    ///
    /// Music / volume / stop requests are not deduplicated (they are idempotent or stateful in themselves).
    pub fn participates_in_dedup(&self) -> bool {
        matches!(
            self,
            AudioRequest::PlaySfx { .. } | AudioRequest::PlayAmbience { .. }
        )
    }

    /// Dedup key (meaningful only for requests that take part in deduplication).
    pub fn dedup_key(&self) -> Option<&str> {
        match self {
            AudioRequest::PlaySfx { event, .. } | AudioRequest::PlayAmbience { event, .. } => {
                Some(event)
            }
            _ => None,
        }
    }

    /// Priority (defaults to `Normal` for non-sound-effect requests).
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
