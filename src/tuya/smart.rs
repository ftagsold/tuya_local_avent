//! Tuya Smart web API (`protect-*.ismartlife.me`): email + password login,
//! device listing, and the per-device stream config.
//!
//! Works with Tuya Smart app accounts (not Smart Life, not vendor apps such as
//! Baby Monitor+, whose accounts live in another namespace). Needs no app
//! signing secrets. Ported from go2rtc `pkg/tuya/smart_api.go` and
//! `helper.go` (MIT, © AlexxIT).

use anyhow::{bail, Context, Result};
use md5::{Digest, Md5};
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use serde_json::{json, Value};

pub struct Region {
    pub name: &'static str,
    pub host: &'static str,
    pub continent: &'static str,
}

pub const REGIONS: &[Region] = &[
    Region { name: "eu-central", host: "protect-eu.ismartlife.me", continent: "EU" },
    Region { name: "eu-east", host: "protect-we.ismartlife.me", continent: "EU" },
    Region { name: "us-west", host: "protect-us.ismartlife.me", continent: "AZ" },
    Region { name: "us-east", host: "protect-ue.ismartlife.me", continent: "AZ" },
    Region { name: "china", host: "protect.ismartlife.me", continent: "AY" },
    Region { name: "india", host: "protect-in.ismartlife.me", continent: "IN" },
];

pub fn region(name: &str) -> Option<&'static Region> {
    REGIONS.iter().find(|r| r.name == name || r.host == name)
}

pub struct SmartClient {
    http: reqwest::Client,
    region: &'static Region,
}

impl SmartClient {
    pub fn new(region: &'static Region) -> Result<Self> {
        let http = reqwest::Client::builder()
            .cookie_store(true)
            .build()
            .context("http client")?;
        Ok(Self { http, region })
    }

    /// Log in and return the login result (holds `uid`, `ecode`, `domain`).
    pub async fn login(&self, email: &str, password: &str) -> Result<Value> {
        let token = self
            .post("/api/login/token", Some(json!({
                "countryCode": self.region.continent,
                "username": email,
                "isUid": false,
            })))
            .await
            .context("login token")?;
        let pb_key = token["pbKey"].as_str().context("login token: no pbKey")?;
        let token_id = token["token"].as_str().context("login token: no token")?;

        let mut req = json!({
            "countryCode": self.region.continent,
            "passwd": encrypt_rsa_pkcs1(password, pb_key)?,
            "token": token_id,
            "ifencrypt": 1,
            "options": r#"{"group":1}"#,
        });
        let path = if email.contains('@') {
            req["email"] = json!(email);
            "/api/private/email/login"
        } else {
            req["mobile"] = json!(email);
            "/api/private/phone/login"
        };
        self.post(path, Some(req)).await.context("login")
    }

    /// Every device in every home the account sees, as raw JSON objects.
    pub async fn devices(&self) -> Result<Vec<Value>> {
        let homes = self.post("/api/new/common/homeList", None).await.context("home list")?;
        let mut out = Vec::new();
        for home in homes.as_array().into_iter().flatten() {
            let home_id = home["gid"].to_string();
            let rooms = self
                .post("/api/new/common/roomList", Some(json!({ "homeId": home_id })))
                .await
                .context("room list")?;
            for room in rooms.as_array().into_iter().flatten() {
                out.extend(room["deviceList"].as_array().into_iter().flatten().cloned());
            }
        }
        Ok(out)
    }

    /// The stream config for one device (`/api/jarvis/config`), raw.
    pub async fn stream_config(&self, device_id: &str) -> Result<Value> {
        let trace = format!("{:x}", rand::random::<u64>());
        self.post(
            "/api/jarvis/config",
            Some(json!({ "devId": device_id, "clientTraceId": trace })),
        )
        .await
        .context("stream config")
    }

    /// POST JSON, check `success`, return `result`.
    async fn post(&self, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("https://{}{}", self.region.host, path);
        let mut req = self
            .http
            .post(&url)
            .header("Accept", "*/*")
            .header("Origin", format!("https://{}", self.region.host));
        if let Some(body) = body {
            req = req.json(&body);
        }
        let resp = req.send().await.with_context(|| format!("POST {path}"))?;
        let status = resp.status();
        let v: Value = resp.json().await.with_context(|| format!("POST {path}: HTTP {status}, not JSON"))?;
        if v["success"].as_bool() != Some(true) {
            let msg = v["errorMsg"].as_str().or(v["status"].as_str()).unwrap_or("unknown error");
            bail!("{path}: {msg}");
        }
        Ok(v["result"].clone())
    }
}

/// MD5-hex the password, then RSA-PKCS1v15 encrypt it under the server's
/// one-time public key, hex-encoded. Shared by the web and mobile logins.
pub fn encrypt_rsa_pkcs1(password: &str, pb_key: &str) -> Result<String> {
    let key = RsaPublicKey::from_public_key_pem(&pem_wrap(pb_key)).context("login public key")?;
    let hashed = hex::encode(Md5::digest(password.as_bytes()));
    let enc = key
        .encrypt(&mut rsa::rand_core::OsRng, Pkcs1v15Encrypt, hashed.as_bytes())
        .context("encrypt password")?;
    Ok(hex::encode(enc))
}

/// Encrypt the login password the way the mobile app does: build the RSA key
/// from the token's decimal `publicKey` (modulus) and `exponent`, encrypt
/// `hex(md5(password))` with RSA PKCS#1 v1.5, hex-encode. This differs from the
/// web path, which uses the `pbKey` SPKI; the mobile token's pbKey is a
/// different key, so using it yields USER_PASSWD_WRONG.
pub fn encrypt_login_rsa(password: &str, modulus_dec: &str, exponent_dec: &str) -> Result<String> {
    use rsa::BigUint;
    let n = BigUint::parse_bytes(modulus_dec.as_bytes(), 10)
        .ok_or_else(|| anyhow::anyhow!("modulus not a decimal integer"))?;
    let e = BigUint::parse_bytes(exponent_dec.as_bytes(), 10)
        .ok_or_else(|| anyhow::anyhow!("exponent not a decimal integer"))?;
    let key = RsaPublicKey::new(n, e).context("build RSA key from modulus/exponent")?;
    let hashed = hex::encode(Md5::digest(password.as_bytes()));
    let enc = key
        .encrypt(&mut rsa::rand_core::OsRng, Pkcs1v15Encrypt, hashed.as_bytes())
        .context("encrypt password")?;
    Ok(hex::encode(enc))
}

/// The server sends the key as one long base64 line. Go's PEM decoder takes
/// that; the strict RFC 7468 parser here needs 64-character lines.
fn pem_wrap(pb_key: &str) -> String {
    let b64: String = pb_key.chars().filter(|c| !c.is_whitespace()).collect();
    let mut pem = String::from("-----BEGIN PUBLIC KEY-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str("-----END PUBLIC KEY-----\n");
    pem
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::RsaPrivateKey;

    #[test]
    fn encrypts_with_a_single_line_server_key() {
        let private = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap();
        let pem = RsaPublicKey::from(&private).to_public_key_pem(LineEnding::LF).unwrap();
        // Strip the armour and join the lines, as the server sends it.
        let one_line: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();

        let enc = encrypt_rsa_pkcs1("secret", &one_line).unwrap();
        let plain = private.decrypt(Pkcs1v15Encrypt, &hex::decode(enc).unwrap()).unwrap();
        assert_eq!(plain, hex::encode(Md5::digest(b"secret")).as_bytes());
    }
}
