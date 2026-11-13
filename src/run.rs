//! `tuya_local_avent run`: the single-container entrypoint.
//!
//! Video: the Go bridge pulls the monitor's stream over the LAN and serves it
//! on loopback only (random credentials); ffmpeg burns a live
//! `yyyy-mm-dd hh:mm:ss | <device name>` overlay, re-encodes to H.264 + AAC and
//! publishes into go2rtc, the one RTSP server reachable from the LAN, which
//! requires `RTSP_USER`/`RTSP_PASS` off loopback. A gate (rtsp_gate.rs) on
//! `RTSP_PORT` lets viewers through only while video flows. ONVIF points UniFi Protect
//! and Home Assistant at that stream; its URI also carries the talkback
//! backchannel (go2rtc → Go bridge → monitor speaker, all LAN).
//!
//! P2P password: taken from `AVENT_P2P_PASSWORD` (or the cache of the last
//! cloud fetch) so start-up needs no cloud login. Only when the monitor's
//! stream does not come up with it does the bridge log in and fetch the
//! current one from the cloud.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::bridge_json::{BridgeConfig, Camera};
use crate::lan_control;
use crate::media::{self, Spec};
use crate::tuya::lan::session::Version;
use crate::tuya::mobile::{AppKeys, MobileClient};

/// The Go bridge's loopback RTSP port.
const BRIDGE_PORT: u16 = 38554;
/// go2rtc's RTSP port behind the gate (rtsp_gate.rs); `RTSP_PORT` is the gate.
const GO2RTC_PORT: u16 = 18554;

pub struct Env {
    pub email: String,
    pub password: String,
    pub region: String,
    pub country_code: String,
    pub device_id: String,
    pub local_key: String,
    pub p2p_password: String,
    pub camera_name: String,
    pub lan_ip: String,
    pub app_key: String,
    pub signing_key: String,
    pub ch_key: String,
    pub data_dir: String,
    pub bridge_bin: String,
    pub go2rtc_bin: String,
    pub rtsp_port: u16,
    pub mqtt_host: String,
    pub mqtt_port: u16,
    pub mqtt_user: String,
    pub mqtt_pass: String,
    pub onvif_user: String,
    pub onvif_pass: String,
    pub onvif_port: u16,
    pub onvif_host_ip: String,
    pub onvif_model: String,
    pub onvif_manufacturer: String,
    pub onvif_mac: String,
    pub rtsp_user: String,
    pub rtsp_pass: String,
    pub overlay_font: String,
    pub ffmpeg: String,
    /// `auto` (VAAPI when it works, else libx264), `vaapi` or `off`.
    pub hwaccel: String,
    pub vaapi_device: String,
}

impl Env {
    pub fn from_env() -> Result<Self> {
        let get = |k: &str| std::env::var(k).unwrap_or_default();
        let or = |k: &str, d: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| d.into());
        let req = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty()).ok_or_else(|| anyhow::anyhow!("{k} is required"));
        let port = |k: &str, d: u16| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        let onvif_user = or("ONVIF_USER", "avent");
        let onvif_pass = req("ONVIF_PASS")?;
        let mut env = Self {
            email: get("AVENT_EMAIL"),
            password: get("AVENT_PASSWORD"),
            region: or("AVENT_REGION", "eu"),
            country_code: or("AVENT_COUNTRY_CODE", "49"),
            device_id: req("AVENT_DEVICE_ID")?,
            local_key: req("AVENT_LOCAL_KEY")?,
            p2p_password: get("AVENT_P2P_PASSWORD"),
            camera_name: get("AVENT_CAMERA_NAME"),
            lan_ip: get("AVENT_LAN_IP"),
            app_key: get("AVENT_APP_KEY"),
            signing_key: get("AVENT_SIGNING_KEY"),
            ch_key: get("AVENT_CH_KEY"),
            data_dir: or("AVENT_DATA", "/data"),
            bridge_bin: or("AVENT_BRIDGE_BIN", "/app/avent-webrtc-bridge"),
            go2rtc_bin: or("AVENT_GO2RTC_BIN", "/app/go2rtc"),
            rtsp_port: port("RTSP_PORT", 8554),
            mqtt_host: get("MQTT_HOST"),
            mqtt_port: port("MQTT_PORT", 1883),
            mqtt_user: get("MQTT_USER"),
            mqtt_pass: get("MQTT_PASS"),
            // The stream reuses the ONVIF credential unless set separately, so
            // Protect's one adoption credential opens both.
            rtsp_user: or("RTSP_USER", &onvif_user),
            rtsp_pass: or("RTSP_PASS", &onvif_pass),
            onvif_user,
            onvif_pass,
            onvif_port: port("ONVIF_PORT", 8000),
            onvif_host_ip: get("ONVIF_BIND_IP"),
            onvif_model: get("ONVIF_MODEL"),
            onvif_manufacturer: get("ONVIF_MANUFACTURER"),
            onvif_mac: get("ONVIF_MAC"),
            overlay_font: or("AVENT_OVERLAY_FONT", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"),
            ffmpeg: or("AVENT_FFMPEG", "ffmpeg"),
            hwaccel: or("AVENT_HWACCEL", "auto").to_ascii_lowercase(),
            vaapi_device: or("AVENT_VAAPI_DEVICE", "/dev/dri/renderD128"),
        };
        if !matches!(env.hwaccel.as_str(), "auto" | "vaapi" | "off") {
            bail!("AVENT_HWACCEL must be auto, vaapi or off");
        }
        // A guessable stream password is an open stream of a child's room.
        for (k, v) in [("ONVIF_PASS", &env.onvif_pass), ("RTSP_PASS", &env.rtsp_pass)] {
            if v.len() < 8 || v.eq_ignore_ascii_case("changeme") {
                bail!("{k} is too weak: use at least 8 characters and not the placeholder");
            }
        }
        env.onvif_mac = if env.onvif_mac.is_empty() {
            crate::onvif::device_mac(&env.device_id)
        } else {
            crate::onvif::parse_mac(&env.onvif_mac).ok_or_else(|| anyhow::anyhow!("ONVIF_MAC must be a unicast MAC like 02:11:22:33:44:55"))?
        };
        if env.p2p_password.is_empty() && !env.has_cloud() {
            bail!("set AVENT_P2P_PASSWORD, or AVENT_EMAIL/AVENT_PASSWORD/AVENT_APP_KEY/AVENT_SIGNING_KEY/AVENT_CH_KEY to fetch it");
        }
        Ok(env)
    }

    fn has_cloud(&self) -> bool {
        ![&self.email, &self.password, &self.app_key, &self.signing_key, &self.ch_key].iter().any(|v| v.is_empty())
    }
}

/// What the last cloud fetch learned, kept in `<data>/cloud.json` (0600).
#[derive(Default, Serialize, Deserialize)]
struct Cache {
    #[serde(default)]
    p2p_password: String,
    #[serde(default)]
    device_name: String,
    #[serde(default)]
    uid: String,
    /// An `AVENT_P2P_PASSWORD` the monitor rejected; while the env still holds
    /// it, the cached `p2p_password` wins.
    #[serde(default)]
    rejected_env_password: String,
}

impl Cache {
    fn load(path: &std::path::Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn save(&self, path: &std::path::Path) -> Result<()> {
        media::write_private(path, &serde_json::to_vec_pretty(self)?)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    Env,
    Cache,
    Cloud,
}

/// Log in and fetch the current P2P password (and the device name) into the cache.
async fn cloud_fetch(env: &Env, cache: &mut Cache, cache_path: &std::path::Path) -> Result<String> {
    if !env.has_cloud() {
        bail!("cloud credentials (AVENT_EMAIL/AVENT_PASSWORD/AVENT_APP_KEY/AVENT_SIGNING_KEY/AVENT_CH_KEY) not set");
    }
    let mut client = MobileClient::new(
        AppKeys { signing_key: env.signing_key.clone(), app_key: env.app_key.clone(), ch_key: env.ch_key.clone() },
        &env.region,
        &env.country_code,
    )?;
    tracing::info!("logging in to the Tuya cloud to fetch the P2P password");
    let login = client
        .interactive_login(&env.email, &env.password, || bail!("MFA code required but run is non-interactive; use fetch-password once"))
        .await
        .context("login")?;
    let rtc = client.rtc_config(&env.device_id).await.context("rtc config")?;
    let password = rtc["password"].as_str().unwrap_or_default().to_string();
    if password.is_empty() {
        bail!("no P2P password returned");
    }
    match client.device_name(&env.device_id).await {
        Ok(n) if !n.is_empty() => cache.device_name = n,
        Ok(_) => {}
        Err(e) => tracing::warn!("device name lookup failed: {e:#}"),
    }
    cache.uid = login["uid"].as_str().unwrap_or_default().to_string();
    cache.p2p_password = password.clone();
    cache.save(cache_path)?;
    Ok(password)
}

pub async fn run(env: Env) -> Result<()> {
    let cache_path = media::data_file(&env.data_dir, "cloud.json");
    let mut cache = Cache::load(&cache_path);

    crate::onvif::check_mac(&env.onvif_mac);

    // 1. P2P password: env, else the last cloud fetch, else the cloud now.
    let (mut p2p, mut source) = if !env.p2p_password.is_empty() {
        if env.p2p_password == cache.rejected_env_password && !cache.p2p_password.is_empty() {
            (cache.p2p_password.clone(), Source::Cache)
        } else {
            (env.p2p_password.clone(), Source::Env)
        }
    } else if !cache.p2p_password.is_empty() {
        (cache.p2p_password.clone(), Source::Cache)
    } else {
        (cloud_fetch(&env, &mut cache, &cache_path).await?, Source::Cloud)
    };
    tracing::info!("P2P password from {source:?}");

    // Device name: env, else the app's name from the last cloud fetch.
    let device_name = [&env.camera_name, &cache.device_name]
        .into_iter()
        .find(|n| !n.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| "Monitor".into());
    let path = media::slug(&device_name, &env.device_id);

    // 2. The monitor's LAN address and protocol. Keep looking until found:
    // a monitor that is off at boot must not crash-loop the container.
    let (mut lan_ip, proto_str) = loop {
        match locate(&env).await {
            Some(found) => break found,
            None => {
                tracing::warn!("monitor {} not found on the LAN; retrying in 30s (set AVENT_LAN_IP to skip discovery)", env.device_id);
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    };
    let version = Version::parse(&proto_str).unwrap_or(Version::V33);
    tracing::info!("monitor at {lan_ip} (protocol {proto_str}), device name {device_name:?}, stream path /{path}");

    // 3. Loopback-only credentials for the internal hops, fresh every start.
    let bridge_user = "bridge".to_string();
    let bridge_pass = media::random_secret();
    let bridge_url = format!("rtsp://{}127.0.0.1:{BRIDGE_PORT}/{path}", media::userinfo(&bridge_user, &bridge_pass));
    // go2rtc sits behind the RTSP gate on an internal port. The gate dials
    // it on the LAN address so go2rtc's auth still applies; without one,
    // go2rtc serves RTSP_PORT itself, ungated.
    let gate_ip = local_ip().and_then(|ip| ip.parse::<std::net::IpAddr>().ok()).filter(|ip| !ip.is_loopback());
    let g2r_port = if gate_ip.is_some() { GO2RTC_PORT } else { env.rtsp_port };
    if gate_ip.is_none() {
        tracing::warn!("no LAN address found: go2rtc serves port {} directly, without the RTSP gate", env.rtsp_port);
    }
    // go2rtc skips auth for loopback, so the relay publishes without one.
    let publish_url = format!("rtsp://127.0.0.1:{g2r_port}/{path}");
    let local_read_url = format!("rtsp://{}127.0.0.1:{g2r_port}/{path}", media::userinfo(&env.rtsp_user, &env.rtsp_pass));

    // 4. Go bridge: loopback bind, internal credentials. Restarted (via the
    // watch) when the P2P password changes.
    let bridge_cfg_path = media::data_file(&env.data_dir, "bridge.json");
    let uid = if cache.uid.is_empty() { env.device_id.clone() } else { cache.uid.clone() };
    let write_bridge_cfg = |p2p: &str, lan_ip: &str| -> Result<()> {
        BridgeConfig {
            uid: uid.clone(),
            lan_streaming: true,
            bridge_port: BRIDGE_PORT,
            rtsp_bind_address: "localhost".into(),
            rtsp_user: bridge_user.clone(),
            rtsp_pass: bridge_pass.clone(),
            talkback: true,
            cameras: vec![Camera {
                camera_id: env.device_id.clone(),
                camera_name: path.clone(),
                product_id: String::new(),
                local_key: env.local_key.clone(),
                p2p_password: p2p.to_string(),
                lan_ip: lan_ip.to_string(),
                lan_protocol: proto_str.clone(),
            }],
        }
        .write(&bridge_cfg_path)
    };
    write_bridge_cfg(&p2p, &lan_ip)?;
    let bridge_spec = || Spec {
        name: "video bridge",
        program: env.bridge_bin.clone(),
        // on_demand: the LAN stream stays eligible; talk opens only while a
        // viewer sends backchannel audio (bridge/pkg/rtsp/lan_talk.go).
        args: ["addon", "--config", &bridge_cfg_path.to_string_lossy(), "--bind", "localhost", "--talkback-mode", "on_demand"]
            .map(String::from)
            .to_vec(),
        watchdog: None,
    };
    let (bridge_tx, bridge_rx) = watch::channel(bridge_spec());
    media::supervise(bridge_rx);

    // 5. go2rtc: the only LAN-facing RTSP server.
    let g2r_cfg_path = media::data_file(&env.data_dir, "go2rtc.yaml");
    media::Go2rtc {
        port: g2r_port,
        path: path.clone(),
        user: env.rtsp_user.clone(),
        pass: env.rtsp_pass.clone(),
        talk_source: bridge_url.clone(),
    }
    .write(&g2r_cfg_path)?;
    let (_g2r_tx, g2r_rx) = watch::channel(Spec {
        name: "go2rtc",
        program: env.go2rtc_bin.clone(),
        args: vec!["-config".into(), g2r_cfg_path.to_string_lossy().into_owned()],
        watchdog: None,
    });
    media::supervise(g2r_rx);

    // 6. ffmpeg relay: overlay + H.264/AAC, bridge → go2rtc (started in 9).
    let overlay_path = media::data_file(&env.data_dir, "overlay.txt");
    media::write_private(&overlay_path, media::overlay_text(&device_name).as_bytes())?;
    // Temperature, top right: the last reading survives a restart (the
    // monitor only reports changes); a space until the first one.
    let temperature_path = media::data_file(&env.data_dir, "temperature.txt");
    if !temperature_path.exists() {
        media::write_overlay_file(&temperature_path, " ")?;
    }
    let drawtext = media::drawtext_supported(&env.ffmpeg, &env.overlay_font);
    if !drawtext {
        tracing::error!(
            "ffmpeg lacks the drawtext filter or the font {} is missing: streaming WITHOUT the overlay",
            env.overlay_font
        );
    }
    let encoder = if env.hwaccel != "off" && media::vaapi_supported(&env.ffmpeg, &env.vaapi_device) {
        media::Encoder::Vaapi(env.vaapi_device.clone())
    } else {
        media::Encoder::X264
    };
    match (&encoder, env.hwaccel.as_str()) {
        (media::Encoder::Vaapi(d), _) => tracing::info!("relay encodes H.264 on the GPU (VAAPI {d})"),
        (_, "vaapi") => tracing::error!("AVENT_HWACCEL=vaapi but VAAPI does not work: encoding with libx264"),
        _ => tracing::info!("relay encodes H.264 with libx264"),
    }
    let relay_health = std::sync::Arc::new(media::Health::default());
    if let Some(ip) = gate_ip {
        let health = relay_health.clone();
        let listen = std::net::SocketAddr::from(([0, 0, 0, 0], env.rtsp_port));
        let upstream = std::net::SocketAddr::new(ip, GO2RTC_PORT);
        // Kept alive like ONVIF: a failed bind retries.
        tokio::spawn(async move {
            loop {
                if let Err(e) = crate::rtsp_gate::run(listen, upstream, health.clone()).await {
                    tracing::error!("RTSP gate ended: {e:#}; restarting in 5s");
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }
    let relay_spec = Spec {
        name: "ffmpeg relay",
        program: env.ffmpeg.clone(),
        args: media::relay_args(
            &bridge_url,
            &publish_url,
            drawtext.then_some(media::Overlay {
                font: &env.overlay_font,
                clock: &overlay_path,
                temperature: &temperature_path,
            }),
            &encoder,
        ),
        // ffmpeg's own -timeout catches a dead input; this catches the rest
        // (input alive but no decodable frames, or a blocked output).
        watchdog: Some((relay_health.clone(), RELAY_STALL)),
    };

    // 7. Local control → Home Assistant (MQTT).
    let (control_ip_tx, control_ip_rx) = watch::channel(lan_ip.clone());
    let control = lan_control::spawn(lan_control::Config {
        device_id: env.device_id.clone(),
        local_key: env.local_key.clone(),
        ip: control_ip_rx,
        version,
    });
    // Temperature pushes (DP 207) → the overlay file.
    let mut temps = control.subscribe();
    let temp_file = temperature_path.clone();
    tokio::spawn(async move {
        loop {
            match temps.recv().await {
                Ok(u) if u.id == crate::dp::TEMPERATURE => {
                    if let Some(raw) = u.value.as_f64() {
                        if let Err(e) = media::write_overlay_file(&temp_file, &media::temperature_text(raw)) {
                            tracing::warn!("temperature overlay: {e:#}");
                        }
                    }
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    if !env.mqtt_host.is_empty() {
        let mcfg = crate::ha::mqtt::Config {
            host: env.mqtt_host.clone(),
            port: env.mqtt_port,
            user: env.mqtt_user.clone(),
            pass: env.mqtt_pass.clone(),
            device_id: env.device_id.clone(),
            device_name: device_name.clone(),
            mac: env.onvif_mac.clone(),
        };
        let c = control.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = crate::ha::mqtt::run(mcfg.clone(), c.clone()).await {
                    tracing::error!("mqtt bridge ended: {e:#}; restarting in 5s");
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    } else {
        tracing::info!("MQTT_HOST unset; Home Assistant control disabled");
    }

    // 8. ONVIF for UniFi Protect / Home Assistant.
    let host_ip = if env.onvif_host_ip.is_empty() { local_ip().unwrap_or_else(|| "127.0.0.1".into()) } else { env.onvif_host_ip.clone() };
    let ocfg = crate::onvif::Config {
        stream_uri: format!("rtsp://{}{host_ip}:{}/{path}", media::userinfo(&env.rtsp_user, &env.rtsp_pass), env.rtsp_port),
        stream_uri_plain: format!("rtsp://{host_ip}:{}/{path}", env.rtsp_port),
        snapshot_src: local_read_url,
        host_ip,
        onvif_port: env.onvif_port,
        user: env.onvif_user.clone(),
        pass: env.onvif_pass.clone(),
        // Protect names an adopted camera "<manufacturer> <model>": default
        // to the camera name alone (empty manufacturer).
        model: if env.onvif_model.is_empty() { device_name.clone() } else { env.onvif_model.clone() },
        manufacturer: env.onvif_manufacturer.clone(),
        mac: env.onvif_mac.clone(),
        device_name: device_name.clone(),
        device_id: env.device_id.clone(),
    };
    // Kept alive: a failed bind (port briefly taken) or a crash retries.
    tokio::spawn(async move {
        loop {
            if let Err(e) = crate::onvif::run(ocfg.clone()).await {
                tracing::error!("onvif service ended: {e:#}; restarting in 5s");
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });

    // 9. Verify the P2P password by the stream coming up; fetch it from the
    // cloud when it does not. Then keep watching the relay and escalate:
    // relay restart (its own watchdog) → bridge restart with a fresh LAN
    // lookup → P2P password refresh from the cloud.
    let refresh_p2p = |cache: &mut Cache, source: &mut Source, p2p: &mut String, fresh: String| -> bool {
        *source = Source::Cloud;
        if !env.p2p_password.is_empty() && fresh != env.p2p_password {
            cache.rejected_env_password = env.p2p_password.clone();
            cache.save(&cache_path).ok();
            tracing::warn!("AVENT_P2P_PASSWORD is stale; using the cloud's (cached in {}). Update the env", cache_path.display());
        }
        if fresh == *p2p {
            return false;
        }
        *p2p = fresh;
        true
    };
    let verify = async {
        loop {
            // Probe for a full 90 s window: a probe that fails fast (bridge
            // still starting, port briefly taken) is not evidence against
            // the password.
            let deadline = std::time::Instant::now() + VERIFY_WINDOW;
            let mut up = false;
            while std::time::Instant::now() < deadline {
                if media::probe_video(&env.ffmpeg, &bridge_url, Duration::from_secs(30)).await {
                    up = true;
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            if up {
                tracing::info!("video stream up (P2P password from {source:?})");
                return;
            }
            if source == Source::Cloud || !env.has_cloud() {
                tracing::warn!("no video from the monitor (offline, or wrong P2P password); retrying in 30s");
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
            tracing::warn!("no video with the P2P password from {source:?}; fetching the current one from the cloud");
            match cloud_fetch(&env, &mut cache, &cache_path).await {
                Ok(fresh) => {
                    if refresh_p2p(&mut cache, &mut source, &mut p2p, fresh) {
                        if let Err(e) = write_bridge_cfg(&p2p, &lan_ip) {
                            tracing::error!("rewrite bridge config: {e:#}");
                        }
                        bridge_tx.send_replace(bridge_spec());
                    }
                }
                Err(e) => {
                    tracing::error!("cloud fetch of the P2P password failed: {e:#}; retrying in 60s");
                    tokio::time::sleep(Duration::from_secs(60)).await;
                }
            }
        }
    };
    tokio::select! {
        _ = verify => {}
        r = shutdown() => return r,
    }
    // The relay starts once the bridge delivers video, so it neither races
    // the bridge's listener nor loops while the password is being sorted out.
    relay_health.touch();
    let (_relay_tx, relay_rx) = watch::channel(relay_spec);
    media::supervise(relay_rx);

    let watch_video = async {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        let mut bridge_restarts = 0u32;
        let mut last_bridge_restart = std::time::Instant::now();
        loop {
            tick.tick().await;
            let age = relay_health.age();
            if age < BRIDGE_STALL {
                if bridge_restarts > 0 {
                    tracing::info!("video recovered");
                    bridge_restarts = 0;
                }
                continue;
            }
            if last_bridge_restart.elapsed() < BRIDGE_STALL {
                continue; // give the last restart time to take effect
            }
            bridge_restarts += 1;
            last_bridge_restart = std::time::Instant::now();
            // Every third failed bridge restart, also refresh the password
            // (the monitor may have been re-paired).
            if bridge_restarts.is_multiple_of(3) && env.has_cloud() {
                tracing::warn!("video still down; refreshing the P2P password from the cloud");
                match cloud_fetch(&env, &mut cache, &cache_path).await {
                    Ok(fresh) => {
                        refresh_p2p(&mut cache, &mut source, &mut p2p, fresh);
                    }
                    Err(e) => tracing::error!("cloud fetch of the P2P password failed: {e:#}"),
                }
            }
            if let Some((ip, _)) = locate(&env).await {
                if ip != lan_ip {
                    tracing::warn!("monitor moved {lan_ip} -> {ip}");
                    lan_ip = ip;
                    control_ip_tx.send_replace(lan_ip.clone());
                }
            }
            tracing::warn!("no video for {}s; restarting the video bridge (attempt {bridge_restarts})", age.as_secs());
            if let Err(e) = write_bridge_cfg(&p2p, &lan_ip) {
                tracing::error!("rewrite bridge config: {e:#}");
            }
            bridge_tx.send_replace(bridge_spec());
        }
    };
    tokio::select! {
        _ = watch_video => Ok(()),
        r = shutdown() => r,
    }
}

/// How long the first video check waits before blaming the P2P password.
const VERIFY_WINDOW: Duration = Duration::from_secs(90);
/// The relay is restarted by its own watchdog after this long without a frame.
const RELAY_STALL: Duration = Duration::from_secs(12);
/// After this long without video the Go bridge itself is restarted.
const BRIDGE_STALL: Duration = Duration::from_secs(60);

/// The monitor's LAN IP and protocol: `AVENT_LAN_IP` if set, else discovery.
async fn locate(env: &Env) -> Option<(String, String)> {
    if !env.lan_ip.is_empty() {
        return Some((env.lan_ip.clone(), "3.3".to_string()));
    }
    let seen = crate::discover::scan(Duration::from_secs(8), Some(&env.device_id)).await;
    seen.get(&env.device_id).map(|a| (a.ip.clone(), a.version.clone()))
}

/// Resolve on SIGTERM (docker stop) or Ctrl-C; dropping the runtime then
/// kills every supervised child.
async fn shutdown() -> Result<()> {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    tracing::info!("shutting down");
    Ok(())
}

/// Best-effort primary LAN IP (the source address for an outbound route).
fn local_ip() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    Some(sock.local_addr().ok()?.ip().to_string())
}
