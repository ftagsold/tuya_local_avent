// Capture the parameter map the Tuya Smart app signs for its own API calls, so
// we can mirror the exact clientId / appVersion the EU gateway accepts. The app
// signs calls on startup (region/config), before any login, so no account is
// needed. For local interop with the user's own account.

Java.perform(function () {
  var seen = {};
  function hookGroup(query) {
    var groups = Java.enumerateMethods(query);
    groups.forEach(function (g) {
      g.classes.forEach(function (c) {
        var C = Java.use(c.name);
        c.methods.forEach(function (mn) {
          try {
            C[mn].overloads.forEach(function (ov) {
              ov.implementation = function () {
                try {
                  var a0 = arguments[0];
                  if (a0 && a0.keySet) {
                    var out = {};
                    var it = a0.keySet().iterator();
                    while (it.hasNext()) {
                      var k = it.next();
                      out[String(k)] = String(a0.get(k));
                    }
                    // Only interesting once per (action, clientId) pair.
                    var key = (out["a"] || "") + "|" + (out["clientId"] || "");
                    if (!seen[key]) {
                      seen[key] = true;
                      send({ type: "params", cls: c.name, m: mn, params: out });
                    }
                  }
                } catch (e) {}
                return ov.apply(this, arguments);
              };
            });
          } catch (e) {}
        });
      });
    });
    return groups.length;
  }

  var n = hookGroup("*!*ignatureSdk*") + hookGroup("*!*enerateSignature*");
  send({ type: "ready", groups: n });
});
