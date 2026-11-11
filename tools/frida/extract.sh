#!/usr/bin/env bash
# One-shot: boot an arm64 Android emulator, install the Tuya Smart app, run
# frida-server, and dump the signing key with dump_signing_key.js.
#
# For recovering the signing key of your own Tuya Smart app, to drive your own
# device locally. Run after the SDK + system image finished installing.
#
#   ./extract.sh
#
# Prereqs installed by the session: android-commandlinetools, platform-tools,
# emulator, system-images;android-34;google_apis;arm64-v8a, the frida venv here,
# and frida-server-17.26.1-android-arm64 in this directory.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
export ANDROID_HOME="${ANDROID_HOME:-/opt/homebrew/share/android-commandlinetools}"
export JAVA_HOME="${JAVA_HOME:-/opt/homebrew/opt/openjdk}"
export PATH="$JAVA_HOME/bin:$ANDROID_HOME/emulator:$ANDROID_HOME/platform-tools:$ANDROID_HOME/cmdline-tools/latest/bin:/opt/homebrew/bin:$PATH"

AVD=tuya
IMAGE="system-images;android-34;google_apis;arm64-v8a"
BUNDLE_DIR="/Users/fta/Downloads"
BASE="$BUNDLE_DIR/tuya-apk/base.apk"
SPLIT="$BUNDLE_DIR/tuya-apk/split_config.arm64_v8a.apk"
FS="$HERE/frida-server-17.26.1-android-arm64"
FRIDA="$HERE/venv/bin/frida"

# The split sits inside the .apkm next to base.apk; extract it once.
if [ ! -f "$SPLIT" ]; then
  unzip -o "$BUNDLE_DIR"/com.tuya.smart_*.apkm split_config.arm64_v8a.apk -d "$BUNDLE_DIR/tuya-apk" >/dev/null
fi

# Create the AVD once. google_apis (not _playstore) so `adb root` works.
if ! avdmanager list avd 2>/dev/null | grep -q "Name: $AVD"; then
  echo "[*] creating AVD $AVD"
  echo no | avdmanager create avd -n "$AVD" -k "$IMAGE" -d pixel_6 --force
fi

echo "[*] booting emulator (a window opens; leave it running)"
emulator -avd "$AVD" -no-snapshot -no-boot-anim -gpu swiftshader_indirect >/tmp/emulator.log 2>&1 &
EMU_PID=$!
trap 'kill $EMU_PID 2>/dev/null || true' EXIT

echo "[*] waiting for boot..."
adb wait-for-device
until [ "$(adb shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" = "1" ]; do sleep 2; done
echo "[*] booted"

echo "[*] installing the app"
adb install-multiple -r "$BASE" "$SPLIT"

echo "[*] starting frida-server as root"
adb root >/dev/null 2>&1 || true
sleep 2
adb push "$FS" /data/local/tmp/frida-server >/dev/null
adb shell chmod 755 /data/local/tmp/frida-server
adb shell "su 0 /data/local/tmp/frida-server &" >/dev/null 2>&1 \
  || adb shell "/data/local/tmp/frida-server &" >/dev/null 2>&1 &
sleep 3

echo "[*] launching the app under frida and scanning memory"
echo "    (if nothing appears in ~30s, tap through to the login screen)"
"$FRIDA" -U -f com.tuya.smart -l "$HERE/dump_signing_key.js"
