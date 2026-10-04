//! 仿真命令：Lua 过场通过队列驱动 sim 的唯一通道。
//!
//! **Lua 永远不直接改物理状态**（AGENTS.md §4.5）。

use reles_math::Vec2;
use reles_world::EntityId;
use serde::{Deserialize, Serialize};

/// 由 Lua 脚本排队、由仿真在下一帧消费的命令。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SimCommand {
    /// 让一个 actor 走（或瞬移）到目标点。
    DummyWalkTo {
        actor: EntityId,
        target: Vec2,
        duration: DurationFrames,
    },
    /// 设置玩家状态。
    SetPlayerState(PlayerState),
    /// 摄像机缩放 / 平移到目标。
    CameraZoom {
        target: Vec2,
        zoom: f32,
        duration: DurationFrames,
    },
    /// 生成一个过场 actor。
    SpawnActor { kind: ActorKind, pos: Vec2 },
    /// 等待若干帧。
    Wait { frames: u32 },
    /// 播放音效 / 音乐。
    PlayAudio { event: String, volume: f32 },
    /// 结束过场。
    EndCutscene,
}

/// 时长（以帧为单位，定点数时间与 60Hz 对齐，保证确定性）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurationFrames(pub u32);

impl DurationFrames {
    /// 从秒转换为帧（向上取整）。
    pub fn from_secs(secs: f32) -> Self {
        DurationFrames((secs * 60.0).ceil().max(0.0) as u32)
    }

    /// 转为秒。
    pub fn as_secs(self) -> f32 {
        self.0 as f32 / 60.0
    }
}

/// 玩家状态（过场用的受限子集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayerState {
    /// 正常控制。
    Normal,
    /// 冻结（不能操作，保留物理）。
    Frozen,
    /// 无敌（过场不被刺死）。
    Invincible,
    /// 无重力（漂浮）。
    NoGravity,
}

/// 过场 actor 类型。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// 假玩家（复制外观，无物理）。
    Dummy,
    /// 老妇人等 NPC（按名称引用资源）。
    Npc(String),
    /// 特效 actor。
    Effect(String),
}

/// 正在执行的命令（带进度）。
#[derive(Debug, Clone)]
pub struct ActiveCommand {
    pub command: SimCommand,
    /// 剩余帧数。
    pub remaining: u32,
    /// 总帧数（用于插值进度）。
    pub total: u32,
}

impl ActiveCommand {
    /// 由命令构造。
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

    /// 进度 `0.0 ..= 1.0`。
    pub fn progress(&self) -> f32 {
        if self.total == 0 {
            1.0
        } else {
            1.0 - (self.remaining as f32 / self.total as f32)
        }
    }

    /// 是否完成。
    pub fn is_done(&self) -> bool {
        self.remaining == 0
    }

    /// 推进一帧，返回是否完成。
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
