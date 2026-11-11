// Dump plaintext HTTP request/response bodies to Tuya's api.json by hooking
// BoringSSL SSL_write / SSL_read in libssl.so. Native only (Frida 17 safe).

function tryHook() {
  var m = Process.findModuleByName("libssl.so");
  if (!m) return false;
  var w = m.findExportByName("SSL_write");
  var r = m.findExportByName("SSL_read");
  if (!w) return false;

  Interceptor.attach(w, {
    onEnter: function (a) {
      try {
        var n = a[2].toInt32();
        if (n > 0 && n < 65536) {
          var s = a[1].readUtf8String(n);
          if (s && (s.indexOf("a=thing") >= 0 || s.indexOf("clientId") >= 0 || s.indexOf("sign=") >= 0)) {
            send({ type: "req", body: s.slice(0, 4000) });
          }
        }
      } catch (e) {}
    },
  });
  if (r) {
    Interceptor.attach(r, {
      onEnter: function (a) {
        this.buf = a[1];
      },
      onLeave: function (ret) {
        try {
          var n = ret.toInt32();
          if (n > 0 && n < 65536) {
            var s = this.buf.readUtf8String(n);
            if (s && (s.indexOf("ILLEGAL_CLIENT") >= 0 || s.indexOf("pbKey") >= 0 || s.indexOf("errorCode") >= 0 || s.indexOf('"result"') >= 0)) {
              send({ type: "resp", body: s.slice(0, 2000) });
            }
          }
        } catch (e) {}
      },
    });
  }
  send({ type: "info", msg: "hooked SSL_write/SSL_read in libssl.so" });
  return true;
}

var n = 0;
var iv = setInterval(function () {
  n++;
  if (tryHook() || n >= 30) {
    clearInterval(iv);
    if (n >= 30) send({ type: "info", msg: "libssl.so/SSL_write not found" });
  }
}, 1000);
