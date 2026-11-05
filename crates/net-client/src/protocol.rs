//! Collaboration protocol definition.
//!
//! # Core principles (AGENTS.md 4.4 / 11)
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
    /// Velocity (used for interpolation / effects).
    pub vx: f32,
    pub vy: f32,
    /// Animation state (decided by the client; the server does not interpret
    /// it).
    pub animation: u8,
    /// Facing.
    pub facing: i8,
    /// The input direction this frame (used for remote rendering).
    pub input_x: i8,
    pub input_y: i8,
}

impl AvatarState {
    /// Zero state (used for spawning / resetting).
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

/// Explicit sync event (low frequency, reliable transport).
///
/// Only entities declared by the map via `[[sync_rule]]` produce such events.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct SyncEvent {
    /// Sync channel name declared in the map (such as `"door_open"`).
    pub channel: String,
    /// Payload (the engine does not interpret it; consumed by the map's Lua /
    /// rules).
    pub payload: SyncPayload,
}

/// Sync payload.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum SyncPayload {
    /// Boolean flag.
    Flag(bool),
    /// Integer.
    Int(i32),
    /// Fixed-point position (for opt-in entities that need position sync).
    Position { x: i32, y: i32 },
    /// One-shot events such as picked up / triggered.
    Trigger,
}

/// Collaboration mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CarryMode {
    /// Grab, follow.
    Grab,
    /// Rope, elastic.
    Tether,
    /// Ride on top.
    Ride,
    /// Throw.
    Launch,
}

/// Client -> server message.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Up {
    /// Handshake.
    Hello { protocol_version: u32, name: String },
    /// Avatar state (high frequency, unreliable).
    Avatar(AvatarState),
    /// Sync event (low frequency, reliable).
    SyncEvent(SyncEvent),
    /// Request to carry a player.
    CarryRequest { target: PlayerId, mode: CarryMode },
    /// Accept a carry.
    CarryAccept { initiator: PlayerId },
    /// End a carry.
    CarryEnd { initiator: PlayerId },
    /// Actively disconnect.
    Bye,
}

/// Server -> client message.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Down {
    /// Handshake reply, assigns a `PlayerId`.
    Welcome {
        player_id: PlayerId,
        protocol_version: u32,
    },
    /// A player joined.
    PlayerJoined { id: PlayerId, name: String },
    /// A player left.
    PlayerLeft { id: PlayerId },
    /// A player's avatar state.
    Avatar(PlayerId, AvatarState),
    /// A sync event triggered by a player.
    SyncEvent(PlayerId, SyncEvent),
    /// A player requested to carry me.
    CarryRequest(PlayerId, CarryMode),
    /// A player accepted a carry.
    CarryAccept(PlayerId),
    /// A player ended a carry.
    CarryEnd(PlayerId),
    /// The server actively disconnects (protocol mismatch, etc.).
    Disconnect { reason: String },
}

/// Protocol version. Compared during the handshake.
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
