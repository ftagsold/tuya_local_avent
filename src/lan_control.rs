//! Persistent local control session to the monitor on TCP 6668.
//!
//! One task owns the session and multiplexes, with `tokio::select!`, three
//! things: incoming STATUS pushes (decoded to DP updates), outbound DP writes,
//! and the keep-alive. It reconnects on any error. Fully local — no cloud.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{interval, Instant};

use crate::tuya::lan::frame;
use crate::tuya::lan::session::{payload_json, Session, Version, HEARTBEAT_INTERVAL};

/// One data-point value from the monitor.
#[derive(Clone, Debug)]
pub struct DpUpdate {
    pub id: u32,
    pub value: Value,
}

/// A request to write data points to the monitor.
#[derive(Clone, Debug)]
pub struct DpWrite {
    pub dps: BTreeMap<String, Value>,
}

#[derive(Clone)]
pub struct Config {
    pub device_id: String,
    pub local_key: String,
    /// Current monitor IP; read on every (re)connect, so a move is followed.
    pub ip: tokio::sync::watch::Receiver<String>,
    pub version: Version,
}

/// Handle to the control task: subscribe to updates, send writes, read health.
#[derive(Clone)]
pub struct Control {
    updates: broadcast::Sender<DpUpdate>,
    writes: mpsc::Sender<DpWrite>,
}

impl Control {
    pub fn subscribe(&self) -> broadcast::Receiver<DpUpdate> {
        self.updates.subscribe()
    }

    pub async fn write(&self, dps: BTreeMap<String, Value>) -> Result<()> {
        self.writes.send(DpWrite { dps }).await.map_err(|_| anyhow::anyhow!("control task gone"))?;
        Ok(())
    }
}

/// Spawn the control task and return a handle. The task runs until the process
/// ends, reconnecting on failure.
pub fn spawn(cfg: Config) -> Control {
    let (updates, _) = broadcast::channel(256);
    let (write_tx, write_rx) = mpsc::channel(32);
    let ctrl = Control { updates: updates.clone(), writes: write_tx };
    tokio::spawn(run(cfg, updates, write_rx));
    ctrl
}

/// Least pause between two writes. Writes from separate HA commands (the
/// card's lullaby play sends timer, volume and track at once) would
/// otherwise reach the monitor back to back; it may drop some of them.
const WRITE_GAP: Duration = Duration::from_millis(300);

async fn run(cfg: Config, updates: broadcast::Sender<DpUpdate>, mut write_rx: mpsc::Receiver<DpWrite>) {
    loop {
        if let Err(e) = session_loop(&cfg, &updates, &mut write_rx).await {
            tracing::warn!("LAN control session for {} ended: {e:#}; reconnecting in 5s", cfg.device_id);
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}


async fn session_loop(
    cfg: &Config,
    updates: &broadcast::Sender<DpUpdate>,
    write_rx: &mut mpsc::Receiver<DpWrite>,
) -> Result<()> {
    let ip = cfg.ip.borrow().clone();
    let mut s = tokio::time::timeout(Duration::from_secs(15), Session::connect(&ip, &cfg.local_key, cfg.version))
        .await
        .map_err(|_| anyhow::anyhow!("connect to {ip} timed out"))??;
    tracing::info!("LAN control connected to {} ({})", cfg.device_id, cfg.version.as_str());

    // Prime: a plain DP_QUERY, then the device22 CONTROL_NEW read (reports
    // state on 22-char-id devices). Both read-only.
    let t = now_secs();
    let q = json!({ "gwId": cfg.device_id, "devId": cfg.device_id, "uid": cfg.device_id, "t": t });
    s.send(frame::CMD_DP_QUERY, q.to_string().as_bytes()).await.ok();
    let read_new = json!({
        "devId": cfg.device_id, "gwId": cfg.device_id, "uid": cfg.device_id, "t": t,
        "dpId": crate::dp::DPS.iter().map(|d| d.id).collect::<Vec<_>>(),
    });
    s.send(frame::CMD_CONTROL_NEW, read_new.to_string().as_bytes()).await.ok();

    let mut beat = interval(HEARTBEAT_INTERVAL);
    beat.tick().await; // first tick is immediate
    let mut next_beat = Instant::now() + HEARTBEAT_INTERVAL;
    // A half-open TCP session keeps accepting writes; only silence shows it.
    // The monitor answers every heartbeat, so 3 missed ones mean it is gone.
    let mut last_rx = Instant::now();
    let mut ip_rx = cfg.ip.clone();
    let mut last_write = Instant::now() - WRITE_GAP;

    loop {
        tokio::select! {
            frame = s.recv(Duration::from_secs(1)) => {
                // None = read timeout, loop.
                if let Some((h, plain)) = frame? {
                    last_rx = Instant::now();
                    if h.cmd == frame::CMD_CONTROL {
                        log_write_reply(&plain);
                    }
                    publish_dps(&plain, updates);
                }
            }
            req = write_rx.recv() => {
                let Some(req) = req else { return Ok(()) }; // handle dropped
                tokio::time::sleep_until(last_write + WRITE_GAP).await;
                write_dps(&mut s, &cfg.device_id, &req.dps).await?;
                last_write = Instant::now();
            }
            // A dropped sender (fixed IP) disables this branch, not fires it.
            Ok(()) = ip_rx.changed() => anyhow::bail!("monitor IP changed"),
            _ = tokio::time::sleep_until(next_beat) => {
                if last_rx.elapsed() > HEARTBEAT_INTERVAL * 3 {
                    anyhow::bail!("no reply for {}s", last_rx.elapsed().as_secs());
                }
                s.heartbeat().await?;
                next_beat = Instant::now() + HEARTBEAT_INTERVAL;
            }
        }
    }
}

fn publish_dps(plain: &[u8], updates: &broadcast::Sender<DpUpdate>) {
    let Some(json) = payload_json(plain) else { return };
    // STATUS shapes: {"dps":{..}} or {"protocol":4,"data":{"dps":{..}}}.
    let dps = json.get("dps").or_else(|| json.get("data").and_then(|d| d.get("dps")));
    if let Some(obj) = dps.and_then(|d| d.as_object()) {
        for (k, v) in obj {
            if let Ok(id) = k.parse::<u32>() {
                let _ = updates.send(DpUpdate { id, value: v.clone() });
            }
        }
    }
}

/// The monitor answers a CONTROL write with an empty body when it takes it,
/// else with an error text (e.g. "data format error"). Log the refusals.
fn log_write_reply(plain: &[u8]) {
    let text = String::from_utf8_lossy(plain);
    let text = text.trim_matches(|c: char| c.is_control() || c.is_whitespace());
    if !text.is_empty() && payload_json(plain).is_none() {
        tracing::warn!("monitor refused a DP write: {text}");
    }
}

async fn write_dps(s: &mut Session, device_id: &str, dps: &BTreeMap<String, Value>) -> Result<()> {
    let body = json!({
        "devId": device_id, "gwId": device_id, "uid": device_id, "t": now_secs(),
        "dps": dps,
    });
    tracing::info!("DP write {}", serde_json::to_string(dps).unwrap_or_default());
    s.send(frame::CMD_CONTROL, body.to_string().as_bytes()).await?;
    Ok(())
}

fn now_secs() -> String {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs().to_string()
}
