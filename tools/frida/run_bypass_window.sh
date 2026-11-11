#!/usr/bin/env bash
# Windowed: emulator opens, app kept alive by the bypass. YOU tap through
# consent -> email login -> enter your email -> "Get verification code".
# That triggers thing.m.user.email.token.create; the native-signer hook logs
# the exact signed string to /tmp/bypass.json. Close with Ctrl+C when done.
set -uo pipefail
HERE=/Users/fta/dev/fta/workspaces/tuya/tools/frida
export ANDROID_HOME=/opt/homebrew/share/android-commandlinetools JAVA_HOME=/opt/homebrew/opt/openjdk
export PATH="$JAVA_HOME/bin:$ANDROID_HOME/emulator:$ANDROID_HOME/platform-tools:/opt/homebrew/bin:$PATH"
BASE=/Users/fta/Downloads/tuya-apk/base.apk; SPLIT=/Users/fta/Downloads/tuya-apk/split_config.arm64_v8a.apk
adb kill-server >/dev/null 2>&1; adb start-server >/dev/null 2>&1
emulator -avd tuya -no-snapshot -no-boot-anim -gpu swiftshader_indirect -no-audio >/tmp/emu.log 2>&1 &
EMU=$!; trap 'kill $EMU 2>/dev/null' EXIT
adb wait-for-device
for i in $(seq 1 120); do [ "$(adb shell getprop sys.boot_completed 2>/dev/null|tr -d '\r')" = 1 ] && break; sleep 2; done
echo "[*] booted"; adb install-multiple -r "$BASE" "$SPLIT" 2>&1 | tail -1
adb root >/dev/null 2>&1; sleep 3
adb push "$HERE/frida-server-16.7.19-android-arm64" /data/local/tmp/frida-server16 >/dev/null
adb shell chmod 755 /data/local/tmp/frida-server16
adb shell "nohup /data/local/tmp/frida-server16 >/dev/null 2>&1 &" >/dev/null 2>&1; sleep 4
echo "[*] app launching under capture. Drive it: Agree -> Log in -> email -> Get code."
echo "[*] signed strings stream to /tmp/bypass.json (Ctrl+C here when done)."
"$HERE/venv311/bin/python3" -u "$HERE/capture.py" bypass_capture.js 600 | tee /tmp/bypass.json
