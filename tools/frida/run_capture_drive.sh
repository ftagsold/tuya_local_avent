#!/usr/bin/env bash
set -uo pipefail
HERE=/Users/fta/dev/fta/workspaces/tuya/tools/frida
export ANDROID_HOME=/opt/homebrew/share/android-commandlinetools JAVA_HOME=/opt/homebrew/opt/openjdk
export PATH="$JAVA_HOME/bin:$ANDROID_HOME/emulator:$ANDROID_HOME/platform-tools:$ANDROID_HOME/cmdline-tools/latest/bin:/opt/homebrew/bin:$PATH"
BASE=/Users/fta/Downloads/tuya-apk/base.apk; SPLIT=/Users/fta/Downloads/tuya-apk/split_config.arm64_v8a.apk
adb kill-server >/dev/null 2>&1; adb start-server >/dev/null 2>&1
emulator -avd tuya -no-window -no-snapshot -no-boot-anim -gpu swiftshader_indirect -no-audio >/tmp/emu.log 2>&1 &
EMU=$!; trap 'kill $EMU 2>/dev/null' EXIT
adb wait-for-device
for i in $(seq 1 120); do [ "$(adb shell getprop sys.boot_completed 2>/dev/null|tr -d '\r')" = 1 ] && break; sleep 2; done
echo "[*] booted"; adb install-multiple -r "$BASE" "$SPLIT" 2>&1 | tail -1
adb root >/dev/null 2>&1; sleep 3
adb push "$HERE/frida-server-17.26.1-android-arm64" /data/local/tmp/frida-server >/dev/null
adb shell chmod 755 /data/local/tmp/frida-server
adb shell "nohup /data/local/tmp/frida-server >/dev/null 2>&1 &" >/dev/null 2>&1; sleep 4
echo "[*] start capture (spawns app)"
"$HERE/venv/bin/python3" "$HERE/capture.py" capture_ssl.js 150 > /tmp/cap.json 2>/tmp/cap.err &
CAP=$!
sleep 18
echo "[*] drive consent UI"
for round in 1 2 3 4 5 6; do
  adb shell uiautomator dump /sdcard/u.xml >/dev/null 2>&1
  adb pull /sdcard/u.xml /tmp/u.xml >/dev/null 2>&1
  COORDS=$(python3 - <<'PY'
import re,sys
try: xml=open('/tmp/u.xml',encoding='utf-8').read()
except: sys.exit()
pat=re.compile(r'<node[^>]*>')
best=None
kw=['agree','accept','同意','got it','continue','allow','next','sign in','log in','login','email']
for m in re.finditer(r'<node ([^>]*?)/?>',xml):
    attrs=dict(re.findall(r'(\w+)="([^"]*)"',m.group(1)))
    t=(attrs.get('text','')+' '+attrs.get('content-desc','')).lower()
    if attrs.get('clickable')=='true' and any(k in t for k in kw):
        b=attrs.get('bounds','')
        mm=re.match(r'\[(\d+),(\d+)\]\[(\d+),(\d+)\]',b)
        if mm:
            x=(int(mm.group(1))+int(mm.group(3)))//2; y=(int(mm.group(2))+int(mm.group(4)))//2
            print(f"{x} {y} {t.strip()[:30]}"); break
PY
)
  if [ -n "$COORDS" ]; then
    X=$(echo "$COORDS"|awk '{print $1}'); Y=$(echo "$COORDS"|awk '{print $2}')
    echo "[*] tap $COORDS"; adb shell input tap "$X" "$Y"
  else
    echo "[*] round $round: no consent/login button found"
  fi
  sleep 6
done
echo "[*] wait for traffic"; sleep 40
kill $CAP 2>/dev/null
echo "=== captured ==="; cat /tmp/cap.json
