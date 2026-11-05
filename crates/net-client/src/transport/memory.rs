//! In-process transport: for headless end-to-end tests.
//!
//! AGENTS.md 9 requires "2 headless clients, local network, run cooperative
//! scenarios". This module provides an interconnect channel with no network
//! dependency so that the test can run in CI.

use crate::protocol::{Down, Up};
use crate::transport::{Transport, TransportError, TransportEvent};

/// Create a pair of interconnected transport endpoints.
///
/// An [`Up`] sent by the client shows up in the server's receive queue, and a
/// [`Down`] sent by the server shows up in the client's event stream.
pub fn pair() -> (MemoryTransport, MemoryServerTransport) {
    let (up_tx, up_rx) = flume::unbounded::<Up>();
    let (down_tx, down_rx) = flume::unbounded::<Down>();

    let client = MemoryTransport {
        tx: up_tx,
        rx: down_rx,
        connected: true,
    };
    let server = MemoryServerTransport {
        rx: up_rx,
        tx: down_tx,
    };
    (client, server)
}

/// Client-side endpoint.
pub struct MemoryTransport {
    tx: flume::Sender<Up>,
    rx: flume::Receiver<Down>,
    connected: bool,
}

impl MemoryTransport {
    /// Simulate the server disconnecting.
    pub fn disconnect(&mut self) {
        self.connected = false;
    }
}

impl Transport for MemoryTransport {
    fn send(&mut self, msg: Up) -> Result<(), TransportError> {
        if !self.connected {
            return Err(TransportError::NotConnected);
        }
        self.tx
            .send(msg)
            .map_err(|e| TransportError::Send(e.to_string()))
    }

    fn poll(&mut self) -> Vec<TransportEvent> {
        if !self.connected {
            return vec![TransportEvent::Disconnected("closed".into())];
        }
        let mut events = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            events.push(TransportEvent::Message(msg));
        }
        events
    }

    fn name(&self) -> &'static str {
        "memory"
    }
}

/// Server-side endpoint.
pub struct MemoryServerTransport {
    rx: flume::Receiver<Up>,
    tx: flume::Sender<Down>,
}

impl MemoryServerTransport {
    /// Non-blocking pull of upstream messages.
    pub fn poll_up(&mut self) -> Vec<Up> {
        let mut out = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            out.push(msg);
        }
        out
    }

    /// Downstream send.
    pub fn send(&self, msg: Down) -> Result<(), TransportError> {
        self.tx
            .send(msg)
            .map_err(|e| TransportError::Send(e.to_string()))
    }

    /// Whether it is still connected.
    pub fn is_connected(&self) -> bool {
        !self.tx.is_disconnected()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{AvatarState, Down, PlayerId};

    #[test]
    fn messages_flow_both_ways() {
        let (mut client, mut server) = pair();

        client.send(Up::Avatar(AvatarState::ZERO)).unwrap();
        let ups = server.poll_up();
        assert_eq!(ups.len(), 1);

        server
            .send(Down::PlayerJoined {
                id: PlayerId(7),
                name: "seven".into(),
            })
            .unwrap();
        let events = client.poll();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], TransportEvent::Message(_)));
    }

    #[test]
    fn send_after_disconnect_fails() {
        let (mut client, _server) = pair();
        client.disconnect();
        assert!(client.send(Up::Bye).is_err());
    }

    #[test]
    fn poll_after_disconnect_reports_disconnect() {
        let (mut client, _server) = pair();
        client.disconnect();
        let events = client.poll();
        assert!(matches!(events[0], TransportEvent::Disconnected(_)));
    }
}
