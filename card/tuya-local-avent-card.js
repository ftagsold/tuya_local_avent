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
 * - Controls: a button group floating over the bottom of the video. A tap
 *   on a button turns the group into that control (on/off toggle + slider);
 *   a tap anywhere outside the group returns to the buttons. The night light,
 *   lullaby and volume are the bridge's MQTT entities, found on the camera's
 *   device unless set explicitly.
 * - Overlays: other entities' states as plain HTML text over the video.
 *
 * No build step, no dependencies: copy to /config/www and add
 * /local/tuya-local-avent-card.js as a dashboard resource (JavaScript module).
 */

const CARD = "tuya-local-avent-card";
const VERSION = "1.1.0";

const ALL_CONTROLS = ["listen", "night_light", "lullaby", "talk"];
// Names from 1.0 whose sliders now live inside their control.
const LEGACY_CONTROLS = ["lullaby_volume", "talk_volume"];
const CONTROL_LABELS = {
  listen: "Listen (mute + volume)",
  night_light: "Night light (on/off + 33 / 66 / 100 %)",
  lullaby: "Lullaby (play/stop + volume)",
  talk: "Talk (on/off + talk volume)",
};
const LIGHT_LEVELS = [33, 66, 100];
const ICONS = {
  listen: ["mdi:volume-high", "mdi:volume-off"],
  night_light: ["mdi:lightbulb-night", "mdi:lightbulb-night-outline"],
  lullaby: ["mdi:music", "mdi:music-off"],
  talk: ["mdi:microphone", "mdi:microphone-off"],
};
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
  volume: { domain: "number", suffix: "volume" },
};

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

const STYLE = `
  :host { display: block; }
  ha-card { overflow: hidden; }
  .video { position: relative; background: #000; aspect-ratio: 16 / 9; }
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
    max-width: calc(100% - 24px); }
  .group[hidden], .group [hidden] { display: none !important; }
  .group.open { width: min(440px, calc(100% - 24px)); }
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
  .panel input[type=range] { flex: 1 1 auto; min-width: 0; accent-color: var(--primary-color, #03a9f4); height: 28px; }
  .panel .val { flex: 0 0 auto; min-width: 3.2em; text-align: right; font-size: 13px;
    font-variant-numeric: tabular-nums; opacity: .9; }
  .panel .note { font-size: 12px; opacity: .85; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
`;

class TuyaLocalAventCard extends HTMLElement {
  constructor() {
    super();
    this._onOutside = (e) => {
      if (!this._open || !this._els) return;
      if (!e.composedPath().includes(this._els.group)) this._setOpen(null);
    };
    this._onKey = (e) => e.key === "Escape" && this._setOpen(null);
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
    const overlays = config.overlays ?? [];
    if (!Array.isArray(overlays)) throw new Error("overlays must be a list");
    const controls = (config.controls ?? ALL_CONTROLS).filter((c) => !LEGACY_CONTROLS.includes(c));
    const bad = controls.filter((c) => !ALL_CONTROLS.includes(c));
    if (bad.length) throw new Error(`unknown controls: ${bad.join(", ")} (use ${ALL_CONTROLS.join(", ")})`);
    const restart = this._config && this._config.camera !== config.camera;
    this._config = { talk_mode: "toggle", ...config, controls, overlays };
    this._talkGain = this._loadNum("talk_gain", (this._config.talk_volume ?? 100) / 100);
    this._listenVol = this._loadNum("listen_volume", 1);
    this._built = false;
    this._open = null;
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
    if (this._hass && this._config && !this._pc && !this._mjpeg) this._startVideo();
  }

  disconnectedCallback() {
    document.removeEventListener("pointerdown", this._onOutside, true);
    document.removeEventListener("keydown", this._onKey);
    this._stopTalk();
    this._stopVideo();
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
    const dev = reg[this._config.camera]?.device_id;
    if (!dev) return undefined;
    const { domain, suffix } = AUTO[key];
    return Object.values(reg).find(
      (e) => e.device_id === dev && e.entity_id.startsWith(`${domain}.`) && e.entity_id.endsWith(suffix),
    )?.entity_id;
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
        const v = this._els?.video;
        return {
          on: !!v && !v.muted,
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
        const st = this._state(this._entity("lullaby"));
        const vol = this._state(this._entity("volume"));
        const n = Number(vol?.state);
        return {
          on: st?.state === "play",
          disabled: unavailable(st),
          slider: Number.isFinite(n)
            ? { min: Number(vol.attributes.min ?? 1), max: Number(vol.attributes.max ?? 100), step: Number(vol.attributes.step ?? 1), value: n, text: `${Math.round(n)}%` }
            : { disabled: true, min: 0, max: 100, step: 1, value: 0, text: "—" },
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
      case "listen": {
        const v = this._els.video;
        v.muted = !v.muted;
        v.volume = this._listenVol;
        v.play().catch(() => {});
        break;
      }
      case "night_light": {
        const entity_id = this._entity("night_light");
        if (m.on) this._call("light", "turn_off", { entity_id });
        else this._call("light", "turn_on", { entity_id, brightness_pct: this._lightLevel() });
        break;
      }
      case "lullaby":
        this._call("select", "select_option", { entity_id: this._entity("lullaby"), option: m.on ? "stop" : "play" });
        break;
      case "talk":
        if (this._config.talk_mode !== "hold") this._talking ? this._stopTalk() : this._startTalk();
        break;
    }
    this._update();
  }

  // Live while dragging (`input`) only for local values; entity writes on `change`.
  _slide(key, value, final) {
    switch (key) {
      case "listen":
        this._listenVol = value / 100;
        this._els.video.volume = this._listenVol;
        if (final) this._saveNum("listen_volume", this._listenVol);
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
        if (final) this._saveNum("talk_gain", this._talkGain);
        break;
    }
  }

  // ---- DOM ----

  _render() {
    if (!this.shadowRoot) this.attachShadow({ mode: "open" });
    const c = this._config;
    const icon = (key) => `<ha-icon icon="${ICONS[key][0]}"></ha-icon>`;
    const buttons = c.controls
      .map((k) => `<button class="btn" data-open="${k}" title="${esc(CONTROL_LABELS[k])}">${icon(k)}</button>`)
      .join("");
    const panels = c.controls
      .map(
        (k) => `<div class="panel" data-panel="${k}" hidden>
          <button class="btn" data-toggle="${k}">${icon(k)}</button>
          <input type="range" data-slider="${k}" aria-label="${esc(CONTROL_LABELS[k])}">
          <span class="val"></span>
        </div>`,
      )
      .join("");
    this.shadowRoot.innerHTML = `<style>${STYLE}</style><ha-card>
      ${c.title ? `<h1 class="card-header">${esc(c.title)}</h1>` : ""}
      <div class="video" id="video">
        <video id="v" autoplay playsinline muted></video>
        <div class="status" id="status">Connecting…</div>
        <div id="overlays"></div>
        <div class="group" id="group" ${c.controls.length ? "" : "hidden"}>
          <div class="main" id="main" style="display:flex;gap:6px">${buttons}</div>
          ${panels}
        </div>
      </div></ha-card>`;
    const $ = (id) => this.shadowRoot.getElementById(id);
    this._els = { video: $("v"), status: $("status"), overlays: $("overlays"), group: $("group"), main: $("main") };
    this._els.video.volume = this._listenVol;
    if (this._stream) this._els.video.srcObject = this._stream;

    const group = this._els.group;
    group.addEventListener("click", (e) => {
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
    for (const s of group.querySelectorAll("[data-slider]")) {
      const key = s.dataset.slider;
      s.addEventListener("input", () => {
        this._slide(key, Number(s.value), false);
        this._sliderText(key, Number(s.value));
      });
      s.addEventListener("change", () => this._slide(key, Number(s.value), true));
    }
    this._built = true;
    this._setOpen(null);
  }

  _setOpen(key) {
    this._open = key;
    if (!this._els) return;
    this._els.main.hidden = !!key;
    this._els.group.classList.toggle("open", !!key);
    for (const p of this._els.group.querySelectorAll("[data-panel]")) p.hidden = p.dataset.panel !== key;
    this._update();
  }

  _sliderText(key, value) {
    const val = this._els.group.querySelector(`[data-panel="${key}"] .val`);
    if (!val) return;
    val.textContent = key === "night_light" ? `${LIGHT_LEVELS[value - 1]}%` : `${value}%`;
  }

  _update() {
    if (!this._els) return;
    const root = this.shadowRoot;
    for (const key of this._config.controls) {
      const m = this._model(key);
      const [onIcon, offIcon] = ICONS[key];
      const btn = root.querySelector(`[data-open="${key}"]`);
      btn.classList.toggle("on", m.on && key !== "talk");
      btn.classList.toggle("talking", key === "talk" && m.on);
      btn.querySelector("ha-icon").setAttribute("icon", m.on || key === "talk" ? onIcon : offIcon);
      const tog = root.querySelector(`[data-toggle="${key}"]`);
      tog.classList.toggle("on", m.on && key !== "talk");
      tog.classList.toggle("talking", key === "talk" && m.on);
      tog.disabled = m.disabled;
      tog.title = m.note || (m.on ? "Turn off" : "Turn on");
      tog.querySelector("ha-icon").setAttribute("icon", m.on || key === "talk" ? onIcon : offIcon);
      const s = root.querySelector(`[data-slider="${key}"]`);
      if (root.activeElement !== s) {
        Object.assign(s, { min: m.slider.min, max: m.slider.max, step: m.slider.step });
        s.value = m.slider.value;
        root.querySelector(`[data-panel="${key}"] .val`).textContent = m.note ? "" : m.slider.text;
      }
      s.disabled = !!m.slider.disabled || (key !== "talk" && key !== "listen" && m.disabled);
      s.hidden = !!m.note;
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
          .map((k) => `${k}:${typeof ov[k] === "number" ? `${ov[k]}%` : ov[k]};`)
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
    try {
      const caps = await this._hass.callWS({ type: "camera/capabilities", entity_id }).catch(() => null);
      if (caps && !caps.frontend_stream_types?.includes("web_rtc")) {
        this._startMjpeg("Live audio and talk need WebRTC (HA's go2rtc); showing MJPEG.");
        return;
      }
      const client = await this._hass.callWS({ type: "camera/webrtc/get_client_config", entity_id });
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
      const stream = new MediaStream();
      this._stream = stream;
      pc.ontrack = (e) => {
        stream.addTrack(e.track);
        if (this._els) {
          this._els.video.srcObject = stream;
          this._els.video.play().catch(() => {});
        }
      };
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
      this._unsub = await this._hass.connection.subscribeMessage(
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
      this._update();
    } catch (e) {
      console.warn(CARD, "webrtc", e);
      this._restartVideo(`Cannot start the stream: ${e.message || e.code || e}`);
    } finally {
      this._starting = false;
    }
  }

  _startMjpeg(note) {
    const st = this._state(this._config.camera);
    if (!st || !this._els) return;
    this._mjpeg = true;
    const img = document.createElement("img");
    img.src = `/api/camera_proxy_stream/${this._config.camera}?token=${st.attributes.access_token}`;
    this._els.video.replaceWith(img);
    this._els.video = img;
    this._els.video.muted = true;
    this._setStatus("");
    this._update();
    console.info(CARD, note);
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
    clearTimeout(this._retry);
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
    this._update();
  }

  // Per-browser, per-camera settings (talk gain, listen volume).
  _loadNum(name, fallback) {
    try {
      const v = Number(localStorage.getItem(`${CARD}:${name}:${this._config.camera}`));
      if (localStorage.getItem(`${CARD}:${name}:${this._config.camera}`) !== null && Number.isFinite(v)) return v;
    } catch (_) {}
    return fallback;
  }

  _saveNum(name, value) {
    try {
      localStorage.setItem(`${CARD}:${name}:${this._config.camera}`, String(value));
    } catch (_) {}
  }
}

// ---- visual editor ----

const SCHEMA = [
  { name: "camera", required: true, selector: { entity: { domain: "camera" } } },
  { name: "title", selector: { text: {} } },
  {
    name: "controls",
    selector: {
      select: { multiple: true, mode: "list", options: ALL_CONTROLS.map((value) => ({ value, label: CONTROL_LABELS[value] })) },
    },
  },
  {
    name: "talk_mode",
    selector: { select: { mode: "dropdown", options: [{ value: "toggle", label: "Tap to start / stop" }, { value: "hold", label: "Hold to talk" }] } },
  },
  {
    type: "expandable",
    name: "entities",
    flatten: true,
    title: "Entities (found on the camera's device when empty)",
    schema: [
      { name: "night_light", selector: { entity: { domain: "light" } } },
      { name: "lullaby", selector: { entity: { domain: "select" } } },
      { name: "volume", selector: { entity: { domain: "number" } } },
    ],
  },
  { name: "overlays", selector: { object: {} } },
];
const EDITOR_LABELS = {
  camera: "Camera (ONVIF)",
  title: "Title",
  controls: "Controls to show",
  talk_mode: "Talk toggle",
  night_light: "Night light",
  lullaby: "Lullaby",
  volume: "Lullaby volume",
  overlays: "Overlays (list of {entity, position, name, attribute, text, style})",
};

class TuyaLocalAventCardEditor extends HTMLElement {
  setConfig(config) {
    this._config = config;
    this._render();
  }

  set hass(hass) {
    this._hass = hass;
    this._render();
  }

  _render() {
    if (!this._hass || !this._config) return;
    if (!this._form) {
      this._form = document.createElement("ha-form");
      this._form.computeLabel = (s) => EDITOR_LABELS[s.name] ?? s.name;
      this._form.computeHelper = (s) =>
        s.name === "overlays"
          ? `position: ${Object.keys(POSITIONS).join(" | ")}, or top/left/right/bottom (number = %)`
          : undefined;
      this._form.addEventListener("value-changed", (e) => {
        const config = { ...e.detail.value };
        for (const k of Object.keys(config)) if (config[k] === "" || config[k] === undefined) delete config[k];
        this._config = config;
        this.dispatchEvent(new CustomEvent("config-changed", { detail: { config }, bubbles: true, composed: true }));
      });
      this.appendChild(this._form);
    }
    this._form.hass = this._hass;
    this._form.schema = SCHEMA;
    const controls = (this._config.controls ?? ALL_CONTROLS).filter((c) => !LEGACY_CONTROLS.includes(c));
    this._form.data = { talk_mode: "toggle", ...this._config, controls };
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
