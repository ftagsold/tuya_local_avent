// Diagnostic: after the app's native libs load, report whether SSL_write is
// hookable and what the security lib exports, so we can pick a capture point.
function scan() {
  var sslMods = [];
  var secExports = [];
  Process.enumerateModules().forEach(function (m) {
    var low = m.name.toLowerCase();
    var interesting = low.indexOf("ssl") >= 0 || low.indexOf("crypto") >= 0 || low.indexOf("conscrypt") >= 0 || low.indexOf("thing_security") >= 0 || low.indexOf("security_algorithm") >= 0;
    if (!interesting) return;
    try {
      var exps = m.enumerateExports();
      var names = exps.map(function (e) { return e.name; });
      var hit = names.filter(function (n) { return n.indexOf("SSL_write") >= 0 || n.indexOf("SSL_read") >= 0 || n.indexOf("doCommand") >= 0 || n.indexOf("sign") >= 0; });
      sslMods.push({ mod: m.name, exports: exps.length, hits: hit.slice(0, 10) });
    } catch (e) {
      sslMods.push({ mod: m.name, err: String(e) });
    }
  });
  send({ type: "scan", mods: sslMods });
}
var n = 0;
var iv = setInterval(function () { n++; scan(); if (n >= 8) clearInterval(iv); }, 2000);
