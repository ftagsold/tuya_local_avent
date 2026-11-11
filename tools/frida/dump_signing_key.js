// dump_signing_key.js — recover the Tuya Mobile SDK signing key from the
// running Tuya Smart app, for local interoperability with your own device.
//
// The Thing security SDK assembles the signing key as a std::string in native
// memory during init: "<package>_<certSHA256>_<embeddedKey>_<appSecret>"
// (PROTOCOL.md §6.3). The embedded key cannot be read statically from
// assets/cers on this app version, so we scan process memory for the string
// whose prefix we already know (package + start of the cert hash) and read it
// out whole. No fixed .so offset, so it survives app updates.
//
// Usage:  frida -U -f com.tuya.smart -l dump_signing_key.js
// Then let the app reach its login screen; the SDK inits and the key appears.

// Known, non-secret anchors for THIS APK (from static extraction):
const PACKAGE = "com.tuya.smart";
const CERT_PREFIX = "93:21:9F:C2:73:E2:20:0F"; // start of the cert SHA-256
// The app secret is the 4th "_" part: a 32-char lowercase alphanumeric run.
const SECRET_RE = /^[0-9a-z]{32}/;

const KNOWN_PREFIX = PACKAGE + "_" + CERT_PREFIX;

function toPattern(s) {
  return Array.from(s)
    .map((c) => ("0" + c.charCodeAt(0).toString(16)).slice(-2))
    .join(" ");
}

function readPrintable(addr, max) {
  const u = new Uint8Array(addr.readByteArray(max));
  let out = "";
  for (let i = 0; i < u.length; i++) {
    const b = u[i];
    if (b < 0x20 || b > 0x7e) break;
    out += String.fromCharCode(b);
  }
  return out;
}

function scanOnce() {
  const pattern = toPattern(KNOWN_PREFIX);
  let ranges = [];
  try {
    ranges = Process.enumerateRanges("rw-").concat(Process.enumerateRanges("r--"));
  } catch (e) {
    return false;
  }
  for (const r of ranges) {
    let hits = [];
    try {
      hits = Memory.scanSync(r.base, r.size, pattern);
    } catch (e) {
      continue;
    }
    for (const h of hits) {
      const full = readPrintable(h.address, 256);
      const parts = full.split("_");
      // pkg_cert_embedded_secret — pkg/cert/emb/secret carry no underscore.
      if (parts.length >= 4 && parts[0] === PACKAGE && SECRET_RE.test(parts[3])) {
        console.log("\n=== SIGNING KEY FOUND ===");
        console.log("signing_key  (AVENT_SIGNING_KEY): " + full);
        console.log("embedded_key                    : " + parts[2]);
        console.log("=========================\n");
        send({ type: "key", signing_key: full, embedded_key: parts[2] });
        return true;
      }
    }
  }
  return false;
}

console.log("[*] waiting for the SDK to assemble the signing key...");
let tries = 0;
const timer = setInterval(() => {
  tries++;
  if (scanOnce()) {
    clearInterval(timer);
  } else if (tries > 40) {
    clearInterval(timer);
    console.log("[!] not found after " + tries + " tries. Open the app's login");
    console.log("    screen (it triggers a signed API call), or the app may be");
    console.log("    blocking Frida/emulator — say so and I'll add a bypass.");
  } else {
    console.log("[*] not found yet (try " + tries + "); interact with the app");
  }
}, 2000);
