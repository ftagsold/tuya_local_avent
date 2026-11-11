#!/usr/bin/env python3
"""Spawn the Tuya Smart app under Frida and print the assembled signing key.

Self-terminating: exits as soon as dump_signing_key.js sends the key, or after
a timeout. Run against a booted emulator with frida-server already running.
"""
import os
import sys
import threading

import frida

HERE = os.path.dirname(os.path.abspath(__file__))
PACKAGE = "com.tuya.smart"
TIMEOUT = 180


def main() -> int:
    js = open(os.path.join(HERE, "dump_signing_key.js")).read()
    device = frida.get_usb_device(timeout=30)
    pid = device.spawn([PACKAGE])
    session = device.attach(pid)
    script = session.create_script(js)

    found = threading.Event()
    result = {}

    def on_message(message, _data):
        if message["type"] == "send" and isinstance(message.get("payload"), dict):
            p = message["payload"]
            if p.get("type") == "key":
                result.update(p)
                found.set()
        elif message["type"] == "error":
            print("JS error:", message.get("stack") or message.get("description"), file=sys.stderr)

    script.on("message", on_message)
    script.load()
    device.resume(pid)

    if found.wait(TIMEOUT):
        print("SIGNING_KEY=" + result["signing_key"])
        print("EMBEDDED_KEY=" + result["embedded_key"])
        return 0
    print("NOT FOUND within %ds (app may block Frida/emulator, or needs a tap to login)" % TIMEOUT, file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
