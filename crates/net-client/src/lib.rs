//! `reles-net-client`: client networking layer.
//!
//! # Core principles (AGENTS.md §4.4)
//! - **The world is not synced by default**. Only entities explicitly
//!   declared by the map via `[[sync_rule]]` produce [`SyncEvent`]s.
//! - Client-authoritative: the server does not touch physics or understand map content.
//! - Carry mechanics matter: the carried player **disables collision locally** (see [`carry`]).
//!
//! # Feature
//! - Default: protocol, carry state machine, session management (pure logic, testable).
//! - `udp`: laminar transport implementation.
//! - `quic`: quinn transport implementation.

#![deny(warnings)]

pub mod carry;
pub mod protocol;
pub mod session;
pub mod transport;

mod client;

pub use carry::{CarryLink, CarryState};
pub use client::{NetClient, NetClientConfig, NetError};
pub use protocol::{
    AvatarState, CarryMode, Down, PlayerId, SyncEvent, SyncPayload, Up, PROTOCOL_VERSION,
};
pub use session::{RemotePlayer, Session, SyncChannel};
pub use transport::{Transport, TransportEvent};
