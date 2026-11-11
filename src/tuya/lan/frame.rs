//! Tuya LAN frame codec on TCP 6668.
//!
//! Two framings share the wire. `55AA` is the classic format used up to
//! protocol 3.3: a CRC32-checked frame whose payload is AES-128-ECB under the
//! device `localKey`. `6699` is the AES-GCM format a 3.5 device speaks, with
//! the header authenticated as additional data.
//!
//! Ported from `aventproxy/avent-webrtc-bridge/pkg/lan/frame.go` (MIT, © 2026
//! thekoma); the 55AA path is added here because the Go bridge delegated 3.3
//! sessions to Python tinytuya, whose 55AA/ECB behaviour this mirrors.

use aes::Aes128;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{AesGcm, Nonce};
use cipher::consts::U12;
use cipher::{BlockDecryptMut, BlockEncryptMut};

/// AES-128-GCM with a 12-byte nonce, as Tuya uses it.
type Aes128Gcm12 = AesGcm<Aes128, U12>;
type Ecb128Enc = ecb::Encryptor<Aes128>;
type Ecb128Dec = ecb::Decryptor<Aes128>;

pub const PREFIX_55AA: u32 = 0x0000_55AA;
pub const PREFIX_6699: u32 = 0x0000_6699;
pub const SUFFIX_55AA: u32 = 0x0000_AA55;
pub const SUFFIX_6699: u32 = 0x0000_9966;

pub const HEADER_55AA_LEN: usize = 16;
pub const HEADER_6699_LEN: usize = 18;
pub const SUFFIX_LEN: usize = 4;
pub const GCM_IV_LEN: usize = 12;
pub const GCM_TAG_LEN: usize = 16;

/// Guards a desynced stream. Far above the largest frame expected (the
/// signalling offer is ~1.6 kB) on purpose: a tighter ceiling silently
/// swallows that offer.
pub const MAX_FRAME_LEN: u32 = 65535;

// Command numbers used on the LAN channel.
pub const CMD_CONTROL: u32 = 7;
pub const CMD_HEARTBEAT: u32 = 9;
pub const CMD_DP_QUERY: u32 = 10;
/// Read or write for 22-character-id devices ("device22"): DP_QUERY is refused
/// on them, and this command with a `dpId` list and no `dps` reports state.
pub const CMD_CONTROL_NEW: u32 = 13;
pub const CMD_DP_QUERY_NEW: u32 = 16;
pub const CMD_SESS_KEY_NEG_START: u32 = 3;
pub const CMD_SESS_KEY_NEG_RESP: u32 = 4;
pub const CMD_SESS_KEY_NEG_FINISH: u32 = 5;
#[allow(dead_code)]
pub const CMD_STATUS: u32 = 8;
/// Carries the WebRTC-style offer/answer exchange (protocol-302 body).
pub const CMD_IPC_LAN_302: u32 = 32;

/// The 15-byte version header a 3.3 device prepends to commands that carry it
/// (CONTROL), but not to DP_QUERY: `"3.3"` then 12 bytes of padding.
pub const VERSION_HEADER_33: &[u8] = b"3.3\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("not enough data for a frame")]
    Short,
    #[error("unrecognised frame prefix")]
    BadPrefix,
    #[error("implausible frame length")]
    BadLength,
    #[error("bad CRC32")]
    BadCrc,
    #[error("crypto error: {0}")]
    Crypto(String),
}

/// A framed message, before its payload is decrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub prefix: u32,
    pub seq: u32,
    pub cmd: u32,
    pub length: u32,
    /// Full on-wire size of the frame, header and suffix included.
    pub total: usize,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Read a frame header from the front of `buf`.
pub fn parse_header(buf: &[u8]) -> Result<Header, FrameError> {
    if buf.len() < 4 {
        return Err(FrameError::Short);
    }
    match be32(&buf[0..4]) {
        PREFIX_6699 => {
            if buf.len() < HEADER_6699_LEN {
                return Err(FrameError::Short);
            }
            let length = be32(&buf[14..18]);
            if length == 0 || length > MAX_FRAME_LEN {
                return Err(FrameError::BadLength);
            }
            Ok(Header {
                prefix: PREFIX_6699,
                seq: be32(&buf[6..10]),
                cmd: be32(&buf[10..14]),
                length,
                total: length as usize + HEADER_6699_LEN + SUFFIX_LEN,
            })
        }
        PREFIX_55AA => {
            if buf.len() < HEADER_55AA_LEN {
                return Err(FrameError::Short);
            }
            let length = be32(&buf[12..16]);
            if length == 0 || length > MAX_FRAME_LEN {
                return Err(FrameError::BadLength);
            }
            Ok(Header {
                prefix: PREFIX_55AA,
                seq: be32(&buf[4..8]),
                cmd: be32(&buf[8..12]),
                length,
                total: length as usize + HEADER_55AA_LEN,
            })
        }
        _ => Err(FrameError::BadPrefix),
    }
}

/// Build an AES-GCM `6699` frame.
///
/// The `iv` is not a free choice: the monitor acknowledges a frame with a
/// random IV and then ignores its payload. It acts only on frames whose IV is
/// the ASCII text of a UUID fragment, which is what the app sends.
pub fn pack_6699(
    key: &[u8],
    iv: &[u8],
    seq: u32,
    cmd: u32,
    payload: &[u8],
) -> Result<Vec<u8>, FrameError> {
    if iv.len() != GCM_IV_LEN {
        return Err(FrameError::Crypto(format!(
            "iv must be {GCM_IV_LEN} bytes, got {}",
            iv.len()
        )));
    }
    let length = (GCM_IV_LEN + payload.len() + GCM_TAG_LEN) as u32;
    let mut head = Vec::with_capacity(HEADER_6699_LEN);
    head.extend_from_slice(&PREFIX_6699.to_be_bytes());
    head.extend_from_slice(&0u16.to_be_bytes());
    head.extend_from_slice(&seq.to_be_bytes());
    head.extend_from_slice(&cmd.to_be_bytes());
    head.extend_from_slice(&length.to_be_bytes());

    let cipher = Aes128Gcm12::new_from_slice(key).map_err(|e| FrameError::Crypto(e.to_string()))?;
    // Everything after the prefix is authenticated but not encrypted (AAD).
    let sealed = cipher
        .encrypt(
            Nonce::from_slice(iv),
            Payload {
                msg: payload,
                aad: &head[4..],
            },
        )
        .map_err(|e| FrameError::Crypto(e.to_string()))?;

    let mut out = Vec::with_capacity(HEADER_6699_LEN + iv.len() + sealed.len() + SUFFIX_LEN);
    out.extend_from_slice(&head);
    out.extend_from_slice(iv);
    out.extend_from_slice(&sealed);
    out.extend_from_slice(&SUFFIX_6699.to_be_bytes());
    Ok(out)
}

/// Decrypt a complete `6699` frame, returning its header and plaintext.
pub fn unpack_6699(key: &[u8], frame: &[u8]) -> Result<(Header, Vec<u8>), FrameError> {
    let h = parse_header(frame)?;
    if h.prefix != PREFIX_6699 {
        return Err(FrameError::BadPrefix);
    }
    if frame.len() < h.total {
        return Err(FrameError::Short);
    }
    let body = &frame[HEADER_6699_LEN..h.total - SUFFIX_LEN];
    if body.len() < GCM_IV_LEN + GCM_TAG_LEN {
        return Err(FrameError::Short);
    }
    let cipher = Aes128Gcm12::new_from_slice(key).map_err(|e| FrameError::Crypto(e.to_string()))?;
    let plain = cipher
        .decrypt(
            Nonce::from_slice(&body[..GCM_IV_LEN]),
            Payload {
                msg: &body[GCM_IV_LEN..],
                aad: &frame[4..HEADER_6699_LEN],
            },
        )
        .map_err(|e| FrameError::Crypto(format!("decrypt cmd {}: {e}", h.cmd)))?;
    Ok((h, plain))
}

/// Build a classic `55AA` (protocol 3.3) frame: the payload is AES-128-ECB
/// under `key`, with `"3.3"` version header for CONTROL and none for DP_QUERY.
pub fn pack_55aa(key: &[u8], seq: u32, cmd: u32, plaintext: &[u8]) -> Result<Vec<u8>, FrameError> {
    // Protocol 3.3 prepends the version header to every command except the
    // two query forms (and heartbeat/handshake, which carry no body here).
    let header = !matches!(
        cmd,
        CMD_DP_QUERY | CMD_DP_QUERY_NEW | CMD_HEARTBEAT | CMD_SESS_KEY_NEG_START | CMD_SESS_KEY_NEG_FINISH
    );
    pack_55aa_with(key, seq, cmd, plaintext, header)
}

/// [`pack_55aa`] with the version header chosen by the caller.
pub fn pack_55aa_with(
    key: &[u8],
    seq: u32,
    cmd: u32,
    plaintext: &[u8],
    version_header: bool,
) -> Result<Vec<u8>, FrameError> {
    let mut encrypted = ecb_encrypt(key, plaintext)?;
    let mut payload = Vec::new();
    if version_header {
        payload.extend_from_slice(VERSION_HEADER_33);
    }
    payload.append(&mut encrypted);

    // length covers payload + crc(4) + suffix(4).
    let length = (payload.len() + 8) as u32;
    let mut out = Vec::new();
    out.extend_from_slice(&PREFIX_55AA.to_be_bytes());
    out.extend_from_slice(&seq.to_be_bytes());
    out.extend_from_slice(&cmd.to_be_bytes());
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&payload);
    let crc = crc32fast::hash(&out); // CRC32 over everything up to here.
    out.extend_from_slice(&crc.to_be_bytes());
    out.extend_from_slice(&SUFFIX_55AA.to_be_bytes());
    Ok(out)
}

/// Decrypt a complete classic `55AA` frame. Strips the return code and any
/// version header, then AES-128-ECB decrypts the remainder.
pub fn unpack_55aa(key: &[u8], frame: &[u8]) -> Result<(Header, Vec<u8>), FrameError> {
    let h = parse_header(frame)?;
    if h.prefix != PREFIX_55AA {
        return Err(FrameError::BadPrefix);
    }
    if frame.len() < h.total {
        return Err(FrameError::Short);
    }
    // Verify CRC: bytes [0 .. total-8], crc at [total-8 .. total-4].
    let crc_pos = h.total - 8;
    let want = be32(&frame[crc_pos..crc_pos + 4]);
    if crc32fast::hash(&frame[..crc_pos]) != want {
        return Err(FrameError::BadCrc);
    }
    // Payload sits between the header and the crc+suffix.
    let mut payload = &frame[HEADER_55AA_LEN..crc_pos];
    // Device frames carry a 4-byte return code (0 on success, 1 when the
    // device rejects the request) ahead of the body; client frames do not.
    // Strip it only when it is a small value and the rest is still a whole
    // number of AES blocks, optionally behind the 15-byte version header, so
    // a client frame's ciphertext is never cut.
    if payload.len() >= 4
        && payload[..3] == [0, 0, 0]
        && matches!((payload.len() - 4) % 16, 0 | 15)
    {
        payload = &payload[4..];
    }
    if payload.starts_with(b"3.3") && payload.len() >= VERSION_HEADER_33.len() {
        payload = &payload[VERSION_HEADER_33.len()..];
    }
    // Empty, or protocol 3.1 plaintext JSON (discovery on UDP 6666).
    if payload.is_empty() || payload.starts_with(b"{") {
        return Ok((h, payload.to_vec()));
    }
    let plain = ecb_decrypt(key, payload)?;
    Ok((h, plain))
}

/// Remove the 4-byte return code a 6699 device frame carries and a client
/// frame does not. Getting this wrong shifts the session-key nonce.
pub fn strip_retcode(payload: &[u8]) -> &[u8] {
    if payload.len() >= 4 && !payload.starts_with(b"{") {
        &payload[4..]
    } else {
        payload
    }
}

fn ecb_encrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, FrameError> {
    use cipher::block_padding::Pkcs7;
    let enc = Ecb128Enc::new_from_slice(key).map_err(|e| FrameError::Crypto(e.to_string()))?;
    Ok(enc.encrypt_padded_vec_mut::<Pkcs7>(data))
}

fn ecb_decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, FrameError> {
    use cipher::block_padding::Pkcs7;
    let dec = Ecb128Dec::new_from_slice(key).map_err(|e| FrameError::Crypto(e.to_string()))?;
    dec.decrypt_padded_vec_mut::<Pkcs7>(data)
        .map_err(|e| FrameError::Crypto(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"0123456789abcdef";

    #[test]
    fn gcm_round_trip() {
        let iv = b"0123456789ab"; // 12 ASCII bytes, as a real IV would be
        let msg = br#"{"dps":{"1":true}}"#;
        let frame = pack_6699(KEY, iv, 1, CMD_IPC_LAN_302, msg).unwrap();
        assert_eq!(be32(&frame[0..4]), PREFIX_6699);
        let (h, plain) = unpack_6699(KEY, &frame).unwrap();
        assert_eq!(h.cmd, CMD_IPC_LAN_302);
        assert_eq!(h.total, frame.len());
        assert_eq!(&plain, msg);
    }

    #[test]
    fn classic_round_trip_dp_query() {
        let msg = br#"{"gwId":"abc","devId":"abc"}"#;
        let frame = pack_55aa(KEY, 1, CMD_DP_QUERY, msg).unwrap();
        assert_eq!(be32(&frame[0..4]), PREFIX_55AA);
        assert_eq!(be32(&frame[frame.len() - 4..]), SUFFIX_55AA);
        // No device return code on a client frame, so unpack sees the body
        // straight away once the (absent) retcode guard passes.
        let (h, plain) = unpack_55aa(KEY, &frame).unwrap();
        assert_eq!(h.cmd, CMD_DP_QUERY);
        assert_eq!(&plain, msg);
    }

    #[test]
    fn classic_strips_error_retcode() {
        // A device reply: return code 1, then the ECB body (the 22-char-id
        // "device22" quirk answers DP_QUERY this way).
        let body = ecb_encrypt(KEY, b"json obj data unvalid").unwrap();
        let mut frame = Vec::new();
        frame.extend_from_slice(&PREFIX_55AA.to_be_bytes());
        frame.extend_from_slice(&1u32.to_be_bytes());
        frame.extend_from_slice(&CMD_DP_QUERY.to_be_bytes());
        frame.extend_from_slice(&((4 + body.len() + 8) as u32).to_be_bytes());
        frame.extend_from_slice(&1u32.to_be_bytes());
        frame.extend_from_slice(&body);
        let crc = crc32fast::hash(&frame);
        frame.extend_from_slice(&crc.to_be_bytes());
        frame.extend_from_slice(&SUFFIX_55AA.to_be_bytes());
        let (_, plain) = unpack_55aa(KEY, &frame).unwrap();
        assert_eq!(plain, b"json obj data unvalid");
    }

    #[test]
    fn classic_rejects_bad_crc() {
        let mut frame = pack_55aa(KEY, 1, CMD_DP_QUERY, b"{\"x\":1}").unwrap();
        let n = frame.len();
        frame[n - 5] ^= 0xff; // corrupt last payload byte
        assert!(matches!(unpack_55aa(KEY, &frame), Err(FrameError::BadCrc)));
    }

    #[test]
    fn header_rejects_unknown_prefix() {
        assert!(matches!(
            parse_header(&[0, 0, 0, 1, 0, 0, 0, 0]),
            Err(FrameError::BadPrefix)
        ));
    }
}
