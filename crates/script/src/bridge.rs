//! [`CutsceneBridge`]: a one-way command queue between Lua and the simulation.

use std::collections::VecDeque;

use tracing::trace;

use crate::command::{ActiveCommand, SimCommand};

/// Cutscene bridge.
///
/// - The Lua side calls [`CutsceneBridge::enqueue`] to queue a command.
/// - The simulation side calls [`CutsceneBridge::drain_new`] every frame to
///   take new commands, and [`CutsceneBridge::step_active`] to advance the
///   commands currently executing.
///
/// This structure does not depend on mlua and can be tested and used on its
/// own without a Lua environment.
#[derive(Debug, Default)]
pub struct CutsceneBridge {
    /// Commands waiting for the simulation to consume.
    pending: VecDeque<SimCommand>,
    /// Commands currently executing.
    active: Vec<ActiveCommand>,
}

impl CutsceneBridge {
    /// Create an empty bridge.
    pub fn new() -> Self {
        Self::default()
    }

    /// Lua side: queue a command.
    ///
    /// Never takes effect immediately -- the simulation consumes it on the
    /// next frame.
    pub fn enqueue(&mut self, command: SimCommand) {
        trace!(?command, "cutscene command queued");
        self.pending.push_back(command);
    }

    /// Simulation side: take all new commands (drains the queue).
    pub fn drain_new(&mut self) -> Vec<SimCommand> {
        self.pending.drain(..).collect()
    }

    /// Number of pending commands.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Whether there are still pending or executing commands.
    pub fn is_busy(&self) -> bool {
        !self.pending.is_empty() || !self.active.is_empty()
    }

    /// Start a command (enters active).
    pub fn start(&mut self, command: SimCommand) {
        self.active.push(ActiveCommand::new(command));
    }

    /// Simulation side: advance all active commands by one frame.
    ///
    /// Returns the commands completed this frame.
    pub fn step_active(&mut self) -> Vec<SimCommand> {
        let mut finished = Vec::new();
        for cmd in &mut self.active {
            if cmd.step() {
                finished.push(cmd.command.clone());
            }
        }
        self.active.retain(|c| !c.is_done());
        finished
    }

    /// The current list of active commands.
    pub fn active(&self) -> &[ActiveCommand] {
        &self.active
    }

    /// Immediately clear all commands (such as a room transition or leaving a
    /// cutscene).
    pub fn clear(&mut self) {
        self.pending.clear();
        self.active.clear();
    }

    /// Whether all active commands are instantaneous (`total == 0`).
    pub fn all_instant(&self) -> bool {
        self.active.iter().all(|c| c.total == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{DurationFrames, PlayerState};
    use releste_math::Vec2;
    use releste_world::EntityId;

    #[test]
    fn enqueue_then_drain() {
        let mut bridge = CutsceneBridge::new();
        bridge.enqueue(SimCommand::Wait { frames: 30 });
        assert_eq!(bridge.pending_len(), 1);
        let drained = bridge.drain_new();
        assert_eq!(drained.len(), 1);
        assert_eq!(bridge.pending_len(), 0);
    }

    #[test]
    fn command_is_never_applied_immediately() {
        let mut bridge = CutsceneBridge::new();
        bridge.enqueue(SimCommand::SetPlayerState(PlayerState::Frozen));
        // Queuing is not the same as executing.
        assert!(bridge.active().is_empty());
        assert!(bridge.is_busy());
    }

    #[test]
    fn step_until_done() {
        let mut bridge = CutsceneBridge::new();
        bridge.start(SimCommand::DummyWalkTo {
            actor: EntityId::new(1),
            target: Vec2::ZERO,
            duration: DurationFrames(3),
        });
        assert!(bridge.step_active().is_empty());
        assert!(bridge.step_active().is_empty());
        let done = bridge.step_active();
        assert_eq!(done.len(), 1);
        assert!(!bridge.is_busy());
    }

    #[test]
    fn clear_resets_everything() {
        let mut bridge = CutsceneBridge::new();
        bridge.enqueue(SimCommand::Wait { frames: 1 });
        bridge.start(SimCommand::Wait { frames: 100 });
        bridge.clear();
        assert!(!bridge.is_busy());
    }
}
