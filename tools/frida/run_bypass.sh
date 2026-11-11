#!/usr/bin/env bash
set -uo pipefail
HERE=/Users/fta/dev/fta/workspaces/tuya/tools/frida
export ANDROID_HOME=/opt/homebrew/share/android-commandlinetools JAVA_HOME=/opt/homebrew/opt/openjdk
export PATH="$JAVA_HOME/bin:$ANDROID_HOME/emulator:$ANDROID_HOME/platform-tools:/opt/homebrew/bin:$PATH"
BASE=/Users/fta/Downloads/tuya-apk/base.apk; SPLIT=/Users/fta/Downloads/tuya-apk/split_config.arm64_v8a.apk
adb kill-server >/dev/null 2>&1; adb start-server >/dev/null 2>&1
emulator -avd tuya -no-window -no-snapshot -no-boot-anim -gpu swiftshader_indirect -no-audio >/tmp/emu.log 2>&1 &
EMU=$!; trap 'kill $EMU 2>/dev/null' EXIT
adb wait-for-device
for i in $(seq 1 120); do [ "$(adb shell getprop sys.boot_completed 2>/dev/null|tr -d '\r')" = 1 ] && break; sleep 2; done
echo "[*] booted"; adb install-multiple -r "$BASE" "$SPLIT" 2>&1 | tail -1
adb root >/dev/null 2>&1; sleep 3
adb push "$HERE/frida-server-16.7.19-android-arm64" /data/local/tmp/frida-server16 >/dev/null
adb shell chmod 755 /data/local/tmp/frida-server16
adb shell "nohup /data/local/tmp/frida-server16 >/dev/null 2>&1 &" >/dev/null 2>&1; sleep 4
"$HERE/venv311/bin/python3" -u "$HERE/capture.py" bypass_capture.js 90 > /tmp/bypass.json 2>/tmp/bypass.err &
CAP=$!
sleep 30
echo "[*] app pid after 30s: $(adb shell pidof com.tuya.smart | tr -d '\r')"
# drive consent/login if UI present
for r in 1 2 3 4 5 6; do
  adb shell uiautomator dump /sdcard/u.xml >/dev/null 2>&1 && adb pull /sdcard/u.xml /tmp/u.xml >/dev/null 2>&1
  C=$(python3 - <<'PY'
import re
try: xml=open('/tmp/u.xml',encoding='utf-8').read()
except: raise SystemExit
for m in re.finditer(r'<node ([^>]*?)/?>',xml):
    at=dict(re.findall(r'([\w-]+)="([^"]*)"',m.group(1)))
    t=(at.get('text','')+' '+at.get('content-desc','')).lower()
    if at.get('clickable')=='true' and any(k in t for k in['agree','accept','同意','got it','continue','allow','experience','start','log in','login','sign in','email','use','ok','confirm','next','got']):
        b=re.match(r'\[(\d+),(\d+)\]\[(\d+),(\d+)\]',at.get('bounds',''))
        if b: print((int(b.group(1))+int(b.group(3)))//2,(int(b.group(2))+int(b.group(4)))//2); break
PY
)
  [ -n "$C" ] && { echo "[*] tap $C"; adb shell input tap $C; } || echo "[*] r$r no button"
  sleep 8
done
sleep 20; kill $CAP 2>/dev/null
echo "=== INFO/WARN ==="; grep -aE '"type": "(info|warn)"' /tmp/bypass.json
echo "=== SIGN (first 3) ==="; grep -a '"type": "sign"' /tmp/bypass.json | head -3
echo "=== REQ (first 2) ==="; grep -a '"type": "req"' /tmp/bypass.json | head -2
