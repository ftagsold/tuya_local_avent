//! An authenticated LAN connection to one monitor on TCP 6668.
//!
//! Protocol 3.3 needs no handshake: every frame is `55AA` with an AES-ECB
//! payload under the `localKey`. Protocol 3.5 first negotiates a session key
//! (commands 3/4/5) and then speaks `6699` AES-GCM under that key.
//!
//! The 3.5 negotiation is ported from
//! `aventproxy/avent-webrtc-bridge/pkg/lan/session.go` (MIT, © 2026 thekoma).

use std::time::Duration;

use aes::Aes128;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{AesGcm, Nonce};
use anyhow::{bail, Context, Result};
use cipher::consts::U12;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use super::frame::{self, FrameError, Header};

pub const PORT: u16 = 6668;
/// The monitor drops an idle connection after roughly 30-45 seconds.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(8);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const NONCE_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    V33,
    V35,
}

impl Version {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "3.3" => Some(Self::V33),
            "3.5" => Some(Self::V35),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::V33 => "3.3",
            Self::V35 => "3.5",
        }
    }
}

pub struct Session {
    pub version: Version,
    stream: TcpStream,
    /// `localKey` on 3.3; the negotiated session key on 3.5.
    key: Vec<u8>,
    seq: u32,
    buf: Vec<u8>,
}

impl Session {
    /// Connect and, on 3.5, complete the session-key negotiation, which also
    /// proves the `localKey` (a wrong key fails the HMAC check).
    pub async fn connect(ip: &str, local_key: &str, version: Version) -> Result<Self> {
        if local_key.len() != 16 {
            bail!("local key must be 16 characters, got {}", local_key.len());
        }
        let stream = timeout(IO_TIMEOUT, TcpStream::connect((ip, PORT)))
            .await
            .context("connect timed out")?
            .with_context(|| format!("connect {ip}:{PORT}"))?;
        let mut s = Self {
            version,
            stream,
            key: local_key.as_bytes().to_vec(),
            seq: 0,
            buf: Vec::new(),
        };
        if version == Version::V35 {
            s.negotiate().await?;
        }
        Ok(s)
    }

    async fn negotiate(&mut self) -> Result<()> {
        let local_key = self.key.clone();
        let mut local_nonce = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut local_nonce);

        self.send_with_key(&local_key, frame::CMD_SESS_KEY_NEG_START, &local_nonce)
            .await
            .context("negotiation start")?;
        let (h, payload) = self.recv_with_key(&local_key, IO_TIMEOUT).await?;
        if h.cmd != frame::CMD_SESS_KEY_NEG_RESP {
            bail!(
                "expected cmd {}, got cmd {} in a {} frame: {}",
                frame::CMD_SESS_KEY_NEG_RESP,
                h.cmd,
                prefix_name(h.prefix),
                String::from_utf8_lossy(&payload).chars().take(200).collect::<String>()
            );
        }
        let payload = frame::strip_retcode(&payload);
        if payload.len() < NONCE_LEN + 32 {
            bail!("negotiation response too short");
        }
        let remote_nonce = &payload[..NONCE_LEN];
        if hmac_sha256(&local_key, &local_nonce) != payload[NONCE_LEN..NONCE_LEN + 32] {
            bail!("negotiation HMAC mismatch: wrong local key");
        }
        let finish = hmac_sha256(&local_key, remote_nonce);
        self.send_with_key(&local_key, frame::CMD_SESS_KEY_NEG_FINISH, &finish)
            .await
            .context("negotiation finish")?;
        self.key = session_key(&local_key, &local_nonce, remote_nonce)?;
        Ok(())
    }

    /// Send a command under the session's key and framing.
    pub async fn send(&mut self, cmd: u32, payload: &[u8]) -> Result<()> {
        let key = self.key.clone();
        self.send_with_key(&key, cmd, payload).await
    }

    /// Send a 3.3 frame with the version header forced on or off. For probing
    /// which form a command needs; 3.5 sessions ignore the choice.
    pub async fn send_33(&mut self, cmd: u32, payload: &[u8], version_header: bool) -> Result<()> {
        if self.version != Version::V33 {
            return self.send(cmd, payload).await;
        }
        self.seq = self.seq.wrapping_add(1);
        let bytes = frame::pack_55aa_with(&self.key, self.seq, cmd, payload, version_header)?;
        timeout(IO_TIMEOUT, self.stream.write_all(&bytes))
            .await
            .context("write timed out")??;
        Ok(())
    }

    async fn send_with_key(&mut self, key: &[u8], cmd: u32, payload: &[u8]) -> Result<()> {
        self.seq = self.seq.wrapping_add(1);
        let bytes = match self.version {
            Version::V33 => frame::pack_55aa(key, self.seq, cmd, payload)?,
            Version::V35 => {
                let mut iv = [0u8; frame::GCM_IV_LEN];
                rand::rng().fill_bytes(&mut iv);
                frame::pack_6699(key, &iv, self.seq, cmd, payload)?
            }
        };
        timeout(IO_TIMEOUT, self.stream.write_all(&bytes))
            .await
            .context("write timed out")??;
        Ok(())
    }

    /// Read the next frame, decrypted. `Ok(None)` when nothing arrived in time.
    pub async fn recv(&mut self, wait: Duration) -> Result<Option<(Header, Vec<u8>)>> {
        let key = self.key.clone();
        match self.recv_with_key(&key, wait).await {
            Ok(frame) => Ok(Some(frame)),
            Err(e) if e.downcast_ref::<tokio::time::error::Elapsed>().is_some() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Read the next whole frame without decrypting it. `Ok(None)` on timeout.
    pub async fn recv_raw(&mut self, wait: Duration) -> Result<Option<(Header, Vec<u8>)>> {
        match self.read_frame(wait).await {
            Ok(frame) => Ok(Some(frame)),
            Err(e) if e.downcast_ref::<tokio::time::error::Elapsed>().is_some() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Decrypt a frame read by [`Session::recv_raw`] under the session's key.
    pub fn decode(&self, h: &Header, raw: &[u8]) -> Result<Vec<u8>, FrameError> {
        decode_with_key(&self.key, h, raw)
    }

    async fn recv_with_key(&mut self, key: &[u8], wait: Duration) -> Result<(Header, Vec<u8>)> {
        let (h, raw) = self.read_frame(wait).await?;
        Ok((h, decode_with_key(key, &h, &raw)?))
    }

    async fn read_frame(&mut self, wait: Duration) -> Result<(Header, Vec<u8>)> {
        loop {
            match frame::parse_header(&self.buf) {
                Ok(h) if self.buf.len() >= h.total => {
                    let raw: Vec<u8> = self.buf.drain(..h.total).collect();
                    return Ok((h, raw));
                }
                Ok(_) | Err(FrameError::Short) => {}
                // Resynchronise on the next plausible prefix instead of giving up.
                Err(_) => match next_prefix(&self.buf) {
                    Some(i) => {
                        self.buf.drain(..i);
                        continue;
                    }
                    None => self.buf.clear(),
                },
            }
            let mut chunk = [0u8; 4096];
            let n = timeout(wait, self.stream.read(&mut chunk)).await??;
            if n == 0 {
                bail!("monitor closed the connection");
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Keep the connection alive. Carries no data points, changes nothing.
    pub async fn heartbeat(&mut self) -> Result<()> {
        self.send(frame::CMD_HEARTBEAT, b"{}").await
    }
}

fn decode_with_key(key: &[u8], h: &Header, raw: &[u8]) -> Result<Vec<u8>, FrameError> {
    Ok(match h.prefix {
        frame::PREFIX_6699 => frame::unpack_6699(key, raw)?.1,
        _ => frame::unpack_55aa(key, raw)?.1,
    })
}

pub fn prefix_name(prefix: u32) -> &'static str {
    match prefix {
        frame::PREFIX_6699 => "6699",
        frame::PREFIX_55AA => "55AA",
        _ => "????",
    }
}

fn next_prefix(buf: &[u8]) -> Option<usize> {
    (1..buf.len().saturating_sub(3)).find(|&i| {
        let v = u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
        v == frame::PREFIX_55AA || v == frame::PREFIX_6699
    })
}

fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC takes any key length");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

/// Mirrors the SDK: encrypt the XOR of both nonces under the `localKey`, in
/// GCM with the local nonce as IV, and keep the first 16 ciphertext bytes.
fn session_key(local_key: &[u8], local_nonce: &[u8], remote_nonce: &[u8]) -> Result<Vec<u8>> {
    let mixed: Vec<u8> = local_nonce
        .iter()
        .zip(remote_nonce)
        .map(|(a, b)| a ^ b)
        .collect();
    let cipher = AesGcm::<Aes128, U12>::new_from_slice(local_key)
        .map_err(|e| anyhow::anyhow!("session key: {e}"))?;
    let sealed = cipher
        .encrypt(
            Nonce::from_slice(&local_nonce[..frame::GCM_IV_LEN]),
            Payload { msg: &mixed, aad: &[] },
        )
        .map_err(|e| anyhow::anyhow!("session key: {e}"))?;
    Ok(sealed[..NONCE_LEN].to_vec())
}

/// Pull the JSON object out of a decrypted payload, skipping any return code
/// or `3.x` version header in front of it.
pub fn payload_json(plain: &[u8]) -> Option<serde_json::Value> {
    let start = plain.iter().position(|&b| b == b'{')?;
    let end = plain.iter().rposition(|&b| b == b'}')?;
    serde_json::from_slice(&plain[start..=end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_json_skips_prefix() {
        let mut p = b"3.5\0\0\0\0\0\0\0\0\0\0\0\0".to_vec();
        p.extend_from_slice(br#"{"dps":{"207":2570}}"#);
        let v = payload_json(&p).unwrap();
        assert_eq!(v["dps"]["207"], 2570);
    }

    #[test]
    fn session_key_is_16_bytes_and_deterministic() {
        let k = b"0123456789abcdef";
        let a = session_key(k, &[1; 16], &[2; 16]).unwrap();
        let b = session_key(k, &[1; 16], &[2; 16]).unwrap();
        assert_eq!(a.len(), 16);
        assert_eq!(a, b);
    }
}
