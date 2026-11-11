//! Write the Go video bridge's config file (`BridgeConfig` in
//! `bridge/cmd/addon/addon.go`). The Rust side owns every secret and produces
//! this file; the Go bridge only reads it. 0600, and never logged.

use std::os::unix::fs::OpenOptionsExt;

use anyhow::{Context, Result};
use serde::Serialize;

#[derive(Serialize)]
pub struct Camera {
    pub camera_id: String,
    pub camera_name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub product_id: String,
    pub local_key: String,
    pub p2p_password: String,
    pub lan_ip: String,
    pub lan_protocol: String,
}

#[derive(Serialize)]
pub struct BridgeConfig {
    pub uid: String,
    pub lan_streaming: bool,
    pub bridge_port: u16,
    pub rtsp_bind_address: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub rtsp_user: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub rtsp_pass: String,
    /// Offer the RTSP talkback backchannel (LAN talk, on demand).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub talkback: bool,
    pub cameras: Vec<Camera>,
}

impl BridgeConfig {
    /// Write the config to `path` with 0600 permissions.
    pub fn write(&self, path: &std::path::Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let json = serde_json::to_vec_pretty(self).context("serialize bridge config")?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("open {}", path.display()))?;
        use std::io::Write;
        f.write_all(&json).context("write bridge config")?;
        Ok(())
    }
}
