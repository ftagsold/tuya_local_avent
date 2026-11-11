//! Find Tuya devices from their UDP discovery broadcasts.
//!
//! Devices announce themselves every few seconds: plaintext JSON on 6666
//! (protocol 3.1), `55AA` + AES-ECB on 6667 (3.3), `6699` + AES-GCM on 6667
//! or 7000 (3.4/3.5). The encrypted forms use one public, fixed key, so no
//! secret is needed to read the device id, IP and protocol version.

use std::collections::BTreeMap;
use std::time::Duration;

use md5::{Digest, Md5};
use serde::Deserialize;
use tokio::net::UdpSocket;
use tokio::time::{timeout_at, Instant};

use crate::tuya::lan::frame;

pub const PORTS: [u16; 3] = [6666, 6667, 7000];

#[derive(Debug, Clone, Deserialize)]
pub struct Announcement {
    #[serde(rename = "gwId")]
    pub device_id: String,
    pub ip: String,
    #[serde(default)]
    pub version: String,
    #[serde(rename = "productKey", default)]
    pub product_key: String,
}

/// The well-known key every Tuya device encrypts its broadcast with.
fn udp_key() -> [u8; 16] {
    Md5::digest(b"yGAdlopoPVldABfn").into()
}

/// Decode one broadcast datagram, or `None` if it is not a Tuya announcement.
pub fn decode(datagram: &[u8]) -> Option<Announcement> {
    let key = udp_key();
    let plain = match frame::parse_header(datagram).ok()?.prefix {
        frame::PREFIX_6699 => frame::unpack_6699(&key, datagram).ok()?.1,
        _ => frame::unpack_55aa(&key, datagram).ok()?.1,
    };
    let json = crate::tuya::lan::session::payload_json(&plain)?;
    serde_json::from_value(json).ok()
}

/// Listen on all discovery ports for `wait`, returning every device heard,
/// keyed by device id. Stops early once `want` (if given) has been heard.
pub async fn scan(wait: Duration, want: Option<&str>) -> BTreeMap<String, Announcement> {
    let mut sockets = Vec::new();
    for port in PORTS {
        match bind_shared(port) {
            Ok(s) => sockets.push(s),
            Err(e) => tracing::warn!("cannot listen on UDP {port}: {e}"),
        }
    }
    let deadline = Instant::now() + wait;
    let mut found = BTreeMap::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    for sock in sockets {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            while let Ok((n, _)) = sock.recv_from(&mut buf).await {
                if tx.send(buf[..n].to_vec()).await.is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);
    while let Ok(Some(dgram)) = timeout_at(deadline, rx.recv()).await {
        if let Some(a) = decode(&dgram) {
            let hit = want.is_some_and(|w| w == a.device_id);
            found.insert(a.device_id.clone(), a);
            if hit {
                break;
            }
        }
    }
    found
}

/// Bind with SO_REUSEADDR/SO_REUSEPORT so we coexist with Home Assistant or
/// another scanner on the same host.
fn bind_shared(port: u16) -> std::io::Result<UdpSocket> {
    let sock = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None)?;
    sock.set_reuse_address(true)?;
    #[cfg(unix)]
    sock.set_reuse_port(true)?;
    sock.set_nonblocking(true)?;
    sock.bind(&std::net::SocketAddr::from(([0, 0, 0, 0], port)).into())?;
    UdpSocket::from_std(sock.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_33_broadcast() {
        let body = br#"{"ip":"192.168.1.46","gwId":"dev123","active":2,"encrypt":true,"productKey":"kzm54lhabeeucq5a","version":"3.3"}"#;
        // A device frame: retcode 0, then the ECB body.
        let mut frame = frame::pack_55aa(&udp_key(), 0, 0x13, body).unwrap();
        // Insert the 4-byte zero return code a device sends, fixing up length and CRC.
        let payload_end = frame.len() - 8;
        let mut payload = vec![0, 0, 0, 0];
        payload.extend_from_slice(&frame[16..payload_end]);
        frame.truncate(12);
        frame.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
        frame.extend_from_slice(&payload);
        let crc = crc32fast::hash(&frame);
        frame.extend_from_slice(&crc.to_be_bytes());
        frame.extend_from_slice(&frame::SUFFIX_55AA.to_be_bytes());

        let a = decode(&frame).unwrap();
        assert_eq!(a.device_id, "dev123");
        assert_eq!(a.ip, "192.168.1.46");
        assert_eq!(a.version, "3.3");
        assert_eq!(a.product_key, "kzm54lhabeeucq5a");
    }
}
