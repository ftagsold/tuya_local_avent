//! The LAN face of the RTSP server: a TCP gate in front of go2rtc.
//!
//! go2rtc accepts a viewer before the monitor's video is up, then rejects its
//! backchannel (`codecs not matched: audio:PCMU => ...`, the Go bridge source
//! is not there yet). And a viewer that joined before an outage stays bound
//! to the old relay publisher: go2rtc keeps its session (keepalives answered)
//! but sends nothing once a new relay publishes. UniFi Protect takes either
//! as a broken camera and stays on a frozen snapshot until it is told to
//! reconnect. So the gate:
//!
//! - lets viewers through only once frames have flowed for `SETTLE`;
//! - answers a viewer it holds back `503 Service Unavailable` (with its CSeq
//!   and Retry-After), the RTSP way of saying "try again shortly";
//! - cuts every session through it when video stops or the relay restarts,
//!   so viewers reconnect to the live publisher instead of freezing;
//! - cuts a viewer that stops reading. Protect can wedge a session: TCP stays
//!   up (zero window), its reader never drains, the live view freezes for
//!   hours. A cut makes it reconnect.
//!
//! The gate dials go2rtc on the host's LAN address, not loopback: go2rtc
//! skips RTSP auth for loopback peers, so a loopback hop would open the
//! stream to anyone.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::media::Health;

/// Video counts as up while the last relay frame is at most this old.
const FRESH: Duration = Duration::from_secs(3);
/// Frames must have flowed this long before viewers get through, so a relay
/// that dies right after its first frames does not let them in.
const SETTLE: Duration = Duration::from_secs(3);
/// How often a session checks that its relay run is still live.
const CHECK: Duration = Duration::from_millis(500);
/// A viewer that takes this long to accept one write has stopped reading.
const STALL: Duration = Duration::from_secs(10);

/// The relay run viewers may join now, if any.
fn live(health: &Health) -> Option<u64> {
    (health.frame_age() <= FRESH && health.up_for() >= SETTLE).then(|| health.run())
}

pub async fn run(listen: SocketAddr, upstream: SocketAddr, health: Arc<Health>) -> Result<()> {
    let listener = TcpListener::bind(listen).await.with_context(|| format!("bind RTSP gate {listen}"))?;
    tracing::info!("RTSP gate on {listen} -> go2rtc {upstream} (closed until video is up)");
    let mut was_open = false;
    loop {
        let (viewer, peer) = listener.accept().await.context("accept")?;
        let run = live(&health);
        if run.is_some() != was_open {
            tracing::info!("RTSP gate {}", if run.is_some() { "open: video up" } else { "closed: no video" });
            was_open = run.is_some();
        }
        let Some(run) = run else {
            // Viewers retry every few seconds while video is down.
            tracing::debug!("RTSP gate: held back {peer} (no video yet): 503");
            tokio::spawn(refuse(viewer));
            continue;
        };
        let health = health.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            tracing::info!("RTSP viewer {peer} connected");
            let why = forward(viewer, upstream, &health, run).await;
            tracing::info!("RTSP viewer {peer} gone after {}s: {why}", started.elapsed().as_secs());
        });
    }
}

/// Pipe one viewer to go2rtc until either side closes or the relay run it
/// joined stops being live. Returns why it ended.
async fn forward(mut viewer: TcpStream, upstream: SocketAddr, health: &Health, run: u64) -> String {
    let mut g2r = match TcpStream::connect(upstream).await {
        Ok(s) => s,
        Err(e) => return format!("go2rtc {upstream}: {e}"),
    };
    let _ = viewer.set_nodelay(true);
    let _ = g2r.set_nodelay(true);
    let watch = async {
        let mut tick = tokio::time::interval(CHECK);
        loop {
            tick.tick().await;
            if health.run() != run {
                return "relay restarted: cut";
            }
            if health.frame_age() > FRESH {
                return "video stopped: cut";
            }
        }
    };
    let (mut v_rd, mut v_wr) = viewer.split();
    let (mut g_rd, mut g_wr) = g2r.split();
    let up = async {
        match tokio::io::copy(&mut v_rd, &mut g_wr).await {
            Ok(_) => "closed".to_string(),
            Err(e) => format!("closed: {e}"),
        }
    };
    let down = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match g_rd.read(&mut buf).await {
                Ok(0) => return "closed".to_string(),
                Ok(n) => n,
                Err(e) => return format!("closed: {e}"),
            };
            match tokio::time::timeout(STALL, v_wr.write_all(&buf[..n])).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return format!("closed: {e}"),
                Err(_) => return format!("viewer stopped reading for {}s: cut", STALL.as_secs()),
            }
        }
    };
    tokio::select! {
        why = up => why,
        why = down => why,
        why = watch => why.into(),
    }
}

/// Answer the viewer's first request with 503, then close.
async fn refuse(mut viewer: TcpStream) {
    let mut buf = vec![0u8; 4096];
    let mut n = 0;
    let read = async {
        while n < buf.len() {
            match viewer.read(&mut buf[n..]).await {
                Ok(0) | Err(_) => break,
                Ok(k) => n += k,
            }
            if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(3), read).await;
    let _ = viewer.write_all(unavailable(&buf[..n]).as_bytes()).await;
    let _ = viewer.shutdown().await;
}

/// `503 Service Unavailable` for an RTSP request, echoing its CSeq.
fn unavailable(request: &[u8]) -> String {
    let req = String::from_utf8_lossy(request);
    let cseq = req
        .lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("cseq")).map(|(_, v)| v.trim().to_string()))
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .unwrap_or_else(|| "0".into());
    format!("RTSP/1.0 503 Service Unavailable\r\nCSeq: {cseq}\r\nRetry-After: 5\r\nContent-Length: 0\r\n\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn echo_upstream() -> SocketAddr {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = upstream.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = upstream.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut b = [0u8; 64];
                    while let Ok(n) = s.read(&mut b).await {
                        if n == 0 || s.write_all(&b[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        addr
    }

    async fn gate(health: Arc<Health>) -> SocketAddr {
        let up_addr = echo_upstream().await;
        let free = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gate_addr = free.local_addr().unwrap();
        drop(free);
        tokio::spawn(run(gate_addr, up_addr, health));
        tokio::time::sleep(Duration::from_millis(100)).await;
        gate_addr
    }

    /// Frames for `SETTLE`, ticking like ffmpeg's -progress.
    async fn settle(health: &Health) {
        let end = Instant::now() + SETTLE + Duration::from_millis(300);
        while Instant::now() < end {
            health.frame();
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    #[test]
    fn unavailable_echoes_cseq() {
        let r = unavailable(b"OPTIONS rtsp://x/Karla RTSP/1.0\r\nCSeq: 7\r\n\r\n");
        assert!(r.starts_with("RTSP/1.0 503 "));
        assert!(r.contains("CSeq: 7\r\n"));
        assert!(unavailable(b"junk").contains("CSeq: 0\r\n"));
    }

    /// Held back with 503 until frames settle, then forwards both ways.
    #[tokio::test]
    async fn opens_only_after_frames_settle() {
        let health = Arc::new(Health::default());
        health.touch(); // a started relay without frames keeps it closed
        let addr = gate(health.clone()).await;

        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(b"OPTIONS rtsp://x/K RTSP/1.0\r\nCSeq: 2\r\n\r\n").await.unwrap();
        let mut reply = String::new();
        c.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("RTSP/1.0 503") && reply.contains("CSeq: 2"), "{reply}");

        health.frame(); // one fresh frame is not enough
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(b"OPTIONS rtsp://x/K RTSP/1.0\r\nCSeq: 3\r\n\r\n").await.unwrap();
        let mut reply = String::new();
        c.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("RTSP/1.0 503"), "{reply}");

        settle(&health).await;
        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(b"RTSP").await.unwrap();
        let mut b = [0u8; 4];
        c.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"RTSP");
    }

    /// A relay restart cuts the sessions through the gate.
    #[tokio::test]
    async fn relay_restart_cuts_sessions() {
        let health = Arc::new(Health::default());
        health.touch();
        let addr = gate(health.clone()).await;
        settle(&health).await;

        let mut c = TcpStream::connect(addr).await.unwrap();
        c.write_all(b"ping").await.unwrap();
        let mut b = [0u8; 4];
        c.read_exact(&mut b).await.unwrap();

        health.touch();
        let n = tokio::time::timeout(Duration::from_secs(2), c.read(&mut b)).await.expect("cut in time").unwrap_or(0);
        assert_eq!(n, 0, "restart must close the viewer");
    }

    /// A viewer that stops reading is cut, while video keeps flowing.
    #[tokio::test]
    async fn stalled_viewer_is_cut() {
        let health = Arc::new(Health::default());
        health.touch();
        // Upstream that floods every connection.
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let up_addr = upstream.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = upstream.accept().await.unwrap();
                tokio::spawn(async move { while s.write_all(&[0u8; 65536]).await.is_ok() {} });
            }
        });
        let frames = health.clone();
        tokio::spawn(async move {
            loop {
                frames.frame();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
        settle(&health).await;
        let free = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gate_addr = free.local_addr().unwrap();
        drop(free);
        tokio::spawn(run(gate_addr, up_addr, health));
        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut c = TcpStream::connect(gate_addr).await.unwrap();
        // Read nothing: buffers fill, the gate's writes block.
        tokio::time::sleep(STALL + Duration::from_secs(3)).await;
        let mut b = vec![0u8; 1 << 20];
        let mut total = 0;
        let closed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match c.read(&mut b).await {
                    Ok(0) | Err(_) => return true,
                    Ok(n) => total += n,
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(closed, "stalled viewer must be cut (read {total} bytes)");
    }

    /// Video stopping cuts the sessions through the gate.
    #[tokio::test]
    async fn stale_video_cuts_sessions() {
        let health = Arc::new(Health::default());
        health.touch();
        let addr = gate(health.clone()).await;
        settle(&health).await;

        let mut c = TcpStream::connect(addr).await.unwrap();
        let mut b = [0u8; 4];
        let n = tokio::time::timeout(FRESH + Duration::from_secs(2), c.read(&mut b)).await.expect("cut in time").unwrap_or(0);
        assert_eq!(n, 0, "no frames must close the viewer");
    }
}
