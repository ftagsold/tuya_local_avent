//! Video pipeline: Go bridge (loopback, internal creds) → ffmpeg (clock +
//! device-name overlay, H.264 + AAC) → go2rtc (the only RTSP server on the
//! LAN, password required, behind the gate in rtsp_gate.rs). Nothing outside the container can reach a stream
//! without credentials: the Go bridge binds loopback with random credentials,
//! and go2rtc requires the configured user for every connection that is not
//! from loopback (the relay publishes, and snapshots read, over loopback).
//!
//! Talkback: the go2rtc stream also has the Go bridge as an audio-only source
//! whose RTSP backchannel reaches the monitor's speaker over the LAN; a viewer
//! asking for `?backchannel=1` gets a return channel to it.

use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::Command;
use tokio::sync::watch;

/// Random hex secret for loopback-only credentials. Regenerated every start.
pub fn random_secret() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

/// RFC 3986 userinfo for an RTSP URL (`user:pass@`), percent-encoded.
pub fn userinfo(user: &str, pass: &str) -> String {
    if user.is_empty() {
        return String::new();
    }
    format!("{}:{}@", pct(user), pct(pass))
}

fn pct(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// RTSP path segment from a display name: ASCII alphanumerics, `_` and `-`
/// kept, everything else folded to `_`. Empty falls back to `fallback`.
pub fn slug(name: &str, fallback: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' {
            s.push(c);
        } else if !s.ends_with('_') {
            s.push('_');
        }
    }
    let s = s.trim_matches('_').to_string();
    if s.is_empty() { fallback.to_string() } else { s }
}

pub struct Go2rtc {
    pub port: u16,
    pub path: String,
    pub user: String,
    pub pass: String,
    /// The Go bridge's loopback stream, used for its talkback backchannel.
    pub talk_source: String,
}

impl Go2rtc {
    /// go2rtc config: RTSP (password required off loopback) and the API on
    /// loopback only; WebRTC, RTMP, SRTP and every exec-capable source off.
    /// Viewers get the relay's H.264 + AAC + Opus (`default_query`); the Go bridge
    /// source contributes audio only, so its raw video never reaches them,
    /// and carries the backchannel. Strings are JSON-quoted, which YAML reads
    /// verbatim.
    pub fn config(&self) -> String {
        let q = |s: &str| serde_json::to_string(s).unwrap_or_default();
        format!(
            "log:
  level: info
app:
  modules: [api, rtsp]
api:
  listen: \"127.0.0.1:1984\"
rtsp:
  listen: \":{port}\"
  username: {user}
  password: {pass}
  default_query: \"video=h264&audio=aac&audio=opus\"
webrtc:
  listen: \"\"
rtmp:
  listen: \"\"
srtp:
  listen: \"\"
streams:
  {path}:
    - {talk}
",
            port = self.port,
            user = q(&self.user),
            pass = q(&self.pass),
            path = q(&self.path),
            talk = q(&format!("{}#media=audio#backchannel=1", self.talk_source)),
        )
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        write_private(path, self.config().as_bytes())
    }
}

/// Write a secret-bearing file with 0600 permissions.
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    use std::io::Write;
    f.write_all(data).with_context(|| format!("write {}", path.display()))
}

/// Overlay text for drawtext's `textfile` (expansion=normal): the live clock,
/// then the device name. `\`, `%`, `{` and `}` in the name are escaped so the
/// expander prints them literally.
pub fn overlay_text(device_name: &str) -> String {
    let mut name = String::new();
    for c in device_name.chars() {
        if matches!(c, '\\' | '%' | '{' | '}') {
            name.push('\\');
        }
        name.push(c);
    }
    format!("%{{localtime:%Y-%m-%d %H\\:%M\\:%S}} | {name}")
}

/// Temperature overlay text (the monitor reports °C x 100): `21.5 °C`.
pub fn temperature_text(raw: f64) -> String {
    format!("{:.1} °C", raw / 100.0)
}

/// Write a text file drawtext re-reads (`reload=1`): a temp file renamed over
/// it, so a frame never sees a half-written file.
pub fn write_overlay_file(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("tmp");
    write_private(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))
}

/// What the relay burns into the picture: the clock and device name top
/// left, the temperature top right (re-read every frame), same style.
pub struct Overlay<'a> {
    pub font: &'a str,
    pub clock: &'a Path,
    pub temperature: &'a Path,
}

/// ffmpeg arguments for the relay: pull the bridge, burn the overlay (when
/// `drawtext` is available), H.264 + AAC (UniFi Protect plays no G.711) +
/// Opus, publish to go2rtc.
///
/// - Opus is for WebRTC viewers (Home Assistant's go2rtc): browsers play no
///   AAC, and HA's own AAC->Opus transcode fails for ONVIF cameras (its
///   internal RTSP lookup 404s on the `mac#profile` stream name).
/// - AAC at 48 kHz: at 24 kHz and below decoders (VLC, Protect) may assume
///   implicit HE-AAC SBR and double the rate, which breaks A/V timing.
/// - H.264 as the camera sends it: one slice per frame (no `zerolatency`
///   sliced threads), SPS/PPS before every keyframe, no B-frames, fixed GOP.
///   Multi-slice frames and out-of-band-only SPS/PPS break Protect and VLC.
/// - `-timeout`: a silent input (bridge stalled) ends ffmpeg instead of
///   hanging; `-progress pipe:1` feeds the stall watchdog in `supervise`.
/// - VAAPI: decode and overlay stay on the CPU, the GPU encodes (~1/4 of
///   the CPU of libx264). h264_vaapi writes one slice per frame and SPS/PPS
///   before every IDR on its own. `-sei 0`: no HRD in the SPS and no timing
///   SEI on every frame (libx264 sends neither); with them Protect's live
///   view stays broken while HA plays.
pub fn relay_args(input: &str, output: &str, overlay: Option<Overlay>, encoder: &Encoder) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    if let Encoder::Vaapi(device) = encoder {
        a.extend(["-vaapi_device".to_string(), device.clone()]);
    }
    a.extend([
        "-hide_banner", "-loglevel", "warning", "-nostdin", "-nostats", "-progress", "pipe:1",
        "-rtsp_transport", "tcp", "-timeout", "10000000", "-i", input,
        "-map", "0:v:0", "-map", "0:a:0?", "-map", "0:a:0?",
    ]
    .iter()
    .map(|s| s.to_string()));
    let mut vf = String::from("fps=25");
    if let Some(o) = overlay {
        let style = format!("fontfile={}:y=10:fontsize=24:fontcolor=white", o.font);
        vf.push_str(&format!(",drawtext={style}:textfile={}:expansion=normal:x=10", o.clock.display()));
        vf.push_str(&format!(",drawtext={style}:textfile={}:reload=1:expansion=none:x=w-tw-10", o.temperature.display()));
    }
    let video: &[&str] = match encoder {
        Encoder::Vaapi(_) => {
            vf.push_str(",format=nv12,hwupload");
            &[
                "-c:v", "h264_vaapi", "-profile:v", "main", "-bf", "0", "-g", "50",
                "-rc_mode", "VBR", "-b:v", "2000k", "-maxrate", "2500k", "-bufsize", "5000k", "-sei", "0",
            ]
        }
        // superfast: ~25% less CPU than veryfast at 720p25. 4 threads:
        // plenty; x264's default (1.5x cores) multiplies frame buffers and
        // latency for nothing.
        Encoder::X264 => &[
            "-c:v", "libx264", "-preset", "superfast", "-threads", "4", "-profile:v", "main", "-pix_fmt", "yuv420p",
            "-bf", "0", "-g", "50", "-keyint_min", "50", "-sc_threshold", "0",
            "-x264-params", "sliced-threads=0:slices=1:repeat-headers=1:rc-lookahead=0:sync-lookahead=0",
            "-maxrate", "2500k", "-bufsize", "5000k",
        ],
    };
    a.push("-vf".into());
    a.push(vf);
    a.extend(video.iter().map(|s| s.to_string()));
    a.extend(
        [
            "-c:a:0", "aac", "-profile:a:0", "aac_low", "-b:a:0", "64k",
            "-c:a:1", "libopus", "-b:a:1", "32k", "-application:a:1", "lowdelay",
            "-ar", "48000", "-ac", "1",
            "-af", "aresample=async=1",
            "-f", "rtsp", "-rtsp_transport", "tcp", output,
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    a
}

/// The relay's H.264 encoder.
#[derive(Clone, Debug, PartialEq)]
pub enum Encoder {
    X264,
    /// GPU encode through this VAAPI render node.
    Vaapi(String),
}

/// Whether ffmpeg can H.264-encode on the VAAPI `device`: a short test
/// encode, so a missing node, driver or encode support all read as no.
/// Logs why not.
pub fn vaapi_supported(ffmpeg: &str, device: &str) -> bool {
    if !Path::new(device).exists() {
        tracing::info!("no VAAPI device {device}");
        return false;
    }
    let out = std::process::Command::new(ffmpeg)
        .args([
            "-hide_banner", "-v", "error", "-nostdin", "-vaapi_device", device,
            "-f", "lavfi", "-i", "color=s=1280x720:r=25", "-frames:v", "5",
            "-vf", "format=nv12,hwupload", "-c:v", "h264_vaapi", "-f", "null", "-",
        ])
        .output();
    match out {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            tracing::info!("VAAPI test encode on {device} failed: {}", err.lines().next().unwrap_or("").trim());
            false
        }
        Err(e) => {
            tracing::info!("VAAPI test encode: {e}");
            false
        }
    }
}

/// Whether ffmpeg has the drawtext filter and the font exists.
pub fn drawtext_supported(ffmpeg: &str, font: &str) -> bool {
    if !Path::new(font).exists() {
        return false;
    }
    std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-filters"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("drawtext"))
        .unwrap_or(false)
}

/// True when `url` delivers real video within `timeout`. The Go bridge
/// answers DESCRIBE from a static SDP before the monitor sends anything, so a
/// codec listing proves nothing: require one H.264 packet on stdout.
pub async fn probe_video(ffmpeg: &str, url: &str, timeout: Duration) -> bool {
    let run = Command::new(ffmpeg)
        .args([
            "-v", "error", "-nostdin", "-rtsp_transport", "tcp", "-i", url,
            "-map", "0:v:0", "-c", "copy", "-frames:v", "1", "-f", "h264", "-",
        ])
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(timeout, run).await {
        Ok(Ok(o)) => o.status.success() && !o.stdout.is_empty(),
        _ => false,
    }
}

/// Child log lines that are expected and harmless: decoder complaints until
/// the first keyframe after joining mid-GOP, and RTP re-packetising.
const NOISE: &[&str] = &["non-existing PPS", "no frame!", "Last message repeated", "RTP packets are too big"];

/// Forward a child's output to our stderr, minus the known noise.
fn forward<R: tokio::io::AsyncRead + Unpin + Send + 'static>(r: R) {
    use tokio::io::AsyncBufReadExt;
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(r).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            if !NOISE.iter().any(|n| l.contains(n)) {
                eprintln!("{l}");
            }
        }
    });
}

/// Liveness of a supervised ffmpeg: when it last made progress (a new frame
/// on its `-progress` output). Shared with the run loop's health check.
#[derive(Default)]
pub struct Health {
    last_ms: std::sync::atomic::AtomicU64,
    /// Last real frame only; `touch` at start does not set it.
    frame_ms: std::sync::atomic::AtomicU64,
    /// First frame of the current unbroken run (0: none since `touch`).
    up_ms: std::sync::atomic::AtomicU64,
    /// Bumped by every `touch`, i.e. every relay (re)start: a new publisher
    /// in go2rtc, so viewers of the old one get nothing more.
    run: std::sync::atomic::AtomicU64,
}

/// A gap this long between frames starts a new run (`Health::up_for`).
const RUN_GAP_MS: u64 = 2000;

impl Health {
    fn now_ms() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64
    }

    pub fn touch(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        self.last_ms.store(Self::now_ms(), Relaxed);
        self.up_ms.store(0, Relaxed);
        self.run.fetch_add(1, Relaxed);
    }

    /// A new frame went through.
    pub fn frame(&self) {
        use std::sync::atomic::Ordering::Relaxed;
        let now = Self::now_ms();
        let prev = self.frame_ms.swap(now, Relaxed);
        self.last_ms.store(now, Relaxed);
        if self.up_ms.load(Relaxed) == 0 || now.saturating_sub(prev) > RUN_GAP_MS {
            self.up_ms.store(now, Relaxed);
        }
    }

    /// How long frames have flowed without a break (zero when none flow).
    pub fn up_for(&self) -> Duration {
        let up = self.up_ms.load(std::sync::atomic::Ordering::Relaxed);
        if up == 0 || self.frame_age() > Duration::from_millis(RUN_GAP_MS) {
            return Duration::ZERO;
        }
        Duration::from_millis(Self::now_ms().saturating_sub(up))
    }

    /// The relay run: changes on every (re)start.
    pub fn run(&self) -> u64 {
        self.run.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Time since the last progress (or since `touch` at start).
    pub fn age(&self) -> Duration {
        Duration::from_millis(Self::now_ms().saturating_sub(self.last_ms.load(std::sync::atomic::Ordering::Relaxed)))
    }

    /// Time since the last real frame (very long before the first).
    pub fn frame_age(&self) -> Duration {
        Duration::from_millis(Self::now_ms().saturating_sub(self.frame_ms.load(std::sync::atomic::Ordering::Relaxed)))
    }
}

/// How to (re)build a supervised child's command line.
#[derive(Clone)]
pub struct Spec {
    pub name: &'static str,
    pub program: String,
    pub args: Vec<String>,
    /// For an ffmpeg run with `-progress pipe:1`: kill and restart it when no
    /// new frame arrives for `stall` (a hung input or output).
    pub watchdog: Option<(std::sync::Arc<Health>, Duration)>,
}

/// Run `spec` forever: restart it 1-8 s after it exits or stalls, or at once
/// when `restart` changes (the new spec is read from it). The child is killed
/// with the task (kill_on_drop), so nothing outlives the bridge.
pub fn supervise(mut restart: watch::Receiver<Spec>) {
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(1);
        loop {
            let spec = restart.borrow_and_update().clone();
            tracing::info!("starting {}", spec.name);
            let spawned = Command::new(&spec.program)
                .args(&spec.args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn();
            let mut child = match spawned {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("spawn {} ({}): {e}; retrying in 5s", spec.name, spec.program);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            let started = std::time::Instant::now();
            if let Some(e) = child.stderr.take() {
                forward(e);
            }
            if let Some(o) = child.stdout.take() {
                match &spec.watchdog {
                    Some((health, _)) => {
                        health.touch();
                        track_progress(o, health.clone());
                    }
                    None => forward(o),
                }
            }
            let mut tick = tokio::time::interval(Duration::from_secs(2));
            let outcome = loop {
                tokio::select! {
                    status = child.wait() => break format!("exited ({status:?})"),
                    changed = restart.changed() => {
                        let _ = child.kill().await;
                        if changed.is_err() {
                            return; // owner gone
                        }
                        tracing::info!("{}: restart requested", spec.name);
                        backoff = Duration::from_secs(1);
                        break String::new();
                    }
                    _ = tick.tick() => {
                        if let Some((health, stall)) = &spec.watchdog {
                            if health.age() > *stall {
                                let _ = child.kill().await;
                                break format!("stalled (no frame for {}s), killed", health.age().as_secs());
                            }
                        }
                    }
                }
            };
            if outcome.is_empty() {
                continue;
            }
            // A run that got going resets the backoff; quick failures back
            // off up to 8 s: enough not to hammer a dead peer, short enough
            // that a peer coming back (go2rtc, the bridge) is picked up fast.
            if started.elapsed() > Duration::from_secs(15) {
                backoff = Duration::from_secs(1);
            }
            tracing::warn!("{} {outcome}; restarting in {}s", spec.name, backoff.as_secs());
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(8));
        }
    });
}

/// Read ffmpeg `-progress` key=value lines; a changed `frame=` is progress.
fn track_progress<R: tokio::io::AsyncRead + Unpin + Send + 'static>(r: R, health: std::sync::Arc<Health>) {
    use tokio::io::AsyncBufReadExt;
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(r).lines();
        let mut last = String::new();
        while let Ok(Some(l)) = lines.next_line().await {
            if let Some(f) = l.strip_prefix("frame=") {
                if f != last && f != "0" {
                    last = f.to_string();
                    health.frame();
                }
            }
        }
    });
}

pub fn data_file(dir: &str, name: &str) -> PathBuf {
    Path::new(dir).join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_folds_specials() {
        assert_eq!(slug("Nursery <3", "x"), "Nursery_3");
        assert_eq!(slug("  ", "dev"), "dev");
        assert_eq!(slug("Baby-Cam 2", "x"), "Baby-Cam_2");
    }

    #[test]
    fn userinfo_percent_encodes() {
        assert_eq!(userinfo("a b", "p@ss:/"), "a%20b:p%40ss%3A%2F@");
        assert_eq!(userinfo("", "x"), "");
    }

    #[test]
    fn temperature_is_drawn_top_right_and_reloaded() {
        let (clock, temp) = (Path::new("/d/overlay.txt"), Path::new("/d/temperature.txt"));
        let a = relay_args("in", "out", Some(Overlay { font: "/f.ttf", clock, temperature: temp }), &Encoder::X264);
        let vf = &a[a.iter().position(|x| x == "-vf").unwrap() + 1];
        assert!(vf.contains("textfile=/d/overlay.txt:expansion=normal:x=10"));
        assert!(vf.contains("textfile=/d/temperature.txt:reload=1:expansion=none:x=w-tw-10"));
        assert_eq!(temperature_text(2150.0), "21.5 °C");
    }

    #[test]
    fn vaapi_uploads_after_overlay() {
        let a = relay_args("in", "out", None, &Encoder::Vaapi("/dev/dri/renderD128".into()));
        assert_eq!(a[..2], ["-vaapi_device", "/dev/dri/renderD128"]);
        let vf = &a[a.iter().position(|x| x == "-vf").unwrap() + 1];
        assert!(vf.ends_with(",format=nv12,hwupload"));
        assert!(a.iter().any(|x| x == "h264_vaapi"));
        assert!(a.windows(2).any(|w| w == ["-sei", "0"]));
        assert!(!a.iter().any(|x| x == "libx264"));
        let x = relay_args("in", "out", None, &Encoder::X264);
        assert!(!x.iter().any(|x| x.contains("vaapi")));
    }

    #[test]
    fn overlay_text_escapes_expander_chars() {
        assert_eq!(overlay_text("Nursery <3"), "%{localtime:%Y-%m-%d %H\\:%M\\:%S} | Nursery <3");
        assert_eq!(overlay_text("50% {x}"), "%{localtime:%Y-%m-%d %H\\:%M\\:%S} | 50\\% \\{x\\}");
    }

    #[test]
    fn go2rtc_config_quotes_secrets_and_locks_down() {
        let g = Go2rtc {
            port: 8554, path: "Nursery".into(), user: "avent".into(), pass: "a\"b".into(),
            talk_source: "rtsp://bridge:x@127.0.0.1:38554/Nursery".into(),
        };
        let c = g.config();
        assert!(c.contains("password: \"a\\\"b\""));
        assert!(c.contains("modules: [api, rtsp]"));
        assert!(c.contains("default_query: \"video=h264&audio=aac&audio=opus\""));
        assert!(c.contains("listen: \"127.0.0.1:1984\""));
        assert!(c.contains("rtsp://bridge:x@127.0.0.1:38554/Nursery#media=audio#backchannel=1"));
        for off in ["webrtc", "rtmp", "srtp"] {
            assert!(c.contains(&format!("{off}:\n  listen: \"\"")), "{off} must be off");
        }
    }
}
