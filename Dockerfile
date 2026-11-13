# Single image: the Rust tuya_local_avent (brain + supervisor), the vendored Go
# video bridge, ffmpeg (overlay relay + snapshots) and go2rtc (the only
# LAN-facing RTSP server, password required, talkback backchannel). Entrypoint
# is the Rust binary, which spawns and supervises the others.
#
# The compilers run on the build machine and cross-compile for the target
# (buildx/podman --platform), so an arm64 Mac builds an amd64 image quickly:
#   podman build --platform linux/amd64 --build-arg TARGET_CPU=znver4 -t tuya_local_avent .
# TARGET_CPU (amd64 only): x86-64-v3 runs on any AVX2 x86 (Haswell / Zen 1+);
# znver4 tunes for Zen 4 (e.g. Ryzen 7040) and needs AVX-512.

# --- Go video bridge ---
FROM --platform=$BUILDPLATFORM golang:1.27-bookworm AS gobridge
ARG TARGETOS
ARG TARGETARCH
WORKDIR /src
COPY bridge/go.mod bridge/go.sum ./
RUN go mod download
COPY bridge/ ./
RUN CGO_ENABLED=0 GOOS=$TARGETOS GOARCH=$TARGETARCH GOAMD64=v3 \
    go build -trimpath -ldflags="-s -w" -o /out/avent-webrtc-bridge .

# --- Rust brain ---
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS rustbuild
ARG TARGETARCH
ARG TARGET_CPU=x86-64-v3
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) T=x86_64-unknown-linux-gnu; G=x86-64-linux-gnu; C=x86_64-linux-gnu-gcc; F="-C target-cpu=$TARGET_CPU" ;; \
      arm64) T=aarch64-unknown-linux-gnu; G=aarch64-linux-gnu; C=aarch64-linux-gnu-gcc; F="" ;; \
      *) echo "unsupported TARGETARCH $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    apt-get update; \
    apt-get install -y --no-install-recommends "gcc-$G" "libc6-dev-$TARGETARCH-cross" \
      || apt-get install -y --no-install-recommends "gcc-$G"; \
    rm -rf /var/lib/apt/lists/*; \
    rustup target add "$T"; \
    printf 'T=%s\nC=%s\nF="%s"\n' "$T" "$C" "$F" > /cross.env
WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY src/ ./src/
# RUSTFLAGS sets the CPU target. .cargo/config.toml (target-cpu=native) is not
# copied: "native" would mean the build machine.
RUN set -eu; . /cross.env; \
    TU=$(echo "$T" | tr 'a-z-' 'A-Z_'); \
    export "CARGO_TARGET_${TU}_LINKER=$C" "CC_$(echo "$T" | tr '-' '_')=$C" RUSTFLAGS="$F"; \
    cargo build --release --target "$T"; \
    install -D "target/$T/release/tuya_local_avent" /out/tuya_local_avent

# --- go2rtc (static binary) ---
FROM alexxit/go2rtc:1.9.15 AS go2rtc

# --- Runtime ---
FROM debian:bookworm-slim
LABEL org.opencontainers.image.source="https://github.com/ftagsold/tuya_local_avent" \
      org.opencontainers.image.description="Local bridge: Philips Avent (Tuya) baby monitor to Home Assistant and UniFi Protect" \
      org.opencontainers.image.licenses="MIT"
# ffmpeg (with drawtext/libfreetype), the overlay font, tzdata for the clock.
# amd64: Mesa's VAAPI driver for GPU H.264 encode (AVENT_HWACCEL), from
# backports: bookworm's Mesa 22.3 predates the Radeon 780M (Ryzen 7040).
ARG TARGETARCH
RUN set -eu; \
    if [ "$TARGETARCH" = amd64 ]; then \
      echo "deb http://deb.debian.org/debian bookworm-backports main" > /etc/apt/sources.list.d/backports.list; \
    fi; \
    apt-get update; \
    apt-get install -y --no-install-recommends ffmpeg ca-certificates fonts-dejavu-core tzdata; \
    if [ "$TARGETARCH" = amd64 ]; then \
      apt-get install -y --no-install-recommends -t bookworm-backports mesa-va-drivers; \
    fi; \
    rm -rf /var/lib/apt/lists/*
COPY --from=gobridge /out/avent-webrtc-bridge /app/avent-webrtc-bridge
COPY --from=rustbuild /out/tuya_local_avent /app/tuya_local_avent
COPY --from=go2rtc /usr/local/bin/go2rtc /app/go2rtc
ENV AVENT_BRIDGE_BIN=/app/avent-webrtc-bridge AVENT_GO2RTC_BIN=/app/go2rtc AVENT_DATA=/data
# The Go bridge keeps its state in ./.tuya-data.
WORKDIR /data
VOLUME ["/data"]
# RTSP 8554 (TCP), ONVIF 8000, WS-Discovery 3702/udp. The Go bridge (38554)
# and the go2rtc API (1984) listen on loopback only.
EXPOSE 8554 8000 3702/udp
ENTRYPOINT ["/app/tuya_local_avent"]
CMD ["run"]
