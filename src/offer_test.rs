//! `tuya_local_avent offer-test`: does the monitor answer a LAN video offer on a
//! 3.3 session? Sends a receive-only imm offer over command 32 and reports
//! the acknowledgement, answer, candidates or refusal. Needs no P2P password:
//! that is only used for the media login after an answer.

use std::time::Duration;

use anyhow::Result;
use tokio::time::Instant;

use crate::discover;
use crate::tuya::lan::frame;
use crate::tuya::lan::session::{payload_json, prefix_name, Session, Version};
use crate::tuya::lan::signal::Offer;

pub struct Args {
    pub device_id: String,
    pub local_key: String,
    pub uid: String,
    pub ip: Option<String>,
}

pub async fn run(args: Args) -> Result<()> {
    let ip = match args.ip {
        Some(ip) => ip,
        None => {
            println!("Finding the monitor...");
            let seen = discover::scan(Duration::from_secs(10), Some(&args.device_id)).await;
            match seen.get(&args.device_id) {
                Some(a) => a.ip.clone(),
                None => anyhow::bail!("monitor {} not heard; pass --ip", args.device_id),
            }
        }
    };

    for version_header in [true, false] {
        println!(
            "\nAttempt: offer over a 3.3 session at {ip}, {} version header",
            if version_header { "with" } else { "without" }
        );
        let mut s = Session::connect(&ip, &args.local_key, Version::V33).await?;
        // The monitor answers only on a primed session (fork PROTOCOL.md §16).
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs().to_string();
        let q = serde_json::json!({ "gwId": args.device_id, "devId": args.device_id, "uid": args.device_id, "t": t });
        s.send(frame::CMD_DP_QUERY, q.to_string().as_bytes()).await?;

        let offer = Offer::new(&args.uid, &args.device_id);
        s.send_33(frame::CMD_IPC_LAN_302, offer.offer_frame().to_string().as_bytes(), version_header)
            .await?;
        println!("  sent offer, session {}", offer.session_id);

        let answered = watch(&mut s, &offer.session_id, Duration::from_secs(10)).await?;
        drop(s); // closing the 6668 session ends the LAN attempt
        if answered {
            println!("\nRESULT: the monitor answered a LAN offer on 3.3. Fully local video is viable.");
            return Ok(());
        }
    }
    println!("\nRESULT: no answer to a LAN offer on 3.3 in either form.");
    Ok(())
}

/// Print every frame for `wait`; true once an answer for our session arrives.
async fn watch(s: &mut Session, session_id: &str, wait: Duration) -> Result<bool> {
    let start = Instant::now();
    let mut answered = false;
    while start.elapsed() < wait {
        let left = wait.saturating_sub(start.elapsed());
        let Some((h, raw)) = s.recv_raw(left.max(Duration::from_millis(10))).await? else { break };
        let tag = format!("  +{:>4}ms {} cmd {:>2} len {:>4}", start.elapsed().as_millis(), prefix_name(h.prefix), h.cmd, h.length);
        let plain = match s.decode(&h, &raw) {
            Ok(p) => p,
            Err(e) => {
                println!("{tag}: cannot decrypt ({e})");
                continue;
            }
        };
        let Some(json) = payload_json(&plain) else {
            println!("{tag}: {:?}", String::from_utf8_lossy(&plain));
            continue;
        };
        if h.cmd != frame::CMD_IPC_LAN_302 {
            println!("{tag}: {json}");
            continue;
        }
        let kind = json["header"]["type"].as_str().unwrap_or("?");
        let ours = json["header"]["sessionid"].as_str() == Some(session_id);
        print!("{tag}: 302 {kind}{}", if ours { " (ours)" } else { " (other session)" });
        match kind {
            "answer" => {
                let sdp = json["msg"]["sdp"].as_str().unwrap_or("");
                let has_key = sdp.lines().any(|l| l.starts_with("a=aes-key:"));
                let candidates = sdp.lines().filter(|l| l.starts_with("a=candidate")).count();
                println!(" sdp: {} lines, aes-key {}, {} candidate(s)", sdp.lines().count(), if has_key { "present" } else { "absent" }, candidates);
                answered |= ours;
            }
            "candidate" => println!(" {}", json["msg"]["candidate"].as_str().unwrap_or("").trim()),
            "disconnect" => println!(" close_reason={} local={}", json["msg"]["close_reason"], json["msg"]["close_reason_local"]),
            _ => println!(),
        }
        if answered {
            // Give the trickled candidates a moment, then stop.
            let _ = tokio::time::timeout(Duration::from_millis(1500), async {
                while let Ok(Some((h, raw))) = s.recv_raw(Duration::from_millis(1500)).await {
                    if let Ok(p) = s.decode(&h, &raw) {
                        if let Some(j) = payload_json(&p) {
                            if j["header"]["type"] == "candidate" {
                                println!("  302 candidate {}", j["msg"]["candidate"].as_str().unwrap_or("").trim());
                            }
                        }
                    }
                }
            })
            .await;
            break;
        }
    }
    Ok(answered)
}
