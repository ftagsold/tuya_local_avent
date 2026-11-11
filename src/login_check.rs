//! `tuya_local_avent login-check`: log in to the Tuya Smart web API, list the
//! account's devices, and show the shape of the monitor's stream config with
//! every value redacted. Answers whether that config carries the P2P password
//! the local video login needs.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::tuya::smart::{self, SmartClient};

pub struct Args {
    pub email: String,
    pub password: String,
    pub region: String,
    pub device_id: Option<String>,
    pub local_key: Option<String>,
}

pub async fn run(args: Args) -> Result<()> {
    let region = smart::region(&args.region).with_context(|| {
        let names: Vec<_> = smart::REGIONS.iter().map(|r| r.name).collect();
        format!("unknown region {:?}; one of {}", args.region, names.join(", "))
    })?;
    let client = SmartClient::new(region)?;

    println!("Logging in at {}...", region.host);
    let login = client.login(&args.email, &args.password).await?;
    println!("  OK, uid {}", login["uid"].as_str().unwrap_or("?"));

    let devices = client.devices().await?;
    println!("\nDevices on the account:");
    for d in &devices {
        println!(
            "  {}  {:<24} category={} product={}",
            d["deviceId"].as_str().unwrap_or("?"),
            d["deviceName"].as_str().unwrap_or("?"),
            d["category"].as_str().unwrap_or("?"),
            d["productId"].as_str().unwrap_or("?"),
        );
    }

    let device_id = match args.device_id {
        Some(id) => id,
        None => match devices.iter().find(|d| d["productId"] == "kzm54lhabeeucq5a").or(devices.first()) {
            Some(d) => d["deviceId"].as_str().unwrap_or_default().to_string(),
            None => return Ok(()),
        },
    };

    println!("\nStream config for {device_id} (values redacted):");
    let config = client.stream_config(&device_id).await?;
    print_shape(&config, 1);

    println!("\n--- summary ---");
    let key = config["localKey"].as_str();
    println!(
        "localKey:      {}",
        match (key, args.local_key.as_deref()) {
            (None, _) => "absent".to_string(),
            (Some(k), Some(known)) if k == known => "present, matches AVENT_LOCAL_KEY".to_string(),
            (Some(_), Some(_)) => "present, DIFFERS from AVENT_LOCAL_KEY (re-paired?)".to_string(),
            (Some(_), None) => "present".to_string(),
        }
    );
    let password_paths = find_keys(&config, "", &|k| k.to_ascii_lowercase().contains("pass") || k.to_ascii_lowercase().contains("pwd"));
    println!(
        "P2P password:  {}",
        if password_paths.is_empty() { "not in this config".to_string() } else { format!("candidate field(s): {}", password_paths.join(", ")) }
    );
    println!("protocol:      {}", config["protocolVersion"].as_str().unwrap_or("?"));
    Ok(())
}

/// Print a JSON value's structure: keys, types and lengths, never the values.
fn print_shape(v: &Value, depth: usize) {
    let pad = "  ".repeat(depth);
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                match child {
                    Value::Object(_) | Value::Array(_) => {
                        println!("{pad}{k}: {}", kind(child));
                        print_shape(child, depth + 1);
                    }
                    _ => println!("{pad}{k}: {}", kind(child)),
                }
            }
        }
        Value::Array(items) => {
            if let Some(first) = items.first() {
                println!("{pad}[0]: {}", kind(first));
                print_shape(first, depth + 1);
            }
        }
        _ => {}
    }
}

fn kind(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(_) => "bool".into(),
        Value::Number(_) => "number".into(),
        Value::String(s) => format!("string ({} chars)", s.len()),
        Value::Array(a) => format!("array ({} items)", a.len()),
        Value::Object(o) => format!("object ({} keys)", o.len()),
    }
}

fn find_keys(v: &Value, path: &str, pred: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                if pred(k) {
                    out.push(p.clone());
                }
                out.extend(find_keys(child, &p, pred));
            }
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                out.extend(find_keys(child, &format!("{path}[{i}]"), pred));
            }
        }
        _ => {}
    }
    out
}
