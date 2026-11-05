//! Relay logic (pure state machine, no network dependency).
//!
//! # Server responsibilities (AGENTS.md 4.4 / 11)
//! The server **does not understand** map content. It only does three things:
//! 1. Assign [`PlayerId`] and maintain the connection list.
//! 2. Forward player avatars (high frequency, unreliable).
//! 3. Forward the collaboration protocol and explicit sync events (low
//!    frequency, reliable).
//!
//! It **never** decides whether some entity needs syncing -- that is declared
//! by the map's `[[sync_rule]]` on the client.

#![deny(warnings)]

use std::collections::HashMap;

use releste_net_client::protocol::{Down, PlayerId, Up, PROTOCOL_VERSION};

/// Connected player.
#[derive(Debug, Clone)]
pub struct PlayerInfo {
    pub id: PlayerId,
    pub name: String,
    /// Whether the handshake has completed.
    pub ready: bool,
}

/// A downstream message waiting to be sent.
#[derive(Debug, Clone, PartialEq)]
pub struct Outgoing {
    pub to: PlayerId,
    pub msg: Down,
}

/// Server relay state machine.
#[derive(Debug, Default)]
pub struct Relay {
    players: HashMap<PlayerId, PlayerInfo>,
    next_id: u32,
}

impl Relay {
    /// New.
    pub fn new() -> Self {
        Relay {
            players: HashMap::new(),
            next_id: 1,
        }
    }

    /// Number of players.
    pub fn player_count(&self) -> usize {
        self.players.len()
    }

    /// A player's information.
    pub fn player(&self, id: PlayerId) -> Option<&PlayerInfo> {
        self.players.get(&id)
    }

    /// The `(id, name)` list of all currently ready players (logging / status
    /// query).
    pub fn roster(&self) -> Vec<(PlayerId, String)> {
        self.players
            .values()
            .filter(|p| p.ready)
            .map(|p| (p.id, p.name.clone()))
            .collect()
    }

    /// A new connection is established: assign an ID.
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

    /// Handle one upstream message; returns the downstream messages to send.
    pub fn on_message(&mut self, from: PlayerId, msg: Up) -> Vec<Outgoing> {
        match msg {
            Up::Hello {
                protocol_version,
                name,
            } => self.on_hello(from, protocol_version, name),
            Up::Avatar(state) => self.broadcast_except(from, Down::Avatar(from, state)),
            Up::SyncEvent(ev) => self.broadcast(Down::SyncEvent(from, ev)),
            Up::CarryRequest { target, mode } => {
                // Directed forwarding: only the target player needs to know.
                self.directed(target, Down::CarryRequest(from, mode))
            }
            Up::CarryAccept { initiator } => self.directed(initiator, Down::CarryAccept(from)),
            Up::CarryEnd { initiator: _ } => {
                // The server does not need to know who initiated it; notify all
                // affected parties.
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

        // 1. Welcome that player
        out.push(Outgoing {
            to: from,
            msg: Down::Welcome {
                player_id: from,
                protocol_version: PROTOCOL_VERSION,
            },
        });

        // 2. Tell the new player about existing players
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

        // 3. Tell existing players about the new player
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

    /// Disconnect: clean up and notify the others.
    pub fn on_disconnect(&mut self, id: PlayerId) -> Vec<Outgoing> {
        if self.players.remove(&id).is_none() {
            return Vec::new();
        }
        self.broadcast(Down::PlayerLeft { id })
    }

    /// Broadcast to all ready players.
    fn broadcast(&self, msg: Down) -> Vec<Outgoing> {
        self.yield_to_ready(msg, None)
    }

    /// Broadcast to all ready players except `except`.
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

    /// Directed send (dropped if the target is offline).
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
    use releste_net_client::protocol::{AvatarState, CarryMode, SyncEvent, SyncPayload};

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
        // The server does not interpret the payload; it forwards it verbatim
        // (including to everyone except the sender itself)
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
        let _ghost = relay.on_connect(); // not handshaken yet

        let out = relay.on_message(a, Up::Avatar(AvatarState::ZERO));
        assert!(out.is_empty(), "unready players must not receive anything");
    }
}
