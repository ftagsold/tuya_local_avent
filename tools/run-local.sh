#!/usr/bin/env bash
# Run the full bridge natively on macOS (Docker/podman on a Mac cannot reach
# the monitor on the LAN). Reads its env from docker-compose.yml, then applies
# the Mac-specific overrides below.
#
# Needs: cargo, go, brew ffmpeg-full (drawtext), go2rtc in tools/go2rtc/
# (gh release download v1.9.15 -R AlexxIT/go2rtc -p go2rtc_mac_arm64.zip).
# Usage: tools/run-local.sh [--no-build]
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT=$PWD

if [[ "${1:-}" != "--no-build" ]]; then
  cargo build --release
  (cd bridge && go build -o avent-webrtc-bridge .)
fi

[[ -f docker-compose.yml ]] || { echo "docker-compose.yml missing (copy docker-compose.example.yml)" >&2; exit 1; }

# Export every `      KEY: value` line of the compose environment block.
eval "$(awk '/environment:/{f=1;next} /^networks:/{f=0}
  f && /^      [A-Z0-9_]+:/ {
    l=$0; sub(/^ +/,"",l); k=l; sub(/:.*/,"",k)
    v=l; sub(/^[A-Z_0-9]+: */,"",v); sub(/ +#.*$/,"",v); gsub(/^"|"$/,"",v)
    gsub(/\047/,"\047\\\047\047",v)
    printf "export %s=\047%s\047\n", k, v
  }' docker-compose.yml)"

FFMPEG_FULL="$(brew --prefix ffmpeg-full 2>/dev/null)/bin/ffmpeg"
[[ -x "$FFMPEG_FULL" ]] || { echo "brew install ffmpeg-full (needed for the drawtext overlay)" >&2; exit 1; }
[[ -x "$ROOT/tools/go2rtc/go2rtc" ]] || { echo "go2rtc missing: see the header of this script" >&2; exit 1; }

LAN_IP="${LAN_IP:-$(ipconfig getifaddr en0 || ipconfig getifaddr en1 || true)}"
[[ -n "$LAN_IP" ]] || { echo "could not find the Mac's LAN IP; set LAN_IP=..." >&2; exit 1; }

export AVENT_DATA="$ROOT/data"
export AVENT_BRIDGE_BIN="$ROOT/bridge/avent-webrtc-bridge"
export AVENT_GO2RTC_BIN="$ROOT/tools/go2rtc/go2rtc"
export AVENT_FFMPEG="$FFMPEG_FULL"
export AVENT_OVERLAY_FONT=/System/Library/Fonts/Supplemental/Arial.ttf
export ONVIF_PORT="${ONVIF_PORT_LOCAL:-8001}"   # 8000 is held by podman's gvproxy
export ONVIF_BIND_IP="$LAN_IP"
unset TZ                                          # use the Mac's local time

# HA control off until the broker credentials are filled in.
if [[ -z "${MQTT_USER:-}" || "${MQTT_USER:-}" == "CHANGEME" ]]; then
  echo "MQTT_USER not set in docker-compose.yml: Home Assistant control disabled" >&2
  unset MQTT_HOST
fi

echo "ONVIF: http://$LAN_IP:$ONVIF_PORT  RTSP: rtsp://$LAN_IP:${RTSP_PORT:-8554}/<name> (auth required)" >&2
# Children of a previous run that was killed hard (SIGKILL skips cleanup)
# would still hold the ports: stop them first, and again on exit.
cleanup() {
  pkill -f "$ROOT/target/release/tuya_local_avent run" 2>/dev/null || true
  pkill -f "$AVENT_DATA/bridge.json" 2>/dev/null || true
  pkill -f "$AVENT_DATA/go2rtc.yaml" 2>/dev/null || true
  pkill -f "127.0.0.1:38554/" 2>/dev/null || true
}
cleanup; sleep 1
trap cleanup EXIT INT TERM

# The Go bridge keeps its state in ./.tuya-data: keep it under data/, as in the container.
mkdir -p "$AVENT_DATA" && cd "$AVENT_DATA"
"$ROOT/target/release/tuya_local_avent" run
