//! Transport abstraction.
//!
//! [`Transport`] decouples "how messages are sent" from "what messages are
//! sent":
//! - [`memory::pair`]: in-process interconnect, for headless end-to-end tests.
//! - `udp` feature: laminar semi-reliable UDP.
//! - `quic` feature: quinn QUIC.

use crate::protocol::{Down, Up};

pub mod memory;

pub use memory::{pair, MemoryServerTransport, MemoryTransport};

/// An event received by the transport layer.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportEvent {
    /// Connected (handshake complete).
    Connected,
    /// A server message was received.
    Message(Down),
    /// The connection was lost (with a reason).
    Disconnected(String),
}

/// Transport error.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport send failed: {0}")]
    Send(String),
    #[error("transport receive failed: {0}")]
    Recv(String),
    #[error("not connected")]
    NotConnected,
    #[error("protocol version mismatch: local {local}, remote {remote}")]
    VersionMismatch { local: u32, remote: u32 },
}

/// Client transport interface.
///
/// The implementor is responsible for encoding [`Up`] before sending and for
/// decoding received bytes into [`Down`].
pub trait Transport: Send {
    /// Send an upstream message.
    ///
    /// The implementation may choose its own reliability level (avatars
    /// unreliable, sync events reliable).
    fn send(&mut self, msg: Up) -> Result<(), TransportError>;

    /// Non-blocking pull of all events that have arrived.
    fn poll(&mut self) -> Vec<TransportEvent>;

    /// Transport name (for logging).
    fn name(&self) -> &'static str;
}
