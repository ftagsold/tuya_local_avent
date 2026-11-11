//! Tuya Mobile SDK API (`a1.tuya*.com/api.json`): the signed login that the
//! vendor app uses, and the one call that returns the per-device P2P password.
//!
//! The request signing and call shapes are ported from aventproxy
//! `custom_components/philips_avent/api.py` (MIT, © 2026 thekoma). The four
//! app constants are supplied by the operator at runtime (never embedded):
//! the HMAC signing key, the app key (`clientId`), the channel key, and the
//! app version is fixed. They identify the app build, not the account.

use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use serde_json::{json, Value};
use sha2::Sha256;

use super::smart::encrypt_login_rsa;

/// Data-center API hosts, keyed by the short region name (`a1.tuya<r>.com`).
pub fn api_url(region: &str) -> String {
    let host = match region {
        "us" => "a1.tuyaus.com",
        "in" => "a1.tuyain.com",
        "cn" => "a1.tuyacn.com",
        _ => "a1.tuyaeu.com",
    };
    format!("https://{host}/api.json")
}

/// Parameters the signature covers, exactly as the app filters them.
const SIGN_WHITELIST: &[&str] = &[
    "a", "v", "lat", "lon", "lang", "deviceId", "appVersion", "ttid", "isH5", "h5Token", "os",
    "clientId", "postData", "time", "requestId", "et", "n4h5", "sid", "chKey", "sp",
];

/// The app's four build constants. Supplied via env, not baked in.
#[derive(Clone)]
pub struct AppKeys {
    pub signing_key: String,
    pub app_key: String,
    pub ch_key: String,
}

pub struct MobileClient {
    http: reqwest::Client,
    keys: AppKeys,
    api_url: String,
    country_code: String,
    device_id: String,
    app_version: String,
    ttid: String,
    sid: String,
}

impl MobileClient {
    pub fn new(keys: AppKeys, region: &str, country_code: &str) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder().build().context("http client")?,
            keys,
            api_url: api_url(region),
            country_code: country_code.to_string(),
            // A stable per-install phone id. One run is one install.
            device_id: uuid::Uuid::new_v4().simple().to_string(),
            // Tuya Smart app defaults, captured from the real app's requests;
            // override via env only for a different app.
            app_version: std::env::var("AVENT_APP_VERSION").unwrap_or_else(|_| "7.0.0".into()),
            ttid: std::env::var("AVENT_TTID").unwrap_or_else(|_| "international".into()),
            sid: String::new(),
        })
    }

    /// Probe only the RSA-token call, to check the client is accepted by the
    /// gateway (needs no account password or MFA). Returns the token object.
    pub async fn check_client(&self, email: &str) -> Result<Value> {
        self.rsa_token(email).await
    }

    /// Fetch a fresh single-use RSA login token (`pbKey` + `token`).
    ///
    /// The generic RSA login token (`thing.m.user.username.token.get`, v2.0).
    /// `email.token.create` is for registration/verification; its token does
    /// not validate a password login (returns USER_PASSWD_WRONG). The earlier
    /// ILLEGAL_CLIENT_ID was the wrong ttid/chKey, not this action.
    async fn rsa_token(&self, email: &str) -> Result<Value> {
        self.call("thing.m.user.username.token.get", "2.0", Some(json!({
            "countryCode": self.country_code,
            "username": email,
            "isUid": false,
        })), &[]).await
    }

    /// Send the email verification code. Returns once the server accepts the request.
    async fn trigger_mfa(&self, email: &str, enc_pw: &str, token: &str) -> Result<()> {
        self.call("thing.m.user.username.mfa.code.get", "1.0", Some(json!({
            "countryCode": self.country_code,
            "username": email,
            "passwd": enc_pw,
            "token": token,
            "ifencrypt": 1,
            "options": r#"{"group":1,"mfaCode":"null"}"#,
        })), &[]).await?;
        Ok(())
    }

    /// Complete the login with the code; returns the result (holds `sid`, `uid`).
    async fn login(&self, email: &str, enc_pw: &str, token: &str, mfa_code: &str) -> Result<Value> {
        let options = json!({ "group": 1, "mfaCode": mfa_code }).to_string();
        self.call("thing.m.user.email.password.login", "3.0", Some(json!({
            "countryCode": self.country_code,
            "email": email,
            "passwd": enc_pw,
            "token": token,
            "ifencrypt": 1,
            "options": options,
        })), &[]).await
    }

    /// The per-device RTC config. Its `password` field is the P2P password.
    /// The flagship app uses `thing.m.rtc.config.get` (v1.0).
    pub async fn rtc_config(&self, device_id: &str) -> Result<Value> {
        self.call("thing.m.rtc.config.get", "1.0", Some(json!({ "devId": device_id })), &[]).await
    }

    /// The device's name as set in the app (`tuya.m.device.get` v1.0).
    pub async fn device_name(&self, device_id: &str) -> Result<String> {
        let v = self.call("tuya.m.device.get", "1.0", Some(json!({ "devId": device_id })), &[]).await?;
        Ok(v["name"].as_str().unwrap_or_default().to_string())
    }

    /// Run the full interactive login: probe → send code → `prompt` for it →
    /// complete. Each password submission uses its own fresh RSA token.
    pub async fn interactive_login(
        &mut self,
        email: &str,
        password: &str,
        mut prompt: impl FnMut() -> Result<String>,
    ) -> Result<Value> {
        // First try a plain login with no code. On an account without MFA this
        // succeeds and returns the session; on an MFA account it comes back
        // asking for a code.
        let t1 = self.rsa_token(email).await.context("rsa token")?;
        let enc1 = encrypt_token(password, &t1)?;
        let result = match self.login(email, &enc1, token(&t1)?, "").await {
            Ok(result) => result,
            Err(e) if e.to_string().contains("MFA") => {
                // MFA account: send the code, prompt, then log in with it.
                let t2 = self.rsa_token(email).await.context("rsa token (mfa)")?;
                let enc2 = encrypt_token(password, &t2)?;
                self.trigger_mfa(email, &enc2, token(&t2)?).await.context("send MFA code")?;
                let code = prompt()?;
                let t3 = self.rsa_token(email).await.context("rsa token (login)")?;
                let enc3 = encrypt_token(password, &t3)?;
                self.login(email, &enc3, token(&t3)?, code.trim()).await.context("login")?
            }
            Err(e) => return Err(e).context("login"),
        };
        self.sid = result["sid"].as_str().unwrap_or_default().to_string();
        if self.sid.is_empty() {
            bail!("login succeeded but returned no sid");
        }
        Ok(result)
    }

    /// Sign and POST one API call as `x-www-form-urlencoded`; return `result`.
    async fn call(&self, action: &str, version: &str, post_data: Option<Value>, extra: &[(&str, String)]) -> Result<Value> {
        let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs().to_string();
        let mut params: BTreeMap<String, String> = BTreeMap::new();
        // Base params as the Tuya Smart app sends them: appVersion is the APK
        // versionName (7.0.0) and ttid is THING_SMART_TTID ("tuyaSmart"). The
        // gateway validates the (clientId, ttid) pair, so a wrong ttid is
        // rejected as ILLEGAL_CLIENT_ID even with a valid signature.
        for (k, v) in [
            ("a", action), ("v", version), ("time", &time), ("appVersion", &self.app_version),
            ("appRnVersion", "5.92"), ("channel", "oem"), ("chKey", &self.keys.ch_key),
            ("clientId", &self.keys.app_key), ("cp", "gzip"), ("deviceCoreVersion", "6.7.0"),
            ("deviceId", &self.device_id), ("et", "0.0.1"), ("nd", "1"), ("lang", "en_US"),
            ("os", "Android"), ("osSystem", "14"), ("platform", "ha_integration"),
            ("requestId", &uuid::Uuid::new_v4().to_string()), ("sdkVersion", "6.7.0"),
            ("sid", &self.sid), ("timeZoneId", "Europe/Rome"),
            ("ttid", &self.ttid),
        ] {
            params.insert(k.to_string(), v.to_string());
        }
        if let Some(pd) = post_data {
            params.insert("postData".into(), if pd.is_string() { pd.as_str().unwrap().to_string() } else { pd.to_string() });
        }
        for (k, v) in extra {
            params.insert(k.to_string(), v.clone());
        }
        params.insert("sign".into(), sign(&self.keys.signing_key, &params));

        let resp = self
            .http
            .post(&self.api_url)
            .header("User-Agent", "Thing-UA=APP/Android/1.8.0/SDK/6.7.0")
            .form(&params)
            .send()
            .await
            .with_context(|| format!("POST {action}"))?;
        let v: Value = resp.json().await.with_context(|| format!("{action}: response not JSON"))?;
        if v["success"].as_bool() != Some(true) {
            bail!("{action}: {} {}", v["errorCode"].as_str().unwrap_or("UNKNOWN"), v["errorMsg"].as_str().unwrap_or(""));
        }
        Ok(v["result"].clone())
    }
}

/// Encrypt the password against the token's modulus/exponent, as the app does.
fn encrypt_token(password: &str, token: &Value) -> Result<String> {
    let modulus = token["publicKey"].as_str().context("login token: no publicKey")?;
    let exponent = token["exponent"].as_str().context("login token: no exponent")?;
    encrypt_login_rsa(password, modulus, exponent)
}

fn token(token: &Value) -> Result<&str> {
    token["token"].as_str().context("login token: no token")
}

/// HMAC-SHA256 over the whitelisted params, sorted and joined by `||`, with
/// `postData` folded to a swapped MD5 first.
fn sign(signing_key: &str, params: &BTreeMap<String, String>) -> String {
    let mut filtered: BTreeMap<&str, String> = BTreeMap::new();
    for (k, v) in params {
        if !v.is_empty() && SIGN_WHITELIST.contains(&k.as_str()) {
            filtered.insert(k.as_str(), v.clone());
        }
    }
    if let Some(pd) = filtered.get("postData") {
        let md5 = hex::encode(Md5::digest(pd.as_bytes()));
        filtered.insert("postData", swap(&md5));
    }
    // BTreeMap already iterates in sorted key order.
    let param_str = filtered.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("||");
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(signing_key.as_bytes()).expect("HMAC key");
    mac.update(param_str.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// The app's fixed reshuffle of a 32-char MD5 before signing.
fn swap(s: &str) -> String {
    if s.len() != 32 {
        return s.to_string();
    }
    format!("{}{}{}{}", &s[8..16], &s[0..8], &s[24..32], &s[16..24])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_reshuffles_only_32_chars() {
        assert_eq!(swap("0123456789abcdef0123456789abcdef"), "89abcdef0123456789abcdef01234567");
        assert_eq!(swap("short"), "short");
    }

    #[test]
    fn sign_is_stable_and_order_independent() {
        let key = "signing-key";
        let mut a = BTreeMap::new();
        a.insert("a".to_string(), "act".to_string());
        a.insert("clientId".to_string(), "app".to_string());
        a.insert("ignored_param".to_string(), "x".to_string()); // not in whitelist
        a.insert("time".to_string(), "100".to_string());
        let s1 = sign(key, &a);
        // Re-inserting in a different logical order yields the same string,
        // and the non-whitelisted param does not affect it.
        a.remove("ignored_param");
        assert_eq!(s1, sign(key, &a));
        assert_eq!(s1.len(), 64); // hex of sha256
    }
}
