//! [`CutsceneBridge`]：Lua 与仿真之间的单向命令队列。

use std::collections::VecDeque;

use tracing::trace;

use crate::command::{ActiveCommand, SimCommand};

/// 过场桥。
///
/// - Lua 侧调用 [`CutsceneBridge::enqueue`] 排队命令。
/// - 仿真侧每帧调用 [`CutsceneBridge::drain_new`] 取出新命令，
///   调用 [`CutsceneBridge::step_active`] 推进正在执行的命令。
///
/// 该结构不依赖 mlua，可在无 Lua 环境下单独测试与使用。
#[derive(Debug, Default)]
pub struct CutsceneBridge {
    /// 待仿真消费的命令。
    pending: VecDeque<SimCommand>,
    /// 正在执行的命令。
    active: Vec<ActiveCommand>,
}

impl CutsceneBridge {
    /// 创建空桥。
    pub fn new() -> Self {
        Self::default()
    }

    /// Lua 侧：排队一条命令。
    ///
    /// 永不立即生效——仿真在下一帧消费。
    pub fn enqueue(&mut self, command: SimCommand) {
        trace!(?command, "cutscene command queued");
        self.pending.push_back(command);
    }

    /// 仿真侧：取出所有新命令（清空队列）。
    pub fn drain_new(&mut self) -> Vec<SimCommand> {
        self.pending.drain(..).collect()
    }

    /// 待处理命令数。
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// 是否还有待处理或执行中的命令。
    pub fn is_busy(&self) -> bool {
        !self.pending.is_empty() || !self.active.is_empty()
    }

    /// 启动一条命令（进入 active）。
    pub fn start(&mut self, command: SimCommand) {
        self.active.push(ActiveCommand::new(command));
    }

    /// 仿真侧：推进所有 active 命令一帧。
    ///
    /// 返回本帧完成的命令。
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

    /// 当前 active 命令列表。
    pub fn active(&self) -> &[ActiveCommand] {
        &self.active
    }

    /// 立即清空所有命令（如房间切换、退出过场）。
    pub fn clear(&mut self) {
        self.pending.clear();
        self.active.clear();
    }

    /// 所有 active 命令是否都是瞬时的（`total == 0`）。
    pub fn all_instant(&self) -> bool {
        self.active.iter().all(|c| c.total == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{DurationFrames, PlayerState};
    use reles_math::Vec2;
    use reles_world::EntityId;

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
        // 排队不等于执行
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
