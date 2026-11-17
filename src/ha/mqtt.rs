//! Home Assistant bridge over MQTT discovery.
//!
//! Publishes one discovery config per data point, mirrors monitor state onto
//! state topics, and turns HA commands back into local DP writes. All control
//! and state stays on the LAN; MQTT only carries it to Home Assistant.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use rumqttc::{AsyncClient, Event, Incoming, LastWill, MqttOptions, QoS};
use serde_json::{json, Value};

use crate::dp::{self, Kind};
use crate::lan_control::Control;

#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
    pub device_id: String,
    pub device_name: String,
    /// The ONVIF camera's MAC, so Home Assistant merges both devices.
    pub mac: String,
}

/// base topic for this device's state/commands.
fn base(device_id: &str) -> String {
    format!("avent/{device_id}")
}

fn avail_topic(device_id: &str) -> String {
    format!("{}/availability", base(device_id))
}

/// Run the MQTT bridge until the process ends (reconnects internally).
pub async fn run(cfg: Config, control: Control) -> Result<()> {
    let avail = avail_topic(&cfg.device_id);
    let mut opts = MqttOptions::new(format!("avent-{}", cfg.device_id), &cfg.host, cfg.port);
    opts.set_keep_alive(Duration::from_secs(30));
    if !cfg.user.is_empty() {
        opts.set_credentials(&cfg.user, &cfg.pass);
    }
    opts.set_last_will(LastWill::new(&avail, "offline", QoS::AtLeastOnce, true));

    let (client, mut eventloop) = AsyncClient::new(opts, 64);
    // Track-id block this monitor's catalogue uses (0 = not reported yet).
    let block = Arc::new(AtomicU32::new(0));

    // Fan monitor updates onto MQTT state topics.
    let mut updates = control.subscribe();
    let state_client = client.clone();
    let dev = cfg.device_id.clone();
    let seen_block = block.clone();
    let raise = control.clone();
    tokio::spawn(async move {
        // Timer state arrives as two DPs: switch (243) and length (244).
        let (mut timer_on, mut timer_secs) = (None::<bool>, None::<i64>);
        loop {
            // Lagged (a burst outran us) only skips; only Closed ends this.
            let u = match updates.recv().await {
                Ok(u) => u,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            };
            // The playing track comes back on 248 (and maybe 202): its id
            // names the track and tells which block this monitor uses.
            if u.id == dp::PLAY_CURRENT || u.id == dp::PLAY_TRACK {
                let id = dp::current_track_id(&u.value);
                match id.and_then(dp::track_by_id) {
                    Some((b, name)) => {
                        seen_block.store(b, Ordering::Relaxed);
                        let topic = format!("{}/lullaby_track/state", base(&dev));
                        let _ = state_client.publish(topic, QoS::AtLeastOnce, true, name).await;
                    }
                    // A block not in dp::TRACK_BLOCKS: log it so it can be added.
                    None => tracing::warn!("lullaby: unknown track id in DP {}: {}", u.id, u.value),
                }
                continue;
            }
            if u.id == dp::TIMER_SWITCH || u.id == dp::TIMER_SECS {
                if u.id == dp::TIMER_SWITCH {
                    timer_on = u.value.as_bool();
                } else {
                    timer_secs = u.value.as_i64();
                }
                // Off needs no length; on needs both.
                let option = match (timer_on, timer_secs) {
                    (Some(false), _) => Some(dp::TIMER_FOREVER),
                    (Some(true), Some(secs)) => Some(dp::timer_option(true, secs)),
                    _ => None,
                };
                if let Some(o) = option {
                    let topic = format!("{}/lullaby_timer/state", base(&dev));
                    let _ = state_client.publish(topic, QoS::AtLeastOnce, true, o).await;
                }
                continue;
            }
            // A volume below the usable floor (set by the app, or by an
            // older bridge) makes the next play near silent, the monitor's
            // own button included: raise it to the floor.
            if u.id == dp::VOLUME && u.value.as_i64().is_some_and(|v| v < dp::VOLUME_MIN) {
                tracing::info!("lullaby volume {} is below {}: raising it", u.value, dp::VOLUME_MIN);
                if let Err(e) = write_one(&raise, dp::VOLUME, json!(dp::VOLUME_MIN)).await {
                    tracing::warn!("raise lullaby volume: {e:#}");
                }
            }
            if let Some(d) = dp::by_id(u.id) {
                let topic = format!("{}/{}/state", base(&dev), d.slug);
                let payload = state_payload(d, &u.value);
                let _ = state_client.publish(topic, QoS::AtLeastOnce, true, payload).await;
            }
        }
    });

    // Drive the connection and handle inbound commands.
    loop {
        match eventloop.poll().await {
            // Every (re)connect is a clean session: the broker has dropped our
            // subscriptions and published the "offline" will, so announce
            // and subscribe again. In a task: the requests queue on the
            // client channel, which only drains while this loop polls.
            Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                tracing::info!("MQTT connected to {}:{}", cfg.host, cfg.port);
                let (client, cfg) = (client.clone(), announce_cfg(&cfg));
                let avail = avail.clone();
                tokio::spawn(async move {
                    let r = async {
                        publish_discovery(&client, &cfg).await.context("publish discovery")?;
                        client.publish(&avail, QoS::AtLeastOnce, true, "online").await?;
                        subscribe_commands(&client, &cfg).await
                    };
                    if let Err(e) = r.await {
                        tracing::warn!("mqtt announce: {e:#}");
                    }
                });
            }
            Ok(Event::Incoming(Incoming::Publish(p))) => {
                let topic = String::from_utf8_lossy(p.topic.as_bytes()).to_string();
                let value = String::from_utf8_lossy(&p.payload).to_string();
                if let Err(e) = handle_command(&cfg, &control, &block, &topic, &value).await {
                    tracing::warn!("command {topic}={value} failed: {e:#}");
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("mqtt connection: {e}; retrying");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

fn announce_cfg(cfg: &Config) -> Config {
    Config {
        host: cfg.host.clone(),
        port: cfg.port,
        user: String::new(),
        pass: String::new(),
        device_id: cfg.device_id.clone(),
        device_name: cfg.device_name.clone(),
        mac: cfg.mac.clone(),
    }
}

fn device_block(cfg: &Config) -> Value {
    json!({
        "identifiers": [format!("avent_{}", cfg.device_id)],
        // Same MAC as the ONVIF camera: Home Assistant merges the ONVIF
        // integration's camera into this device.
        "connections": [["mac", cfg.mac]],
        "name": cfg.device_name,
        "manufacturer": "Philips Avent",
        "model": "SCD9xx (Tuya)",
    })
}

async fn publish_discovery(client: &AsyncClient, cfg: &Config) -> Result<()> {
    let dev = device_block(cfg);
    let avail = avail_topic(&cfg.device_id);
    // Brightness used to be its own number entity; clear that retained config.
    if let Some(b) = dp::DPS.iter().find(|d| d.kind == Kind::Brightness) {
        let old = format!("homeassistant/number/avent_{}/{}/config", cfg.device_id, b.slug);
        client.publish(old, QoS::AtLeastOnce, true, "").await?;
    }
    for d in dp::DPS {
        let (component, mut config) = match d.kind {
            Kind::Switch => ("switch", json!({ "payload_on": "true", "payload_off": "false", "command_topic": cmd_topic(cfg, d.slug) })),
            Kind::Light => ("light", light_config(cfg)),
            Kind::Brightness => continue, // part of the light
            Kind::Number => ("number", json!({ "min": d.min, "max": d.max, "command_topic": cmd_topic(cfg, d.slug), "unit_of_measurement": d.unit })),
            Kind::Select | Kind::Track | Kind::Timer => ("select", json!({ "options": d.options, "command_topic": cmd_topic(cfg, d.slug) })),
            Kind::Sensor => ("sensor", json!({ "unit_of_measurement": d.unit, "device_class": d.device_class })),
            Kind::BinarySensor => {
                let mut c = json!({ "device_class": d.device_class, "payload_on": "true", "payload_off": "false" });
                // Alert events have no "off": clear them after 30 s.
                if d.on_value.is_empty() {
                    c["off_delay"] = json!(30);
                }
                ("binary_sensor", c)
            }
        };
        let obj = config.as_object_mut().unwrap();
        obj.insert("name".into(), json!(d.name));
        obj.insert("unique_id".into(), json!(format!("avent_{}_{}", cfg.device_id, d.slug)));
        obj.insert("state_topic".into(), json!(format!("{}/{}/state", base(&cfg.device_id), d.slug)));
        obj.insert("availability_topic".into(), json!(avail));
        obj.insert("device".into(), dev.clone());
        let topic = format!("homeassistant/{component}/avent_{}/{}/config", cfg.device_id, d.slug);
        client.publish(topic, QoS::AtLeastOnce, true, config.to_string()).await?;
    }
    // A LAN "Listen URL" sensor existed briefly (unreleased): clear it and
    // its retained state, which held an access token.
    let topic = format!("homeassistant/sensor/avent_{}/listen_url/config", cfg.device_id);
    client.publish(topic, QoS::AtLeastOnce, true, "").await?;
    client.publish(format!("{}/listen_url/state", base(&cfg.device_id)), QoS::AtLeastOnce, true, "").await?;
    Ok(())
}

/// The night light: on/off plus the brightness DP as its 1-100 dimmer, so
/// `light.turn_on` with `brightness_pct` sets both.
fn light_config(cfg: &Config) -> Value {
    let mut c = json!({ "payload_on": "true", "payload_off": "false" });
    for d in dp::DPS {
        match d.kind {
            Kind::Light => c["command_topic"] = json!(cmd_topic(cfg, d.slug)),
            Kind::Brightness => {
                c["brightness_command_topic"] = json!(cmd_topic(cfg, d.slug));
                c["brightness_state_topic"] = json!(format!("{}/{}/state", base(&cfg.device_id), d.slug));
                c["brightness_scale"] = json!(d.max);
            }
            _ => {}
        }
    }
    c
}

fn cmd_topic(cfg: &Config, slug: &str) -> String {
    format!("{}/{}/set", base(&cfg.device_id), slug)
}

async fn subscribe_commands(client: &AsyncClient, cfg: &Config) -> Result<()> {
    for d in dp::DPS.iter().filter(|d| d.writable) {
        client.subscribe(cmd_topic(cfg, d.slug), QoS::AtLeastOnce).await?;
    }
    Ok(())
}

/// Convert a raw DP value to the string HA expects on the state topic.
fn state_payload(d: &dp::Dp, raw: &Value) -> String {
    match d.kind {
        Kind::BinarySensor if !d.on_value.is_empty() => (raw.as_str() == Some(d.on_value)).to_string(),
        Kind::Switch | Kind::Light | Kind::BinarySensor => {
            // Tuya booleans; alert DPs may be non-empty strings meaning "on".
            match raw {
                Value::Bool(b) => b.to_string(),
                Value::String(s) => (!s.is_empty()).to_string(),
                other => (!other.is_null()).to_string(),
            }
        }
        Kind::Sensor | Kind::Number | Kind::Brightness => match raw.as_f64() {
            Some(n) if d.scale != 1.0 => format!("{}", n / d.scale),
            Some(n) => format!("{n}"),
            None => raw.as_str().unwrap_or("").to_string(),
        },
        Kind::Select | Kind::Track | Kind::Timer => raw.as_str().unwrap_or("").to_string(),
    }
}

/// Turn an HA command into a DP write, with the raw value the monitor expects.
async fn handle_command(cfg: &Config, control: &Control, block: &Arc<AtomicU32>, topic: &str, value: &str) -> Result<()> {
    let slug = topic.strip_prefix(&format!("{}/", base(&cfg.device_id))).and_then(|s| s.strip_suffix("/set"));
    let Some(slug) = slug else { return Ok(()) };
    let Some(d) = dp::DPS.iter().find(|d| d.slug == slug) else { return Ok(()) };
    if !d.writable {
        return Ok(());
    }
    tracing::info!("HA command {slug} = {value}");
    let raw: Value = match d.kind {
        Kind::Switch | Kind::Light => json!(value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("on")),
        Kind::Number => {
            let Ok(n) = value.parse::<f64>() else { return Ok(()) };
            let n = if d.max > d.min { n.clamp(d.min, d.max) } else { n };
            json!((n * d.scale) as i64)
        }
        // HA sends 0..=brightness_scale; the monitor takes 1..=100.
        Kind::Brightness => json!(value.parse::<f64>().map(|n| n.round().clamp(d.min, d.max) as i64).unwrap_or(d.max as i64)),
        // Only the advertised options reach the monitor.
        Kind::Select if d.options.contains(&value) => json!(value),
        Kind::Select => {
            tracing::warn!("MQTT {slug}: ignoring unknown option {value:?}");
            return Ok(());
        }
        // Paced multi-DP writes run in a task, so the MQTT loop keeps polling.
        Kind::Track | Kind::Timer => {
            let Some(index) = d.options.iter().position(|o| *o == value) else {
                tracing::warn!("MQTT {slug}: ignoring unknown option {value:?}");
                return Ok(());
            };
            let (control, known, kind, slug) = (control.clone(), block.load(Ordering::Relaxed), d.kind, d.slug);
            let secs = dp::timer_secs(value);
            tokio::spawn(async move {
                let r = match kind {
                    Kind::Track => play_track(&control, known, index).await,
                    _ => set_timer(&control, secs).await,
                };
                if let Err(e) = r {
                    tracing::warn!("lullaby {slug}: {e:#}");
                }
            });
            return Ok(());
        }
        _ => return Ok(()),
    };
    let mut dps = BTreeMap::new();
    dps.insert(d.id.to_string(), raw.clone());
    // Motion/sound detectors are mutually exclusive: clear the partner.
    if raw.as_bool() == Some(true) {
        if let Some(p) = dp::exclusive_partner(d.id) {
            dps.insert(p.to_string(), json!(false));
        }
    }
    control.write(dps).await
}

/// Pause between the writes of one multi-DP command.
const WRITE_GAP: Duration = Duration::from_millis(300);

/// Write one DP on its own (the monitor ignores some DPs batched together).
async fn write_one(control: &Control, id: u32, value: Value) -> Result<()> {
    control.write(BTreeMap::from([(id.to_string(), value)])).await
}

/// Play catalogue track `index`: DP 202, then `play` on 201, each in its own
/// write (the monitor ignores 201 and 202 batched together). Until the
/// monitor has reported a track id its block is unknown, so the id is sent
/// for every block: it ignores the ones outside its own.
async fn play_track(control: &Control, known: u32, index: usize) -> Result<()> {
    let blocks: Vec<u32> = if known != 0 { vec![known] } else { dp::TRACK_BLOCKS.to_vec() };
    for b in blocks {
        write_one(control, dp::PLAY_TRACK, json!(dp::track_value(b, index))).await?;
        tokio::time::sleep(WRITE_GAP).await;
    }
    write_one(control, dp::PLAY_CONTROL, json!("play")).await
}

/// Set the lullaby timer: length (244), then switch on (243), or just the
/// switch off for "Forever".
async fn set_timer(control: &Control, secs: Option<i64>) -> Result<()> {
    if let Some(secs) = secs {
        write_one(control, dp::TIMER_SECS, json!(secs)).await?;
        tokio::time::sleep(WRITE_GAP).await;
    }
    write_one(control, dp::TIMER_SWITCH, json!(secs.is_some())).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_carries_the_brightness_dp() {
        let cfg = Config { host: String::new(), port: 0, user: String::new(), pass: String::new(), device_id: "dev1".into(), mac: crate::onvif::device_mac("dev1"), device_name: "Nursery".into() };
        let c = light_config(&cfg);
        assert_eq!(c["command_topic"], "avent/dev1/night_light/set");
        assert_eq!(c["brightness_command_topic"], "avent/dev1/brightness/set");
        assert_eq!(c["brightness_state_topic"], "avent/dev1/brightness/state");
        assert_eq!(c["brightness_scale"], 100.0);
    }

    #[test]
    fn lullaby_playing_reads_the_play_state() {
        let d = dp::by_id(246).unwrap();
        assert_eq!(state_payload(d, &json!("playing")), "true");
        assert_eq!(state_payload(d, &json!("stopping")), "false");
    }
}
