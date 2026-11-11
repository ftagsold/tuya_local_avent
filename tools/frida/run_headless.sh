#!/usr/bin/env bash
set -uo pipefail
HERE=/Users/fta/dev/fta/workspaces/tuya/tools/frida
export ANDROID_HOME=/opt/homebrew/share/android-commandlinetools
export JAVA_HOME=/opt/homebrew/opt/openjdk
export PATH="$JAVA_HOME/bin:$ANDROID_HOME/emulator:$ANDROID_HOME/platform-tools:$ANDROID_HOME/cmdline-tools/latest/bin:/opt/homebrew/bin:$PATH"
AVD=tuya; IMAGE="system-images;android-34;google_apis;arm64-v8a"
BASE=/Users/fta/Downloads/tuya-apk/base.apk
SPLIT=/Users/fta/Downloads/tuya-apk/split_config.arm64_v8a.apk
[ -f "$SPLIT" ] || unzip -o /Users/fta/Downloads/com.tuya.smart_*.apkm split_config.arm64_v8a.apk -d /Users/fta/Downloads/tuya-apk >/dev/null
avdmanager list avd 2>/dev/null | grep -q "Name: $AVD" || echo no | avdmanager create avd -n "$AVD" -k "$IMAGE" --force
adb kill-server >/dev/null 2>&1; adb start-server >/dev/null 2>&1
echo "[*] boot"; emulator -avd "$AVD" -no-window -no-snapshot -no-boot-anim -gpu swiftshader_indirect -no-audio >/tmp/emu.log 2>&1 &
EMU=$!; trap 'kill $EMU 2>/dev/null' EXIT
adb wait-for-device
for i in $(seq 1 120); do [ "$(adb shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = 1 ] && break; sleep 2; done
echo "[*] boot_completed=$(adb shell getprop sys.boot_completed | tr -d '\r')"
adb install-multiple -r "$BASE" "$SPLIT" 2>&1 | tail -1
adb root >/dev/null 2>&1; sleep 3
adb push "$HERE/frida-server-17.26.1-android-arm64" /data/local/tmp/frida-server >/dev/null
adb shell chmod 755 /data/local/tmp/frida-server
adb shell "nohup /data/local/tmp/frida-server >/dev/null 2>&1 &" >/dev/null 2>&1
sleep 4
echo "[*] frida-ps check:"; "$HERE/venv/bin/frida-ps" -U 2>&1 | grep -i tuya || echo "(tuya not listed yet)"
echo "[*] dump:"
"$HERE/venv/bin/python3" "$HERE/dump.py"
