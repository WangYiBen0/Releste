//! NetClient：客户端网络服务。

use reles_kernel::{Context, FrameId, Service, ServiceId};
use reles_math::Fx;
use tracing::{info, warn};

use crate::carry::CarryState;
use crate::protocol::{AvatarState, CarryMode, Down, PlayerId, SyncPayload, Up, PROTOCOL_VERSION};
use crate::session::Session;
use crate::transport::{Transport, TransportError, TransportEvent};

/// 客户端配置。
#[derive(Debug, Clone)]
pub struct NetClientConfig {
    /// 服务器地址（传输实现解释）。
    pub server_addr: String,
    /// 玩家名。
    pub player_name: String,
    /// 化身发送频率（Hz，默认 30）。
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

/// 客户端暴露给游戏逻辑的事件。
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

/// 网络错误。
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
    #[error("not connected")]
    NotConnected,
}

/// 客户端网络服务。
pub struct NetClient {
    transport: Box<dyn Transport>,
    config: NetClientConfig,
    session: Session,
    carry: CarryState,
    /// 本地化身状态（由游戏循环每帧写入）。
    local_avatar: AvatarState,
    /// 事件队列（游戏循环消费）。
    events: Vec<NetClientEvent>,
    /// 距上次发送化身的帧数。
    frames_since_avatar: u32,
    /// 化身发送间隔（帧）。
    avatar_interval: u32,
}

impl NetClient {
    /// 创建。
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

    /// 会话（只读）。
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// 会话（可变，用于声明同步通道）。
    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// 携带状态。
    pub fn carry(&self) -> &CarryState {
        &self.carry
    }

    /// 本地玩家 ID。
    pub fn local_id(&self) -> Option<PlayerId> {
        self.session.local_id()
    }

    /// 写入本地化身状态（游戏循环每帧调用）。
    pub fn set_local_avatar(&mut self, state: AvatarState) {
        self.local_avatar = state;
    }

    /// 取出累积的事件。
    pub fn drain_events(&mut self) -> Vec<NetClientEvent> {
        std::mem::take(&mut self.events)
    }

    /// 声明一个显式同步通道（由地图的 `[[sync_rule]]` 驱动）。
    pub fn declare_sync_channel(&mut self, name: impl Into<String>) {
        self.session.allow_channel(name);
    }

    /// 发送一个同步事件（仅当地图声明过）。
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

    /// 请求携带某玩家。
    pub fn request_carry(&mut self, target: PlayerId, mode: CarryMode) -> Result<(), NetError> {
        self.transport.send(Up::CarryRequest { target, mode })?;
        Ok(())
    }

    /// 接受携带请求。
    ///
    /// `mode` 必须与请求方发来的模式一致（协议在
    /// [`Up::CarryRequest`] 中携带，接受方需要记住它）。
    pub fn accept_carry(&mut self, initiator: PlayerId, mode: CarryMode) -> Result<(), NetError> {
        if !self.carry.accept_from(initiator, mode) {
            warn!("cannot accept carry: already in a carry relation");
        }
        self.transport.send(Up::CarryAccept { initiator })?;
        Ok(())
    }

    /// 结束携带。
    pub fn end_carry(&mut self, initiator: PlayerId) -> Result<(), NetError> {
        self.carry.end();
        self.transport.send(Up::CarryEnd { initiator })?;
        Ok(())
    }

    /// 公开的握手（`attach` 也会调用）。
    pub fn handshake(&mut self) -> Result<(), NetError> {
        self.transport.send(Up::Hello {
            protocol_version: PROTOCOL_VERSION,
            name: self.config.player_name.clone(),
        })?;
        Ok(())
    }

    /// 处理一条下行消息。
    fn handle_down(&mut self, msg: Down) {
        // 携带相关消息单独处理（与 carry 状态机耦合）。
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

        // 记录需要暴露的事件（在 session 消费之前取快照）。
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
        // 1. 收包
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

        // 2. 会话插值
        self.session.tick();

        // 3. 携带状态推进
        self.carry.tick();

        // 4. 节流发送化身
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

        // 服务器应答 Welcome
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
        // 未声明 → 不发
        client.send_sync("door", SyncPayload::Flag(true)).unwrap();
        // 声明后 → 发
        client.declare_sync_channel("door");
        client.send_sync("door", SyncPayload::Flag(true)).unwrap();
    }

    #[test]
    fn avatar_is_throttled() {
        let (mut client, mut server) = connected_client();
        client.update(&mut test_ctx(), FrameId::ZERO, Fx::zero()); // drain welcome
        let _ = client.drain_events();
        // 清空 handshake
        let _ = server.poll_up();

        // 30Hz → 每 2 帧发一次；连跑 6 帧应有 3 次
        for _ in 0..6 {
            client.update(&mut test_ctx(), FrameId::ZERO, Fx::zero());
        }
        let ups = server.poll_up();
        let avatars = ups.iter().filter(|m| matches!(m, Up::Avatar(_))).count();
        assert_eq!(avatars, 3, "expected 3 avatar updates in 6 frames at 30Hz");
    }

    fn test_ctx() -> Context<'static> {
        // 测试用：泄漏一个事件总线以获得 'static 上下文。
        let bus: &'static mut reles_kernel::EventBus =
            Box::leak(Box::new(reles_kernel::EventBus::new()));
        Context::new(bus)
    }
}
