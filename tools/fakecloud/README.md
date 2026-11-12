# fakecloud

Fake internet for the Avent monitor while its WAN access is blocked.
Answers every DNS A query with this host's IP, serves SNTP, accepts TCP on
Tuya cloud ports. Logs every lookup and connection as JSON lines (TLS SNI,
ALPN, PSK offers, HTTP request lines). Stdlib Python, no deps.

TLS is never completed: the monitor pins Tuya certs / PSK. Goal: find out
whether DNS + NTP + an open TCP port is enough to stop its restart loop.

## Run on the Mac

    python3 tools/fakecloud/fakecloud.py | tee fakecloud.jsonl

macOS binds ports < 1024 without root. Env options: see the script header.

## Point the monitor at it (UniFi)

Settings > Routing > NAT (Network 9+), two entries:

1. DNAT: source 192.168.1.46, protocol TCP/UDP, destination port 53,
   translate to `<fakecloud IP>`:53.
2. Masquerade: source 192.168.1.46, destination `<fakecloud IP>` port 53.
   Same-subnet DNAT needs this, else replies come from the wrong IP.

DNS log then shows src = gateway. TCP/NTP come straight from 192.168.1.46.

## Run on the HA host later

    docker build -t fakecloud tools/fakecloud
    docker run -d --name fakecloud --network tuya-local-avent --ip 192.168.1.91 \
      -e FAKE_IP=192.168.1.91 fakecloud

Then change the UniFi NAT target to 192.168.1.91.
