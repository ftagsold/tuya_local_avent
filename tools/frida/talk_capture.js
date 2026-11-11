// Frida 16: talkback capture. Emulator bypass (from bypass_capture.js) plus hooks on
// the camera SDK's talk path and the P2P layer's plaintext app messages.

var PROP = {
  "ro.kernel.qemu": "0",
  "ro.boot.qemu": "0",
  "ro.hardware": "qcom",
  "ro.hardware.chipname": "SM8450",
  "ro.board.platform": "gs101",
  "ro.product.model": "Pixel 6 Pro",
  "ro.product.manufacturer": "Google",
  "ro.product.brand": "google",
  "ro.product.name": "raven",
  "ro.product.device": "raven",
  "ro.product.board": "raven",
  "ro.build.characteristics": "nosdcard",
  "ro.build.fingerprint": "google/raven/raven:14/UP1A.231005.007/10817346:user/release-keys",
  "ro.bootloader": "slider-1.0-10646350",
  "ro.bootmode": "unknown",
  "ro.secure": "1",
  "ro.debuggable": "0",
  "ro.serialno": "1A2B3C4D5E",
  "init.svc.qemud": "",
  "init.svc.qemu-props": "",
  "qemu.hw.mainkeys": "",
  "qemu.sf.lcd_density": "",
  "gsm.version.baseband": "g5300g-240205",
};

function hookProps() {
  var f = Module.findExportByName("libc.so", "__system_property_get");
  if (!f) return;
  Interceptor.attach(f, {
    onEnter: function (a) {
      this.name = a[0].readCString();
      this.out = a[1];
    },
    onLeave: function (ret) {
      if (this.name && Object.prototype.hasOwnProperty.call(PROP, this.name)) {
        var v = PROP[this.name];
        this.out.writeUtf8String(v);
        ret.replace(ptr(v.length));
      }
    },
  });
}

var HIDE = ["qemu", "goldfish", "ranchu", "genymotion", "/dev/socket/qemud", "vbox", "/sys/qemu_trace", "bstfolder"];
function hookFiles() {
  ["fopen", "open", "access", "stat", "__xstat", "stat64"].forEach(function (name) {
    var f = Module.findExportByName("libc.so", name);
    if (!f) return;
    Interceptor.attach(f, {
      onEnter: function (a) {
        try {
          var p = a[0].readCString();
          this.block = p && HIDE.some(function (h) { return p.indexOf(h) >= 0; });
        } catch (e) { this.block = false; }
      },
      onLeave: function (ret) {
        if (this.block) ret.replace(ptr("0x0")); // NULL / -1-ish: pretend missing
      },
    });
  });
}

function hex(p, n) {
  var b = new Uint8Array(p.readByteArray(n)), s = "";
  for (var i = 0; i < b.length; i++) s += ("0" + b[i].toString(16)).slice(-2);
  return s;
}

var talking = false;

// libThingP2PSDK: every outgoing app message is AES-CBC encrypted (mode 1)
// right before KCP; capture its plaintext. Incoming (mode 0) only when small,
// i.e. control replies, not video. ikcp_send names the conversation.
function attachP2P() {
  var m = Process.findModuleByName("libThingP2PSDK.so");
  if (!m) return false;
  Interceptor.attach(m.findExportByName("mbedtls_aes_crypt_cbc"), {
    onEnter: function (a) {
      this.mode = a[1].toInt32(); this.len = a[2].toUInt32(); this.out = a[5];
      if (this.mode === 1) {
        var n = this.len <= 2048 ? this.len : 128;
        send({ type: "enc", t: Date.now(), talking: talking, len: this.len, hex: hex(a[4], n) });
      }
    },
    onLeave: function () {
      if (this.mode === 0 && this.len <= 512) send({ type: "dec", t: Date.now(), talking: talking, len: this.len, hex: hex(this.out, this.len) });
    },
  });
  Interceptor.attach(m.findExportByName("ikcp_send"), {
    onEnter: function (a) { send({ type: "kcp", t: Date.now(), conv: a[0].readU32(), len: a[2].toInt32() }); },
  });
  send({ type: "info", msg: "p2p hooks on" });
  return true;
}

function attachCam() {
  var m = Process.findModuleByName("libThingCameraSDK.so");
  if (!m) return false;
  [["_ZN13ThingCameraV414StartAudioTalkEiP17ThingBaseCallBack", "startTalk"],
   ["_ZN13ThingCameraV413StopAudioTalkEi", "stopTalk"]].forEach(function (x) {
    var f = m.findExportByName(x[0]);
    if (f) Interceptor.attach(f, { onEnter: function () { talking = x[1] === "startTalk"; send({ type: "mark", t: Date.now(), ev: x[1] }); } });
  });
  var n = 0;
  Interceptor.attach(m.findExportByName("_ZN13ThingCameraV417SendAudioTalkDataEPKhi"), {
    onEnter: function (a) { if (n++ < 5) send({ type: "talkin", t: Date.now(), len: a[2].toInt32(), hex: hex(a[1], Math.min(64, a[2].toInt32())) }); },
  });
  var k = 0;
  Interceptor.attach(m.findExportByName("_ZN13ThingSmartIPC3CXX23ThingNetProtocolManager13SendAudioDataEPhji17Thing_AV_CODEC_ID23TRANSFER_AUDIO_SAMPLE_E20Thing_AUDIO_DATABITS24TRANSFER_AUDIO_CHANNEL_E"), {
    onEnter: function (a) {
      if (k++ < 5) send({ type: "sendaudio", t: Date.now(), len: a[2].toUInt32(), arg: a[3].toInt32(), codec: a[4].toInt32(), sample: a[5].toInt32(), bits: a[6].toInt32(), chan: a[7].toInt32(), hex: hex(a[1], Math.min(64, a[2].toUInt32())) });
    },
  });
  send({ type: "info", msg: "camera hooks on" });
  return true;
}

Java.perform(function () {
  hookProps();
  hookFiles();
  try {
    var B = Java.use("android.os.Build");
    B.FINGERPRINT.value = PROP["ro.build.fingerprint"];
    B.MODEL.value = "Pixel 6 Pro"; B.MANUFACTURER.value = "Google"; B.BRAND.value = "google";
    B.PRODUCT.value = "raven"; B.DEVICE.value = "raven"; B.HARDWARE.value = "qcom";
    B.BOARD.value = "raven"; B.TAGS.value = "release-keys"; B.HOST.value = "abfarm-release";
  } catch (e) {}
  // The camera libs load when a camera panel opens: poll until both are in.
  var p2p = false, cam = false;
  var iv = setInterval(function () {
    if (!p2p) p2p = attachP2P();
    if (!cam) cam = attachCam();
    if (p2p && cam) clearInterval(iv);
  }, 500);
  send({ type: "info", msg: "bypass on; waiting for camera libs" });
});
