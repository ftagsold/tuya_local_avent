/*
 * tuya-local-avent-card: Home Assistant Lovelace card for the
 * tuya_local_avent bridge.
 *
 * - Video + listen audio: the ONVIF camera entity over Home Assistant's own
 *   WebRTC (camera/webrtc/offer, served by HA's built-in go2rtc). No
 *   transcoding in the card.
 * - Talk: the browser microphone on a second, send-only audio track of the
 *   same peer connection. go2rtc forwards it into the stream's RTSP
 *   backchannel, the bridge plays it on the monitor. Needs HTTPS (or
 *   localhost): browsers only grant the microphone to secure pages.
 * - Controls: a button group floating over the bottom of the video, in the
 *   configured order. Hidden until the video is tapped; hides again after
 *   `autohide` seconds without interaction, except while a control is open
 *   or talk is on. A tap on a button turns the group into that control
 *   (on/off toggle + slider); a tap on the video returns to the buttons. The
 *   night light, lullaby, lullaby track, lullaby playing and volume are the
 *   bridge's MQTT entities, found on the camera's device unless set
 *   explicitly. The lullaby panel also picks the track and the auto-stop
 *   timer; play first sets `default_timer`, then starts `default_track` (or
 *   resumes the monitor's current track).
 * - Sliders: drawn by the card (pointer events), not <input type=range>:
 *   they follow the turned landscape stage and hold a sent value until the
 *   entity reports it, so a state update does not jump the thumb back.
 * - Full screen: the video with its overlays and controls (Fullscreen API,
 *   or a fixed full-window layer where the browser has none, e.g. iPhone).
 *   On a phone held upright it shows landscape: orientation lock where the
 *   browser allows it, else the picture is turned by CSS.
 * - Defaults: listen on/off, listen volume and talk volume when the card
 *   loads. Browsers allow unmuted autoplay only after a user gesture: if
 *   refused, the card starts muted, shows "Tap for sound" and turns listen
 *   on at the first tap anywhere on the page.
 * - Background: listen audio plays from its own <audio> element, so it keeps
 *   playing while the page is hidden or the phone is locked, where the
 *   browser or app allows it (lock-screen media controls via Media Session).
 *   The Android Companion app pauses the page when it stops, so in full
 *   screen on Android a button hands the camera's sound to a media app
 *   such as VLC, which plays audio on with the screen locked. The stream is
 *   the tuya_local_avent integration's audio-only view, through Home
 *   Assistant's own URL (works remotely), as a path signed for this user
 *   (`auth/sign_path`, LISTEN_SIGN_S). Without the integration: Home
 *   Assistant's HLS stream (video too, which players may pause on lock).
 * - Overlays: other entities' states as plain HTML text over the video:
 *   the editor's three corner pickers, plus the `overlays` YAML list.
 *
 * No build step, no dependencies: copy to /config/www and add
 * /local/tuya-local-avent-card.js as a dashboard resource (JavaScript module).
 */

const CARD = "tuya-local-avent-card";
const VERSION = "1.5.0";

const ALL_CONTROLS = ["listen", "night_light", "lullaby", "talk", "fullscreen", "background"];
// Controls that open a panel (toggle + slider); the rest act on tap.
const PANEL_CONTROLS = ["listen", "night_light", "lullaby", "talk"];
// Names from 1.0 whose sliders now live inside their control.
const LEGACY_CONTROLS = ["lullaby_volume", "talk_volume"];
const CONTROL_LABELS = {
  listen: "Listen (mute + volume)",
  night_light: "Night light (on/off + 33 / 66 / 100 %)",
  lullaby: "Lullaby (play/stop + track + timer + volume)",
  talk: "Talk (on/off + talk volume)",
  fullscreen: "Full screen",
  background: "Listen in background (full screen, Android: opens a media app)",
};
const LIGHT_LEVELS = [33, 66, 100];
const ICONS = {
  listen: ["mdi:volume-high", "mdi:volume-off"],
  night_light: ["mdi:lightbulb-night", "mdi:lightbulb-night-outline"],
  lullaby: ["mdi:music", "mdi:music-off"],
  talk: ["mdi:microphone", "mdi:microphone-off"],
  fullscreen: ["mdi:fullscreen-exit", "mdi:fullscreen"],
  background: ["mdi:headphones", "mdi:headphones"],
};
// The monitor's built-in tracks (the bridge's lullaby track options); the
// editor's fallback when that entity is not found.
const TRACKS = [
  "Baa Baa Black Sheep", "Brahms' Lullaby", "Rock-a-Bye Baby", "Golden Slumbers", "Hush Little Baby",
  "Mother's Shh", "Calming River", "Heartbeat", "Vacuum Cleaner", "White Noise",
  "Garden Bird Song", "Valley Wind", "Ocean Shore", "Night-time Nature", "Rain Shower",
];
// How long a signed listen link (handed to a media app) stays valid: one
// night. The app reconnects with it after a network drop.
const LISTEN_SIGN_S = 12 * 3600;
// The bridge's lullaby timer options.
const TIMERS = ["5 min", "10 min", "20 min", "30 min", "60 min", "90 min", "Forever"];
// Editor corner pickers: config key -> overlay position.
const CORNERS = { overlay_top_right: "top-right", overlay_bottom_left: "bottom-left", overlay_bottom_right: "bottom-right" };
// A slider value sent to an entity wins over its state for this long, or
// until the state matches.
const PENDING_MS = 5000;
// Config defaults; the editor leaves keys at these values out of the YAML.
const DEFAULTS = { talk_mode: "toggle", listen: false, listen_volume: 100, talk_volume: 100, autohide: 5 };
// No decoded video frame for this long = dead stream: reconnect.
const STALL_MS = 15000;
const POSITIONS = {
  "top-left": "top:8px;left:8px;",
  "top-center": "top:8px;left:50%;transform:translateX(-50%);",
  "top-right": "top:8px;right:8px;text-align:right;",
  "center-left": "top:50%;left:8px;transform:translateY(-50%);",
  center: "top:50%;left:50%;transform:translate(-50%,-50%);text-align:center;",
  "center-right": "top:50%;right:8px;transform:translateY(-50%);text-align:right;",
  "bottom-left": "bottom:8px;left:8px;",
  "bottom-center": "bottom:8px;left:50%;transform:translateX(-50%);",
  "bottom-right": "bottom:8px;right:8px;text-align:right;",
};
// Entities the bridge creates (MQTT discovery), matched on the camera's device.
const AUTO = {
  night_light: { domain: "light", suffix: "night_light" },
  lullaby: { domain: "select", suffix: "lullaby" },
  lullaby_track: { domain: "select", suffix: "lullaby_track" },
  lullaby_playing: { domain: "binary_sensor", suffix: "lullaby_playing" },
  lullaby_timer: { domain: "select", suffix: "lullaby_timer" },
  volume: { domain: "number", suffix: "volume" },
};

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

const STYLE = `
  :host { display: block; }
  ha-card { overflow: hidden; }
  .video { position: relative; background: #000; aspect-ratio: 16 / 9; }
  .video:fullscreen { aspect-ratio: auto; width: 100%; height: 100%; }
  .video.pseudo-fs { position: fixed; inset: 0; z-index: 9999; aspect-ratio: auto; }
  /* Everything over the video lives in the stage, so it can be turned to
     landscape on a portrait phone (the fullscreen element itself cannot be
     transformed). */
  .stage { position: absolute; inset: 0; }
  .video.rotated .stage { inset: auto; top: 50%; left: 50%; width: 100vh; height: 100vw;
    width: 100dvh; height: 100dvw; transform: translate(-50%, -50%) rotate(90deg); }
  .hint { position: absolute; z-index: 2; top: 8px; left: 50%; transform: translateX(-50%);
    display: flex; align-items: center; gap: 6px; padding: 4px 12px 4px 8px; border-radius: 999px;
    background: rgba(20, 20, 20, .55); color: #fff; font-size: 13px; pointer-events: none; white-space: nowrap; }
  .hint[hidden] { display: none; }
  .video video, .video img { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: contain; }
  .overlay { position: absolute; z-index: 2; pointer-events: none; color: #fff; font-size: 14px;
    line-height: 1.3; text-shadow: 0 0 3px #000, 0 0 6px #000; white-space: nowrap; }
  .status { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center;
    color: #ccc; font-size: 14px; z-index: 1; text-align: center; padding: 16px; }
  .status[hidden] { display: none; }

  /* Floating control group */
  .group { position: absolute; z-index: 3; left: 50%; bottom: 12px; transform: translateX(-50%);
    display: flex; align-items: center; gap: 6px; padding: 6px; border-radius: 999px;
    background: rgba(20, 20, 20, .55); -webkit-backdrop-filter: blur(12px); backdrop-filter: blur(12px);
    box-shadow: 0 4px 16px rgba(0, 0, 0, .35); color: #fff;
    max-width: calc(100% - 24px); transition: opacity .25s; }
  .group.away { opacity: 0; pointer-events: none; }
  .group[hidden], .group [hidden] { display: none !important; }
  .group.open { width: min(440px, calc(100% - 24px)); }
  .group.open.wide { width: min(600px, calc(100% - 24px)); }
  .btn { flex: 0 0 auto; width: 44px; height: 44px; border-radius: 50%; border: 0; padding: 0;
    display: inline-flex; align-items: center; justify-content: center; cursor: pointer;
    background: rgba(255, 255, 255, .12); color: #fff; transition: background .15s, transform .1s;
    touch-action: manipulation; -webkit-tap-highlight-color: transparent; }
  .btn:hover { background: rgba(255, 255, 255, .22); }
  .btn:active { transform: scale(.94); }
  .btn.on { background: var(--primary-color, #03a9f4); color: var(--text-primary-color, #fff); }
  .btn.talking { background: var(--error-color, #db4437); color: #fff;
    box-shadow: 0 0 0 0 rgba(219, 68, 55, .6); animation: pulse 1.4s infinite; }
  .btn[disabled] { opacity: .4; cursor: default; }
  @keyframes pulse { 70% { box-shadow: 0 0 0 10px rgba(219, 68, 55, 0); } 100% { box-shadow: 0 0 0 0 rgba(219, 68, 55, 0); } }
  .panel { display: flex; align-items: center; gap: 12px; flex: 1 1 auto; min-width: 0; padding-right: 10px; }
  .slider { flex: 1 1 auto; min-width: 0; height: 40px; display: flex; align-items: center; cursor: pointer;
    touch-action: none; -webkit-tap-highlight-color: transparent; outline: none; }
  .slider .rail { position: relative; width: 100%; height: 6px; border-radius: 3px; background: rgba(255, 255, 255, .25); }
  .slider .fill { position: absolute; left: 0; top: 0; bottom: 0; border-radius: 3px; background: var(--primary-color, #03a9f4); }
  .slider .thumb { position: absolute; top: 50%; width: 20px; height: 20px; margin: -10px 0 0 -10px; border-radius: 50%;
    background: #fff; box-shadow: 0 1px 4px rgba(0, 0, 0, .45); }
  .slider:focus-visible .thumb { box-shadow: 0 0 0 3px var(--primary-color, #03a9f4); }
  .slider.disabled { opacity: .4; pointer-events: none; }
  .panel select { flex: 0 1 auto; min-width: 0; max-width: 38%; height: 32px; padding: 0 8px; border: 0; border-radius: 999px;
    background: rgba(255, 255, 255, .12); color: #fff; font: inherit; font-size: 13px; text-overflow: ellipsis; }
  .panel select option { color: #000; }
  .panel .val { flex: 0 0 auto; min-width: 3.2em; text-align: right; font-size: 13px;
    font-variant-numeric: tabular-nums; opacity: .9; }
  .panel .note { font-size: 12px; opacity: .85; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
`;

class TuyaLocalAventCard extends HTMLElement {
  constructor() {
    super();
    this._gen = 0; // bumped by _stopVideo, see _startVideo
    // Taps inside the card are handled by the video's click handler.
    this._onOutside = (e) => {
      if (!this._open || !this._els) return;
      if (!e.composedPath().includes(this)) this._setOpen(null);
    };
    this._onKey = (e) => {
      if (e.key !== "Escape") return;
      if (this._pseudoFs) this._setPseudoFs(false);
      this._setOpen(null);
    };
    this._onFullscreen = () => {
      if (!this._isFullscreen()) this._unlockOrientation();
      this._fitRotation();
      this._update();
    };
    this._onResize = () => this._fitRotation();
    this._unlockListen = () => {
      document.removeEventListener("pointerdown", this._unlockListen, true);
      if (this._els) this._els.hint.hidden = true;
      if (this._listen) this._applyListen();
      this._update();
    };
    this._onVisible = () => {
      if (document.hidden) return;
      if (this._mjpeg) return this._reloadMjpeg();
      // A browser may pause media while hidden; resume it.
      if (this._stream) {
        this._els?.video.play?.()?.catch(() => {});
        if (this._listen) this._els?.audio.play().catch(() => {});
      }
    };
  }

  static getConfigElement() {
    return document.createElement(`${CARD}-editor`);
  }

  static getStubConfig(hass) {
    const camera = Object.keys(hass.states).find((e) => e.startsWith("camera.")) || "";
    return { camera, controls: [...ALL_CONTROLS] };
  }

  setConfig(config) {
    if (!config || !config.camera) throw new Error("camera is required (the ONVIF camera entity)");
    if (config.overlays !== undefined && !Array.isArray(config.overlays)) throw new Error("overlays must be a list");
    const overlays = [
      ...(config.overlays ?? []),
      ...Object.entries(CORNERS)
        .filter(([k]) => config[k])
        .map(([k, position]) => ({ entity: config[k], position })),
    ];
    const controls = (config.controls ?? ALL_CONTROLS).filter((c) => !LEGACY_CONTROLS.includes(c));
    const bad = controls.filter((c) => !ALL_CONTROLS.includes(c));
    if (bad.length) throw new Error(`unknown controls: ${bad.join(", ")} (use ${ALL_CONTROLS.join(", ")})`);
    const range = (key, min, max) => {
      const v = config[key];
      if (v !== undefined && !(Number.isFinite(Number(v)) && v >= min && v <= max)) throw new Error(`${key} must be ${min}-${max}`);
    };
    range("listen_volume", 0, 100);
    range("talk_volume", 0, 200);
    range("autohide", 0, 3600);
    const restart = this._config && this._config.camera !== config.camera;
    this._autoReg = null; // entity overrides / camera may have changed
    this._config = { ...DEFAULTS, ...config, controls, overlays };
    this._talkGain = this._config.talk_volume / 100;
    this._listenVol = this._config.listen_volume / 100;
    this._listen = !!this._config.listen;
    this._built = false;
    this._open = null;
    this._pending = {};
    if (restart) {
      this._stopVideo();
      this._mjpeg = false;
    }
    if (this._hass) this._render();
  }

  set hass(hass) {
    const first = !this._hass;
    this._hass = hass;
    if (!this._config) return;
    if (first || !this._built) this._render();
    else this._update();
    if (this.isConnected && !this._pc && !this._starting && !this._retry && !this._mjpeg) this._startVideo();
  }

  connectedCallback() {
    document.addEventListener("pointerdown", this._onOutside, true);
    document.addEventListener("keydown", this._onKey);
    document.addEventListener("fullscreenchange", this._onFullscreen);
    document.addEventListener("webkitfullscreenchange", this._onFullscreen);
    document.addEventListener("visibilitychange", this._onVisible);
    window.addEventListener("resize", this._onResize);
    if (this._mjpeg) this._reloadMjpeg();
    else if (this._hass && this._config && !this._pc) this._startVideo();
  }

  disconnectedCallback() {
    document.removeEventListener("pointerdown", this._onOutside, true);
    document.removeEventListener("keydown", this._onKey);
    document.removeEventListener("fullscreenchange", this._onFullscreen);
    document.removeEventListener("webkitfullscreenchange", this._onFullscreen);
    document.removeEventListener("visibilitychange", this._onVisible);
    document.removeEventListener("pointerdown", this._unlockListen, true);
    window.removeEventListener("resize", this._onResize);
    clearTimeout(this._hideTimer);
    this._stopTalk();
    this._stopVideo();
    // A detached <img> keeps downloading the MJPEG stream.
    if (this._mjpeg) this._els?.video.removeAttribute("src");
  }

  getCardSize() {
    return 4;
  }

  getGridOptions() {
    return { columns: 12, min_columns: 6, rows: "auto" };
  }

  // ---- entities ----

  _entity(key) {
    if (this._config[key]) return this._config[key];
    const reg = this._hass.entities || {};
    // hass updates arrive often; rescan the registry only when it changes.
    if (this._autoReg !== reg) {
      this._autoReg = reg;
      this._auto = {};
    }
    if (key in this._auto) return this._auto[key];
    const dev = reg[this._config.camera]?.device_id;
    const { domain, suffix } = AUTO[key];
    return (this._auto[key] = dev
      ? Object.values(reg).find((e) => e.device_id === dev && e.entity_id.startsWith(`${domain}.`) && e.entity_id.endsWith(suffix))
          ?.entity_id
      : undefined);
  }

  _state(id) {
    return id ? this._hass.states[id] : undefined;
  }

  _call(domain, service, data) {
    return this._hass.callService(domain, service, data).catch((e) => console.warn(CARD, service, e));
  }

  // ---- control model: per control, its state and what toggle/slider do ----

  _lightLevel() {
    const st = this._state(this._entity("night_light"));
    const pct = Math.round(((st?.attributes.brightness ?? 255) / 255) * 100);
    return LIGHT_LEVELS.reduce((a, b) => (Math.abs(b - pct) < Math.abs(a - pct) ? b : a));
  }

  _model(key) {
    const unavailable = (st) => !st || st.state === "unavailable" || st.state === "unknown";
    switch (key) {
      case "listen": {
        const a = this._els?.audio;
        return {
          on: !!a && !a.muted,
          disabled: !!this._mjpeg,
          slider: { min: 0, max: 100, step: 1, value: Math.round(this._listenVol * 100), text: `${Math.round(this._listenVol * 100)}%` },
        };
      }
      case "night_light": {
        const st = this._state(this._entity("night_light"));
        const level = this._lightLevel();
        return {
          on: st?.state === "on",
          disabled: unavailable(st),
          slider: { min: 1, max: LIGHT_LEVELS.length, step: 1, value: LIGHT_LEVELS.indexOf(level) + 1, text: `${level}%` },
        };
      }
      case "lullaby": {
        // The monitor reports no state until something changes, so
        // "unknown" entities stay usable; only "unavailable" disables.
        const gone = (st) => !st || st.state === "unavailable";
        const st = this._state(this._entity("lullaby"));
        const playing = this._state(this._entity("lullaby_playing"));
        const vol = this._state(this._entity("volume"));
        const n = Number(vol?.state);
        const min = Number(vol?.attributes.min ?? 1);
        const max = Number(vol?.attributes.max ?? 100);
        return {
          on: playing && !unavailable(playing) ? playing.state === "on" : st?.state === "play",
          disabled: gone(st),
          slider: gone(vol)
            ? { disabled: true, min: 0, max: 100, step: 1, value: 0, text: "—" }
            : { min, max, step: Number(vol.attributes.step ?? 1), value: Number.isFinite(n) ? n : min, text: Number.isFinite(n) ? `${Math.round(n)}%` : "—" },
        };
      }
      case "talk": {
        const secure = window.isSecureContext && !!navigator.mediaDevices?.getUserMedia;
        return {
          on: !!this._talking,
          disabled: !secure || !this._talkSender,
          note: secure ? "" : "Talk needs Home Assistant over HTTPS",
          slider: { min: 0, max: 200, step: 5, value: Math.round(this._talkGain * 100), text: `${Math.round(this._talkGain * 100)}%` },
        };
      }
    }
  }

  _toggle(key) {
    const m = this._model(key);
    if (m.disabled) return;
    switch (key) {
      case "listen":
        this._listen = !m.on;
        this._applyListen();
        break;
      case "night_light": {
        const entity_id = this._entity("night_light");
        if (m.on) this._call("light", "turn_off", { entity_id });
        else this._call("light", "turn_on", { entity_id, brightness_pct: this._lightLevel() });
        break;
      }
      case "lullaby": {
        if (m.on) {
          this._call("select", "select_option", { entity_id: this._entity("lullaby"), option: "stop" });
          break;
        }
        // Timer before play: the monitor starts its countdown with the track.
        const timer = this._entity("lullaby_timer");
        const track = this._entity("lullaby_track");
        const { default_timer: t, default_track: def } = this._config;
        (t && timer ? this._call("select", "select_option", { entity_id: timer, option: t }) : Promise.resolve()).then(() => {
          if (def && track) this._call("select", "select_option", { entity_id: track, option: def });
          else this._call("select", "select_option", { entity_id: this._entity("lullaby"), option: "play" });
        });
        break;
      }
      case "talk":
        if (this._config.talk_mode !== "hold") this._talking ? this._stopTalk() : this._startTalk();
        break;
    }
    this._update();
  }

  // Live while dragging only for local values; entity writes on release.
  _slide(key, value, final) {
    if (final && (key === "night_light" || key === "lullaby")) this._pending[key] = { value, until: Date.now() + PENDING_MS };
    switch (key) {
      case "listen":
        this._listenVol = value / 100;
        this._els.audio.volume = this._listenVol;
        break;
      case "night_light":
        if (final) this._call("light", "turn_on", { entity_id: this._entity("night_light"), brightness_pct: LIGHT_LEVELS[value - 1] });
        break;
      case "lullaby":
        if (final) this._call("number", "set_value", { entity_id: this._entity("volume"), value });
        break;
      case "talk":
        this._talkGain = value / 100;
        if (this._gain) this._gain.gain.value = this._talkGain;
        break;
    }
  }

  // ---- DOM ----

  _render() {
    if (!this.shadowRoot) this.attachShadow({ mode: "open" });
    const c = this._config;
    const icon = (key) => `<ha-icon icon="${ICONS[key][0]}"></ha-icon>`;
    const panelKeys = c.controls.filter((k) => PANEL_CONTROLS.includes(k));
    const buttons = c.controls
      .map((k) =>
        PANEL_CONTROLS.includes(k)
          ? `<button class="btn" data-open="${k}" title="${esc(CONTROL_LABELS[k])}">${icon(k)}</button>`
          : `<button class="btn" ${k === "fullscreen" ? "data-fs" : "data-bg hidden"} title="${esc(CONTROL_LABELS[k])}">${icon(k)}</button>`,
      )
      .join("");
    const panels = panelKeys
      .map(
        (k) => `<div class="panel" data-panel="${k}" hidden>
          <button class="btn" data-toggle="${k}">${icon(k)}</button>
          ${k === "lullaby" ? `<select data-pick="lullaby_track" aria-label="Lullaby track" hidden></select><select data-pick="lullaby_timer" aria-label="Lullaby timer" hidden></select>` : ""}
          <div class="slider" data-slider="${k}" role="slider" tabindex="0" aria-label="${esc(CONTROL_LABELS[k])}">
            <div class="rail"><div class="fill"></div><div class="thumb"></div></div>
          </div>
          <span class="val"></span>
        </div>`,
      )
      .join("");
    this.shadowRoot.innerHTML = `<style>${STYLE}</style><ha-card>
      ${c.title ? `<h1 class="card-header">${esc(c.title)}</h1>` : ""}
      <div class="video" id="video"><div class="stage">
        <video id="v" autoplay playsinline muted></video>
        <audio id="a" autoplay playsinline muted></audio>
        <div class="status" id="status">Connecting…</div>
        <div class="hint" id="hint" hidden><ha-icon icon="mdi:volume-off"></ha-icon>Tap for sound</div>
        <div id="overlays"></div>
        <div class="group" id="group" ${c.controls.length ? "" : "hidden"}>
          <div class="main" id="main" style="display:flex;gap:6px">${buttons}</div>
          ${panels}
        </div>
      </div></div></ha-card>`;
    const $ = (id) => this.shadowRoot.getElementById(id);
    this._els = {
      wrap: $("video"), video: $("v"), audio: $("a"), status: $("status"), hint: $("hint"),
      overlays: $("overlays"), group: $("group"), main: $("main"),
    };
    this._overlayHtml = null;
    this._pseudoFs = false;
    if (this._mjpeg) this._showMjpeg();
    this._els.audio.volume = this._listenVol;
    if (this._stream) {
      this._els.video.srcObject = this._stream;
      this._els.audio.srcObject = this._audioStream;
      this._applyListen();
    }

    const group = this._els.group;
    // A tap on the video shows or hides the controls; with a control open,
    // it goes back to the buttons. A mouse shows them on move, so a click
    // never hides them.
    this._els.wrap.addEventListener("click", (e) => {
      if (e.composedPath().includes(group)) return;
      if (this._open) this._setOpen(null);
      else if (this._shown && this._config.autohide > 0 && e.pointerType !== "mouse") this._setShown(false);
      else this._poke();
    });
    this._els.wrap.addEventListener("pointermove", (e) => e.pointerType === "mouse" && this._poke());
    for (const ev of ["pointerdown", "input"]) group.addEventListener(ev, () => this._poke());
    group.addEventListener("click", (e) => {
      if (e.target.closest("[data-fs]")) return this._toggleFullscreen();
      if (e.target.closest("[data-bg]")) return this._listenInBackground();
      const open = e.target.closest("[data-open]");
      if (open) return this._setOpen(open.dataset.open);
      const tog = e.target.closest("[data-toggle]");
      if (tog && !(tog.dataset.toggle === "talk" && c.talk_mode === "hold")) this._toggle(tog.dataset.toggle);
    });
    if (c.talk_mode === "hold") {
      const talk = group.querySelector('[data-toggle="talk"]');
      if (talk) {
        talk.addEventListener("pointerdown", (e) => {
          talk.setPointerCapture?.(e.pointerId);
          this._startTalk();
        });
        for (const ev of ["pointerup", "pointercancel"]) talk.addEventListener(ev, () => this._stopTalk());
        talk.addEventListener("contextmenu", (e) => e.preventDefault());
      }
    }
    for (const s of group.querySelectorAll("[data-slider]")) this._bindSlider(s);
    for (const sel of group.querySelectorAll("[data-pick]")) {
      sel.addEventListener("change", () => {
        const entity_id = this._entity(sel.dataset.pick);
        if (entity_id && sel.value) this._call("select", "select_option", { entity_id, option: sel.value });
        // Focus would keep _updatePick from showing later changes.
        sel.blur();
      });
    }
    this._built = true;
    this._setShown(!(c.autohide > 0));
    this._setOpen(null);
  }

  // ---- sliders ----

  // Pointer drag along the rail. In the turned landscape stage the rail runs
  // top to bottom on screen, so the position comes from clientY.
  _bindSlider(s) {
    const key = s.dataset.slider;
    const at = (e) => {
      const r = s.querySelector(".rail").getBoundingClientRect();
      const turned = this._els.wrap.classList.contains("rotated");
      const f = turned ? (e.clientY - r.top) / r.height : (e.clientX - r.left) / r.width;
      const { min, max, step } = s._range;
      const v = min + Math.round((Math.min(1, Math.max(0, f)) * (max - min)) / step) * step;
      return Math.min(max, Math.max(min, v));
    };
    const move = (v, final) => {
      if (v === s._value && !final) return;
      this._setSlider(s, v);
      this._sliderText(key, v);
      this._slide(key, v, final);
    };
    s.addEventListener("pointerdown", (e) => {
      if (s.classList.contains("disabled") || e.button > 0) return;
      e.preventDefault();
      s.setPointerCapture?.(e.pointerId);
      s._drag = { from: s._value };
      s.focus({ preventScroll: true });
      move(at(e), false);
    });
    s.addEventListener("pointermove", (e) => s._drag && move(at(e), false));
    const end = (e) => {
      if (!s._drag) return;
      const changed = s._value !== s._drag.from;
      s._drag = null;
      if (e.type === "pointerup" || changed) move(e.type === "pointerup" ? at(e) : s._value, true);
    };
    s.addEventListener("pointerup", end);
    s.addEventListener("pointercancel", end);
    s.addEventListener("keydown", (e) => {
      const d = { ArrowRight: 1, ArrowUp: 1, ArrowLeft: -1, ArrowDown: -1 }[e.key];
      if (!d || s.classList.contains("disabled")) return;
      e.preventDefault();
      const { min, max, step } = s._range;
      move(Math.min(max, Math.max(min, s._value + d * step)), true);
    });
  }

  _setSlider(s, value) {
    const { min, max } = s._range;
    s._value = value;
    const pct = max > min ? ((value - min) / (max - min)) * 100 : 0;
    s.querySelector(".fill").style.width = `${pct}%`;
    s.querySelector(".thumb").style.left = `${pct}%`;
    s.setAttribute("aria-valuenow", value);
  }

  _setOpen(key) {
    this._open = key;
    if (!this._els) return;
    this._els.main.hidden = !!key;
    this._els.group.classList.toggle("open", !!key);
    this._els.group.classList.toggle("wide", key === "lullaby");
    for (const p of this._els.group.querySelectorAll("[data-panel]")) p.hidden = p.dataset.panel !== key;
    if (!key) this._rearm();
    this._update();
  }

  // ---- control visibility (autohide) ----

  _setShown(on) {
    this._shown = on;
    this._els?.group.classList.toggle("away", !on);
  }

  // Show the controls and restart the hide countdown.
  _poke() {
    if (!this._els) return;
    this._setShown(true);
    this._rearm();
  }

  // Restart the countdown if shown. An open control or a live talk keeps
  // the group up; closing it / stopping talk calls this again.
  _rearm() {
    clearTimeout(this._hideTimer);
    const secs = Number(this._config.autohide);
    if (!this._shown || !(secs > 0)) return;
    this._hideTimer = setTimeout(() => {
      if (!this._open && !this._talking) this._setShown(false);
    }, secs * 1000);
  }

  // ---- full screen ----

  _isFullscreen() {
    if (this._pseudoFs) return true;
    const fs = this.shadowRoot?.fullscreenElement ?? null;
    return !!fs && fs === this._els?.wrap;
  }

  async _toggleFullscreen() {
    const wrap = this._els.wrap;
    if (this._isFullscreen()) {
      if (this._pseudoFs) this._setPseudoFs(false);
      else (document.exitFullscreen || document.webkitExitFullscreen)?.call(document);
      this._unlockOrientation();
      return;
    }
    const req = wrap.requestFullscreen || wrap.webkitRequestFullscreen;
    try {
      if (!req) throw new Error("no Fullscreen API");
      await req.call(wrap, { navigationUI: "hide" });
      // Phones: landscape. Allowed only in fullscreen, and not everywhere
      // (iOS, some WebViews): _fitRotation turns the stage instead.
      if (this._phone()) screen.orientation?.lock?.("landscape").catch(() => {});
    } catch (e) {
      console.info(CARD, "fullscreen fallback:", e.message || e);
      this._setPseudoFs(true);
    }
  }

  // Fixed full-window layer for browsers without element fullscreen.
  _setPseudoFs(on) {
    this._pseudoFs = on;
    this._els.wrap.classList.toggle("pseudo-fs", on);
    this._fitRotation();
    this._update();
  }

  _phone() {
    return window.matchMedia?.("(pointer: coarse)").matches ?? false;
  }

  // Full screen on a phone held upright: show the video turned to landscape.
  // Re-checked on resize, so turning the phone (or an orientation lock that
  // took effect) drops the rotation.
  _fitRotation() {
    if (!this._els) return;
    const on = this._isFullscreen() && this._phone() && window.innerHeight > window.innerWidth;
    this._els.wrap.classList.toggle("rotated", on);
  }

  _unlockOrientation() {
    try {
      screen.orientation?.unlock?.();
    } catch (_) {}
  }

  _android() {
    return /Android/i.test(navigator.userAgent);
  }

  // Hand the listen audio to a media app (Android "open with"): it plays on
  // with the screen locked and shows lock-screen controls. Preferred: the
  // tuya_local_avent integration's audio-only view, signed for this user.
  // Fallback: HA's HLS stream (stream token, video too). The card's own
  // listen goes off, else both play once the app is back.
  async _listenInBackground() {
    try {
      let url;
      let type = "audio/aac";
      if (this._hass.config?.components?.includes("tuya_local_avent")) {
        const path = `/api/tuya_local_avent/listen/${encodeURIComponent(this._config.camera)}`;
        url = (await this._hass.callWS({ type: "auth/sign_path", path, expires: LISTEN_SIGN_S })).path;
      } else {
        console.info(CARD, "tuya_local_avent integration not loaded: background listen uses HLS (video too)");
        url = (await this._hass.callWS({ type: "camera/stream", entity_id: this._config.camera, format: "hls" })).url;
        type = "application/vnd.apple.mpegurl";
      }
      const u = new URL(url, location.href);
      const title = this._config.title || this._state(this._config.camera)?.attributes.friendly_name || "Baby monitor";
      this._listen = false;
      this._applyListen();
      // Navigations to intent: URIs are launched by the Companion app (and Chrome).
      location.href =
        `intent://${u.host}${u.pathname}${u.search}#Intent;scheme=${u.protocol.replace(":", "")};` +
        `action=android.intent.action.VIEW;type=${type};S.title=${encodeURIComponent(title)};end`;
    } catch (e) {
      console.warn(CARD, "background listen", e);
      this._setStatus(`No HLS stream from Home Assistant: ${e.message || e.code || e}`);
      setTimeout(() => this._pc?.connectionState === "connected" && this._setStatus(""), 5000);
    }
  }

  // ---- listen ----

  // The video element follows the listen state. Unmuting without a user
  // gesture is refused (Chrome pauses the video): play muted and retry at
  // the next tap anywhere.
  _applyListen() {
    const a = this._els?.audio;
    if (!a || this._mjpeg) return;
    this._els.hint.hidden = true;
    a.volume = this._listenVol;
    a.muted = !this._listen;
    this._els.video.play?.()?.catch(() => {});
    this._mediaSession();
    this._update();
    a.play().catch((e) => {
      if (e.name !== "NotAllowedError" || a.muted) return;
      a.muted = true;
      a.play().catch(() => {});
      this._els.hint.hidden = false;
      document.addEventListener("pointerdown", this._unlockListen, true);
      this._update();
    });
  }

  // Lock-screen / notification entry while listening: tells the OS this page
  // plays audio, and gives a pause (mute) control there.
  _mediaSession() {
    const ms = navigator.mediaSession;
    if (!ms || !this._listen) return;
    try {
      const name = this._config.title || this._state(this._config.camera)?.attributes.friendly_name || "Baby monitor";
      ms.metadata = new MediaMetadata({ title: name, artist: "Listening" });
      ms.playbackState = "playing";
      ms.setActionHandler("pause", () => {
        this._listen = false;
        this._applyListen();
        ms.playbackState = "paused";
      });
      ms.setActionHandler("play", () => {
        this._listen = true;
        this._applyListen();
      });
    } catch (_) {}
  }

  _sliderText(key, value) {
    const val = this._els.group.querySelector(`[data-panel="${key}"] .val`);
    if (!val) return;
    val.textContent = key === "night_light" ? `${LIGHT_LEVELS[value - 1]}%` : `${value}%`;
  }

  _update() {
    if (!this._els) return;
    const root = this.shadowRoot;
    const fs = root.querySelector("[data-fs] ha-icon");
    if (fs) fs.setAttribute("icon", ICONS.fullscreen[this._isFullscreen() ? 0 : 1]);
    const bg = root.querySelector("[data-bg]");
    if (bg) bg.hidden = !(this._isFullscreen() && this._android());
    for (const key of this._config.controls) {
      if (!PANEL_CONTROLS.includes(key)) continue;
      const m = this._model(key);
      const talk = key === "talk";
      const icon = ICONS[key][m.on || talk ? 0 : 1];
      const tog = root.querySelector(`[data-toggle="${key}"]`);
      for (const b of [root.querySelector(`[data-open="${key}"]`), tog]) {
        b.classList.toggle("on", m.on && !talk);
        b.classList.toggle("talking", talk && m.on);
        b.querySelector("ha-icon").setAttribute("icon", icon);
      }
      tog.disabled = m.disabled;
      tog.title = m.note || (m.on ? "Turn off" : "Turn on");
      const s = root.querySelector(`[data-slider="${key}"]`);
      // Not while dragging, and a just-sent value until the entity has it.
      const p = this._pending[key];
      if (p && (Date.now() > p.until || p.value === m.slider.value)) delete this._pending[key];
      if (!s._drag) {
        s._range = { min: m.slider.min, max: m.slider.max, step: m.slider.step };
        const v = this._pending[key]?.value ?? m.slider.value;
        this._setSlider(s, v);
        const val = root.querySelector(`[data-panel="${key}"] .val`);
        if (m.note) val.textContent = "";
        else if (this._pending[key]) this._sliderText(key, v);
        else val.textContent = m.slider.text;
      }
      s.classList.toggle("disabled", !!m.slider.disabled || (key !== "talk" && key !== "listen" && m.disabled));
      s.hidden = !!m.note;
      if (key === "lullaby") {
        this._updatePick("lullaby_track", "Track", this._config.default_track, m.disabled);
        this._updatePick("lullaby_timer", "Timer", this._config.default_timer, m.disabled);
      }
      let note = root.querySelector(`[data-panel="${key}"] .note`);
      if (m.note && !note) {
        note = document.createElement("span");
        note.className = "note";
        s.after(note);
      }
      if (note) note.textContent = m.note || "";
    }
    this._renderOverlays();
  }

  // A lullaby picker (track, timer): the entity's options, showing its state
  // (else the configured default).
  _updatePick(key, placeholder, fallback, disabled) {
    const sel = this.shadowRoot.querySelector(`[data-pick="${key}"]`);
    if (!sel) return;
    const st = this._state(this._entity(key));
    const options = st?.attributes.options ?? [];
    sel.hidden = !options.length;
    if (!options.length) return;
    const list = options.join("\n");
    if (sel._key !== list) {
      sel._key = list;
      sel.innerHTML = `<option value="" disabled>${placeholder}</option>${options.map((o) => `<option>${esc(o)}</option>`).join("")}`;
    }
    if (this.shadowRoot.activeElement !== sel) {
      const cur = [st.state, fallback].find((o) => options.includes(o));
      sel.value = cur ?? "";
    }
    sel.disabled = disabled || st.state === "unavailable";
  }

  _renderOverlays() {
    const html = this._config.overlays
      .map((o) => {
        const ov = typeof o === "string" ? { entity: o } : o;
        const st = this._state(ov.entity);
        let text;
        if (!st) text = ov.entity ? "—" : "";
        else if (ov.attribute) text = `${st.attributes[ov.attribute] ?? "—"}`;
        else if (this._hass.formatEntityState) text = this._hass.formatEntityState(st);
        else text = `${st.state}${st.attributes.unit_of_measurement ? ` ${st.attributes.unit_of_measurement}` : ""}`;
        if (ov.text) text = `${ov.text}`.replace("{state}", text);
        const name = ov.name === true ? st?.attributes.friendly_name : ov.name;
        const label = name ? `${esc(name)}: ` : "";
        const pos = POSITIONS[ov.position || "top-right"] ?? "";
        const custom = ["top", "left", "right", "bottom"]
          .filter((k) => ov[k] !== undefined)
          .map((k) => `${k}:${typeof ov[k] === "number" ? `${ov[k]}%` : esc(ov[k])};`)
          .join("");
        const style = `${custom || pos}${ov.style ? esc(ov.style) : ""}`;
        return `<div class="overlay" style="${style}">${label}${esc(text)}</div>`;
      })
      .join("");
    if (html !== this._overlayHtml) {
      this._els.overlays.innerHTML = html;
      this._overlayHtml = html;
    }
  }

  _setStatus(text) {
    if (!this._els) return;
    this._els.status.hidden = !text;
    this._els.status.textContent = text || "";
  }

  // ---- video (HA WebRTC) ----

  async _startVideo() {
    if (this._starting || this._pc || !this._hass || !this._config) return;
    this._starting = true;
    const entity_id = this._config.camera;
    // _stopVideo (disconnect, camera change, retry) bumps the generation: a
    // start still awaiting HA must not leave a connection behind.
    const gen = this._gen;
    const stale = () => gen !== this._gen || !this.isConnected;
    try {
      const caps = await this._hass.callWS({ type: "camera/capabilities", entity_id }).catch(() => null);
      if (stale()) return;
      if (caps && !caps.frontend_stream_types?.includes("web_rtc")) {
        this._startMjpeg("No WebRTC in Home Assistant (go2rtc): low-quality MJPEG, no listen or talk.");
        return;
      }
      const client = await this._hass.callWS({ type: "camera/webrtc/get_client_config", entity_id });
      if (stale()) return;
      const pc = new RTCPeerConnection(client.configuration || {});
      this._pc = pc;
      if (client.dataChannel) pc.createDataChannel(client.dataChannel);
      pc.addTransceiver("video", { direction: "recvonly" });
      pc.addTransceiver("audio", { direction: "recvonly" });
      // Talk track: send-only, no track until talk starts (so no RTP and no
      // open talk session on the monitor), then replaceTrack(mic).
      this._talkSender = this._config.controls.includes("talk")
        ? pc.addTransceiver("audio", { direction: "sendonly" }).sender
        : null;
      // Video plays muted; listen audio has its own element, which keeps
      // playing where a browser pauses a hidden video.
      const stream = new MediaStream();
      const audio = new MediaStream();
      this._stream = stream;
      this._audioStream = audio;
      pc.ontrack = (e) => {
        (e.track.kind === "audio" ? audio : stream).addTrack(e.track);
        if (this._els) {
          this._els.video.srcObject = stream;
          this._els.audio.srcObject = audio;
          this._applyListen();
        }
      };
      this._watch(pc);
      pc.onconnectionstatechange = () => {
        if (pc !== this._pc) return;
        if (pc.connectionState === "connected") this._setStatus("");
        if (["failed", "closed"].includes(pc.connectionState)) this._restartVideo("Connection lost, retrying…");
      };
      let sessionId = null;
      const pending = [];
      const sendCandidate = (candidate) =>
        this._hass
          .callWS({ type: "camera/webrtc/candidate", entity_id, session_id: sessionId, candidate })
          .catch((e) => console.debug(CARD, "candidate", e));
      pc.onicecandidate = (e) => {
        if (!e.candidate) return;
        const c = e.candidate.toJSON();
        if (sessionId) sendCandidate(c);
        else pending.push(c);
      };
      const remote = [];
      let answered = false;
      const offer = await pc.createOffer();
      await pc.setLocalDescription(offer);
      if (pc !== this._pc) return; // stopped meanwhile: _stopVideo closed it
      const unsub = await this._hass.connection.subscribeMessage(
        async (msg) => {
          if (pc !== this._pc) return;
          switch (msg.type) {
            case "session":
              sessionId = msg.session_id;
              pending.splice(0).forEach(sendCandidate);
              break;
            case "answer":
              await pc.setRemoteDescription({ type: "answer", sdp: msg.answer });
              answered = true;
              for (const c of remote.splice(0)) await pc.addIceCandidate(c).catch(() => {});
              break;
            case "candidate":
              if (answered) await pc.addIceCandidate(msg.candidate).catch(() => {});
              else remote.push(msg.candidate);
              break;
            case "error":
              this._restartVideo(`Stream error: ${msg.message || msg.code}`);
              break;
          }
        },
        { type: "camera/webrtc/offer", entity_id, offer: offer.sdp },
      );
      // Ends HA's WebRTC session; drop it at once if stopped meanwhile.
      if (pc !== this._pc) unsub().catch?.(() => {});
      else this._unsub = unsub;
      this._update();
    } catch (e) {
      console.warn(CARD, "webrtc", e);
      this._restartVideo(`Cannot start the stream: ${e.message || e.code || e}`);
    } finally {
      this._starting = false;
      // Re-attached while this start was in flight: that connect was
      // refused by `_starting`, so start again for the current generation.
      if (stale() && this.isConnected && !this._pc && !this._retry && !this._mjpeg) this._startVideo();
    }
  }

  // Restart a connection whose video stopped decoding (connection state
  // can stay "connected" while no media flows, e.g. after the source
  // restarted or the app was backgrounded). Paused while the page is hidden.
  _watch(pc) {
    let frames = -1;
    let since = Date.now();
    clearInterval(this._watchTimer);
    this._watchTimer = setInterval(async () => {
      if (document.hidden) {
        since = Date.now();
        return;
      }
      const stats = await pc.getStats().catch(() => null);
      if (pc !== this._pc || !stats) return;
      let n = 0;
      stats.forEach((r) => {
        if (r.type === "inbound-rtp" && r.kind === "video") n = r.framesDecoded ?? 0;
      });
      if (n !== frames) {
        frames = n;
        since = Date.now();
      } else if (Date.now() - since > STALL_MS) {
        console.warn(CARD, `no video frame for ${STALL_MS / 1000} s, reconnecting`);
        this._restartVideo("Stream stalled, reconnecting…");
      }
    }, 3000);
  }

  _startMjpeg(note) {
    if (!this._els) return;
    this._mjpeg = true;
    this._showMjpeg();
    this._setStatus(note);
    setTimeout(() => this._mjpeg && this._setStatus(""), 8000);
    this._update();
    console.info(CARD, note);
  }

  // Swap the <video> for an MJPEG <img> (also after a re-render).
  _showMjpeg() {
    const img = document.createElement("img");
    // HA ends the MJPEG stream on errors / restarts: reload after a pause.
    img.onerror = () => {
      clearTimeout(this._retry);
      this._retry = setTimeout(() => this._reloadMjpeg(), 5000);
    };
    img.muted = true;
    this._els.video.replaceWith(img);
    this._els.video = img;
    this._reloadMjpeg();
  }

  _reloadMjpeg() {
    const st = this._state(this._config.camera);
    const img = this._els?.video;
    if (!st || !(img instanceof HTMLImageElement)) return;
    const q = new URLSearchParams({ token: st.attributes.access_token, t: Date.now() });
    img.src = `/api/camera_proxy_stream/${encodeURIComponent(this._config.camera)}?${q}`;
  }

  _restartVideo(msg) {
    this._stopTalk();
    this._stopVideo();
    this._setStatus(msg);
    if (!this.isConnected) return;
    this._retry = setTimeout(() => {
      this._retry = null;
      this._startVideo();
    }, 5000);
  }

  _stopVideo() {
    this._gen = (this._gen ?? 0) + 1;
    clearTimeout(this._retry);
    clearInterval(this._watchTimer);
    this._retry = null;
    if (this._unsub) {
      this._unsub().catch?.(() => {});
      this._unsub = null;
    }
    if (this._pc) {
      const pc = this._pc;
      this._pc = null;
      pc.close();
    }
    this._talkSender = null;
    this._stream = null;
    this._audioStream = null;
  }

  // ---- talk ----

  async _startTalk() {
    if (this._talking || !this._talkSender) return;
    this._talking = true;
    this._update();
    try {
      const mic = await navigator.mediaDevices.getUserMedia({
        audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      });
      if (!this._talking) return mic.getTracks().forEach((t) => t.stop()); // released while asking
      // WebAudio gain stage = talk volume.
      const ctx = new AudioContext();
      const src = ctx.createMediaStreamSource(mic);
      const gain = ctx.createGain();
      gain.gain.value = this._talkGain;
      const dst = ctx.createMediaStreamDestination();
      src.connect(gain).connect(dst);
      this._mic = { mic, ctx };
      this._gain = gain;
      await this._talkSender.replaceTrack(dst.stream.getAudioTracks()[0]);
    } catch (e) {
      console.warn(CARD, "talk", e);
      this._stopTalk();
      this._setStatus(`Microphone unavailable: ${e.message || e}`);
      setTimeout(() => this._pc?.connectionState === "connected" && this._setStatus(""), 4000);
    }
  }

  _stopTalk() {
    this._talking = false;
    // No track = no RTP: the bridge closes the monitor's talk after 1 s.
    this._talkSender?.replaceTrack(null).catch(() => {});
    if (this._mic) {
      this._mic.mic.getTracks().forEach((t) => t.stop());
      this._mic.ctx.close().catch(() => {});
      this._mic = null;
    }
    this._gain = null;
    this._rearm();
    this._update();
  }
}

// ---- visual editor ----

const SCHEMA = [
  { name: "camera", required: true, selector: { entity: { domain: "camera" } } },
  { name: "title", selector: { text: {} } },
  {
    name: "talk_mode",
    selector: { select: { mode: "dropdown", options: [{ value: "toggle", label: "Tap to start / stop" }, { value: "hold", label: "Hold to talk" }] } },
  },
  {
    type: "expandable",
    name: "defaults",
    flatten: true,
    title: "Defaults (when the card loads)",
    schema: [
      { name: "listen", selector: { boolean: {} } },
      { name: "listen_volume", selector: { number: { min: 0, max: 100, step: 5, mode: "slider", unit_of_measurement: "%" } } },
      { name: "talk_volume", selector: { number: { min: 0, max: 200, step: 5, mode: "slider", unit_of_measurement: "%" } } },
      { name: "autohide", selector: { number: { min: 0, max: 60, step: 1, mode: "box", unit_of_measurement: "s" } } },
      {
        name: "default_track",
        selector: { select: { mode: "dropdown", options: [{ value: "", label: "The monitor's current track" }, ...TRACKS.map((t) => ({ value: t, label: t }))] } },
      },
      {
        name: "default_timer",
        selector: { select: { mode: "dropdown", options: [{ value: "", label: "The monitor's current timer" }, ...TIMERS.map((t) => ({ value: t, label: t }))] } },
      },
    ],
  },
  {
    type: "expandable",
    name: "entities",
    flatten: true,
    title: "Entities (found on the camera's device when empty)",
    schema: [
      { name: "night_light", selector: { entity: { domain: "light" } } },
      { name: "lullaby", selector: { entity: { domain: "select" } } },
      { name: "lullaby_track", selector: { entity: { domain: "select" } } },
      { name: "lullaby_playing", selector: { entity: { domain: "binary_sensor" } } },
      { name: "lullaby_timer", selector: { entity: { domain: "select" } } },
      { name: "volume", selector: { entity: { domain: "number" } } },
    ],
  },
  {
    type: "expandable",
    name: "overlay_corners",
    flatten: true,
    expanded: true,
    title: "Overlays (entity state shown in a corner)",
    schema: Object.keys(CORNERS).map((name) => ({ name, selector: { entity: {} } })),
  },
  {
    type: "expandable",
    name: "advanced",
    flatten: true,
    title: "More overlays (YAML)",
    schema: [{ name: "overlays", selector: { object: {} } }],
  },
];
const EDITOR_LABELS = {
  camera: "Camera (ONVIF)",
  title: "Title",
  talk_mode: "Talk toggle",
  listen: "Listen on",
  listen_volume: "Listen volume",
  talk_volume: "Talk volume",
  autohide: "Hide controls after (0 = always shown)",
  night_light: "Night light",
  default_track: "Lullaby to play (empty = the monitor's current one)",
  default_timer: "Lullaby timer (empty = the monitor's current one)",
  lullaby_timer: "Lullaby timer",
  overlay_top_right: "Top right",
  overlay_bottom_left: "Bottom left",
  overlay_bottom_right: "Bottom right",
  lullaby: "Lullaby",
  lullaby_track: "Lullaby track",
  lullaby_playing: "Lullaby playing",
  volume: "Lullaby volume",
  overlays: "Overlays (list of {entity, position, name, attribute, text, style})",
};
const EDITOR_STYLE = `
  .tlac-controls { margin: 16px 0 8px; }
  .tlac-controls h3 { font-size: 14px; font-weight: 500; margin: 0 0 4px; }
  .tlac-controls .hint { font-size: 12px; color: var(--secondary-text-color); margin-bottom: 4px; }
  .tlac-row { display: flex; align-items: center; gap: 8px; padding: 2px 0; }
  .tlac-row.off { opacity: .55; }
  .tlac-row label { flex: 1 1 auto; display: flex; align-items: center; gap: 8px; cursor: pointer; }
  .tlac-row button { width: 32px; height: 32px; border: 0; border-radius: 50%; background: none; cursor: pointer;
    color: var(--primary-text-color); display: inline-flex; align-items: center; justify-content: center; }
  .tlac-row button:hover:not([disabled]) { background: var(--secondary-background-color, rgba(127, 127, 127, .15)); }
  .tlac-row button[disabled] { opacity: .3; cursor: default; }
`;

class TuyaLocalAventCardEditor extends HTMLElement {
  setConfig(config) {
    this._config = config;
    this._render();
  }

  set hass(hass) {
    this._hass = hass;
    this._render();
  }

  _controls() {
    return (this._config.controls ?? ALL_CONTROLS).filter((c) => ALL_CONTROLS.includes(c));
  }

  _emit(config) {
    for (const k of Object.keys(config)) {
      if (config[k] === "" || config[k] === undefined || (k in DEFAULTS && config[k] === DEFAULTS[k])) delete config[k];
    }
    this._config = config;
    this.dispatchEvent(new CustomEvent("config-changed", { detail: { config }, bubbles: true, composed: true }));
  }

  _render() {
    if (!this._hass || !this._config) return;
    if (!this._form) {
      const style = document.createElement("style");
      style.textContent = EDITOR_STYLE;
      this._form = document.createElement("ha-form");
      this._form.computeLabel = (s) => EDITOR_LABELS[s.name] ?? s.name;
      this._form.computeHelper = (s) =>
        s.name === "overlays"
          ? `position: ${Object.keys(POSITIONS).join(" | ")}, or top/left/right/bottom (number = %)`
          : undefined;
      this._form.addEventListener("value-changed", (e) => this._emit({ ...e.detail.value, controls: this._controls() }));
      this._list = document.createElement("div");
      this._list.className = "tlac-controls";
      this._list.addEventListener("click", (e) => this._onList(e));
      this._list.addEventListener("change", (e) => this._onList(e));
      this.append(style, this._form, this._list);
    }
    this._form.hass = this._hass;
    this._form.schema = SCHEMA;
    this._form.data = { ...DEFAULTS, ...this._config };
    this._renderList();
  }

  // Controls: checkbox = shown, arrows = order in the button group.
  _renderList() {
    const on = this._controls();
    const rows = [...on, ...ALL_CONTROLS.filter((k) => !on.includes(k))];
    this._list.innerHTML = `<h3>Controls</h3><div class="hint">Shown controls, in button order</div>${rows
      .map((k) => {
        const i = on.indexOf(k);
        return `<div class="tlac-row ${i < 0 ? "off" : ""}">
          <label><input type="checkbox" data-key="${k}" ${i < 0 ? "" : "checked"}>
            <ha-icon icon="${ICONS[k][PANEL_CONTROLS.includes(k) ? 0 : 1]}"></ha-icon>${esc(CONTROL_LABELS[k])}</label>
          <button data-move="${k}" data-dir="-1" title="Move up" ${i > 0 ? "" : "disabled"}><ha-icon icon="mdi:arrow-up"></ha-icon></button>
          <button data-move="${k}" data-dir="1" title="Move down" ${i >= 0 && i < on.length - 1 ? "" : "disabled"}><ha-icon icon="mdi:arrow-down"></ha-icon></button>
        </div>`;
      })
      .join("")}`;
  }

  _onList(e) {
    const on = this._controls();
    const cb = e.type === "change" && e.target.closest("input[data-key]");
    const mv = e.type === "click" && e.target.closest("button[data-move]");
    if (cb) {
      const k = cb.dataset.key;
      // Newly checked controls go to the end of the order.
      const next = cb.checked ? [...on.filter((x) => x !== k), k] : on.filter((x) => x !== k);
      this._emit({ ...this._config, controls: next });
    } else if (mv) {
      const i = on.indexOf(mv.dataset.move);
      const j = i + Number(mv.dataset.dir);
      if (i < 0 || j < 0 || j >= on.length) return;
      [on[i], on[j]] = [on[j], on[i]];
      this._emit({ ...this._config, controls: on });
    } else return;
    this._renderList();
  }
}

customElements.get(CARD) || customElements.define(CARD, TuyaLocalAventCard);
customElements.get(`${CARD}-editor`) || customElements.define(`${CARD}-editor`, TuyaLocalAventCardEditor);
window.customCards = window.customCards || [];
if (!window.customCards.some((c) => c.type === CARD)) {
  window.customCards.push({
    type: CARD,
    name: "Tuya Local Avent",
    description: "Baby monitor: live video, talk, night light, lullaby and entity overlays.",
    preview: false,
  });
}
console.info(`%c${CARD} ${VERSION}`, "color:#03a9f4");
