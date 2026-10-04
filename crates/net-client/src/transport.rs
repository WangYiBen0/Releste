//! 传输抽象。
//!
//! [`Transport`] 把"如何发消息"与"发什么消息"解耦：
//! - [`memory::pair`]：进程内互联，用于 headless 端到端测试。
//! - `udp` feature：laminar 半可靠 UDP。
//! - `quic` feature：quinn QUIC。

use crate::protocol::{Down, Up};

pub mod memory;

pub use memory::{pair, MemoryServerTransport, MemoryTransport};

/// 传输层收到的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum TransportEvent {
    /// 已连接（握手完成）。
    Connected,
    /// 收到一条服务器消息。
    Message(Down),
    /// 连接断开（含原因）。
    Disconnected(String),
}

/// 传输错误。
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

/// 客户端传输接口。
///
/// 实现者负责把 [`Up`] 编码后发出，把收到的字节解码为 [`Down`]。
pub trait Transport: Send {
    /// 发送一条上行消息。
    ///
    /// 实现可自行决定可靠性等级（化身不可靠、同步事件可靠）。
    fn send(&mut self, msg: Up) -> Result<(), TransportError>;

    /// 非阻塞拉取所有已到达的事件。
    fn poll(&mut self) -> Vec<TransportEvent>;

    /// 传输名称（日志用）。
    fn name(&self) -> &'static str;
}
