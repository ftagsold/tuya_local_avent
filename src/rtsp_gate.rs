//! The LAN face of the RTSP server: a TCP gate in front of go2rtc.
//!
//! go2rtc accepts a viewer before the monitor's video is up, then rejects its
//! backchannel (`codecs not matched: audio:PCMU => ...`, the Go bridge source
//! is not there yet). UniFi Protect takes that as a broken camera and stays
//! on a frozen snapshot until it is told to reconnect. So viewers only get
//! through while the relay delivers frames; otherwise the connection is
//! closed at once, which Protect treats as "offline" and retries.
//!
//! The gate dials go2rtc on the host's LAN address, not loopback: go2rtc
//! skips RTSP auth for loopback peers, so a loopback hop would open the
//! stream to anyone. Sessions already through stay up when the gate closes.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::net::{TcpListener, TcpStream};

use crate::media::Health;

/// Open while the last relay frame is at most this old.
const FRESH: Duration = Duration::from_secs(5);

pub async fn run(listen: SocketAddr, upstream: SocketAddr, health: Arc<Health>) -> Result<()> {
    let listener = TcpListener::bind(listen).await.with_context(|| format!("bind RTSP gate {listen}"))?;
    tracing::info!("RTSP gate on {listen} -> go2rtc {upstream} (closed until video is up)");
    let mut was_open = false;
    loop {
        let (mut viewer, peer) = listener.accept().await.context("accept")?;
        let open = health.frame_age() <= FRESH;
        if open != was_open {
            tracing::info!("RTSP gate {}", if open { "open: video up" } else { "closed: no video" });
            was_open = open;
        }
        if !open {
            tracing::debug!("RTSP gate: refused {peer} (no video)");
            continue; // dropping `viewer` closes it
        }
        tokio::spawn(async move {
            let mut g2r = match TcpStream::connect(upstream).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("RTSP gate: go2rtc {upstream}: {e}");
                    return;
                }
            };
            let _ = viewer.set_nodelay(true);
            let _ = g2r.set_nodelay(true);
            let _ = tokio::io::copy_bidirectional(&mut viewer, &mut g2r).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Closed before the first frame, then forwards both ways.
    #[tokio::test]
    async fn opens_only_with_fresh_frames() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up_addr = upstream.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = upstream.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut b = [0u8; 4];
                    s.read_exact(&mut b).await.unwrap();
                    s.write_all(&b).await.unwrap();
                });
            }
        });
        let free = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gate_addr = free.local_addr().unwrap();
        drop(free);
        let health = Arc::new(Health::default());
        health.touch(); // a started relay without frames keeps it closed
        tokio::spawn(run(gate_addr, up_addr, health.clone()));
        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut c = TcpStream::connect(gate_addr).await.unwrap();
        let mut b = [0u8; 4];
        assert_eq!(c.read(&mut b).await.unwrap(), 0, "closed gate must close the viewer");

        health.frame();
        let mut c = TcpStream::connect(gate_addr).await.unwrap();
        c.write_all(b"RTSP").await.unwrap();
        c.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"RTSP");
    }
}
