//! `tuya_local_avent fetch-password`: the one-time signed login that mints the P2P
//! password for local video. Prompts for the email MFA code, fetches the
//! per-device RTC config, and reports whether the P2P password is present.
//! The password is tied to the pairing, so this is needed once, then cached.

use std::io::{self, Write};

use anyhow::{bail, Context, Result};

use crate::tuya::mobile::{AppKeys, MobileClient};

pub struct Args {
    pub email: String,
    pub password: String,
    pub region: String,
    pub country_code: String,
    pub device_id: String,
    pub local_key: Option<String>,
    pub app_key: String,
    pub signing_key: String,
    pub ch_key: String,
}

pub async fn run(args: Args) -> Result<()> {
    let keys = AppKeys {
        signing_key: args.signing_key,
        app_key: args.app_key,
        ch_key: args.ch_key,
    };
    let mut client = MobileClient::new(keys, &args.region, &args.country_code)?;

    println!("Logging in to the Tuya Mobile API ({})...", crate::tuya::mobile::api_url(&args.region));
    let login = client
        .interactive_login(&args.email, &args.password, prompt_mfa)
        .await
        .context("mobile login")?;
    println!("  OK, uid {}", login["uid"].as_str().unwrap_or("?"));

    println!("\nFetching the RTC config for {}...", args.device_id);
    let rtc = client.rtc_config(&args.device_id).await.context("rtc config")?;

    let password = rtc["password"].as_str().unwrap_or("");
    let cloud_key = rtc["localKey"].as_str();

    println!("\n--- summary ---");
    if password.is_empty() {
        println!("P2P password:  NOT present (field 'password' empty). Other keys: {}", keys_of(&rtc));
    } else {
        println!("P2P password:  present ({} chars)", password.len());
        println!("media login:   md5(password || localKey) is computable");
    }
    match (cloud_key, args.local_key.as_deref()) {
        (Some(k), Some(known)) if k == known => println!("localKey:      matches AVENT_LOCAL_KEY"),
        (Some(_), Some(_)) => println!("localKey:      DIFFERS from AVENT_LOCAL_KEY (re-paired?)"),
        (Some(_), None) => println!("localKey:      present in RTC config"),
        (None, _) => println!("localKey:      absent from RTC config"),
    }
    println!("protocol:      {}", rtc["protocolVersion"].as_str().unwrap_or("?"));

    if password.is_empty() {
        bail!("no P2P password in the RTC config; cannot build the local media login yet");
    }

    // Discover the monitor's LAN address + protocol, then write the Go bridge
    // config with the password (value never printed).
    let data = std::env::var("AVENT_DATA").unwrap_or_else(|_| "./data".into());
    let seen = crate::discover::scan(std::time::Duration::from_secs(8), Some(&args.device_id)).await;
    let (lan_ip, protocol) = match seen.get(&args.device_id) {
        Some(a) => (a.ip.clone(), a.version.clone()),
        None => (String::new(), "3.3".to_string()),
    };
    let local_key = args.local_key.unwrap_or_default();
    let cfg = crate::bridge_json::BridgeConfig {
        uid: login["uid"].as_str().unwrap_or_default().to_string(),
        lan_streaming: true,
        bridge_port: 38554,
        rtsp_bind_address: std::env::var("AVENT_RTSP_BIND").unwrap_or_else(|_| "localhost".into()),
        rtsp_user: std::env::var("RTSP_USER").unwrap_or_default(),
        rtsp_pass: std::env::var("RTSP_PASS").unwrap_or_default(),
        talkback: false,
        cameras: vec![crate::bridge_json::Camera {
            camera_id: args.device_id.clone(),
            camera_name: std::env::var("AVENT_CAMERA_NAME").unwrap_or_else(|_| "Monitor".into()),
            product_id: rtc["productId"].as_str().unwrap_or_default().to_string(),
            local_key,
            p2p_password: password.to_string(),
            lan_ip,
            lan_protocol: if protocol.is_empty() { "3.3".into() } else { protocol },
        }],
    };
    let path = std::path::Path::new(&data).join("bridge.json");
    cfg.write(&path)?;
    println!("wrote bridge config: {} (0600)", path.display());
    Ok(())
}

fn prompt_mfa() -> Result<String> {
    print!("\nEnter the 6-digit code sent to your email: ");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin().read_line(&mut line).context("reading MFA code")?;
    Ok(line.trim().to_string())
}

fn keys_of(v: &serde_json::Value) -> String {
    v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>().join(", ")).unwrap_or_default()
}
