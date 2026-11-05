//! NetClient: the client networking service.

use releste_kernel::{Context, FrameId, Service, ServiceId};
use releste_math::Fx;
use tracing::{info, warn};

use crate::carry::CarryState;
use crate::protocol::{AvatarState, CarryMode, Down, PlayerId, SyncPayload, Up, PROTOCOL_VERSION};
use crate::session::Session;
use crate::transport::{Transport, TransportError, TransportEvent};

/// Client configuration.
#[derive(Debug, Clone)]
pub struct NetClientConfig {
    /// Server address (interpreted by the transport implementation).
    pub server_addr: String,
    /// Player name.
    pub player_name: String,
    /// Avatar send rate (Hz, default 30).
    pub avatar_send_hz: u32,
}

impl Default for NetClientConfig {
    fn default() -> Self {
        NetClientConfig {
            server_addr: "127.0.0.1:7777".into(),
            player_name: "player".into(),
            avatar_send_hz: 30,
        }
    }
}

/// An event the client exposes to game logic.
#[derive(Debug, Clone)]
pub enum NetClientEvent {
    Connected(PlayerId),
    Disconnected(String),
    PlayerJoined { id: PlayerId, name: String },
    PlayerLeft { id: PlayerId },
    CarryRequested { from: PlayerId, mode: CarryMode },
    CarryAccepted { by: PlayerId },
    CarryEnded { by: PlayerId },
    SyncChanged { channel: String, value: SyncPayload },
    Error(String),
}

/// Network error.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
    #[error("not connected")]
    NotConnected,
}

/// Client networking service.
pub struct NetClient {
    transport: Box<dyn Transport>,
    config: NetClientConfig,
    session: Session,
    carry: CarryState,
    /// Local avatar state (written by the game loop every frame).
    local_avatar: AvatarState,
    /// Event queue (consumed by the game loop).
    events: Vec<NetClientEvent>,
    /// Frames since the avatar was last sent.
    frames_since_avatar: u32,
    /// Avatar send interval (frames).
    avatar_interval: u32,
}

impl NetClient {
    /// Create.
    pub fn new(transport: Box<dyn Transport>, config: NetClientConfig) -> Self {
        let hz = config.avatar_send_hz.max(1);
        let avatar_interval = (60 / hz).max(1);
        NetClient {
            transport,
            config,
            session: Session::new(),
            carry: CarryState::new(),
            local_avatar: AvatarState::ZERO,
            events: Vec::new(),
            frames_since_avatar: 0,
            avatar_interval,
        }
    }

    /// Session (read-only).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Session (mutable, for declaring sync channels).
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Carry state.
    pub fn carry(&self) -> &CarryState {
        &self.carry
    }

    /// Local player ID.
    pub fn local_id(&self) -> Option<PlayerId> {
        self.session.local_id()
    }

    /// Write the local avatar state (called by the game loop every frame).
    pub fn set_local_avatar(&mut self, state: AvatarState) {
        self.local_avatar = state;
    }

    /// Take the accumulated events.
    pub fn drain_events(&mut self) -> Vec<NetClientEvent> {
        std::mem::take(&mut self.events)
    }

    /// Declare an explicit sync channel (driven by the map's `[[sync_rule]]`).
    pub fn declare_sync_channel(&mut self, name: impl Into<String>) {
        self.session.allow_channel(name);
    }

    /// Send a sync event (only if the map declared it).
    pub fn send_sync(&mut self, channel: &str, payload: SyncPayload) -> Result<(), NetError> {
        if !self.session.channel_allowed(channel) {
            warn!(
                channel,
                "refusing to send sync event for non-declared channel"
            );
            return Ok(());
        }
        self.transport
            .send(Up::SyncEvent(crate::protocol::SyncEvent {
                channel: channel.to_string(),
                payload,
            }))?;
        Ok(())
    }

    /// Request to carry a player.
    pub fn request_carry(&mut self, target: PlayerId, mode: CarryMode) -> Result<(), NetError> {
        self.transport.send(Up::CarryRequest { target, mode })?;
        Ok(())
    }

    /// Accept a carry request.
    ///
    /// `mode` must match the mode sent by the requester (the protocol carries
    /// it in [`Up::CarryRequest`]; the accepter needs to remember it).
    pub fn accept_carry(&mut self, initiator: PlayerId, mode: CarryMode) -> Result<(), NetError> {
        if !self.carry.accept_from(initiator, mode) {
            warn!("cannot accept carry: already in a carry relation");
        }
        self.transport.send(Up::CarryAccept { initiator })?;
        Ok(())
    }

    /// End the carry.
    pub fn end_carry(&mut self, initiator: PlayerId) -> Result<(), NetError> {
        self.carry.end();
        self.transport.send(Up::CarryEnd { initiator })?;
        Ok(())
    }

    /// Public handshake (`attach` also calls it).
    pub fn handshake(&mut self) -> Result<(), NetError> {
        self.transport.send(Up::Hello {
            protocol_version: PROTOCOL_VERSION,
            name: self.config.player_name.clone(),
        })?;
        Ok(())
    }

    /// Handle one downstream message.
    fn handle_down(&mut self, msg: Down) {
        // Carry-related messages are handled separately (coupled with the carry
        // state machine).
        match &msg {
            Down::CarryRequest(from, mode) => {
                self.events.push(NetClientEvent::CarryRequested {
                    from: *from,
                    mode: *mode,
                });
            }
            Down::CarryAccept(from) => {
                self.carry.end();
                self.events
                    .push(NetClientEvent::CarryAccepted { by: *from });
            }
            Down::CarryEnd(from) => {
                self.carry.end();
                self.events.push(NetClientEvent::CarryEnded { by: *from });
            }
            Down::Disconnect { reason } => {
                self.events
                    .push(NetClientEvent::Disconnected(reason.clone()));
            }
            _ => {}
        }

        // Record the events that need to be exposed (take a snapshot before the
        // session consumes them).
        match &msg {
            Down::Welcome {
                player_id,
                protocol_version,
            } => {
                if *protocol_version != PROTOCOL_VERSION {
                    warn!(
                        local = PROTOCOL_VERSION,
                        remote = protocol_version,
                        "protocol version mismatch"
                    );
                }
                self.events.push(NetClientEvent::Connected(*player_id));
            }
            Down::PlayerJoined { id, name } => {
                self.events.push(NetClientEvent::PlayerJoined {
                    id: *id,
                    name: name.clone(),
                });
            }
            Down::PlayerLeft { id } => {
                self.events.push(NetClientEvent::PlayerLeft { id: *id });
            }
            Down::SyncEvent(_, ev) if self.session.channel_allowed(&ev.channel) => {
                self.events.push(NetClientEvent::SyncChanged {
                    channel: ev.channel.clone(),
                    value: ev.payload.clone(),
                });
            }
            _ => {}
        }

        self.session.handle(msg);
    }
}

impl Service for NetClient {
    fn id(&self) -> ServiceId {
        ServiceId::new("net-client")
    }

    fn attach(&mut self, _ctx: &mut Context) {
        if let Err(e) = self.handshake() {
            warn!(error = %e, "failed to send handshake");
            self.events.push(NetClientEvent::Error(e.to_string()));
        } else {
            info!(transport = self.transport.name(), "net client attached");
        }
    }

    fn update(&mut self, _ctx: &mut Context, _frame: FrameId, _dt: Fx) {
        // 1. Receive packets
        for event in self.transport.poll() {
            match event {
                TransportEvent::Connected => {}
                TransportEvent::Message(msg) => self.handle_down(msg),
                TransportEvent::Disconnected(reason) => {
                    self.session.clear();
                    self.events.push(NetClientEvent::Disconnected(reason));
                }
            }
        }

        // 2. Session interpolation
        self.session.tick();

        // 3. Advance the carry state
        self.carry.tick();

        // 4. Throttle avatar sending
        self.frames_since_avatar += 1;
        if self.frames_since_avatar >= self.avatar_interval {
            self.frames_since_avatar = 0;
            let state = self.local_avatar;
            if let Err(e) = self.transport.send(Up::Avatar(state)) {
                self.events.push(NetClientEvent::Error(e.to_string()));
            }
        }
    }

    fn dispose(&mut self) {
        let _ = self.transport.send(Up::Bye);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::memory;

    fn connected_client() -> (NetClient, memory::MemoryServerTransport) {
        let (client_t, mut server_t) = memory::pair();
        let mut client = NetClient::new(Box::new(client_t), NetClientConfig::default());
        client.handshake().unwrap();

        // The server replies with Welcome
        let ups = server_t.poll_up();
        assert!(matches!(ups[0], Up::Hello { .. }));
        server_t
            .send(Down::Welcome {
                player_id: PlayerId(1),
                protocol_version: PROTOCOL_VERSION,
            })
            .unwrap();
        (client, server_t)
    }

    #[test]
    fn handshake_and_welcome() {
        let (mut client, _server) = connected_client();
        client.update(&mut test_ctx(), FrameId::ZERO, Fx::zero());
        let events = client.drain_events();
        assert!(events
            .iter()
            .any(|e| matches!(e, NetClientEvent::Connected(PlayerId(1)))));
        assert_eq!(client.local_id(), Some(PlayerId(1)));
    }

    #[test]
    fn sync_requires_declaration() {
        let (mut client, _server) = connected_client();
        // Not declared -> do not send
        client.send_sync("door", SyncPayload::Flag(true)).unwrap();
        // After declaring -> send
        client.declare_sync_channel("door");
        client.send_sync("door", SyncPayload::Flag(true)).unwrap();
    }

    #[test]
    fn avatar_is_throttled() {
        let (mut client, mut server) = connected_client();
        client.update(&mut test_ctx(), FrameId::ZERO, Fx::zero()); // drain welcome
        let _ = client.drain_events();
        // Clear the handshake
        let _ = server.poll_up();

        // 30Hz -> send once every 2 frames; 6 consecutive frames should yield 3
        for _ in 0..6 {
            client.update(&mut test_ctx(), FrameId::ZERO, Fx::zero());
        }
        let ups = server.poll_up();
        let avatars = ups.iter().filter(|m| matches!(m, Up::Avatar(_))).count();
        assert_eq!(avatars, 3, "expected 3 avatar updates in 6 frames at 30Hz");
    }

    fn test_ctx() -> Context<'static> {
        // For tests: leak an event bus to obtain a 'static context.
        let bus: &'static mut releste_kernel::EventBus =
            Box::leak(Box::new(releste_kernel::EventBus::new()));
        Context::new(bus)
    }
}
