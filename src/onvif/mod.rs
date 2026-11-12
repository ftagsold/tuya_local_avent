//! Minimal ONVIF Profile S server so UniFi Protect (and Home Assistant's ONVIF
//! integration) can adopt the monitor as a third-party camera. It answers the
//! handful of SOAP operations Protect uses, points the stream URI at the
//! authenticated go2rtc stream, serves a JPEG snapshot grabbed from it, and
//! replies to WS-Discovery probes.
//!
//! Nothing that shows the child is open: the stream URI carries credentials
//! only for a request with a valid WS-Security token, and the snapshot needs
//! either HTTP Basic with the ONVIF credentials or the access token that only
//! an authenticated GetSnapshotUri hands out.

mod soap;

use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone)]
pub struct Config {
    /// The address Protect reaches this service on (its own macvlan IP).
    pub host_ip: String,
    pub onvif_port: u16,
    /// The LAN stream URI, with credentials (for authenticated requests) and
    /// without (for tokenless ones).
    pub stream_uri: String,
    pub stream_uri_plain: String,
    /// Loopback stream URL (with credentials) the snapshot is grabbed from.
    pub snapshot_src: String,
    pub user: String,
    pub pass: String,
    pub device_name: String,
    pub device_id: String,
}

pub async fn run(cfg: Config) -> Result<()> {
    let discovery = tokio::spawn(ws_discovery(cfg.clone()));
    let http = tokio::spawn(http_server(cfg));
    tokio::select! {
        r = discovery => r.context("ws-discovery task")?,
        r = http => r.context("onvif http task")?,
    }
}

async fn http_server(cfg: Config) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", cfg.onvif_port))
        .await
        .with_context(|| format!("bind ONVIF HTTP :{}", cfg.onvif_port))?;
    tracing::info!("ONVIF service on :{} (host {})", cfg.onvif_port, cfg.host_ip);
    // Bounded: at most 32 connections, each closed after 20 s, so idle or
    // slow clients cannot pile up sockets and tasks.
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(32));
    loop {
        let (sock, _) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("onvif accept: {e}");
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            drop(sock); // over the cap: refuse rather than queue
            continue;
        };
        let cfg = cfg.clone();
        tokio::spawn(async move {
            let _permit = permit;
            match tokio::time::timeout(Duration::from_secs(20), handle_conn(sock, &cfg)).await {
                Ok(Err(e)) => tracing::debug!("onvif conn: {e:#}"),
                Err(_) => tracing::debug!("onvif conn: timed out"),
                Ok(Ok(())) => {}
            }
        });
    }
}

/// Largest request accepted (headers + body). ONVIF requests are a few KiB;
/// the cap stops a client from growing the buffer with a huge Content-Length.
const MAX_REQUEST: usize = 256 * 1024;

async fn handle_conn(mut sock: TcpStream, cfg: &Config) -> Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    // Read headers.
    let (headers_end, content_len) = loop {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_headers_end(&buf) {
            let head = String::from_utf8_lossy(&buf[..pos]);
            let len = head
                .lines()
                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                .unwrap_or(0);
            break (pos + 4, len);
        }
        if buf.len() > MAX_REQUEST {
            return Ok(());
        }
    };
    if headers_end + content_len > MAX_REQUEST {
        sock.write_all(b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
        return Ok(());
    }
    while buf.len() < headers_end + content_len {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..headers_end]).to_string();
    let body = String::from_utf8_lossy(&buf[headers_end..]).to_string();
    let request_line = head.lines().next().unwrap_or("");

    // Snapshot endpoint: access token from GetSnapshotUri, or HTTP Basic.
    if request_line.starts_with("GET") && request_line.contains("/snapshot") {
        if !snapshot_authorized(request_line, &head, cfg) {
            tracing::warn!("ONVIF snapshot: unauthorized request refused");
            let msg = "unauthorized";
            let out = format!(
                "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"avent\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                msg.len(), msg
            );
            sock.write_all(out.as_bytes()).await?;
            return Ok(());
        }
        return serve_snapshot(&mut sock, cfg).await;
    }

    // Everything else is SOAP POST. Auth-protected ops validate WSSE, but
    // GetSystemDateAndTime and the capability discovery stay open, as Protect
    // probes those before it has credentials.
    let op = soap::operation(&body);
    // Optional raw-request capture for debugging adoption (set AVENT_ONVIF_CAPTURE).
    if let Ok(path) = std::env::var("AVENT_ONVIF_CAPTURE") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let authok = soap::wsse_ok(&body, &cfg.user, &cfg.pass);
            let _ = writeln!(f, "===== op={op} wsse_ok={authok}\n--HEAD--\n{head}\n--BODY--\n{body}\n");
        }
    }
    // Protect's first probes carry no token; later media calls do. Reject a
    // present-but-wrong credential, but let tokenless discovery through.
    let auth = soap::wsse_check(&body, &cfg.user, &cfg.pass);
    if auth == Some(false) {
        tracing::warn!("ONVIF {op}: bad credentials");
        let fault = soap::auth_fault();
        let out = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/soap+xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            fault.len(), fault
        );
        sock.write_all(out.as_bytes()).await?;
        return Ok(());
    }
    let service = request_line.split_whitespace().nth(1).unwrap_or("");
    let (status, response) = match soap::dispatch(&op, service, cfg, auth == Some(true)) {
        Some(r) => ("200 OK", r),
        None => {
            note_unsupported(&op);
            ("500 Internal Server Error", soap::not_supported_fault())
        }
    };
    let out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/soap+xml; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.len(),
        response
    );
    sock.write_all(out.as_bytes()).await?;
    Ok(())
}

/// Log each unsupported operation once, so what a client (Protect, Home
/// Assistant) asks for shows up without flooding the log on every poll.
fn note_unsupported(op: &str) {
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if !seen.iter().any(|s| s == op) && seen.len() < 64 {
        seen.push(op.to_string());
        tracing::info!("ONVIF: unsupported operation {op} (answered ActionNotSupported)");
    }
}

/// Stable, locally administered MAC for the bridge, derived from the device
/// id. Shared by the ONVIF camera and the MQTT device so Home Assistant
/// merges them into one device.
pub fn device_mac(device_id: &str) -> String {
    use md5::{Digest, Md5};
    let h = Md5::digest(device_id.as_bytes());
    let b = [(h[0] & 0xfc) | 0x02, h[1], h[2], h[3], h[4], h[5]];
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":")
}

/// The snapshot access token: HMAC-SHA256(ONVIF password, device id), so it
/// survives restarts and changes with the password.
pub fn snapshot_token(cfg: &Config) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<sha2::Sha256> as Mac>::new_from_slice(cfg.pass.as_bytes()).expect("HMAC key");
    mac.update(b"snapshot:");
    mac.update(cfg.device_id.as_bytes());
    hex::encode(&mac.finalize().into_bytes()[..16])
}

fn snapshot_authorized(request_line: &str, head: &str, cfg: &Config) -> bool {
    let target = request_line.split_whitespace().nth(1).unwrap_or("");
    let token = snapshot_token(cfg);
    if target.split(['?', '&']).any(|kv| kv.strip_prefix("t=").is_some_and(|t| soap::ct_eq(t, &token))) {
        return true;
    }
    use base64::Engine;
    let want = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", cfg.user, cfg.pass));
    head.lines().any(|l| {
        let Some((k, v)) = l.split_once(':') else { return false };
        let mut parts = v.trim().splitn(2, ' ');
        k.trim().eq_ignore_ascii_case("authorization")
            && parts.next().is_some_and(|s| s.eq_ignore_ascii_case("basic"))
            && parts.next().is_some_and(|v| soap::ct_eq(v.trim(), &want))
    })
}

/// Last snapshot and when it was taken. The lock also makes grabs
/// single-flight: concurrent requests wait for one ffmpeg instead of each
/// opening its own stream.
static SNAPSHOT: tokio::sync::Mutex<Option<(std::time::Instant, std::sync::Arc<Vec<u8>>)>> = tokio::sync::Mutex::const_new(None);

async fn snapshot(src: &str) -> Result<std::sync::Arc<Vec<u8>>> {
    let mut slot = SNAPSHOT.lock().await;
    if let Some((at, jpeg)) = slot.as_ref() {
        if at.elapsed() < Duration::from_secs(3) {
            return Ok(jpeg.clone());
        }
    }
    let jpeg = std::sync::Arc::new(grab_jpeg(src).await?);
    *slot = Some((std::time::Instant::now(), jpeg.clone()));
    Ok(jpeg)
}

async fn serve_snapshot(sock: &mut TcpStream, cfg: &Config) -> Result<()> {
    let jpeg = match snapshot(&cfg.snapshot_src).await {
        Ok(j) => j,
        Err(e) => {
            tracing::warn!("snapshot grab failed: {e:#}");
            let msg = "snapshot unavailable";
            let out = format!("HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", msg.len(), msg);
            sock.write_all(out.as_bytes()).await?;
            return Ok(());
        }
    };
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        jpeg.len()
    );
    sock.write_all(header.as_bytes()).await?;
    sock.write_all(&jpeg).await?;
    Ok(())
}

/// Grab one JPEG frame from the RTSP stream with ffmpeg.
async fn grab_jpeg(rtsp_url: &str) -> Result<Vec<u8>> {
    let ffmpeg = std::env::var("AVENT_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let out = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(&ffmpeg)
            .args([
                "-v", "error", "-nostdin", "-rtsp_transport", "tcp", "-timeout", "5000000", "-i", rtsp_url,
                "-frames:v", "1", "-q:v", "4", "-f", "mjpeg", "-",
            ])
            .stdin(std::process::Stdio::null())
            // Killed if the timeout drops the future; otherwise a stuck grab
            // leaks an ffmpeg and an open stream per request.
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("ffmpeg timed out")??;
    if !out.status.success() || out.stdout.is_empty() {
        anyhow::bail!("ffmpeg produced no frame");
    }
    Ok(out.stdout)
}

async fn ws_discovery(cfg: Config) -> Result<()> {
    use socket2::{Domain, Protocol, Socket, Type};
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], 3702));
    let sock = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.bind(&addr.into())?;
    sock.join_multicast_v4(&Ipv4Addr::new(239, 255, 255, 250), &Ipv4Addr::UNSPECIFIED)?;
    sock.set_nonblocking(true)?;
    let udp = tokio::net::UdpSocket::from_std(sock.into())?;
    tracing::info!("ONVIF WS-Discovery listening on 239.255.255.250:3702");

    let mut buf = [0u8; 8192];
    loop {
        let (n, from) = match udp.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("ws-discovery recv: {e}");
                continue;
            }
        };
        // Discovery is a LAN protocol: never answer (or reflect spoofed
        // probes to) addresses outside private, link-local or loopback.
        let local = match from.ip() {
            std::net::IpAddr::V4(ip) => ip.is_private() || ip.is_link_local() || ip.is_loopback(),
            std::net::IpAddr::V6(_) => false,
        };
        let msg = String::from_utf8_lossy(&buf[..n]);
        if !local || !msg.contains("Probe") {
            continue;
        }
        let relates = soap::extract_message_id(&msg);
        let reply = soap::probe_match(&cfg, &relates);
        let _ = udp.send_to(reply.as_bytes(), from).await;
    }
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            host_ip: "192.168.1.90".into(), onvif_port: 8000,
            stream_uri: String::new(), stream_uri_plain: String::new(), snapshot_src: String::new(),
            user: "avent".into(), pass: "secret".into(),
            device_name: "Nursery".into(), device_id: "dev1".into(),
        }
    }

    #[test]
    fn snapshot_needs_token_or_basic() {
        let c = cfg();
        let t = snapshot_token(&c);
        assert!(!snapshot_authorized("GET /snapshot HTTP/1.1", "GET /snapshot HTTP/1.1\r\n", &c));
        assert!(snapshot_authorized(&format!("GET /snapshot?t={t} HTTP/1.1"), "", &c));
        assert!(!snapshot_authorized("GET /snapshot?t=wrong HTTP/1.1", "", &c));
        // base64("avent:secret")
        assert!(snapshot_authorized("GET /snapshot HTTP/1.1", "Authorization: Basic YXZlbnQ6c2VjcmV0\r\n", &c));
        assert!(!snapshot_authorized("GET /snapshot HTTP/1.1", "Authorization: Basic YXZlbnQ6bm9wZQ==\r\n", &c));
    }
}
