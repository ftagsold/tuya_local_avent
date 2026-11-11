// Fake Home Assistant frontend for tuya-local-avent-card.
//
// - hass.states / hass.entities: mock entities on one device, editable in the
//   side panel; service calls change them like the MQTT entities would.
// - camera/webrtc/*: bridged to go2rtc's WebSocket API (run.sh), which plays
//   the part of Home Assistant's built-in go2rtc. Video, listen audio and
//   talk are real.
// - ha-card / ha-icon / ha-form: minimal stand-ins for HA's elements.

const GO2RTC_WS = "ws://127.0.0.1:11984/api/ws";
const MDI_CSS = "https://cdn.jsdelivr.net/npm/@mdi/font@7.4.47/css/materialdesignicons.min.css";
const $ = (id) => document.getElementById(id);

// ---------- log ----------
function log(...parts) {
  const line = `${new Date().toLocaleTimeString()} ${parts.map((p) => (typeof p === "string" ? p : JSON.stringify(p))).join(" ")}`;
  const el = $("log");
  el.textContent = `${line}\n${el.textContent}`.slice(0, 20000);
}

// ---------- HA element stand-ins ----------
customElements.define(
  "ha-card",
  class extends HTMLElement {
    constructor() {
      super();
      this.attachShadow({ mode: "open" }).innerHTML = `<style>
        :host { display: block; background: var(--card-background-color); color: var(--primary-text-color);
          border-radius: 12px; border: 1px solid var(--divider-color); overflow: hidden; }
        ::slotted(.card-header) { font-size: 24px; font-weight: 400; margin: 0; padding: 12px 16px 8px; }
      </style><slot></slot>`;
    }
  },
);

customElements.define(
  "ha-icon",
  class extends HTMLElement {
    static get observedAttributes() {
      return ["icon"];
    }
    constructor() {
      super();
      this.attachShadow({ mode: "open" });
    }
    connectedCallback() {
      this.render();
    }
    attributeChangedCallback() {
      this.render();
    }
    render() {
      const name = (this.getAttribute("icon") || "").replace(/^mdi:/, "");
      this.shadowRoot.innerHTML = `<link rel="stylesheet" href="${MDI_CSS}">
        <style>:host{display:inline-flex;width:24px;height:24px;align-items:center;justify-content:center}
        i{font-size:24px;line-height:1}</style><i class="mdi mdi-${name}"></i>`;
    }
  },
);

// ha-form: renders the schema kinds the card's editor uses.
customElements.define(
  "ha-form",
  class extends HTMLElement {
    set hass(h) { this._hass = h; this.queue(); }
    set schema(s) { this._schema = s; this.queue(); }
    set data(d) { this._data = d; this.queue(); }
    queue() {
      if (this._q) return;
      this._q = true;
      queueMicrotask(() => {
        this._q = false;
        if (this.contains(document.activeElement)) return; // do not clobber typing
        this.render();
      });
    }
    label(s) { return (this.computeLabel && this.computeLabel(s)) || s.name; }
    emit(name, value) {
      const data = { ...this._data, [name]: value };
      this._data = data;
      this.dispatchEvent(new CustomEvent("value-changed", { detail: { value: data } }));
    }
    field(s) {
      const wrap = document.createElement("div");
      wrap.style.margin = "8px 0";
      if (s.type === "expandable") {
        const det = document.createElement("details");
        det.innerHTML = `<summary>${s.title || s.name}</summary>`;
        s.schema.forEach((c) => det.appendChild(this.field(c)));
        wrap.appendChild(det);
        return wrap;
      }
      const sel = s.selector || {};
      const v = this._data?.[s.name];
      const title = document.createElement("div");
      title.textContent = this.label(s) + (s.required ? " *" : "");
      title.style.cssText = "font-size:12px;color:var(--secondary-text-color)";
      wrap.appendChild(title);
      let input;
      if (sel.entity) {
        input = document.createElement("select");
        const ids = Object.keys(this._hass?.states || {}).filter((e) => !sel.entity.domain || e.startsWith(`${sel.entity.domain}.`));
        input.innerHTML = `<option value="">(auto)</option>` + ids.map((e) => `<option ${e === v ? "selected" : ""}>${e}</option>`).join("");
        input.onchange = () => this.emit(s.name, input.value || undefined);
      } else if (sel.select && sel.select.multiple) {
        input = document.createElement("div");
        for (const o of sel.select.options) {
          const l = document.createElement("label");
          l.style.cssText = "display:flex;gap:6px;align-items:center";
          const cb = document.createElement("input");
          cb.type = "checkbox";
          cb.checked = (v || []).includes(o.value);
          cb.onchange = () => {
            const cur = new Set(this._data?.[s.name] || []);
            cb.checked ? cur.add(o.value) : cur.delete(o.value);
            // keep the option order
            this.emit(s.name, sel.select.options.map((x) => x.value).filter((x) => cur.has(x)));
          };
          l.append(cb, o.label);
          input.appendChild(l);
        }
      } else if (sel.select) {
        input = document.createElement("select");
        input.innerHTML = sel.select.options.map((o) => `<option value="${o.value}" ${o.value === v ? "selected" : ""}>${o.label}</option>`).join("");
        input.onchange = () => this.emit(s.name, input.value);
      } else if (sel.object) {
        input = document.createElement("textarea");
        input.rows = 6;
        input.value = v === undefined ? "" : window.jsyaml ? jsyaml.dump(v) : JSON.stringify(v, null, 2);
        input.onchange = () => {
          try {
            this.emit(s.name, input.value.trim() ? parse(input.value) : undefined);
          } catch (e) {
            log("editor: bad YAML", e.message);
          }
        };
      } else {
        input = document.createElement("input");
        input.value = v ?? "";
        input.style.width = "100%";
        input.onchange = () => this.emit(s.name, input.value);
      }
      wrap.appendChild(input);
      const help = this.computeHelper && this.computeHelper(s);
      if (help) {
        const h = document.createElement("div");
        h.textContent = help;
        h.style.cssText = "font-size:11px;color:var(--secondary-text-color)";
        wrap.appendChild(h);
      }
      return wrap;
    }
    render() {
      if (!this._schema) return;
      this.replaceChildren(...this._schema.map((s) => this.field(s)));
    }
  },
);

// ---------- mock entities ----------
const DEVICE = "harness_device";
const now = () => new Date().toISOString();
const st = (entity_id, state, attributes = {}) => ({ entity_id, state, attributes, last_changed: now(), last_updated: now() });
const states = {
  "camera.nursery": st("camera.nursery", "streaming", { friendly_name: "Nursery", access_token: "harness", frontend_stream_type: "web_rtc" }),
  "light.nursery_night_light": st("light.nursery_night_light", "off", { friendly_name: "Nursery Night Light", brightness: null, supported_color_modes: ["brightness"] }),
  "select.nursery_lullaby": st("select.nursery_lullaby", "stop", { friendly_name: "Nursery Lullaby", options: ["play", "pause", "stop", "next", "prev"] }),
  "number.nursery_volume": st("number.nursery_volume", "40", { friendly_name: "Nursery Volume", min: 1, max: 100, step: 1, unit_of_measurement: "%" }),
  "sensor.nursery_temperature": st("sensor.nursery_temperature", "21.4", { friendly_name: "Nursery Temperature", unit_of_measurement: "°C", device_class: "temperature" }),
  "binary_sensor.nursery_motion": st("binary_sensor.nursery_motion", "off", { friendly_name: "Nursery Motion", device_class: "motion" }),
  "binary_sensor.nursery_sound": st("binary_sensor.nursery_sound", "off", { friendly_name: "Nursery Sound", device_class: "sound" }),
  "sensor.nursery_humidity": st("sensor.nursery_humidity", "48", { friendly_name: "Nursery Humidity", unit_of_measurement: "%" }),
};
const entities = Object.fromEntries(
  Object.keys(states).map((id) => [id, { entity_id: id, device_id: id.startsWith("sensor.nursery") ? "other" : DEVICE, platform: "mqtt" }]),
);

function setState(id, state, attrs) {
  const old = states[id];
  states[id] = { ...old, state: String(state), attributes: { ...old.attributes, ...attrs }, last_changed: now(), last_updated: now() };
}

// ---------- fake hass ----------
const sessions = new Map(); // session_id -> { ws, queue }

function formatEntityState(s) {
  if (!s) return "";
  if (s.state === "on" || s.state === "off") return s.state === "on" ? "On" : "Off";
  const u = s.attributes.unit_of_measurement;
  return u ? `${s.state} ${u}` : s.state;
}

async function callService(domain, service, data) {
  log("service", `${domain}.${service}`, data);
  const id = data.entity_id;
  if (!states[id]) throw new Error(`unknown entity ${id}`);
  if (domain === "light" && service === "turn_on") {
    const pct = data.brightness_pct ?? Math.round(((states[id].attributes.brightness ?? 255) / 255) * 100);
    setState(id, "on", { brightness: Math.round(pct * 2.55) });
  } else if (domain === "light" && service === "turn_off") {
    setState(id, "off", { brightness: null });
  } else if (domain === "select" && service === "select_option") {
    setState(id, data.option);
  } else if (domain === "number" && service === "set_value") {
    setState(id, data.value);
  } else {
    log("  (not simulated)");
    return;
  }
  // The real path (MQTT -> monitor -> state push) takes a moment.
  setTimeout(push, 150);
}

async function callWS(msg) {
  if (msg.type !== "camera/webrtc/candidate") log("ws", msg.type);
  switch (msg.type) {
    case "camera/capabilities":
      return { frontend_stream_types: $("nowebrtc").checked ? ["hls"] : ["hls", "web_rtc"] };
    case "camera/webrtc/get_client_config":
      return { configuration: { iceServers: [] } };
    case "camera/webrtc/candidate": {
      const s = sessions.get(msg.session_id);
      const out = JSON.stringify({ type: "webrtc/candidate", value: msg.candidate.candidate });
      if (s?.ws.readyState === WebSocket.OPEN) s.ws.send(out);
      else s?.queue.push(out);
      return null;
    }
    default:
      throw { code: "unknown_command", message: msg.type };
  }
}

// camera/webrtc/offer: the same event sequence HA sends (session, answer,
// candidates), produced from go2rtc's WebSocket API.
async function subscribeMessage(cb, msg) {
  if (msg.type !== "camera/webrtc/offer") throw { code: "unknown_command", message: msg.type };
  log("ws camera/webrtc/offer", msg.entity_id, `(${msg.offer.split("\nm=").length - 1} m-lines)`);
  const session_id = crypto.randomUUID();
  const ws = new WebSocket(`${GO2RTC_WS}?src=${encodeURIComponent(msg.entity_id)}`);
  const s = { ws, queue: [] };
  sessions.set(session_id, s);
  ws.onopen = () => {
    ws.send(JSON.stringify({ type: "webrtc/offer", value: msg.offer }));
    s.queue.splice(0).forEach((m) => ws.send(m));
  };
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    if (m.type === "webrtc/answer") {
      log("go2rtc answer:", m.value.split("\n").filter((l) => /^m=|^a=(sendonly|recvonly|sendrecv|inactive)/.test(l)).map((l) => l.trim().replace(/ UDP.*/, "")).join(" "));
      cb({ type: "answer", answer: m.value });
    } else if (m.type === "webrtc/candidate") {
      cb({ type: "candidate", candidate: { candidate: m.value, sdpMid: "0", sdpMLineIndex: 0 } });
    } else if (m.type === "error") {
      log("go2rtc error:", m.value);
      cb({ type: "error", code: "go2rtc", message: m.value });
    }
  };
  ws.onerror = () => {
    log("go2rtc websocket failed: is run.sh running?");
    cb({ type: "error", code: "go2rtc", message: "go2rtc not reachable on 127.0.0.1:11984" });
  };
  ws.onclose = () => sessions.delete(session_id);
  queueMicrotask(() => cb({ type: "session", session_id }));
  return async () => {
    log("ws unsubscribe", session_id.slice(0, 8));
    ws.close();
  };
}

function makeHass() {
  return {
    states: { ...states },
    entities,
    devices: { [DEVICE]: { id: DEVICE, name: "Nursery" } },
    callService,
    callWS,
    connection: { subscribeMessage },
    formatEntityState,
    language: "en",
    locale: { language: "en" },
    themes: { darkMode: $("dark").checked },
    user: { name: "harness", is_admin: true },
  };
}

let hass;
let card;
let editor;
function push() {
  hass = makeHass();
  if (card) card.hass = hass;
  if (editor) editor.hass = hass;
  renderEntities();
}

// ---------- config ----------
const DEFAULT_CONFIG = `type: custom:tuya-local-avent-card
camera: camera.nursery
title: Nursery (harness)
controls: [listen, night_light, lullaby, talk]
talk_mode: toggle
overlays:
  - entity: sensor.nursery_temperature
    position: top-right
  - entity: binary_sensor.nursery_motion
    name: Motion
    position: bottom-left
  - entity: sensor.nursery_humidity
    top: 12
    left: 3
    style: "font-size: 18px; color: #ffd54f"
`;

function parse(text) {
  return window.jsyaml ? jsyaml.load(text) : JSON.parse(text);
}

function dump(obj) {
  return window.jsyaml ? jsyaml.dump(obj, { flowLevel: 2 }) : JSON.stringify(obj, null, 2);
}

function saved(key, fallback) {
  try {
    return localStorage.getItem(`harness2:${key}`) ?? fallback;
  } catch (_) {
    return fallback;
  }
}

function save(key, value) {
  try {
    localStorage.setItem(`harness2:${key}`, value);
  } catch (_) {}
}

function applyConfig(text) {
  $("config-err").textContent = "";
  let config;
  try {
    config = parse(text);
  } catch (e) {
    $("config-err").textContent = `YAML: ${e.message}`;
    return;
  }
  const Card = customElements.get("tuya-local-avent-card");
  const el = document.createElement("tuya-local-avent-card");
  try {
    el.setConfig(config);
  } catch (e) {
    $("config-err").textContent = `setConfig: ${e.message}`;
    return;
  }
  card?.remove();
  card = el;
  card.hass = hass;
  $("stage").appendChild(card);
  save("config", text);
  if (editor) editor.setConfig(config);
  log("card", `config applied (getCardSize ${card.getCardSize?.()})`, Card ? "" : "(card not loaded)");
}

// ---------- entity panel ----------
function renderEntities() {
  const host = $("entities");
  if (host.contains(document.activeElement)) return;
  host.replaceChildren(
    ...Object.values(states)
      .filter((s) => !s.entity_id.startsWith("camera."))
      .map((s) => {
        const row = document.createElement("div");
        row.className = "ent";
        const name = document.createElement("code");
        name.textContent = s.entity_id;
        name.title = s.entity_id;
        const input = document.createElement("input");
        input.value = s.state;
        input.onchange = () => {
          setState(s.entity_id, input.value);
          log("mock", s.entity_id, "=", input.value);
          push();
        };
        const extra = document.createElement("input");
        if (s.entity_id.startsWith("light.")) {
          extra.placeholder = "bri 0-255";
          extra.value = s.attributes.brightness ?? "";
          extra.onchange = () => {
            setState(s.entity_id, s.state, { brightness: extra.value === "" ? null : Number(extra.value) });
            push();
          };
        } else extra.style.visibility = "hidden";
        row.append(name, input, extra);
        return row;
      }),
  );
}

// ---------- boot ----------
async function boot() {
  try {
    document.documentElement.dataset.theme = saved("dark", "0") === "1" ? "dark" : "light";
    $("dark").checked = saved("dark", "0") === "1";
  } catch (_) {}
  $("dark").onchange = () => {
    document.documentElement.dataset.theme = $("dark").checked ? "dark" : "light";
    save("dark", $("dark").checked ? "1" : "0");
    push();
  };
  $("width").value = saved("width", "500px");
  $("stage").style.maxWidth = $("width").value;
  $("width").onchange = () => {
    $("stage").style.maxWidth = $("width").value;
    save("width", $("width").value);
  };
  $("nowebrtc").onchange = () => applyConfig($("config").value);

  $("status").textContent = `secure context: ${window.isSecureContext ? "yes (mic allowed)" : "NO: open via http://localhost"}`;
  hass = makeHass();
  await import(`/tuya-local-avent-card.js?t=${Date.now()}`);
  $("config").value = saved("config", DEFAULT_CONFIG);
  $("apply").onclick = () => applyConfig($("config").value);
  $("config").addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") applyConfig($("config").value);
  });
  $("toggle-editor").onclick = () => {
    const host = $("editor-host");
    host.hidden = !host.hidden;
    if (!host.hidden && !editor) {
      editor = customElements.get("tuya-local-avent-card").getConfigElement();
      editor.hass = hass;
      editor.setConfig(parse($("config").value));
      editor.addEventListener("config-changed", (e) => {
        const text = dump({ type: "custom:tuya-local-avent-card", ...e.detail.config });
        $("config").value = text;
        applyConfig(text);
      });
      host.appendChild(editor);
    }
  };
  renderEntities();
  applyConfig($("config").value);
}

boot().catch((e) => {
  $("config-err").textContent = `boot: ${e.message || e}`;
  console.error(e);
});
