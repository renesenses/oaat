//! Controller-side clock sync responder.
//!
//! The RFC (§6.2) specifies endpoint-initiated clock sync: the endpoint sends
//! SYNC_REQUEST to the controller's clock port announced in the Hello message,
//! and the controller stamps t2/t3 in the response. This responder is that
//! listener. Without it, endpoints can never learn their offset from the
//! controller clock, and PTS-based playback scheduling is impossible.

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;
use tracing::{error, info};

use oaat_core::OaatError;
use oaat_core::wire::{ClockSyncPacket, ClockSyncType};

pub struct ClockResponder;

impl ClockResponder {
    /// Bind `addr` and answer clock sync requests until aborted.
    /// Returns the actual bound port (useful with port 0) and the task handle.
    pub async fn spawn(addr: SocketAddr) -> Result<(u16, JoinHandle<()>), OaatError> {
        let socket = Arc::new(UdpSocket::bind(addr).await?);
        let port = socket.local_addr()?.port();
        info!(port, "clock sync responder listening");

        let handle = tokio::spawn(async move {
            let mut buf = [0u8; ClockSyncPacket::SIZE];
            loop {
                let (n, peer) = match socket.recv_from(&mut buf).await {
                    Ok(r) => r,
                    Err(e) => {
                        error!(error = %e, "clock responder recv error");
                        break;
                    }
                };
                if n < ClockSyncPacket::SIZE {
                    continue;
                }
                let pkt = match ClockSyncPacket::decode(&buf) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                if pkt.kind != ClockSyncType::Request {
                    continue;
                }

                let t2 = now_ns();
                let t3 = now_ns();
                let response = ClockSyncPacket {
                    version: 1,
                    kind: ClockSyncType::Response,
                    sequence: pkt.sequence,
                    t1: pkt.t1,
                    t2,
                    t3,
                };
                let mut resp_buf = [0u8; ClockSyncPacket::SIZE];
                response.encode(&mut resp_buf);
                let _ = socket.send_to(&resp_buf, peer).await;
            }
        });

        Ok((port, handle))
    }
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
}
