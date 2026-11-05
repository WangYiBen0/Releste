//! Session: the local view of remote players and explicit sync channels.

use std::collections::HashMap;

use releste_math::{Fx, Vec2};
use tracing::{debug, warn};

use crate::protocol::{AvatarState, Down, PlayerId, SyncPayload};

/// Remote player.
#[derive(Debug, Clone)]
pub struct RemotePlayer {
    pub id: PlayerId,
    pub name: String,
    /// The most recently received authoritative state.
    pub state: AvatarState,
    /// The locally interpolated render state.
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

/// Explicit sync channel.
///
/// Only channels declared by the map `[[sync_rule]]` are created.
#[derive(Debug, Clone)]
pub struct SyncChannel {
    pub name: String,
    /// The most recent value.
    pub value: SyncPayload,
    /// The server frame of the last update (for conflict debugging).
    pub last_update: u64,
}

/// Client session.
///
/// Manages the remote player list and sync channels. Makes **no assumption**
/// that any entity needs syncing.
#[derive(Debug)]
pub struct Session {
    local_id: Option<PlayerId>,
    players: HashMap<PlayerId, RemotePlayer>,
    /// Whitelist of sync channels declared by the map (only events in this list
    /// are accepted).
    allowed_channels: HashMap<String, ()>,
    channels: HashMap<String, SyncChannel>,
    frame: u64,
}

/// Remote position interpolation rate (fraction of the way to the
/// authoritative position per frame).
const INTERPOLATION_RATE: f32 = 0.35;

impl Session {
    /// Create a new session.
    pub fn new() -> Self {
        Session {
            local_id: None,
            players: HashMap::new(),
            allowed_channels: HashMap::new(),
            channels: HashMap::new(),
            frame: 0,
        }
    }

    /// Local player ID.
    pub fn local_id(&self) -> Option<PlayerId> {
        self.local_id
    }

    /// Declare the sync channels the map permits.
    ///
    /// Events not declared here are rejected -- this is the enforcement point
    /// of "sync opt-in".
    pub fn allow_channel(&mut self, name: impl Into<String>) {
        self.allowed_channels.insert(name.into(), ());
    }

    /// Whether a channel is allowed to be accepted.
    pub fn channel_allowed(&self, name: &str) -> bool {
        self.allowed_channels.contains_key(name)
    }

    /// Established sync channels.
    pub fn channels(&self) -> &HashMap<String, SyncChannel> {
        &self.channels
    }

    /// Query a channel's value.
    pub fn channel_value(&self, name: &str) -> Option<&SyncPayload> {
        self.channels.get(name).map(|c| &c.value)
    }

    /// Remote player list.
    pub fn players(&self) -> &HashMap<PlayerId, RemotePlayer> {
        &self.players
    }

    /// Get a remote player.
    pub fn player(&self, id: PlayerId) -> Option<&RemotePlayer> {
        self.players.get(&id)
    }

    /// Handle one server message.
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
                // Avatar packets for unknown players are dropped for now; wait
                // for PlayerJoined.
            }
            Down::SyncEvent(id, event) => {
                if !self.channel_allowed(&event.channel) {
                    // Sync must be opt-in: reject if the map did not declare it.
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
                // Handled by the upper-layer NetClient (coupled with the carry
                // state machine).
            }
        }
    }

    /// Advance each frame: interpolate remote player positions.
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
            // Velocity and animation take the authoritative values directly
            // (jitter can be smoothed by the render layer).
            p.interpolated.vx = p.state.vx;
            p.interpolated.vy = p.state.vy;
            p.interpolated.animation = p.state.animation;
            p.interpolated.facing = p.state.facing;
            p.interpolated.input_x = p.state.input_x;
            p.interpolated.input_y = p.state.input_y;
        }
    }

    /// Clear (on disconnect).
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
        // Not declared -> reject
        s.handle(Down::SyncEvent(PlayerId(1), ev.clone()));
        assert!(s.channel_value("door_open").is_none());

        // After declaring -> accept
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
        // After one frame it should have moved part of the way toward the
        // target, but not teleported
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
