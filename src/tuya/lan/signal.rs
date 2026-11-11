//! Protocol-302 video signalling on the LAN session (command 32).
//!
//! Ported from `aventproxy/avent-webrtc-bridge/pkg/lan/signal.go` (MIT,
//! © 2026 thekoma; LAN path by Leonard Pitzu). The offer describes the "imm"
//! transport (AES/KCP over ICE), the only one the monitor accepts on the LAN.

use rand::Rng;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

pub const P2P_SKILL: u32 = 1635;
pub const SECURITY_LEVEL: u32 = 3;

pub struct Offer {
    pub uid: String,
    pub device_id: String,
    pub session_id: String,
    pub trace_id: String,
    /// 16 ASCII characters; the media AES key, sent hex-encoded in the SDP.
    pub aes_key: String,
    pub ice_ufrag: String,
    pub ice_pwd: String,
}

impl Offer {
    pub fn new(uid: &str, device_id: &str) -> Self {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        let trace = uuid::Uuid::new_v4().to_string().to_uppercase();
        Self {
            uid: uid.into(),
            device_id: device_id.into(),
            session_id: format!("{device_id}{now}{}", random_ascii(8)),
            trace_id: format!("{trace}_{device_id}_{}", now * 1000),
            aes_key: random_ascii(16),
            // The app uses a 4-character ufrag and a 24-character password.
            ice_ufrag: random_ascii(4),
            ice_pwd: random_ascii(24),
        }
    }

    /// The imm SDP. The monitor rejects a WebRTC/DTLS description on the LAN
    /// path with close_reason 7.
    pub fn sdp(&self) -> String {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        [
            "v=0".to_string(),
            format!("o=- {now} 1 IN IP4 127.0.0.1"),
            "s=-".into(),
            "t=0 0".into(),
            "a=group:BUNDLE imm0".into(),
            format!("a=msid-semantic: WMS {}", self.session_id),
            "m=application 9 imm 6001".into(),
            "c=IN IP4 0.0.0.0".into(),
            "a=rtcp:9 IN IP4 0.0.0.0".into(),
            format!("a=ice-ufrag:{}", self.ice_ufrag),
            format!("a=ice-pwd:{}", self.ice_pwd),
            "a=ice-options:trickle".into(),
            format!("a=aes-key:{}", hex::encode(self.aes_key.as_bytes())),
            "a=mid:imm0".into(),
            "a=rtpmap:6001 AES/KCP 330".into(),
            format!("a=ssrc:0 cname:{}", self.uid),
            String::new(),
        ]
        .join("\r\n")
    }

    fn header(&self, kind: &str) -> Value {
        json!({
            "from": self.uid,
            "to": self.device_id,
            "moto_id": "",
            "path": "lan",
            "type": kind,
            "is_pre": 0,
            "p2p_skill": P2P_SKILL,
            "security_level": SECURITY_LEVEL,
            "sessionid": self.session_id,
            "trace_id": self.trace_id,
        })
    }

    /// The offer as a 302 frame body.
    pub fn offer_frame(&self) -> Value {
        json!({
            "header": self.header("offer"),
            "msg": {
                "sdp": self.sdp(),
                "preconnect": true,
                // The monitor gathers no candidates when this is empty, and
                // resolves a hostname before gathering; an unroutable IP
                // literal (RFC 5737 TEST-NET-1) satisfies both.
                "token": [{ "urls": "stun:192.0.2.1:3478" }],
            },
        })
    }
}

fn random_ascii(n: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::rng();
    (0..n).map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_shape() {
        let o = Offer::new("eu1uid", "dev1");
        let f = o.offer_frame();
        assert_eq!(f["header"]["type"], "offer");
        assert_eq!(f["header"]["path"], "lan");
        assert_eq!(f["header"]["sessionid"], o.session_id);
        assert!(o.session_id.starts_with("dev1"));
        assert_eq!(o.aes_key.len(), 16);
        let sdp = f["msg"]["sdp"].as_str().unwrap();
        assert!(sdp.contains("a=rtpmap:6001 AES/KCP 330"));
        assert!(sdp.contains(&format!("a=aes-key:{}", hex::encode(&o.aes_key))));
    }
}
