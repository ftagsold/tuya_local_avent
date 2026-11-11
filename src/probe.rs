//! `tuya_local_avent probe`: find the monitor, learn its protocol, and prove the
//! local key, without changing anything on the device.
//!
//! Read-only by construction. It sends a session handshake, one DP_QUERY
//! (command 10, which carries no data points) and heartbeats. It never sends
//! CONTROL or UPDATEDPS: `updatedps` is documented to crash this firmware.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use tokio::time::Instant;

use crate::discover;
use crate::tuya::lan::frame;
use crate::tuya::lan::session::{payload_json, prefix_name, Session, Version, HEARTBEAT_INTERVAL};

/// The DP ids worth asking a device22 monitor to report (from aventproxy's
/// DPS reference). A read list only; no values are attached.
const READABLE_DPS: &[u32] = &[
    101, 102, 106, 134, 138, 139, 140, 158, 168, 169, 201, 202, 203, 204, 205, 206, 207, 208, 209,
    231, 232, 233, 234, 237, 239, 240, 241, 242, 243, 244, 245, 246, 247, 248,
];

fn hex(bytes: &[u8], max: usize) -> String {
    let shown: String = bytes.iter().take(max).map(|b| format!("{b:02x}")).collect();
    if bytes.len() > max { format!("{shown}… ({} bytes)", bytes.len()) } else { shown }
}

pub struct Args {
    pub device_id: Option<String>,
    pub local_key: Option<String>,
    pub ip: Option<String>,
    pub scan_secs: u64,
    pub listen_secs: u64,
}

pub async fn run(args: Args) -> Result<()> {
    println!("Listening for Tuya broadcasts ({} s)...", args.scan_secs);
    let seen = discover::scan(Duration::from_secs(args.scan_secs), args.device_id.as_deref()).await;
    if seen.is_empty() {
        println!("  nothing heard. Is this host on the monitor's network (no VLAN/AP isolation)?");
    }
    for a in seen.values() {
        let tag = if a.product_key == "kzm54lhabeeucq5a" { "  <- Philips Avent SCD973/SCD923 family" } else { "" };
        println!("  {}  ip={}  protocol={}  product={}{}", a.device_id, a.ip, a.version, a.product_key, tag);
    }

    let Some(device_id) = args.device_id else {
        println!("\nSet AVENT_DEVICE_ID and AVENT_LOCAL_KEY to test the local session.");
        return Ok(());
    };
    let Some(local_key) = args.local_key else {
        println!("\nSet AVENT_LOCAL_KEY to test the local session.");
        return Ok(());
    };
    let announced = seen.get(&device_id);
    let Some(ip) = args.ip.or_else(|| announced.map(|a| a.ip.clone())) else {
        println!("\nDevice {device_id} was not heard. Pass --ip to connect anyway.");
        return Ok(());
    };
    let announced_version = announced.and_then(|a| Version::parse(&a.version));
    println!(
        "\nMonitor {device_id} at {ip}, announces protocol {}",
        announced_version.map_or("unknown", Version::as_str)
    );

    // 3.5 decides local video: a successful negotiation also proves the key.
    print!("Trying a 3.5 session handshake... ");
    let v35 = match tokio::time::timeout(Duration::from_secs(10), Session::connect(&ip, &local_key, Version::V35)).await {
        Ok(Ok(_)) => {
            println!("OK (local key proven)");
            true
        }
        Ok(Err(e)) => {
            println!("no ({e:#})");
            false
        }
        Err(_) => {
            println!("no (timed out)");
            false
        }
    };

    let version = if v35 { Version::V35 } else { Version::V33 };
    println!("Opening a {} session and listening {} s for state pushes.", version.as_str(), args.listen_secs);
    println!("Change something in the app (light, volume) to trigger a push.\n");
    let mut s = Session::connect(&ip, &local_key, version).await?;

    let t = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs().to_string();
    // Plain DP_QUERY first (works on normal devices).
    let query = serde_json::json!({ "gwId": device_id, "devId": device_id, "uid": device_id, "t": t });
    s.send(frame::CMD_DP_QUERY, query.to_string().as_bytes()).await?;
    // Then the device22 read: CONTROL_NEW with a dpId list and no dps values.
    // This is a request to report state; it sets nothing.
    let read_new = serde_json::json!({
        "devId": device_id, "gwId": device_id, "uid": device_id, "t": t,
        "dpId": READABLE_DPS,
    });
    s.send(frame::CMD_CONTROL_NEW, read_new.to_string().as_bytes()).await?;
    println!("Sent DP_QUERY and a device22 CONTROL_NEW read (no values set).\n");

    let start = Instant::now();
    let deadline = start + Duration::from_secs(args.listen_secs);
    let mut next_beat = start + HEARTBEAT_INTERVAL;
    let mut decrypted = false;
    let mut query_answered = false;
    let mut lan_302_seen = false;
    let mut dps_seen = BTreeSet::new();

    while Instant::now() < deadline {
        let wait = next_beat.min(deadline).saturating_duration_since(Instant::now());
        match s.recv_raw(wait.max(Duration::from_millis(10))).await {
            Ok(Some((h, raw))) => {
                let secs = start.elapsed().as_secs();
                let tag = format!("+{secs:>3}s {} cmd {:>2} len {:>4}", prefix_name(h.prefix), h.cmd, h.length);
                match s.decode(&h, &raw) {
                    Ok(plain) => match payload_json(&plain) {
                        Some(json) => {
                            decrypted = true;
                            if matches!(h.cmd, frame::CMD_DP_QUERY | frame::CMD_CONTROL_NEW | frame::CMD_DP_QUERY_NEW) {
                                query_answered = true;
                            }
                            if h.cmd == frame::CMD_IPC_LAN_302 {
                                lan_302_seen = true;
                            }
                            if let Some(dps) = json.get("dps").and_then(|d| d.as_object()) {
                                dps_seen.extend(dps.keys().cloned());
                            }
                            println!("  {tag}: {json}");
                        }
                        None if plain.is_empty() => println!("  {tag}: (empty ack)"),
                        None => {
                            // Readable text after ECB unpadding means the key is right
                            // (e.g. "json obj data unvalid", the 22-char-id DP_QUERY reply).
                            decrypted |= std::str::from_utf8(&plain).is_ok();
                            println!("  {tag}: not JSON: {:?}", String::from_utf8_lossy(&plain));
                        }
                    },
                    Err(e) => println!("  {tag}: cannot decrypt ({e}); raw: {}", hex(&raw, 64)),
                }
            }
            Ok(None) => {}
            Err(e) => {
                println!("  session ended: {e:#}");
                break;
            }
        }
        if Instant::now() >= next_beat {
            s.heartbeat().await?;
            next_beat += HEARTBEAT_INTERVAL;
        }
    }

    println!("\n--- summary ---");
    println!("protocol:        {}", version.as_str());
    println!(
        "local key:       {}",
        if v35 || decrypted { "proven" } else { "not proven yet (no push decrypted; change a setting and rerun)" }
    );
    println!(
        "state read:      {}",
        if query_answered {
            "answered (monitor reported its DPs on request)"
        } else {
            "not answered (state only arrives on change; HA would fill from the cloud at startup)"
        }
    );
    println!("DPs seen:        {}", if dps_seen.is_empty() { "none".into() } else { dps_seen.into_iter().collect::<Vec<_>>().join(" ") });
    println!(
        "local video:     {}",
        if v35 {
            "possible (3.5 LAN signalling)"
        } else if lan_302_seen {
            "likely: the monitor carries LAN video signalling (cmd 32) on this 3.3 session; needs a test with the P2P password"
        } else {
            "unknown on 3.3; open the live view in the app during the probe to see whether it signals over the LAN"
        }
    );
    Ok(())
}
