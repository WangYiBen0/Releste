//! Collaboration protocol definition.
//!
//! # Core principles (AGENTS.md §4.4 / §11)
//! - **The world is not synced by default**. Syncing is opt-in, declared by the map's `sync_rule`.
//! - Client-authoritative, pure-relay server: the server does not understand map content.
//! - The protocol carries only three kinds of information: player avatars,
//!   collaboration actions, and explicit sync events.

use serde::{Deserialize, Serialize};

/// Player ID (assigned by the server on connect).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct PlayerId(pub u32);

/// Player avatar state (high frequency, unreliable transport).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct AvatarState {
    /// Position.
    pub x: f32,
    pub y: f32,
    /// 速度（用于插值 / 特效）。
    pub vx: f32,
    pub vy: f32,
    /// 动画状态（由客户端决定，服务器不解释）。
    pub animation: u8,
    /// 朝向。
    pub facing: i8,
    /// 本帧输入的方向（用于远程渲染）。
    pub input_x: i8,
    pub input_y: i8,
}

impl AvatarState {
    /// 零状态（用于生成 / 重置）。
    pub const ZERO: AvatarState = AvatarState {
        x: 0.0,
        y: 0.0,
        vx: 0.0,
        vy: 0.0,
        animation: 0,
        facing: 1,
        input_x: 0,
        input_y: 0,
    };
}

/// 显式同步事件（低频，可靠传输）。
///
/// 只有地图通过 `[[sync_rule]]` 声明的实体才会产生此类事件。
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SyncEvent {
    /// 地图中声明的同步通道名（如 `"door_open"`）。
    pub channel: String,
    /// 负载（引擎不解释，由地图的 Lua / 规则消费）。
    pub payload: SyncPayload,
}

/// 同步负载。
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum SyncPayload {
    /// 布尔标记。
    Flag(bool),
    /// 整数。
    Int(i32),
    /// 定点位置（用于需要位置同步的 opt-in 实体）。
    Position { x: i32, y: i32 },
    /// 已拾取 / 已触发等一次性事件。
    Trigger,
}

/// 协作模式。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CarryMode {
    /// 抓住，跟随。
    Grab,
    /// 绳子，弹性。
    Tether,
    /// 踩在头上。
    Ride,
    /// 抛出。
    Launch,
}

/// 客户端 → 服务器消息。
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Up {
    /// 握手。
    Hello { protocol_version: u32, name: String },
    /// 化身状态（高频，不可靠）。
    Avatar(AvatarState),
    /// 同步事件（低频，可靠）。
    SyncEvent(SyncEvent),
    /// 请求携带某玩家。
    CarryRequest { target: PlayerId, mode: CarryMode },
    /// 接受携带。
    CarryAccept { initiator: PlayerId },
    /// 结束携带。
    CarryEnd { initiator: PlayerId },
    /// 主动断开。
    Bye,
}

/// 服务器 → 客户端消息。
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Down {
    /// 握手应答，分配 `PlayerId`。
    Welcome {
        player_id: PlayerId,
        protocol_version: u32,
    },
    /// 某玩家加入。
    PlayerJoined { id: PlayerId, name: String },
    /// 某玩家离开。
    PlayerLeft { id: PlayerId },
    /// 某玩家的化身状态。
    Avatar(PlayerId, AvatarState),
    /// 某玩家触发的同步事件。
    SyncEvent(PlayerId, SyncEvent),
    /// 某玩家请求携带我。
    CarryRequest(PlayerId, CarryMode),
    /// 某玩家接受携带。
    CarryAccept(PlayerId),
    /// 某玩家结束携带。
    CarryEnd(PlayerId),
    /// 服务器主动断开（protocol mismatch 等）。
    Disconnect { reason: String },
}

/// 协议版本。握手时比对。
pub const PROTOCOL_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_up() {
        let msg = Up::Avatar(AvatarState {
            x: 1.5,
            y: -2.5,
            ..AvatarState::ZERO
        });
        let bytes = bincode::serialize(&msg).unwrap();
        let back: Up = bincode::deserialize(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn roundtrip_down() {
        let msg = Down::SyncEvent(
            PlayerId(3),
            SyncEvent {
                channel: "door_open".into(),
                payload: SyncPayload::Flag(true),
            },
        );
        let bytes = bincode::serialize(&msg).unwrap();
        let back: Down = bincode::deserialize(&bytes).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn carry_modes_roundtrip() {
        for mode in [
            CarryMode::Grab,
            CarryMode::Tether,
            CarryMode::Ride,
            CarryMode::Launch,
        ] {
            let msg = Up::CarryRequest {
                target: PlayerId(2),
                mode,
            };
            let bytes = bincode::serialize(&msg).unwrap();
            let back: Up = bincode::deserialize(&bytes).unwrap();
            assert_eq!(msg, back);
        }
    }
}
