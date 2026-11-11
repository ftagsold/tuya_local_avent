#!/usr/bin/env bash
# Browser harness for the Lovelace card, without Home Assistant.
#
# Starts a go2rtc that does what Home Assistant's built-in go2rtc does for the
# ONVIF camera (RTSP from the bridge with the talkback backchannel, plus an
# ffmpeg opus transcode for listen audio) and a static server for the page.
# The page fakes `hass`, so the real card runs against real video and talk.
#
# Usage:
#   tools/card-harness/run.sh            # stream from a running bridge (tools/run-local.sh)
#   tools/card-harness/run.sh --fake     # test pattern, no bridge (talk has no backchannel)
# Env: BRIDGE_HOST (default 127.0.0.1), STREAM_PATH (default: from data/go2rtc.yaml),
#      HARNESS_PORT (default 8125). go2rtc API on 127.0.0.1:11984, WebRTC on 127.0.0.1:18555.
#
# Open http://localhost:8125/ (localhost, not the LAN IP: the mic needs a secure context).
set -euo pipefail

cd "$(dirname "$0")/../.."
ROOT=$PWD
WORK="$ROOT/tools/card-harness/work"
G2R="$ROOT/tools/go2rtc/go2rtc"
[[ -x "$G2R" ]] || { echo "go2rtc missing: see the header of tools/run-local.sh" >&2; exit 1; }
FFMPEG="$(brew --prefix ffmpeg-full 2>/dev/null)/bin/ffmpeg"
[[ -x "$FFMPEG" ]] || FFMPEG="$(command -v ffmpeg)"
mkdir -p "$WORK"

# Config holds the RTSP credential: written 0600 into the gitignored work dir.
MODE="${1:-}" BRIDGE_HOST="${BRIDGE_HOST:-127.0.0.1}" STREAM_PATH="${STREAM_PATH:-}" \
FFMPEG="$FFMPEG" ROOT="$ROOT" OUT="$WORK/go2rtc.yaml" python3 -I - <<'EOF'
import json, os, re, pathlib
from urllib.parse import quote
root = pathlib.Path(os.environ["ROOT"])
if os.environ["MODE"] == "--fake":
    sources = ["ffmpeg:virtual?video=testsrc&size=720#video=h264"]
else:
    env = {}
    comp = root / "docker-compose.yml"
    if comp.exists():
        for line in comp.read_text().splitlines():
            m = re.match(r'\s+([A-Z0-9_]+):\s*"?([^"#\s]+)"?', line)
            if m:
                env[m.group(1)] = m.group(2)
    user = env.get("RTSP_USER") or env.get("ONVIF_USER") or "avent"
    pw = env.get("RTSP_PASS") or env.get("ONVIF_PASS") or ""
    path = os.environ["STREAM_PATH"]
    if not path:
        cfg = root / "data" / "go2rtc.yaml"
        m = re.search(r'^streams:\n\s+"?([^":\n]+)"?:', cfg.read_text(), re.M) if cfg.exists() else None
        if not m:
            raise SystemExit("no stream path: run the bridge once (data/go2rtc.yaml) or set STREAM_PATH")
        path = m.group(1)
    auth = f"{quote(user, safe='')}:{quote(pw, safe='')}@" if pw else ""
    sources = [f"rtsp://{auth}{os.environ['BRIDGE_HOST']}:8554/{path}?backchannel=1"]
# Same shape as Home Assistant's go2rtc integration: the camera's stream
# source plus an ffmpeg opus transcode of it for the browser.
sources.append("ffmpeg:camera.nursery#audio=opus#query=log_level=critical")
cfg = f"""log: {{level: info}}
api: {{listen: "127.0.0.1:11984", origin: "*"}}
rtsp: {{listen: "127.0.0.1:18554"}}
webrtc: {{listen: "127.0.0.1:18555", candidates: ["127.0.0.1:18555"]}}
rtmp: {{listen: ""}}
srtp: {{listen: ""}}
ffmpeg: {{bin: {json.dumps(os.environ["FFMPEG"])}}}
streams:
  camera.nursery:
""" + "".join(f"    - {json.dumps(s)}\n" for s in sources)
fd = os.open(os.environ["OUT"], os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
os.write(fd, cfg.encode())
os.close(fd)
EOF

cleanup() { kill "${G2R_PID:-}" "${SRV_PID:-}" 2>/dev/null || true; }
trap cleanup EXIT INT TERM
"$G2R" -config "$WORK/go2rtc.yaml" > "$WORK/go2rtc.log" 2>&1 &
G2R_PID=$!
python3 -I "$ROOT/tools/card-harness/serve.py" "${HARNESS_PORT:-8125}" &
SRV_PID=$!
echo "go2rtc log: $WORK/go2rtc.log   (Ctrl-C stops both)" >&2
wait
