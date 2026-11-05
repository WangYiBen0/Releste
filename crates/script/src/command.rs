//! Simulation commands: the only channel through which Lua cutscenes drive
//! the sim via the queue.
//!
//! **Lua never directly modifies physics state** (AGENTS.md 4.5).

use releste_math::Vec2;
use releste_world::EntityId;
use serde::{Deserialize, Serialize};

/// A command queued by a Lua script and consumed by the simulation on the next
/// frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SimCommand {
    /// Make an actor walk (or teleport) to a target point.
    DummyWalkTo {
        actor: EntityId,
        target: Vec2,
        duration: DurationFrames,
    },
    /// Set the player state.
    SetPlayerState(PlayerState),
    /// Zoom / pan the camera to a target.
    CameraZoom {
        target: Vec2,
        zoom: f32,
        duration: DurationFrames,
    },
    /// Spawn a cutscene actor.
    SpawnActor { kind: ActorKind, pos: Vec2 },
    /// Wait for a number of frames.
    Wait { frames: u32 },
    /// Play a sound effect / music.
    PlayAudio { event: String, volume: f32 },
    /// End the cutscene.
    EndCutscene,
}

/// Duration (in frames; fixed-point time aligned to 60Hz for determinism).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurationFrames(pub u32);

impl DurationFrames {
    /// Convert from seconds to frames (rounding up).
    pub fn from_secs(secs: f32) -> Self {
        DurationFrames((secs * 60.0).ceil().max(0.0) as u32)
    }

    /// Convert to seconds.
    pub fn as_secs(self) -> f32 {
        self.0 as f32 / 60.0
    }
}

/// Player state (the restricted subset used by cutscenes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerState {
    /// Normal control.
    Normal,
    /// Frozen (no control, physics retained).
    Frozen,
    /// Invincible (not killed by spikes during a cutscene).
    Invincible,
    /// No gravity (floating).
    NoGravity,
}

/// Cutscene actor type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// Dummy player (copies the appearance, no physics).
    Dummy,
    /// NPCs such as the old woman (reference resources by name).
    Npc(String),
    /// Effect actor.
    Effect(String),
}

/// A command currently executing (with progress).
#[derive(Debug, Clone)]
pub struct ActiveCommand {
    pub command: SimCommand,
    /// Remaining frames.
    pub remaining: u32,
    /// Total frames (used to interpolate progress).
    pub total: u32,
}

impl ActiveCommand {
    /// Construct from a command.
    pub fn new(command: SimCommand) -> Self {
        let total = match &command {
            SimCommand::DummyWalkTo { duration, .. } => duration.0,
            SimCommand::CameraZoom { duration, .. } => duration.0,
            SimCommand::Wait { frames } => *frames,
            _ => 0,
        };
        ActiveCommand {
            command,
            remaining: total,
            total,
        }
    }

    /// Progress `0.0 ..= 1.0`.
    pub fn progress(&self) -> f32 {
        if self.total == 0 {
            1.0
        } else {
            1.0 - (self.remaining as f32 / self.total as f32)
        }
    }

    /// Whether it is done.
    pub fn is_done(&self) -> bool {
        self.remaining == 0
    }

    /// Advance one frame; returns whether it is done.
    pub fn step(&mut self) -> bool {
        if self.remaining > 0 {
            self.remaining -= 1;
        }
        self.is_done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_conversion() {
        assert_eq!(DurationFrames::from_secs(1.0).0, 60);
        assert_eq!(DurationFrames::from_secs(0.5).0, 30);
        assert!((DurationFrames(30).as_secs() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn active_command_progress() {
        let mut cmd = ActiveCommand::new(SimCommand::Wait { frames: 10 });
        assert_eq!(cmd.progress(), 0.0);
        assert!(!cmd.step());
        assert_eq!(cmd.remaining, 9);
        for _ in 0..9 {
            cmd.step();
        }
        assert!(cmd.is_done());
        assert_eq!(cmd.progress(), 1.0);
    }

    #[test]
    fn zero_duration_is_immediately_done() {
        let cmd = ActiveCommand::new(SimCommand::SetPlayerState(PlayerState::Normal));
        assert!(cmd.is_done());
        assert_eq!(cmd.progress(), 1.0);
    }
}
