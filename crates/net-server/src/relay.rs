//! 中继逻辑（纯状态机，无网络依赖）。
//!
//! # 服务器职责（AGENTS.md §4.4 / §11）
//! 服务器**不理解**地图内容。它只做三件事：
//! 1. 分配 [`PlayerId`]、维护连接列表。
//! 2. 转发玩家化身（高频、不可靠）。
//! 3. 转发协作协议与显式同步事件（低频、可靠）。
//!
//! 它**从不**判断某个实体是否需要同步——那由地图的
//! `[[sync_rule]]` 在客户端声明。

#![deny(warnings)]

use std::collections::HashMap;

use reles_net_client::protocol::{Down, PlayerId, Up, PROTOCOL_VERSION};

/// 已连接玩家。
#[derive(Debug, Clone)]
pub struct PlayerInfo {
    pub id: PlayerId,
    pub name: String,
    /// 是否已完成握手。
    pub ready: bool,
}

/// 一条待发送的下行消息。
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    pub to: PlayerId,
    pub msg: Down,
}

/// 服务器中继状态机。
#[derive(Debug, Default)]
pub struct Relay {
    players: HashMap<PlayerId, PlayerInfo>,
    next_id: u32,
}

impl Relay {
    /// 新建。
    pub fn new() -> Self {
        Relay {
            players: HashMap::new(),
            next_id: 1,
        }
    }

    /// 玩家数量。
    pub fn player_count(&self) -> usize {
        self.players.len()
    }

    /// 某玩家的信息。
    pub fn player(&self, id: PlayerId) -> Option<&PlayerInfo> {
        self.players.get(&id)
    }

    /// 当前所有已就绪玩家的 `(id, name)` 列表（日志 / 状态查询）。
    pub fn roster(&self) -> Vec<(PlayerId, String)> {
        self.players
            .values()
            .filter(|p| p.ready)
            .map(|p| (p.id, p.name.clone()))
            .collect()
    }

    /// 新连接建立：分配 ID。
    pub fn on_connect(&mut self) -> PlayerId {
        let id = PlayerId(self.next_id);
        self.next_id += 1;
        self.players.insert(
            id,
            PlayerInfo {
                id,
                name: String::new(),
                ready: false,
            },
        );
        id
    }

    /// 处理一条上行消息，返回需要发出的下行消息。
    pub fn on_message(&mut self, from: PlayerId, msg: Up) -> Vec<Outgoing> {
        match msg {
            Up::Hello {
                protocol_version,
                name,
            } => self.on_hello(from, protocol_version, name),
            Up::Avatar(state) => self.broadcast_except(from, Down::Avatar(from, state)),
            Up::SyncEvent(ev) => self.broadcast(Down::SyncEvent(from, ev)),
            Up::CarryRequest { target, mode } => {
                // 定向转发：只有目标玩家需要知道。
                self.directed(target, Down::CarryRequest(from, mode))
            }
            Up::CarryAccept { initiator } => self.directed(initiator, Down::CarryAccept(from)),
            Up::CarryEnd { initiator: _ } => {
                // 服务器不需要知道是谁发起的；通知所有相关方。
                self.broadcast(Down::CarryEnd(from))
            }
            Up::Bye => self.on_disconnect(from),
        }
    }

    fn on_hello(&mut self, from: PlayerId, protocol_version: u32, name: String) -> Vec<Outgoing> {
        let mut out = Vec::new();

        if protocol_version != PROTOCOL_VERSION {
            out.push(Outgoing {
                to: from,
                msg: Down::Disconnect {
                    reason: format!(
                        "protocol version mismatch: server {PROTOCOL_VERSION}, client {protocol_version}"
                    ),
                },
            });
            return out;
        }

        let Some(info) = self.players.get_mut(&from) else {
            return out;
        };
        info.name = name.clone();
        info.ready = true;

        // 1. 欢迎该玩家
        out.push(Outgoing {
            to: from,
            msg: Down::Welcome {
                player_id: from,
                protocol_version: PROTOCOL_VERSION,
            },
        });

        // 2. 把已有玩家告诉新玩家
        let existing: Vec<(PlayerId, String)> = self
            .players
            .values()
            .filter(|p| p.id != from && p.ready)
            .map(|p| (p.id, p.name.clone()))
            .collect();
        for (id, name) in existing {
            out.push(Outgoing {
                to: from,
                msg: Down::PlayerJoined { id, name },
            });
        }

        // 3. 把新玩家告诉已有玩家
        for player in self.players.values() {
            if player.id != from && player.ready {
                out.push(Outgoing {
                    to: player.id,
                    msg: Down::PlayerJoined {
                        id: from,
                        name: name.clone(),
                    },
                });
            }
        }

        out
    }

    /// 断开：清理并通知其他人。
    pub fn on_disconnect(&mut self, id: PlayerId) -> Vec<Outgoing> {
        if self.players.remove(&id).is_none() {
            return Vec::new();
        }
        self.broadcast(Down::PlayerLeft { id })
    }

    /// 广播给所有已就绪玩家。
    fn broadcast(&self, msg: Down) -> Vec<Outgoing> {
        self.yield_to_ready(msg, None)
    }

    /// 广播给除 `except` 外的所有已就绪玩家。
    fn broadcast_except(&self, except: PlayerId, msg: Down) -> Vec<Outgoing> {
        self.yield_to_ready(msg, Some(except))
    }

    fn yield_to_ready(&self, msg: Down, except: Option<PlayerId>) -> Vec<Outgoing> {
        self.players
            .values()
            .filter(|p| p.ready && Some(p.id) != except)
            .map(|p| Outgoing {
                to: p.id,
                msg: msg.clone(),
            })
            .collect()
    }

    /// 定向发送（目标不在线则丢弃）。
    fn directed(&self, to: PlayerId, msg: Down) -> Vec<Outgoing> {
        match self.players.get(&to) {
            Some(p) if p.ready => vec![Outgoing { to, msg }],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reles_net_client::protocol::{AvatarState, CarryMode, SyncEvent, SyncPayload};

    fn ready(relay: &mut Relay, name: &str) -> PlayerId {
        let id = relay.on_connect();
        relay.on_message(
            id,
            Up::Hello {
                protocol_version: PROTOCOL_VERSION,
                name: name.into(),
            },
        );
        id
    }

    #[test]
    fn hello_assigns_id_and_welcomes() {
        let mut relay = Relay::new();
        let id = relay.on_connect();
        let out = relay.on_message(
            id,
            Up::Hello {
                protocol_version: PROTOCOL_VERSION,
                name: "a".into(),
            },
        );
        assert!(out.iter().any(|o| matches!(
            o.msg,
            Down::Welcome { player_id, .. } if player_id == id
        )));
    }

    #[test]
    fn version_mismatch_disconnects() {
        let mut relay = Relay::new();
        let id = relay.on_connect();
        let out = relay.on_message(
            id,
            Up::Hello {
                protocol_version: 999,
                name: "a".into(),
            },
        );
        assert!(matches!(out[0].msg, Down::Disconnect { .. }));
    }

    #[test]
    fn avatar_is_relayed_to_others_only() {
        let mut relay = Relay::new();
        let a = ready(&mut relay, "a");
        let b = ready(&mut relay, "b");

        let out = relay.on_message(a, Up::Avatar(AvatarState::ZERO));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].to, b, "avatar must not echo back to sender");
        assert!(matches!(out[0].msg, Down::Avatar(id, _) if id == a));
    }

    #[test]
    fn sync_event_is_broadcast_verbatim() {
        let mut relay = Relay::new();
        let a = ready(&mut relay, "a");
        let b = ready(&mut relay, "b");

        let ev = SyncEvent {
            channel: "door".into(),
            payload: SyncPayload::Flag(true),
        };
        let out = relay.on_message(a, Up::SyncEvent(ev.clone()));
        // 服务器不解释 payload，原样转发（包括回发给自己以外的所有人）
        assert!(
            out.iter()
                .any(|o| o.to == b
                    && matches!(&o.msg, Down::SyncEvent(id, e) if *id == a && *e == ev))
        );
    }

    #[test]
    fn carry_request_is_directed() {
        let mut relay = Relay::new();
        let a = ready(&mut relay, "a");
        let b = ready(&mut relay, "b");
        let c = ready(&mut relay, "c");

        let out = relay.on_message(
            a,
            Up::CarryRequest {
                target: b,
                mode: CarryMode::Grab,
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].to, b);
        assert!(
            !out.iter().any(|o| o.to == c),
            "carry request must not reach third parties"
        );
    }

    #[test]
    fn disconnect_notifies_others() {
        let mut relay = Relay::new();
        let a = ready(&mut relay, "a");
        let b = ready(&mut relay, "b");

        let out = relay.on_disconnect(a);
        assert!(out
            .iter()
            .any(|o| o.to == b && matches!(o.msg, Down::PlayerLeft { id } if id == a)));
        assert_eq!(relay.player_count(), 1);
    }

    #[test]
    fn unready_players_are_not_targets() {
        let mut relay = Relay::new();
        let a = ready(&mut relay, "a");
        let _ghost = relay.on_connect(); // 未握手

        let out = relay.on_message(a, Up::Avatar(AvatarState::ZERO));
        assert!(out.is_empty(), "unready players must not receive anything");
    }
}
