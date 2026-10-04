//! 会话：远程玩家与显式同步通道的本地视图。

use std::collections::HashMap;

use reles_math::{Fx, Vec2};
use tracing::{debug, warn};

use crate::protocol::{AvatarState, Down, PlayerId, SyncPayload};

/// 远程玩家。
#[derive(Debug, Clone)]
pub struct RemotePlayer {
    pub id: PlayerId,
    pub name: String,
    /// 最近一次收到的权威状态。
    pub state: AvatarState,
    /// 本地插值后的渲染状态。
    pub interpolated: AvatarState,
}

impl RemotePlayer {
    fn new(id: PlayerId, name: String) -> Self {
        RemotePlayer {
            id,
            name,
            state: AvatarState::ZERO,
            interpolated: AvatarState::ZERO,
        }
    }
}

/// 显式同步通道。
///
/// 只有地图 `[[sync_rule]]` 声明过的 channel 才会被创建。
#[derive(Debug, Clone)]
pub struct SyncChannel {
    pub name: String,
    /// 最近的值。
    pub value: SyncPayload,
    /// 最后一次更新的服务器帧（用于冲突调试）。
    pub last_update: u64,
}

/// 客户端会话。
///
/// 管理远程玩家列表与同步通道。**不假设**任何实体需要同步。
#[derive(Debug)]
pub struct Session {
    local_id: Option<PlayerId>,
    players: HashMap<PlayerId, RemotePlayer>,
    /// 地图声明的同步通道白名单（只有在此列表中的事件才被接受）。
    allowed_channels: HashMap<String, ()>,
    channels: HashMap<String, SyncChannel>,
    frame: u64,
}

/// 远端位置插值速率（每帧向权威位置靠拢的比例）。
const INTERPOLATION_RATE: f32 = 0.35;

impl Session {
    /// 新建会话。
    pub fn new() -> Self {
        Session {
            local_id: None,
            players: HashMap::new(),
            allowed_channels: HashMap::new(),
            channels: HashMap::new(),
            frame: 0,
        }
    }

    /// 本机玩家 ID。
    pub fn local_id(&self) -> Option<PlayerId> {
        self.local_id
    }

    /// 声明地图允许的同步通道。
    ///
    /// 未在此声明的事件会被拒绝——这是"同步 opt-in"的强制点。
    pub fn allow_channel(&mut self, name: impl Into<String>) {
        self.allowed_channels.insert(name.into(), ());
    }

    /// 是否有权接受某通道。
    pub fn channel_allowed(&self, name: &str) -> bool {
        self.allowed_channels.contains_key(name)
    }

    /// 已建立的同步通道。
    pub fn channels(&self) -> &HashMap<String, SyncChannel> {
        &self.channels
    }

    /// 查询某通道的值。
    pub fn channel_value(&self, name: &str) -> Option<&SyncPayload> {
        self.channels.get(name).map(|c| &c.value)
    }

    /// 远程玩家列表。
    pub fn players(&self) -> &HashMap<PlayerId, RemotePlayer> {
        &self.players
    }

    /// 取某远程玩家。
    pub fn player(&self, id: PlayerId) -> Option<&RemotePlayer> {
        self.players.get(&id)
    }

    /// 处理一条服务器消息。
    pub fn handle(&mut self, msg: Down) {
        self.frame += 1;
        match msg {
            Down::Welcome {
                player_id,
                protocol_version,
            } => {
                debug!(?player_id, protocol_version, "session welcome");
                self.local_id = Some(player_id);
            }
            Down::PlayerJoined { id, name } => {
                self.players
                    .entry(id)
                    .or_insert_with(|| RemotePlayer::new(id, name));
            }
            Down::PlayerLeft { id } => {
                self.players.remove(&id);
            }
            Down::Avatar(id, state) => {
                if let Some(p) = self.players.get_mut(&id) {
                    p.state = state;
                }
                // 未知玩家的化身包先丢弃，等 PlayerJoined。
            }
            Down::SyncEvent(id, event) => {
                if !self.channel_allowed(&event.channel) {
                    // 同步必须 opt-in：地图没声明就拒绝。
                    warn!(
                        channel = %event.channel,
                        from = ?id,
                        "rejected sync event for non-declared channel"
                    );
                    return;
                }
                self.channels.insert(
                    event.channel.clone(),
                    SyncChannel {
                        name: event.channel,
                        value: event.payload,
                        last_update: self.frame,
                    },
                );
            }
            Down::CarryRequest(_, _)
            | Down::CarryAccept(_)
            | Down::CarryEnd(_)
            | Down::Disconnect { .. } => {
                // 由上层 NetClient 处理（与 carry 状态机耦合）。
            }
        }
    }

    /// 每帧推进：插值远程玩家位置。
    pub fn tick(&mut self) {
        let rate = Fx::from_f32(INTERPOLATION_RATE);
        for p in self.players.values_mut() {
            let target = Vec2::from_f32s(p.state.x, p.state.y);
            let current = Vec2::from_f32s(p.interpolated.x, p.interpolated.y);
            let delta = target - current;
            let next = if delta.abs().x == Fx::zero() && delta.abs().y == Fx::zero() {
                target
            } else {
                current + delta * rate
            };
            p.interpolated.x = next.x.to_f32();
            p.interpolated.y = next.y.to_f32();
            // 速度与动画直接采用权威值（抖动可由渲染层平滑）。
            p.interpolated.vx = p.state.vx;
            p.interpolated.vy = p.state.vy;
            p.interpolated.animation = p.state.animation;
            p.interpolated.facing = p.state.facing;
            p.interpolated.input_x = p.state.input_x;
            p.interpolated.input_y = p.state.input_y;
        }
    }

    /// 清空（断开时）。
    pub fn clear(&mut self) {
        self.players.clear();
        self.channels.clear();
        self.local_id = None;
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{SyncEvent, Up};

    fn join(s: &mut Session, id: u32) {
        s.handle(Down::PlayerJoined {
            id: PlayerId(id),
            name: format!("p{id}"),
        });
    }

    #[test]
    fn join_and_leave() {
        let mut s = Session::new();
        join(&mut s, 1);
        assert_eq!(s.players().len(), 1);
        s.handle(Down::PlayerLeft { id: PlayerId(1) });
        assert_eq!(s.players().len(), 0);
    }

    #[test]
    fn sync_requires_declared_channel() {
        let mut s = Session::new();
        join(&mut s, 1);
        let ev = SyncEvent {
            channel: "door_open".into(),
            payload: SyncPayload::Flag(true),
        };
        // 未声明 → 拒绝
        s.handle(Down::SyncEvent(PlayerId(1), ev.clone()));
        assert!(s.channel_value("door_open").is_none());

        // 声明后 → 接受
        s.allow_channel("door_open");
        s.handle(Down::SyncEvent(PlayerId(1), ev));
        assert_eq!(s.channel_value("door_open"), Some(&SyncPayload::Flag(true)));
    }

    #[test]
    fn avatar_interpolates_toward_target() {
        let mut s = Session::new();
        join(&mut s, 1);
        s.handle(Down::Avatar(
            PlayerId(1),
            AvatarState {
                x: 100.0,
                y: 0.0,
                ..AvatarState::ZERO
            },
        ));
        // 一帧后应该向目标移动了一部分，但不是瞬移
        s.tick();
        let x = s.player(PlayerId(1)).unwrap().interpolated.x;
        assert!(x > 0.0 && x < 100.0, "x should interpolate, got {x}");
    }

    #[test]
    fn unknown_player_avatar_is_dropped() {
        let mut s = Session::new();
        s.handle(Down::Avatar(
            PlayerId(9),
            AvatarState {
                x: 5.0,
                ..AvatarState::ZERO
            },
        ));
        assert!(s.player(PlayerId(9)).is_none());
    }

    #[test]
    fn up_messages_are_serializable() {
        let msgs = [
            Up::Hello {
                protocol_version: crate::protocol::PROTOCOL_VERSION,
                name: "tester".into(),
            },
            Up::Avatar(AvatarState::ZERO),
            Up::Bye,
        ];
        for m in msgs {
            let bytes = bincode::serialize(&m).unwrap();
            let back: Up = bincode::deserialize(&bytes).unwrap();
            assert_eq!(m, back);
        }
    }
}
