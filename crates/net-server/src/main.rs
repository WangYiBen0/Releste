//! Releste 中继服务器。
//!
//! 纯中继：不理解地图内容，不参与物理，不判断同步。
//! 详见 [`relay`]。
//!
//! # 运行
//! ```text
//! cargo run -p reles-net-server -- --port 7777
//! cargo run -p reles-net-server -- --selftest     # 纯逻辑自检，不需要网络
//! ```

#![deny(warnings)]

mod relay;

use std::process::ExitCode;

use clap::Parser;
use tracing::{info, warn};

use relay::Relay;

/// 命令行参数。
#[derive(Parser, Debug)]
#[command(name = "net-server", about = "Releste 中继服务器")]
struct Args {
    /// 监听端口。
    #[arg(short, long, default_value_t = 7777)]
    port: u16,

    /// 最大玩家数（0 = 不限）。
    #[arg(short = 'm', long, default_value_t = 8)]
    max_players: usize,

    /// 运行纯逻辑自检并退出（不监听网络）。
    #[arg(long)]
    selftest: bool,
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();

    if args.selftest {
        return match selftest() {
            Ok(()) => {
                info!("selftest passed");
                ExitCode::SUCCESS
            }
            Err(e) => {
                warn!(error = %e, "selftest failed");
                ExitCode::FAILURE
            }
        };
    }

    info!(
        port = args.port,
        max_players = args.max_players,
        "starting relay"
    );

    #[cfg(feature = "udp")]
    {
        match run_udp(args.port, args.max_players) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                warn!(error = %e, "server error");
                ExitCode::FAILURE
            }
        }
    }

    #[cfg(not(feature = "udp"))]
    {
        warn!(
            "built without the `udp` feature: no listener started. \
             Rebuild with `--features udp` for real networking."
        );
        ExitCode::SUCCESS
    }
}

/// 纯逻辑自检：驱动一次完整握手 + 化身转发 + 断开。
fn selftest() -> anyhow::Result<()> {
    use reles_net_client::protocol::{AvatarState, Down, Up, PROTOCOL_VERSION};

    let mut relay = Relay::new();
    let a = relay.on_connect();
    let b = relay.on_connect();

    // A 握手
    let out = relay.on_message(
        a,
        Up::Hello {
            protocol_version: PROTOCOL_VERSION,
            name: "a".into(),
        },
    );
    anyhow::ensure!(
        out.iter()
            .any(|o| matches!(o.msg, Down::Welcome { player_id, .. } if player_id == a)),
        "A should receive Welcome"
    );

    // B 握手，A 应被告知 B 加入
    let out = relay.on_message(
        b,
        Up::Hello {
            protocol_version: PROTOCOL_VERSION,
            name: "b".into(),
        },
    );
    anyhow::ensure!(
        out.iter().any(|o| o.to == a),
        "A should be told about B joining"
    );
    anyhow::ensure!(
        out.iter().any(|o| o.to == b),
        "B should be told about A joining"
    );

    // A 发送化身，B 应收到，A 不应收到自己的
    let out = relay.on_message(a, Up::Avatar(AvatarState::ZERO));
    anyhow::ensure!(out.iter().any(|o| o.to == b), "B should receive A's avatar");
    anyhow::ensure!(
        !out.iter().any(|o| o.to == a),
        "A must not receive its own avatar"
    );

    // A 断开
    let out = relay.on_disconnect(a);
    anyhow::ensure!(
        out.iter().any(|o| o.to == b),
        "B should be notified of A leaving"
    );
    anyhow::ensure!(relay.player_count() == 1, "one player should remain");

    // 幸存者名册
    let roster = relay.roster();
    anyhow::ensure!(
        roster.len() == 1 && roster[0].1 == "b",
        "roster should contain only b, got {roster:?}"
    );
    anyhow::ensure!(
        relay.player(b).is_some_and(|p| p.ready),
        "b should still be present and ready"
    );

    Ok(())
}

/// 真实 UDP 中继（laminar）。
#[cfg(feature = "udp")]
fn run_udp(port: u16, max_players: usize) -> anyhow::Result<()> {
    use std::net::SocketAddr;

    use laminar::{Config, Packet, Socket, SocketEvent};
    use reles_net_client::protocol::{Down, PlayerId, Up};

    use std::collections::HashMap;

    let addr: SocketAddr = format!("0.0.0.0:{port}").parse()?;
    let mut socket = Socket::bind_with_config(addr, Config::default())?;
    let mut relay = Relay::new();
    let mut id_of: HashMap<SocketAddr, PlayerId> = HashMap::new();
    let mut addr_of: HashMap<PlayerId, SocketAddr> = HashMap::new();
    let sender = socket.get_packet_sender();
    let receiver = socket.get_event_receiver();

    info!(%addr, "listening (laminar/udp)");

    loop {
        socket.manual_poll(std::time::Instant::now());
        match receiver.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(SocketEvent::Packet(packet)) => {
                let src = packet.addr();
                let Ok(msg) = bincode::deserialize::<Up>(packet.payload()) else {
                    warn!(%src, "dropping malformed packet");
                    continue;
                };

                // 新连接
                let from = match id_of.get(&src) {
                    Some(id) => *id,
                    None => {
                        if max_players != 0 && relay.player_count() >= max_players {
                            warn!(%src, "rejecting connection: server full");
                            continue;
                        }
                        let id = relay.on_connect();
                        id_of.insert(src, id);
                        addr_of.insert(id, src);
                        info!(%src, ?id, "client connected");
                        id
                    }
                };

                for outgoing in relay.on_message(from, msg) {
                    if let Some(dst) = addr_of.get(&outgoing.to) {
                        let bytes = bincode::serialize(&outgoing.msg)?;
                        let _ = sender.send(Packet::reliable_unordered(*dst, bytes));
                    }
                }
            }
            Ok(SocketEvent::Timeout(_)) => {}
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    Ok(())
}
