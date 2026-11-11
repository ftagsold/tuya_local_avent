// Frida 16 (Java bridge). Keep the Tuya Smart app alive on the emulator by
// spoofing the obvious emulator tells, then dump the exact parameter map the
// app signs (ThingApiSignManager.generateSignatureSdk) and the sign it gets
// back. Reveals the real clientId / ttid / appVersion / et / whether postData
// is encrypted — the ground truth for our own signed login.

Java.perform(function () {
  // --- spoof emulator detection so the app proceeds to its first API call ---
  try {
    var Build = Java.use("android.os.Build");
    Build.FINGERPRINT.value = "google/raven/raven:14/UP1A.231005.007/10817346:user/release-keys";
    Build.MODEL.value = "Pixel 6 Pro";
    Build.MANUFACTURER.value = "Google";
    Build.BRAND.value = "google";
    Build.PRODUCT.value = "raven";
    Build.DEVICE.value = "raven";
    Build.HARDWARE.value = "raven";
    Build.BOARD.value = "raven";
    Build.TAGS.value = "release-keys";
    Build.HOST.value = "abfarm";
  } catch (e) {
    send({ type: "warn", msg: "Build spoof: " + e });
  }

  // --- dump the signed param map ---
  try {
    var Sign = Java.use("com.thingclips.sdk.network.ThingApiSignManager");
    Sign.generateSignatureSdk.implementation = function (map) {
      var out = {};
      try {
        var it = map.keySet().iterator();
        while (it.hasNext()) {
          var k = it.next();
          out[k.toString()] = String(map.get(k));
        }
      } catch (e) {}
      var sig = this.generateSignatureSdk(map);
      send({ type: "sign", params: out, sign: String(sig) });
      return sig;
    };
    send({ type: "info", msg: "hooked generateSignatureSdk" });
  } catch (e) {
    send({ type: "warn", msg: "sign hook: " + e });
  }
});
