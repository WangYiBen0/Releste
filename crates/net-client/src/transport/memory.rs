//! 进程内传输：用于 headless 端到端测试。
//!
//! AGENTS.md §9 要求"headless 客户端 × 2，本地网络，跑合作场景"。
//! 本模块提供无网络依赖的互联通道，使该测试可在 CI 中运行。

use crate::protocol::{Down, Up};
use crate::transport::{Transport, TransportError, TransportEvent};

/// 创建一对互联的传输端点。
///
/// 客户端发送的 [`Up`] 出现在服务器的接收队列，
/// 服务器发送的 [`Down`] 出现在客户端的事件流。
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

/// 客户端侧端点。
pub struct MemoryTransport {
    tx: flume::Sender<Up>,
    rx: flume::Receiver<Down>,
    connected: bool,
}

impl MemoryTransport {
    /// 模拟服务器断开。
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

/// 服务器侧端点。
pub struct MemoryServerTransport {
    rx: flume::Receiver<Up>,
    tx: flume::Sender<Down>,
}

impl MemoryServerTransport {
    /// 非阻塞拉取上行消息。
    pub fn poll_up(&mut self) -> Vec<Up> {
        let mut out = Vec::new();
        while let Ok(msg) = self.rx.try_recv() {
            out.push(msg);
        }
        out
    }

    /// 下行发送。
    pub fn send(&self, msg: Down) -> Result<(), TransportError> {
        self.tx
            .send(msg)
            .map_err(|e| TransportError::Send(e.to_string()))
    }

    /// 是否仍连接。
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
