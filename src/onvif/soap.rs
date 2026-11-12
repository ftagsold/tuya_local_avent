//! SOAP request classification and response templates for the ONVIF server.

use super::Config;

/// The ONVIF operation: local name of the first element inside the SOAP
/// Body (`<trt:GetServiceCapabilities/>` -> `GetServiceCapabilities`).
/// Exact, so one operation's name inside another's never misroutes it.
pub fn operation(body: &str) -> String {
    let after_body = body
        .match_indices('<')
        .find_map(|(i, _)| {
            let tag = &body[i + 1..];
            let name = tag.split(|c: char| c.is_whitespace() || c == '>' || c == '/').next()?;
            (name.rsplit(':').next() == Some("Body")).then(|| &body[i + 1 + name.len()..])
        });
    let Some(rest) = after_body else { return "Unknown".into() };
    let Some(open) = rest.find('<') else { return "Unknown".into() };
    let name = rest[open + 1..].split(|c: char| c.is_whitespace() || c == '>' || c == '/').next().unwrap_or("");
    let local = name.rsplit(':').next().unwrap_or("");
    if local.is_empty() { "Unknown".into() } else { local.to_string() }
}

/// `authed`: the request carried a valid WS-Security token. Without one the
/// stream URI comes back without embedded credentials and the snapshot URI
/// without its access token.
/// `service`: the request path (`/onvif/device_service`, `/onvif/media_service`),
/// which picks the answer for per-service calls like GetServiceCapabilities.
/// `None`: not supported, answered with an ActionNotSupported fault.
pub fn dispatch(op: &str, service: &str, cfg: &Config, authed: bool) -> Option<String> {
    Some(match op {
        "GetSystemDateAndTime" => envelope(&system_date()),
        "GetDeviceInformation" => envelope(&device_info(cfg)),
        "GetCapabilities" => envelope(&capabilities(cfg)),
        "GetServices" => envelope(&services(cfg)),
        "GetScopes" => envelope(&scopes(cfg)),
        "GetVideoSources" => envelope(&video_sources()),
        "GetProfiles" => envelope(&profiles(cfg)),
        "GetProfile" => envelope(&profile(cfg)),
        "GetStreamUri" => envelope(&stream_uri(cfg, authed)),
        "GetSnapshotUri" => envelope(&snapshot_uri(cfg, authed)),
        "GetVideoEncoderConfigurations" => envelope(&video_encoders()),
        "GetVideoSourceConfigurations" => envelope(&video_source_configs()),
        "GetAudioSources" => envelope(&audio_sources()),
        "GetAudioSourceConfigurations" => envelope(&audio_source_configs()),
        "GetAudioEncoderConfigurations" => envelope(&audio_encoders()),
        "GetNetworkInterfaces" => envelope(&network_interfaces(cfg)),
        "GetHostname" => envelope(&hostname(cfg)),
        "GetServiceCapabilities" if service.contains("media") => envelope(MEDIA_CAPABILITIES),
        "GetServiceCapabilities" if service.contains("device") => envelope(DEVICE_CAPABILITIES),
        _ => return None,
    })
}

/// SOAP fault for an operation this server does not implement.
pub fn not_supported_fault() -> String {
    envelope(
        "<s:Fault><s:Code><s:Value>s:Receiver</s:Value><s:Subcode>\
<s:Value>ter:ActionNotSupported</s:Value></s:Subcode></s:Code>\
<s:Reason><s:Text xml:lang=\"en\">Optional Action Not Implemented</s:Text></s:Reason></s:Fault>",
    )
}

const MEDIA_CAPABILITIES: &str = "<trt:GetServiceCapabilitiesResponse>\
<trt:Capabilities SnapshotUri=\"true\" Rotation=\"false\" VideoSourceMode=\"false\" OSD=\"false\">\
<trt:ProfileCapabilities MaximumNumberOfProfiles=\"1\"/>\
<trt:StreamingCapabilities RTPMulticast=\"false\" RTP_TCP=\"true\" RTP_RTSP_TCP=\"true\" NonAggregateControl=\"false\" NoRTSPStreaming=\"false\"/>\
</trt:Capabilities></trt:GetServiceCapabilitiesResponse>";

const DEVICE_CAPABILITIES: &str = "<tds:GetServiceCapabilitiesResponse><tds:Capabilities>\
<tds:Network IPFilter=\"false\" ZeroConfiguration=\"false\" IPVersion6=\"false\" DynDNS=\"false\"/>\
<tds:Security UsernameToken=\"true\" HttpDigest=\"false\"/>\
<tds:System DiscoveryResolve=\"false\" DiscoveryBye=\"false\" RemoteDiscovery=\"false\" SystemBackup=\"false\" \
SystemLogging=\"false\" FirmwareUpgrade=\"false\"/>\
</tds:Capabilities></tds:GetServiceCapabilitiesResponse>";

/// One enabled interface carrying the bridge's stable MAC: Home Assistant's
/// ONVIF integration keys the camera device on it, and the MQTT device
/// announces the same MAC, so both land on one device.
fn network_interfaces(cfg: &Config) -> String {
    format!(
        "<tds:GetNetworkInterfacesResponse><tds:NetworkInterfaces token=\"eth0\">\
<tt:Enabled>true</tt:Enabled><tt:Info><tt:Name>eth0</tt:Name><tt:HwAddress>{mac}</tt:HwAddress><tt:MTU>1500</tt:MTU></tt:Info>\
<tt:IPv4><tt:Enabled>true</tt:Enabled><tt:Config><tt:Manual><tt:Address>{ip}</tt:Address><tt:PrefixLength>24</tt:PrefixLength></tt:Manual>\
<tt:DHCP>false</tt:DHCP></tt:Config></tt:IPv4></tds:NetworkInterfaces></tds:GetNetworkInterfacesResponse>",
        mac = xml(&cfg.mac),
        ip = xml(&cfg.host_ip),
    )
}

fn hostname(cfg: &Config) -> String {
    format!(
        "<tds:GetHostnameResponse><tds:HostnameInformation><tt:FromDHCP>false</tt:FromDHCP>\
<tt:Name>{}</tt:Name></tds:HostnameInformation></tds:GetHostnameResponse>",
        xml(&crate::media::slug(&cfg.device_name, "avent"))
    )
}

/// Validate a WS-Security UsernameToken against the configured credentials.
/// Returns true when there is no token (Protect's early, unauthenticated
/// probes) or when the digest matches. A present-but-wrong token returns false.
pub fn wsse_ok(body: &str, user: &str, pass: &str) -> bool {
    wsse_check(body, user, pass) != Some(false)
}

/// `None` when the request carries no token, else whether it is valid.
pub fn wsse_check(body: &str, user: &str, pass: &str) -> Option<bool> {
    let username = elem_text(body, "Username")?;
    Some(ct_eq(&username, user) && wsse_digest_ok(body, pass))
}

/// Constant-time string comparison for secrets (timing does not reveal how
/// many leading bytes matched).
pub fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn wsse_digest_ok(body: &str, pass: &str) -> bool {
    let supplied = elem_text(body, "Password").unwrap_or_default();
    let nonce_b64 = elem_text(body, "Nonce").unwrap_or_default();
    let created = elem_text(body, "Created").unwrap_or_default();
    if nonce_b64.is_empty() {
        // PasswordText form.
        return ct_eq(&supplied, pass);
    }
    use base64::Engine;
    use sha1::{Digest, Sha1};
    let Ok(nonce) = base64::engine::general_purpose::STANDARD.decode(nonce_b64.trim()) else {
        return false;
    };
    let mut h = Sha1::new();
    h.update(&nonce);
    h.update(created.trim().as_bytes());
    h.update(pass.as_bytes());
    let digest = base64::engine::general_purpose::STANDARD.encode(h.finalize());
    ct_eq(&digest, supplied.trim())
}

/// Pull the WS-Addressing MessageID so a ProbeMatch can relate to it.
pub fn extract_message_id(msg: &str) -> String {
    between(msg, "MessageID>", "</").unwrap_or_default()
}

fn between(s: &str, start: &str, end: &str) -> Option<String> {
    let i = s.find(start)? + start.len();
    let j = s[i..].find(end)? + i;
    Some(s[i..j].trim().to_string())
}

/// Text content of an XML element by local name, tolerant of a namespace
/// prefix (`wsse:Name`) and attributes (`<Created xmlns="...">`). Matches the
/// element only when `name` is a whole tag token — so `Username` does not
/// match `UsernameToken` — then returns the text up to the next `<`.
fn elem_text(body: &str, name: &str) -> Option<String> {
    let bytes = body.as_bytes();
    let mut from = 0;
    while let Some(rel) = body[from..].find(name) {
        let abs = from + rel;
        from = abs + name.len();
        let before = if abs > 0 { bytes[abs - 1] } else { b'<' };
        let after = bytes.get(abs + name.len()).copied().unwrap_or(b'<');
        // A real element start: preceded by '<' or a prefix ':', and followed
        // by '>', whitespace, or '/'.
        if !matches!(before, b'<' | b':') || !matches!(after, b'>' | b' ' | b'\t' | b'/' | b'\r' | b'\n') {
            continue;
        }
        let gt = body[abs..].find('>')? + abs;
        let start = gt + 1;
        let lt = body[start..].find('<')? + start;
        return Some(body[start..lt].trim().to_string());
    }
    None
}

const NS: &str = concat!(
    "xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\" ",
    "xmlns:tt=\"http://www.onvif.org/ver10/schema\" ",
    "xmlns:tds=\"http://www.onvif.org/ver10/device/wsdl\" ",
    "xmlns:trt=\"http://www.onvif.org/ver10/media/wsdl\" ",
    "xmlns:ter=\"http://www.onvif.org/ver10/error\""
);

fn envelope(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<s:Envelope {NS}><s:Body>{body}</s:Body></s:Envelope>"
    )
}

fn system_date() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    // Crude UTC breakdown; ONVIF clients mainly need a plausible current time.
    let days = now / 86400;
    let secs = now % 86400;
    let (h, mi, se) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!(
        "<tds:GetSystemDateAndTimeResponse><tds:SystemDateAndTime>\
<tt:DateTimeType>Manual</tt:DateTimeType><tt:DaylightSavings>false</tt:DaylightSavings>\
<tt:UTCDateTime><tt:Time><tt:Hour>{h}</tt:Hour><tt:Minute>{mi}</tt:Minute><tt:Second>{se}</tt:Second></tt:Time>\
<tt:Date><tt:Year>{y}</tt:Year><tt:Month>{mo}</tt:Month><tt:Day>{d}</tt:Day></tt:Date></tt:UTCDateTime>\
</tds:SystemDateAndTime></tds:GetSystemDateAndTimeResponse>"
    )
}

fn device_info(cfg: &Config) -> String {
    format!(
        "<tds:GetDeviceInformationResponse>\
<tds:Manufacturer>Philips Avent</tds:Manufacturer>\
<tds:Model>{}</tds:Model>\
<tds:FirmwareVersion>1.0</tds:FirmwareVersion>\
<tds:SerialNumber>{}</tds:SerialNumber>\
<tds:HardwareId>avent-{}</tds:HardwareId>\
</tds:GetDeviceInformationResponse>",
        xml(&cfg.model), xml(&cfg.device_id), xml(&cfg.device_id)
    )
}

fn device_xaddr(cfg: &Config) -> String {
    format!("http://{}:{}/onvif/device_service", cfg.host_ip, cfg.onvif_port)
}
fn media_xaddr(cfg: &Config) -> String {
    format!("http://{}:{}/onvif/media_service", cfg.host_ip, cfg.onvif_port)
}

fn capabilities(cfg: &Config) -> String {
    format!(
        "<tds:GetCapabilitiesResponse><tds:Capabilities>\
<tt:Device><tt:XAddr>{}</tt:XAddr></tt:Device>\
<tt:Media><tt:XAddr>{}</tt:XAddr><tt:StreamingCapabilities><tt:RTPMulticast>false</tt:RTPMulticast>\
<tt:RTP_TCP>true</tt:RTP_TCP><tt:RTP_RTSP_TCP>true</tt:RTP_RTSP_TCP></tt:StreamingCapabilities></tt:Media>\
</tds:Capabilities></tds:GetCapabilitiesResponse>",
        device_xaddr(cfg), media_xaddr(cfg)
    )
}

fn services(cfg: &Config) -> String {
    format!(
        "<tds:GetServicesResponse>\
<tds:Service><tds:Namespace>http://www.onvif.org/ver10/device/wsdl</tds:Namespace>\
<tds:XAddr>{}</tds:XAddr><tds:Version><tt:Major>2</tt:Major><tt:Minor>5</tt:Minor></tds:Version></tds:Service>\
<tds:Service><tds:Namespace>http://www.onvif.org/ver10/media/wsdl</tds:Namespace>\
<tds:XAddr>{}</tds:XAddr><tds:Version><tt:Major>2</tt:Major><tt:Minor>5</tt:Minor></tds:Version></tds:Service>\
</tds:GetServicesResponse>",
        device_xaddr(cfg), media_xaddr(cfg)
    )
}

fn scopes(_cfg: &Config) -> String {
    "<tds:GetScopesResponse>\
<tds:Scopes><tt:ScopeDef>Fixed</tt:ScopeDef><tt:ScopeItem>onvif://www.onvif.org/Profile/Streaming</tt:ScopeItem></tds:Scopes>\
<tds:Scopes><tt:ScopeDef>Fixed</tt:ScopeDef><tt:ScopeItem>onvif://www.onvif.org/type/video_encoder</tt:ScopeItem></tds:Scopes>\
</tds:GetScopesResponse>".to_string()
}

fn video_sources() -> String {
    "<trt:GetVideoSourcesResponse><trt:VideoSources token=\"vs0\">\
<tt:Framerate>25</tt:Framerate><tt:Resolution><tt:Width>1280</tt:Width><tt:Height>720</tt:Height></tt:Resolution>\
</trt:VideoSources></trt:GetVideoSourcesResponse>".to_string()
}

fn video_source_configs() -> String {
    "<trt:GetVideoSourceConfigurationsResponse><trt:Configurations token=\"vsc0\">\
<tt:Name>vsc0</tt:Name><tt:UseCount>1</tt:UseCount><tt:SourceToken>vs0</tt:SourceToken>\
<tt:Bounds x=\"0\" y=\"0\" width=\"1280\" height=\"720\"/></trt:Configurations>\
</trt:GetVideoSourceConfigurationsResponse>".to_string()
}

fn video_encoders() -> String {
    format!("<trt:GetVideoEncoderConfigurationsResponse>{}</trt:GetVideoEncoderConfigurationsResponse>", encoder_cfg())
}

fn encoder_cfg() -> String {
    "<trt:Configurations token=\"vec0\"><tt:Name>vec0</tt:Name><tt:UseCount>1</tt:UseCount>\
<tt:Encoding>H264</tt:Encoding><tt:Resolution><tt:Width>1280</tt:Width><tt:Height>720</tt:Height></tt:Resolution>\
<tt:Quality>4</tt:Quality><tt:RateControl><tt:FrameRateLimit>25</tt:FrameRateLimit><tt:EncodingInterval>1</tt:EncodingInterval>\
<tt:BitrateLimit>2048</tt:BitrateLimit></tt:RateControl><tt:H264><tt:GovLength>20</tt:GovLength><tt:H264Profile>Main</tt:H264Profile></tt:H264>\
</trt:Configurations>".to_string()
}

/// Shared encoder/multicast blocks. Element order follows tt:Profile:
/// VideoSource, AudioSource, VideoEncoder, AudioEncoder.
const MULTICAST: &str = "<tt:Multicast><tt:Address><tt:Type>IPv4</tt:Type><tt:IPv4Address>0.0.0.0</tt:IPv4Address></tt:Address>\
<tt:Port>0</tt:Port><tt:TTL>0</tt:TTL><tt:AutoStart>false</tt:AutoStart></tt:Multicast>";

fn audio_source_cfg_inner() -> &'static str {
    "<tt:Name>asc0</tt:Name><tt:UseCount>1</tt:UseCount><tt:SourceToken>as0</tt:SourceToken>"
}

/// The relay's audio: AAC-LC, mono, 48 kHz, 64 kbit/s.
fn audio_encoder_cfg_inner() -> String {
    format!(
        "<tt:Name>aec0</tt:Name><tt:UseCount>1</tt:UseCount><tt:Encoding>AAC</tt:Encoding>\
<tt:Bitrate>64</tt:Bitrate><tt:SampleRate>48</tt:SampleRate>{MULTICAST}<tt:SessionTimeout>PT60S</tt:SessionTimeout>"
    )
}

fn audio_sources() -> String {
    "<trt:GetAudioSourcesResponse><trt:AudioSources token=\"as0\"><tt:Channels>1</tt:Channels></trt:AudioSources>\
</trt:GetAudioSourcesResponse>".to_string()
}

fn audio_source_configs() -> String {
    format!(
        "<trt:GetAudioSourceConfigurationsResponse><trt:Configurations token=\"asc0\">{}</trt:Configurations>\
</trt:GetAudioSourceConfigurationsResponse>",
        audio_source_cfg_inner()
    )
}

fn audio_encoders() -> String {
    format!(
        "<trt:GetAudioEncoderConfigurationsResponse><trt:Configurations token=\"aec0\">{}</trt:Configurations>\
</trt:GetAudioEncoderConfigurationsResponse>",
        audio_encoder_cfg_inner()
    )
}

/// The one profile: video + AAC audio, and a stream URI that carries the
/// talkback backchannel. Protect and Home Assistant both use it.
fn profile_body(cfg: &Config) -> String {
    format!(
        "<tt:Name>{name}</tt:Name>\
<tt:VideoSourceConfiguration token=\"vsc0\"><tt:Name>vsc0</tt:Name><tt:UseCount>1</tt:UseCount>\
<tt:SourceToken>vs0</tt:SourceToken><tt:Bounds x=\"0\" y=\"0\" width=\"1280\" height=\"720\"/></tt:VideoSourceConfiguration>\
<tt:AudioSourceConfiguration token=\"asc0\">{asc}</tt:AudioSourceConfiguration>\
<tt:VideoEncoderConfiguration token=\"vec0\"><tt:Name>vec0</tt:Name><tt:UseCount>1</tt:UseCount>\
<tt:Encoding>H264</tt:Encoding><tt:Resolution><tt:Width>1280</tt:Width><tt:Height>720</tt:Height></tt:Resolution>\
<tt:Quality>4</tt:Quality><tt:RateControl><tt:FrameRateLimit>25</tt:FrameRateLimit><tt:EncodingInterval>1</tt:EncodingInterval>\
<tt:BitrateLimit>2048</tt:BitrateLimit></tt:RateControl><tt:H264><tt:GovLength>50</tt:GovLength><tt:H264Profile>Main</tt:H264Profile></tt:H264>\
{MULTICAST}<tt:SessionTimeout>PT60S</tt:SessionTimeout></tt:VideoEncoderConfiguration>\
<tt:AudioEncoderConfiguration token=\"aec0\">{aec}</tt:AudioEncoderConfiguration>",
        name = xml(&cfg.device_name),
        asc = audio_source_cfg_inner(),
        aec = audio_encoder_cfg_inner(),
    )
}

fn profiles(cfg: &Config) -> String {
    format!("<trt:GetProfilesResponse><trt:Profiles token=\"profile0\" fixed=\"true\">{}</trt:Profiles></trt:GetProfilesResponse>", profile_body(cfg))
}

fn profile(cfg: &Config) -> String {
    format!("<trt:GetProfileResponse><trt:Profile token=\"profile0\" fixed=\"true\">{}</trt:Profile></trt:GetProfileResponse>", profile_body(cfg))
}

/// `?backchannel=1` makes go2rtc offer the talkback track; clients that do
/// not talk ignore it.
fn stream_uri(cfg: &Config, authed: bool) -> String {
    let base = if authed { &cfg.stream_uri } else { &cfg.stream_uri_plain };
    let uri = xml(&format!("{base}?backchannel=1"));
    format!(
        "<trt:GetStreamUriResponse><trt:MediaUri><tt:Uri>{uri}</tt:Uri>\
<tt:InvalidAfterConnect>false</tt:InvalidAfterConnect><tt:InvalidAfterReboot>false</tt:InvalidAfterReboot>\
<tt:Timeout>PT30S</tt:Timeout></trt:MediaUri></trt:GetStreamUriResponse>"
    )
}

fn snapshot_uri(cfg: &Config, authed: bool) -> String {
    let mut uri = format!("http://{}:{}/snapshot", cfg.host_ip, cfg.onvif_port);
    if authed {
        uri.push_str(&format!("?t={}", super::snapshot_token(cfg)));
    }
    format!(
        "<trt:GetSnapshotUriResponse><trt:MediaUri><tt:Uri>{}</tt:Uri>\
<tt:InvalidAfterConnect>false</tt:InvalidAfterConnect><tt:InvalidAfterReboot>false</tt:InvalidAfterReboot>\
<tt:Timeout>PT30S</tt:Timeout></trt:MediaUri></trt:GetSnapshotUriResponse>",
        xml(&uri)
    )
}

/// Escape text for an XML element or attribute.
pub fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

pub fn auth_fault() -> String {
    envelope(
        "<s:Fault><s:Code><s:Value>s:Sender</s:Value><s:Subcode>\
<s:Value>ter:NotAuthorized</s:Value></s:Subcode></s:Code>\
<s:Reason><s:Text xml:lang=\"en\">Authentication failed</s:Text></s:Reason></s:Fault>",
    )
}

pub fn probe_match(cfg: &Config, relates_to: &str) -> String {
    let xaddr = device_xaddr(cfg);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\" \
xmlns:a=\"http://schemas.xmlsoap.org/ws/2004/08/addressing\" \
xmlns:d=\"http://schemas.xmlsoap.org/ws/2005/04/discovery\" \
xmlns:dn=\"http://www.onvif.org/ver10/network/wsdl\">\
<s:Header><a:MessageID>urn:uuid:{id}</a:MessageID>\
<a:RelatesTo>{relates}</a:RelatesTo>\
<a:To>http://schemas.xmlsoap.org/ws/2004/08/addressing/role/anonymous</a:To>\
<a:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/ProbeMatches</a:Action></s:Header>\
<s:Body><d:ProbeMatches><d:ProbeMatch>\
<a:EndpointReference><a:Address>urn:uuid:avent-{dev}</a:Address></a:EndpointReference>\
<d:Types>dn:NetworkVideoTransmitter</d:Types>\
<d:Scopes>onvif://www.onvif.org/Profile/Streaming onvif://www.onvif.org/name/{name}</d:Scopes>\
<d:XAddrs>{xaddr}</d:XAddrs><d:MetadataVersion>1</d:MetadataVersion>\
</d:ProbeMatch></d:ProbeMatches></s:Body></s:Envelope>",
        id = xml(&cfg.device_id),
        dev = xml(&cfg.device_id),
        // From the probe (untrusted): escaped.
        relates = xml(relates_to),
        // Scope items are URIs: keep RFC 3986 unreserved chars only.
        name = cfg.device_name.chars().map(|c| if c.is_ascii_alphanumeric() || "-._~".contains(c) { c } else { '_' }).collect::<String>(),
    )
}


/// Days since the Unix epoch to a (year, month, day) civil date (Howard
/// Hinnant's algorithm). Used only to fill GetSystemDateAndTime.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            host_ip: "192.168.1.90".into(), onvif_port: 8000,
            stream_uri: "rtsp://avent:pw@192.168.1.90:8554/Nursery".into(),
            stream_uri_plain: "rtsp://192.168.1.90:8554/Nursery".into(),
            snapshot_src: String::new(),
            user: "avent".into(), pass: "secret".into(),
            device_name: "Nursery <3".into(), model: "Nursery <3".into(), mac: super::super::device_mac("dev1"), device_id: "dev1".into(),
        }
    }

    #[test]
    fn stream_uri_creds_only_when_authed() {
        let r = dispatch("GetStreamUri", "", &cfg(), true).unwrap();
        assert!(r.contains("rtsp://avent:pw@192.168.1.90:8554/Nursery"));
        let r = dispatch("GetStreamUri", "", &cfg(), false).unwrap();
        assert!(r.contains("<tt:Uri>rtsp://192.168.1.90:8554/Nursery?backchannel=1</tt:Uri>"));
        let r = dispatch("GetSnapshotUri", "", &cfg(), false).unwrap();
        assert!(r.contains("<tt:Uri>http://192.168.1.90:8000/snapshot</tt:Uri>"));
        let r = dispatch("GetSnapshotUri", "", &cfg(), true).unwrap();
        assert!(r.contains("/snapshot?t="));
    }

    #[test]
    fn profile_has_aac_audio_and_escaped_name() {
        let r = dispatch("GetProfiles", "", &cfg(), false).unwrap();
        assert!(r.contains("<tt:Encoding>AAC</tt:Encoding>") && r.contains("AudioSourceConfiguration"));
        assert!(r.contains("<tt:Name>Nursery &lt;3</tt:Name>"));
        assert!(!r.contains("Nursery <3"));
    }

    #[test]
    fn profiles_advertise_h264_720p() {
        let r = dispatch("GetProfiles", "", &cfg(), false).unwrap();
        assert!(r.contains("H264") && r.contains("<tt:Width>1280</tt:Width>"));
    }

    #[test]
    fn probe_match_escapes_message_id() {
        let m = probe_match(&cfg(), "</a:RelatesTo><evil/>");
        assert!(!m.contains("<evil/>"));
        assert!(m.contains("&lt;/a:RelatesTo&gt;&lt;evil/&gt;"));
    }

    #[test]
    fn ct_eq_compares() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
    }

    #[test]
    fn probe_match_is_wellformed() {
        let r = probe_match(&cfg(), "urn:uuid:abc");
        assert!(r.contains("ProbeMatches") && r.contains("NetworkVideoTransmitter"));
        assert!(r.contains("http://192.168.1.90:8000/onvif/device_service"));
        assert!(r.contains("<a:RelatesTo>urn:uuid:abc</a:RelatesTo>"));
    }

    #[test]
    fn wsse_no_token_allowed_wrong_rejected() {
        assert!(wsse_ok("<s:Body/>", "avent", "secret")); // no token: let probes through
        let bad = "<Username>avent</Username><Password Type=\"x#PasswordText\">nope</Password>";
        assert!(!wsse_ok(bad, "avent", "secret"));
        let good = "<Username>avent</Username><Password>secret</Password>";
        assert!(wsse_ok(good, "avent", "secret"));
    }

    #[test]
    fn wsse_protect_digest_token() {
        // Protect's exact shape: UsernameToken wrapper, attributes on Created,
        // no namespace prefix. Digest = base64(sha1(nonce + created + pass)).
        use base64::Engine;
        use sha1::{Digest, Sha1};
        let b64 = base64::engine::general_purpose::STANDARD;
        let nonce = b"0123456789abcdef";
        let created = "2026-11-07T07:47:23.001Z";
        let mut h = Sha1::new();
        h.update(nonce);
        h.update(created.as_bytes());
        h.update(b"nursery");
        let digest = b64.encode(h.finalize());
        let nonce_b64 = b64.encode(nonce);
        let body = format!(
            "<s:Header><Security xmlns=\"...secext...\"><UsernameToken>\
<Username>avent</Username>\
<Password Type=\"...#PasswordDigest\">{digest}</Password>\
<Nonce EncodingType=\"...#Base64Binary\">{nonce_b64}</Nonce>\
<Created xmlns=\"...utility...\">{created}</Created></UsernameToken></Security></s:Header>"
        );
        assert!(wsse_ok(&body, "avent", "nursery"), "correct digest must pass");
        assert!(!wsse_ok(&body, "avent", "wrong"), "wrong pass must fail");
        assert!(!wsse_ok(&body, "other", "nursery"), "wrong user must fail");
    }

    #[test]
    fn one_profile_with_the_backchannel_uri() {
        let r = dispatch("GetProfiles", "", &cfg(), true).unwrap();
        assert_eq!(r.matches("<trt:Profiles ").count(), 1);
        assert!(r.contains("token=\"profile0\"") && r.contains("Nursery &lt;3"));
        let r = dispatch("GetStreamUri", "", &cfg(), true).unwrap();
        assert!(r.contains("8554/Nursery?backchannel=1</tt:Uri>"));
    }

    #[test]
    fn operation_is_the_exact_body_element() {
        let b = |op: &str| format!("<s:Envelope><s:Header><x>GetCapabilities</x></s:Header><s:Body xmlns:a=\"b\"><trt:{op} xmlns:trt=\"x\"/></s:Body></s:Envelope>");
        assert_eq!(operation(&b("GetServiceCapabilities")), "GetServiceCapabilities");
        assert_eq!(operation(&b("GetProfile")), "GetProfile");
        assert_eq!(operation("<Envelope><Body><GetProfiles/></Body></Envelope>"), "GetProfiles");
        assert_eq!(operation("garbage"), "Unknown");
    }

    #[test]
    fn service_capabilities_per_service_and_faults() {
        assert!(dispatch("GetServiceCapabilities", "/onvif/media_service", &cfg(), false).unwrap().contains("SnapshotUri=\"true\""));
        assert!(dispatch("GetServiceCapabilities", "/onvif/device_service", &cfg(), false).unwrap().contains("tds:Capabilities"));
        let mac = super::super::device_mac("dev1");
        let first = u8::from_str_radix(&mac[..2], 16).unwrap();
        assert!(first & 0x02 != 0 && first & 0x01 == 0, "locally administered unicast");
        assert!(dispatch("GetNetworkInterfaces", "", &cfg(), false).unwrap().contains(&format!("<tt:HwAddress>{mac}</tt:HwAddress>")));
        assert!(dispatch("GetAudioOutputs", "", &cfg(), false).is_none());
    }

    #[test]
    fn elem_text_not_fooled_by_wrapper() {
        assert_eq!(elem_text("<UsernameToken><Username>bob</Username>", "Username").as_deref(), Some("bob"));
        assert_eq!(elem_text("<wsu:Created xmlns:wsu=\"x\">T</wsu:Created>", "Created").as_deref(), Some("T"));
    }

    #[test]
    fn device_info_reports_model_escaped() {
        let r = dispatch("GetDeviceInformation", "", &cfg(), false).unwrap();
        assert!(r.contains("<tds:Model>Nursery &lt;3</tds:Model>"));
    }
}
