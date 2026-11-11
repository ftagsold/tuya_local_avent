#!/usr/bin/env python3
"""Spawn Tuya Smart under Frida, load a streaming hook, print every `send`
payload for a window, then exit. Used for diagnostics (e.g. capturing the real
signed-request params)."""
import json
import os
import sys
import time

import frida

HERE = os.path.dirname(os.path.abspath(__file__))
PACKAGE = "com.tuya.smart"


def main() -> int:
    script_file = sys.argv[1] if len(sys.argv) > 1 else "capture_sign_params.js"
    window = int(sys.argv[2]) if len(sys.argv) > 2 else 45
    js = open(os.path.join(HERE, script_file)).read()

    device = frida.get_usb_device(timeout=30)
    pid = device.spawn([PACKAGE])
    session = device.attach(pid)
    script = session.create_script(js)

    def on_message(m, _d):
        if m["type"] == "send":
            print(json.dumps(m.get("payload", m), default=str), flush=True)
        else:
            print("ERR " + json.dumps(m, default=str), file=sys.stderr, flush=True)

    script.on("message", on_message)
    script.load()
    device.resume(pid)
    time.sleep(window)
    try:
        session.detach()
    except Exception:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
