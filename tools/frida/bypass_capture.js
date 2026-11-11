// Frida 16 (Java + native). Make the Tuya Smart app believe it's a real Pixel
// so it survives on the emulator and makes its signed API calls, then capture
// the exact signed param map (Java hook) and the api.json request body (native
// SSL_write). For local interop with the user's own account/device.

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

function hookSSL() {
  var m = Process.findModuleByName("libssl.so");
  if (!m) return false;
  var w = m.findExportByName("SSL_write");
  if (!w) return false;
  Interceptor.attach(w, {
    onEnter: function (a) {
      try {
        var n = a[2].toInt32();
        if (n > 0 && n < 65536) {
          var s = a[1].readUtf8String(n);
          if (s && (s.indexOf("a=thing") >= 0 || s.indexOf("clientId") >= 0 || s.indexOf("sign=") >= 0))
            send({ type: "req", body: s.slice(0, 4000) });
        }
      } catch (e) {}
    },
  });
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
  try {
    var S = Java.use("com.thingclips.sdk.network.ThingApiSignManager");
    S.generateSignatureSdk.implementation = function (map) {
      var out = {};
      try { var it = map.keySet().iterator(); while (it.hasNext()) { var k = it.next(); out[k.toString()] = String(map.get(k)); } } catch (e) {}
      var sig = this.generateSignatureSdk(map);
      send({ type: "sign", params: out, sign: String(sig) });
      return sig;
    };
  } catch (e) { send({ type: "warn", msg: "sign hook: " + e }); }
  // The native signer, caught regardless of transport (OkHttp api.json or the
  // encrypted highway path). cmd==1 is a signature op; arg a is the exact
  // "k=v||..." string being signed.
  // jadx renames com.thingclips.smart.* to com.thingclips.animation.*; the real
  // runtime names use `smart`. The sdk.* packages are not renamed. Hook the
  // static wrapper ThingNetworkSecurity.doCommandNative (and try the jni class
  // under both names) to catch every native sign input.
  var StringC = Java.use("java.lang.String");
  function hookDoCommand(cls) {
    try {
      var C = Java.use(cls);
      C.doCommandNative.overload("android.content.Context", "int", "[B", "[B", "boolean").implementation = function (ctx, cmd, a, b, z) {
        var ret = this.doCommandNative(ctx, cmd, a, b, z);
        try {
          if (cmd === 1 && a !== null) {
            send({ type: "signnative", cls: cls, input: String(StringC.$new(a)).slice(0, 2000), out: ret ? String(ret).slice(0, 80) : null });
          }
        } catch (e) {}
        return ret;
      };
      send({ type: "info", msg: "hooked doCommandNative in " + cls });
    } catch (e) {}
  }
  hookDoCommand("com.thingclips.sdk.network.ThingNetworkSecurity");
  hookDoCommand("com.thingclips.smart.security.jni.JNICLibrary");

  var n = 0, iv = setInterval(function () { n++; if (hookSSL() || n >= 25) clearInterval(iv); }, 1000);
  send({ type: "info", msg: "bypass + hooks installed" });
});
