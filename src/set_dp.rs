//! `tuya_local_avent set-dp`: exercise the Home Assistant control-write path.
//! Connects the local control session, writes one data point (the same
//! CONTROL(7) write the MQTT bridge issues), and prints the monitor's STATUS
//! pushes so you can confirm the change took. Writes a real value to the
//! monitor — use a reversible DP (e.g. the night light 138).

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{bail, Result};
use serde_json::{json, Value};
use tokio::time::Instant;

use crate::discover;
use crate::lan_control::{self, Config};
use crate::tuya::lan::session::Version;

pub struct Args {
    pub device_id: String,
    pub local_key: String,
    pub ip: Option<String>,
    pub dp: u32,
    pub value: String,
    pub listen_secs: u64,
}

pub async fn run(args: Args) -> Result<()> {
    let (ip, version) = match args.ip {
        Some(ip) => (ip, Version::V33),
        None => {
            let seen = discover::scan(Duration::from_secs(8), Some(&args.device_id)).await;
            match seen.get(&args.device_id) {
                Some(a) => (a.ip.clone(), Version::parse(&a.version).unwrap_or(Version::V33)),
                None => bail!("monitor {} not found on the LAN; pass --ip", args.device_id),
            }
        }
    };

    let control = lan_control::spawn(Config {
        device_id: args.device_id.clone(),
        local_key: args.local_key.clone(),
        ip: tokio::sync::watch::channel(ip).1,
        version,
    });
    let mut updates = control.subscribe();
    tokio::time::sleep(Duration::from_secs(2)).await; // let the session come up

    let value = parse_value(&args.value);
    println!("Writing DP {} = {}", args.dp, value);
    let mut dps = BTreeMap::new();
    dps.insert(args.dp.to_string(), value.clone());
    if value.as_bool() == Some(true) {
        if let Some(p) = crate::dp::exclusive_partner(args.dp) {
            dps.insert(p.to_string(), json!(false));
        }
    }
    control.write(dps).await?;

    println!("Listening {}s for the monitor's STATUS pushes...", args.listen_secs);
    let start = Instant::now();
    let mut echoed = false;
    while start.elapsed() < Duration::from_secs(args.listen_secs) {
        match tokio::time::timeout(Duration::from_secs(1), updates.recv()).await {
            Ok(Ok(u)) => {
                let mark = if u.id == args.dp { echoed = true; " <-- our DP" } else { "" };
                println!("  +{:>3}s dp {} = {}{}", start.elapsed().as_secs(), u.id, u.value, mark);
            }
            _ => {}
        }
    }
    println!("\nresult: {}", if echoed { "monitor echoed the DP — control-write WORKS" } else { "no echo of that DP (it may push only on real change)" });
    Ok(())
}

fn parse_value(s: &str) -> Value {
    match s {
        "true" => json!(true),
        "false" => json!(false),
        _ => match s.parse::<i64>() {
            Ok(n) => json!(n),
            Err(_) => json!(s),
        },
    }
}
