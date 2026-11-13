# tuya_local_avent

A single Rust process (plus a vendored Go video bridge) that turns a **Philips
Avent / Tuya baby monitor** into:

- a **Home Assistant** device — night light (dimmable), lullaby (play
  control, track, playing state), volume,
  temperature, motion/sound alerts, privacy — over local MQTT discovery,
- a **Lovelace card** (`card/tuya-local-avent-card.js`) with live video, talk,
  night light, lullaby and entity overlays, and
- an **ONVIF Profile S camera** that **UniFi Protect** can adopt, fed by a
  local RTSP stream.

Everything at runtime is **local**: control and video go straight to the
monitor over your LAN. With `AVENT_P2P_PASSWORD` set the cloud is never
touched; the bridge logs in only if the monitor rejects that password.

## Status

| Capability | State |
|---|---|
| Local 3.3 LAN session, control + sensor pushes | built |
| Fully-local video (3.3 LAN signalling) | **working** — `h264 1280x720`, no cloud |
| Overlay `yyyy-mm-dd hh:mm:ss \| <name>`, H.264 + AAC | **working** |
| Authenticated RTSP (go2rtc), no open stream | **working** |
| Talkback (monitor speaker) over the LAN | **working** end to end via HA's go2rtc (tested with a silent mic; audibility not yet checked) |
| P2P password from env, cloud fallback on rejection | **working** |
| ONVIF Profile S + snapshot for UniFi Protect / HA | working (adopted by Protect) |
| Home Assistant control via MQTT discovery | built; needs broker credentials |
| Lovelace card (video, talk, night light, lullaby, overlays) | built (`card/`) |

## How it works

```
 ┌────────────────── tuya_local_avent (one container, macvlan) ───────────────────┐
 │ Go bridge ──RTSP, 127.0.0.1:38554, random creds──▶ ffmpeg                        │
 │   (LAN video from the monitor)                     overlay: clock | name        │
 │      ▲ talk (backchannel, PCMU)                    H.264 + AAC                  │
 │      │                                               │ publish, loopback only   │
 │      │                                               ▼                          │
 │      └──────────────────────────── go2rtc :18554 ◀─ gate :8554 ◀─ Protect / HA │
 │ ONVIF :8000 ─ stream/snapshot URI ─────────▶  (RTSP_USER/RTSP_PASS required)    │
 │ LAN control (TCP 6668, 3.3) ── MQTT ──▶ Home Assistant                          │
 └─────────────────────────────────────────────────────────────────────────────────┘
```

- The only RTSP port on the LAN is go2rtc's `8554` (TCP), and every
  connection that is not from loopback needs `RTSP_USER`/`RTSP_PASS`. The Go
  bridge listens on loopback with a per-start random credential. go2rtc runs
  only its `api` (bound to `127.0.0.1`) and `rtsp` modules: no WebRTC, RTMP,
  SRTP, exec or ffmpeg sources. Anyone holding the RTSP credential could also
  push into the stream, so treat it like a camera admin password.
- RTSP gate: `8554` is a TCP gate in front of go2rtc (internal `18554`). It
  lets viewers through once video frames have flowed for 3 s, answers them
  `503 Service Unavailable` before that, and cuts every session through it
  when video stops or the relay restarts. go2rtc would accept a viewer early
  and reject its backchannel, and keep a viewer from before an outage on the
  dead publisher; UniFi Protect then freezes until reconnected by hand. Cut,
  it reconnects to the live stream. The gate dials go2rtc on the LAN
  address, so go2rtc's auth still applies.
- **Talkback.** The monitor's speaker is reached over the same LAN session
  as the video (`bridge/pkg/lan/talk.go`, reverse-engineered from the Tuya
  Smart app with `tools/frida/talk_capture.js`). go2rtc carries an RTSP
  backchannel to it: a viewer opening `…/<name>?backchannel=1` (the ONVIF
  stream URI) can send
  G.711 audio, which the Go bridge resamples to 16 kHz PCM and sends as talk
  messages. The talk opens on the first packet and closes after 1 s of
  silence, so watching never opens the speaker. Open/close go on the KCP
  control conversation (0); the audio goes on the monitor's audio
  conversation (2), as the app does. Audio on conv 0 crashes the monitor.
- The monitor sends G.711 µ-law audio, which UniFi Protect does not play. The
  relay re-encodes it to AAC-LC (48 kHz mono; ≤ 24 kHz AAC is misread as
  HE-AAC by VLC and others), and the ONVIF profile advertises it.
- Video is re-encoded (for the overlay) the way the camera sends it: one
  slice per frame, SPS/PPS before every keyframe, no B-frames, 2 s GOP,
  25 fps. x264's `zerolatency` tune splits frames into many slices, which
  Protect and VLC fail to play.
- ONVIF hands out the stream URI with credentials, and the snapshot URI with
  its access token, only for requests that carry a valid WS-Security token.
  `/snapshot` otherwise requires HTTP Basic with the ONVIF credentials.

The monitor speaks Tuya LAN **protocol 3.3**. It will answer a LAN video offer
(so video is local) only when the offer is sent without the 3.3 version header
and over a session primed with `DP_QUERY` — a path the vendored Go bridge was
patched to take (`bridge/pkg/lan`). The media login is
`md5(p2p_password‖localKey)`.

### Self-healing

Everything restarts itself; nothing needs a container restart.

| failure | detected by | action |
|---|---|---|
| relay input dead (bridge gone, socket stalled) | ffmpeg `-timeout 10s` | relay restarts (1 s → 30 s backoff) |
| relay running but no new frames | `-progress` watchdog, 20 s | relay killed and restarted |
| no video for 60 s | health loop | Go bridge restarted, monitor IP re-discovered |
| still no video after 3 bridge restarts | health loop | P2P password re-fetched from the cloud |
| monitor changed IP | discovery during recovery | bridge and LAN control follow the new IP |
| LAN control half-open | 3 missed heartbeats | session reconnects |
| MQTT broker restart | reconnect | discovery, availability and subscriptions re-sent |
| go2rtc / Go bridge crash | supervisor | restarted (≈10 s to video) |
| monitor off at boot | discovery | retried every 30 s instead of exiting |

Resource bounds: ONVIF serves at most 32 connections, each closed after
20 s; snapshots are single-flight and cached for 3 s, so polling clients
share one ffmpeg; every child process (bridge, go2rtc, relay, snapshot,
probe) is killed with its owner, so none outlive a restart.

### P2P password

1. `AVENT_P2P_PASSWORD` is used as is, no login.
2. The bridge waits for real video from the monitor (up to ~90 s).
3. No video: it logs in (`AVENT_EMAIL`/`AVENT_PASSWORD` + app keys), fetches
   the current password, caches it in `/data/cloud.json` and restarts the video
   bridge. A rejected env value is remembered, so later starts use the cached
   one until you update the env.

Without `AVENT_P2P_PASSWORD` the cached value is tried first, then the cloud.
`tuya_local_avent fetch-password` prints whether a password exists and writes it
to `data/bridge.json` (`cameras[0].p2p_password`).

## Configuration (env)

Required:

| var | meaning |
|---|---|
| `AVENT_DEVICE_ID`, `AVENT_LOCAL_KEY` | the monitor's Tuya id + local key |
| `AVENT_P2P_PASSWORD` | the monitor's P2P password, **or** the cloud vars below |
| `ONVIF_PASS` | ONVIF password (≥ 8 chars; also the RTSP password by default) |

Cloud fallback (needed if `AVENT_P2P_PASSWORD` is unset or rejected):
`AVENT_EMAIL`, `AVENT_PASSWORD`, `AVENT_APP_KEY`, `AVENT_SIGNING_KEY`,
`AVENT_CH_KEY`.

Optional:

| var | default |
|---|---|
| `AVENT_CAMERA_NAME` | app name from the last cloud fetch, else `Monitor` |
| `AVENT_REGION` / `AVENT_COUNTRY_CODE` | `eu` / `49` |
| `AVENT_LAN_IP` | auto-discovered (set to skip discovery) |
| `MQTT_HOST` / `MQTT_PORT` / `MQTT_USER` / `MQTT_PASS` | — / 1883 / — / — (unset `MQTT_HOST` disables HA control) |
| `ONVIF_USER` / `ONVIF_PORT` / `ONVIF_BIND_IP` | `avent` / 8000 / auto |
| `ONVIF_MODEL` | camera name (Protect names the adopted camera "manufacturer model") |
| `ONVIF_MANUFACTURER` | empty (the model is then reported as the manufacturer, so Protect shows it alone) |
| `ONVIF_MAC` | derived from the device id (reported over ONVIF and MQTT; give the container this MAC) |
| `RTSP_USER` / `RTSP_PASS` | `ONVIF_USER` / `ONVIF_PASS` |
| `RTSP_PORT` | `8554` (TCP) |
| `TZ` | UTC (overlay clock timezone, e.g. `Europe/Berlin`) |
| `AVENT_HWACCEL` | `auto`: GPU H.264 encode (VAAPI) when a test encode works, else libx264; `vaapi` logs an error on fallback; `off` |
| `AVENT_VAAPI_DEVICE` | `/dev/dri/renderD128` (pass it in: `devices: [/dev/dri/renderD128]`) |
| `AVENT_OVERLAY_FONT` | `/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf` |
| `AVENT_DATA` | `/data` |
| `RUST_LOG` | `info` |

The bridge refuses to start with an `ONVIF_PASS`/`RTSP_PASS` shorter than 8
characters or equal to `CHANGEME`.

### Getting the app signing values

`AVENT_APP_KEY`, `AVENT_SIGNING_KEY` and `AVENT_CH_KEY` are
constants of the Tuya Smart Android app — shared by all users, stable until
Tuya re-signs the app. They are not account secrets, but this repo does not
ship them: keep them in `docker-compose.yml` (gitignored). `tools/frida/`
extracts them: `dump.py` reads the assembled signing key from the running
app's memory, and `bypass_capture.js` captures the signed base params of a
real request (`chKey` is the channel key; `ttid=international`). See the
comments in each script.

## Run

The container needs its **own MAC/IP** so UniFi Protect discovers it, so run it
on a Docker **macvlan** network:

```sh
docker network create -d macvlan \
  --subnet=192.168.1.0/24 --gateway=192.168.1.1 \
  -o parent=eth0 tuya-local-avent

cp docker-compose.example.yml docker-compose.yml   # then fill in secrets
docker compose up -d --build
```

Set `ONVIF_BIND_IP` to the container's `ipv4_address`. Protect keys a camera
by MAC, and macvlan hands out a random one on every create: set the network's
`mac_address` (`docker run --mac-address`) to the MAC the bridge logs at start
(`ONVIF MAC ...`, derived from the device id, or `ONVIF_MAC`). The log warns
when the container's MAC differs; Protect then offers the adopted camera again
on every discovery. containrrr/watchtower on Docker 29 drops `--mac-address`
when it recreates the container: exclude this container from watchtower and
update by recreating it yourself.

Logs: everything (the bridge, ffmpeg, go2rtc) goes to the container's
stdout; the compose file caps it at 3 x 10 MB (`logging:` json-file). With
plain `docker run`, pass `--log-opt max-size=10m --log-opt max-file=3`.

- **Home Assistant control**: set `MQTT_HOST` and the broker's
  `MQTT_USER`/`MQTT_PASS` (the Mosquitto add-on refuses anonymous clients);
  the entities appear via MQTT discovery. Use the broker's IP: `.local` names
  do not resolve inside the container. The night light is one dimmable light
  (`light.turn_on` with `brightness_pct`). Lullaby: `select.*_lullaby`
  (play/pause/stop/next/prev, DP 201), `select.*_lullaby_track` (the 15
  built-in tracks; choosing one plays it, DP 202, current track from DP 248),
  `binary_sensor.*_lullaby_playing` (DP 246) and `select.*_lullaby_timer`
  (auto-stop after 5/10/20/30/60/90 min or `Forever`: DP 244 seconds + DP
  243 on/off; a length set elsewhere shows as the nearest option). The monitor reports no
  state when the bridge connects, only changes, so these read `unknown`
  until something changes; they still work. Tracks chosen on the handheld
  unit or in the app show up too (DP 248). The track ids differ between
  units (four known blocks): until the monitor has reported a track, a
  chosen one is sent for every block and the monitor ignores the others. An
  id outside the known blocks is logged (`lullaby: unknown track id`).
- **Home Assistant video + talk**: add the *ONVIF* integration with the
  container IP, port `8000` and the ONVIF credentials. It creates one camera;
  its stream URI carries the talkback backchannel, which HA's built-in go2rtc
  forwards from the browser microphone. Clients that do not talk ignore the
  extra track. The bridge's MQTT device and the ONVIF camera share a MAC, so
  HA shows them as one device.
- **Lovelace card**: see [Card](#card).
- **UniFi Protect**: *Settings → Cameras → Add → Third-party (ONVIF)*; it should
  discover the camera, or add it by IP with the `ONVIF_USER`/`ONVIF_PASS`
  credentials.

## Card

`card/tuya-local-avent-card.js` is a dependency-free Lovelace card:

- **Video + listen audio** from the ONVIF camera entity over Home
  Assistant's own WebRTC (built-in go2rtc). No transcoding in the card. The
  bridge's stream carries Opus next to AAC for this: HA's go2rtc cannot
  transcode audio for ONVIF cameras (its internal lookup 404s on the
  `mac#profile` stream name). If
  HA offers no WebRTC for the camera (go2rtc integration missing), the card
  falls back to HA's MJPEG proxy and says so: low quality, no listen/talk.
  A WebRTC stream that stops decoding for 15 s is reconnected.
- **Talk**: the browser microphone on a send-only track of the same WebRTC
  session, with a talk volume (gain, 0-200 %). Tap to start/stop, or
  `talk_mode: hold`. Needs HA over HTTPS (browsers grant the microphone only
  to secure pages).
- **Controls**: a button group floating at the bottom center of the video:
  listen, night light, lullaby, talk, full screen, listen in background
  (each optional, in the
  configured order). Hidden until the video is tapped; hidden again after
  `autohide` seconds without interaction, but kept while a control is open
  or talk is on. Tapping a button turns the group into that control: an
  on/off toggle on the left and its slider (listen volume, night light
  33/66/100 %, lullaby volume, talk volume); the lullaby control also has a
  track picker. Lullaby play starts `default_track`, or resumes the
  monitor's current track when unset. Sliders are drawn by the card
  (pointer events), so they work in the turned landscape view and on touch
  screens, and keep a sent value until the entity reports it. A tap on the video (or Esc)
  goes back to the buttons. The MQTT entities are found on the camera's
  device; set them explicitly if HA did not merge the devices.
- **Full screen**: the video with overlays and controls. Uses the
  Fullscreen API, or a full-window layer where the browser has none
  (iPhone). On a phone held upright it goes landscape (orientation lock, or
  the picture turned by CSS where the browser refuses the lock).
- **Defaults**: listen on/off, listen volume and talk volume when the card
  loads. Browsers allow sound only after a user gesture: if refused, the
  card starts muted, shows "Tap for sound" and turns listen on at the first
  tap. To start with sound right away, allow autoplay for the HA site in the
  browser (Firefox, Safari: site settings) or in the Companion app.
- **Background listening**: listen audio has its own `<audio>` element,
  separate from the video, and registers a Media Session (lock-screen
  controls). In a browser that keeps hidden pages playing (Chrome, Firefox
  on Android), it plays on with the screen locked if the card was in full
  screen, and is muted while hidden otherwise. The Companion apps and iOS
  Safari pause the page when it is hidden. Home
  Assistant also closes its connection, and with it the stream, 5 minutes
  after the page is hidden: turn off *Profile → Browser settings →
  Automatically close connection* on that phone.
- **Listen in background** (control `background`, shown on Android): opens
  Home Assistant's own HLS stream of the camera in VLC, in VLC's own task
  (its own entry in recent apps), named after the stream (the camera's
  device). The URL carries HA's stream token, so VLC needs no login, and it
  works wherever HA does (remote access included). The card's own listen
  turns off. Without VLC, the Companion app opens its store page. One-time
  VLC setting: *Settings → Video → Background/PiP mode → Play videos in
  background*; then locking the phone keeps the sound playing, with lock-screen
  controls. HLS lags a few seconds.
- **Overlays**: other entities' states as transparent HTML text over the
  video. The top corners are burnt into the picture by the bridge: clock
  and name top left, the monitor's temperature top right (updated on each
  temperature report; the last value survives a restart). The editor has a
  picker (with search) for bottom left and bottom right
  (`overlay_bottom_left`, `overlay_bottom_right`); YAML overlays placed top
  right are dropped. The `overlays`
  YAML list adds more, at a named position or at explicit
  `top/left/right/bottom`.

Install: copy the file to `/config/www/`, then *Settings → Dashboards → ⋮ →
Resources → Add* `/local/tuya-local-avent-card.js` (JavaScript module). The
card has a visual editor; YAML:

```yaml
type: custom:tuya-local-avent-card
camera: camera.nursery
title: Nursery                 # optional
controls: [listen, night_light, lullaby, talk, fullscreen, background]   # shown, in order
talk_mode: toggle            # or hold
listen: false                # default: listen on at load
listen_volume: 100           # default listen volume, 0-100 %
talk_volume: 100             # default talk gain, 0-200 %
autohide: 5                  # hide controls after N s; 0 = always shown
default_track: Brahms' Lullaby   # lullaby play starts this; unset = current track
default_timer: 30 min        # set before play: 5|10|20|30|60|90 min, Forever
overlay_bottom_left: binary_sensor.nursery_motion   # corner overlays (editor pickers)
# overlay_bottom_right: ...
# night_light: light.nursery_night_light   # optional overrides
# lullaby: select.nursery_lullaby
# lullaby_track: select.nursery_lullaby_track
# lullaby_playing: binary_sensor.nursery_lullaby_playing
# volume: number.nursery_volume
overlays:
  - entity: sensor.nursery_temperature
    position: bottom-right   # top-left|top-center|center-left|center|
                             # center-right|bottom-left|bottom-center|bottom-right
  - entity: binary_sensor.nursery_motion
    name: Motion             # label; `name: true` = friendly name
    position: bottom-left
  - entity: sensor.nursery_humidity
    top: 12                  # number = %, string = any CSS length
    left: 3
    style: "font-size: 20px; color: #ffd54f"
  - entity: sensor.nursery_temperature
    attribute: unit_of_measurement   # show an attribute instead of the state
    text: "Temp unit: {state}"       # template around the value
```

### Testing the card without Home Assistant

`tools/card-harness/run.sh` runs the real card in your browser against a fake
`hass`. A local go2rtc plays the part of Home Assistant's built-in go2rtc, so
video, listen audio and talk are real; the night light, lullaby, volume and
overlay entities are mocks you can edit in the side panel.

```sh
tools/run-local.sh                 # the bridge (other terminal)
tools/card-harness/run.sh          # then open http://localhost:8125/
tools/card-harness/run.sh --fake   # test pattern, no bridge needed (no talk)
```

Use `localhost`, not the LAN IP: browsers only grant the microphone to secure
pages. The card file is re-read on every reload. The page also has the card's
visual editor, a dark theme, card widths and an MJPEG-fallback switch.

## Known issues

- go2rtc's RTSP server is TCP only: RTSP clients forced to UDP fail.

## Diagnostic subcommands

```
tuya_local_avent probe          # find the monitor, detect protocol, prove the local key (read-only)
tuya_local_avent token-check    # check the app signing values are accepted (no password/MFA)
tuya_local_avent fetch-password # one-time signed login → writes data/bridge.json
tuya_local_avent run            # the full bridge (the container default)
```

## Security

- `docker-compose.yml` and `data/` (`bridge.json`, `cloud.json`,
  `go2rtc.yaml`, all 0600) hold secrets and are gitignored.
- No unauthenticated stream: see *How it works*. Use a long random
  `ONVIF_PASS`/`RTSP_PASS`.
- RTSP and ONVIF are plain-text protocols. Keep the container on a trusted
  VLAN. This is video of a child: never expose it to the internet.
- The card's VLC hand-off passes Home Assistant's HLS URL, which carries
  HA's stream token (anyone with the URL can watch until HA ends that
  stream, 30 s after the last request). It goes only to VLC on the phone,
  over HA's own connection (HTTPS when HA is).
- The burnt-in temperature (`data/temperature.txt`) is 0600 like the other
  data files.

## Credits / licence

MIT. The video bridge in `bridge/` is vendored from
[`thekoma/aventproxy`](https://github.com/thekoma/aventproxy) (MIT), whose LAN
streaming work is by Leonard Pitzu, itself building on seydx's
`tuya-ipc-terminal`. The Tuya login/signing is ported from aventproxy; the
Tuya Smart web login references [`AlexxIT/go2rtc`](https://github.com/AlexxIT/go2rtc).
Their LICENSE files are kept under `bridge/`.
