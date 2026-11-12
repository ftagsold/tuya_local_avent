#!/usr/bin/env python3
"""Fake internet for the Avent monitor while its real internet is blocked.

Answers every DNS A query with this host's IP, serves SNTP, and accepts TCP
on the usual Tuya cloud ports. Every lookup and connection attempt is logged
as one JSON line (TLS SNI, ALPN, PSK offers, HTTP request lines), so we can
see what the monitor wants before it gives up and restarts.

Stdlib only. Config via env:
  FAKE_IP         IP returned for every A query (default: auto-detect)
  FAKE_BIND       listen address (default 0.0.0.0)
  FAKE_TCP_PORTS  comma list (default 80,443,1443,8883,1883,8886)
  FAKE_UDP_PORTS  comma list of UDP ports to log only (default 3478,3479)
  FAKE_LOG        also append JSON lines to this file
  FAKE_HOLD       seconds to keep idle TLS/unknown TCP sessions open (default 30)
"""

import json
import os
import signal
import socket
import socketserver
import struct
import sys
import threading
import time
from collections import Counter

BIND = os.environ.get("FAKE_BIND", "0.0.0.0")
TCP_PORTS = [int(p) for p in os.environ.get("FAKE_TCP_PORTS", "80,443,1443,8883,1883,8886").split(",") if p]
UDP_PORTS = [int(p) for p in os.environ.get("FAKE_UDP_PORTS", "3478,3479").split(",") if p]
LOG_FILE = os.environ.get("FAKE_LOG")
HOLD = float(os.environ.get("FAKE_HOLD", "30"))

_log_lock = threading.Lock()
_names = Counter()
_ports = Counter()


def log(kind, **fields):
    rec = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S"), "kind": kind, **fields}
    line = json.dumps(rec, separators=(",", ":"))
    with _log_lock:
        print(line, flush=True)
        if LOG_FILE:
            with open(LOG_FILE, "a") as f:
                f.write(line + "\n")


def detect_ip():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        s.connect(("192.168.1.1", 53))
        return s.getsockname()[0]
    finally:
        s.close()


FAKE_IP = os.environ.get("FAKE_IP") or detect_ip()

# ---------------------------------------------------------------- DNS

QTYPES = {1: "A", 28: "AAAA", 5: "CNAME", 12: "PTR", 15: "MX", 16: "TXT", 33: "SRV", 65: "HTTPS"}


def parse_qname(data, off):
    labels = []
    while True:
        n = data[off]
        off += 1
        if n == 0:
            return ".".join(labels), off
        if n & 0xC0:
            raise ValueError("compressed qname")
        labels.append(data[off:off + n].decode("ascii", "replace"))
        off += n


def dns_answer(data):
    """Return (response bytes, qname, qtype) for a DNS query."""
    qid, flags, qd = struct.unpack(">HHH", data[:6])
    if qd < 1:
        raise ValueError("no question")
    qname, off = parse_qname(data, 12)
    qtype, qclass = struct.unpack(">HH", data[off:off + 4])
    question = data[12:off + 4]
    rflags = 0x8180 | (flags & 0x0100)  # QR, RA, copy RD
    answers = b""
    if qtype == 1 and qclass == 1:
        answers = b"\xc0\x0c" + struct.pack(">HHIH", 1, 1, 60, 4) + socket.inet_aton(FAKE_IP)
    resp = struct.pack(">HHHHHH", qid, rflags, 1, 1 if answers else 0, 0, 0) + question + answers
    return resp, qname, QTYPES.get(qtype, str(qtype))


class DNSUDP(socketserver.BaseRequestHandler):
    def handle(self):
        data, sock = self.request
        try:
            resp, name, qtype = dns_answer(data)
        except Exception as e:
            log("dns-bad", src=self.client_address[0], err=str(e))
            return
        _names[name] += 1
        log("dns", src=self.client_address[0], name=name, type=qtype)
        sock.sendto(resp, self.client_address)


class DNSTCP(socketserver.BaseRequestHandler):
    def handle(self):
        s = self.request
        s.settimeout(5)
        try:
            (n,) = struct.unpack(">H", recv_exact(s, 2))
            resp, name, qtype = dns_answer(recv_exact(s, n))
        except Exception as e:
            log("dns-bad", src=self.client_address[0], proto="tcp", err=str(e))
            return
        _names[name] += 1
        log("dns", src=self.client_address[0], name=name, type=qtype, proto="tcp")
        s.sendall(struct.pack(">H", len(resp)) + resp)


def recv_exact(s, n):
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("short read")
        buf += chunk
    return buf

# ---------------------------------------------------------------- NTP

NTP_EPOCH = 2208988800


def ntp_ts(t):
    return struct.pack(">II", int(t) + NTP_EPOCH, int((t % 1) * 2**32))


class NTP(socketserver.BaseRequestHandler):
    def handle(self):
        data, sock = self.request
        if len(data) < 48:
            return
        rx = time.time()
        vn = (data[0] >> 3) & 7
        resp = bytes([(vn << 3) | 4, 2, data[2], 0xEC]) + b"\0" * 8 + b"LOCL"
        resp += ntp_ts(rx) + data[40:48] + ntp_ts(rx) + ntp_ts(time.time())
        log("ntp", src=self.client_address[0])
        sock.sendto(resp, self.client_address)

# ---------------------------------------------------------------- TCP sinks

# TLS 1.2 PSK cipher suites (RFC 4279/5487/5489/8442 ranges).
PSK_SUITES = [(0x008A, 0x0095), (0x00A8, 0x00B9), (0xC034, 0xC03B), (0xC064, 0xC06B),
              (0xC08E, 0xC09B), (0xC0A4, 0xC0AB), (0xCCAB, 0xCCAE), (0xD001, 0xD005)]


def parse_client_hello(b):
    """Pull SNI, ALPN, versions and PSK hints out of a TLS ClientHello."""
    info = {}
    if len(b) < 9 or b[0] != 0x16 or b[5] != 0x01:
        return None
    p = 9
    info["hello_version"] = "%d.%d" % (b[p], b[p + 1])
    p += 2 + 32
    p += 1 + b[p]
    (cl,) = struct.unpack(">H", b[p:p + 2])
    suites = [struct.unpack(">H", b[p + 2 + i:p + 4 + i])[0] for i in range(0, cl, 2)]
    p += 2 + cl
    info["suites"] = len(suites)
    psk = [hex(c) for c in suites if any(lo <= c <= hi for lo, hi in PSK_SUITES)]
    if psk:
        info["psk_suites"] = psk
    p += 1 + b[p]
    if p + 2 > len(b):
        return info
    (el,) = struct.unpack(">H", b[p:p + 2])
    p += 2
    end = min(len(b), p + el)
    while p + 4 <= end:
        et, ln = struct.unpack(">HH", b[p:p + 4])
        v = b[p + 4:p + 4 + ln]
        if et == 0 and len(v) > 5:
            info["sni"] = v[5:5 + struct.unpack(">H", v[3:5])[0]].decode("ascii", "replace")
        elif et == 16 and len(v) > 2:
            alpn, q = [], 2
            while q < len(v):
                alpn.append(v[q + 1:q + 1 + v[q]].decode("ascii", "replace"))
                q += 1 + v[q]
            info["alpn"] = alpn
        elif et == 43 and v:
            info["tls_versions"] = ["%d.%d" % (v[i], v[i + 1]) for i in range(1, 1 + v[0], 2)]
        elif et in (41, 45):
            info["tls13_psk"] = True
        p += 4 + ln
    return info


HTTP_METHODS = (b"GET ", b"POST ", b"PUT ", b"HEAD ", b"DELETE ", b"OPTIONS ", b"CONNECT ")


class Sink(socketserver.BaseRequestHandler):
    def handle(self):
        s = self.request
        port = s.getsockname()[1]
        src = self.client_address[0]
        _ports[port] += 1
        start = time.time()
        s.settimeout(5)
        try:
            first = s.recv(4096)
        except OSError:
            first = b""
        rec = {"src": src, "port": port, "bytes": len(first)}
        if not first:
            log("tcp", **rec, note="no data")
            return
        hello = parse_client_hello(first)
        if hello is not None:
            log("tls", **rec, **hello)
        elif first.startswith(HTTP_METHODS):
            head = first.split(b"\r\n\r\n", 1)[0].decode("latin-1").split("\r\n")
            host = next((h.split(":", 1)[1].strip() for h in head[1:] if h.lower().startswith("host:")), None)
            log("http", **rec, request=head[0], host=host)
            body = json.dumps({"success": True, "t": int(time.time())}).encode()
            s.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n"
                      b"Content-Length: %d\r\n\r\n%s" % (len(body), body))
            return
        else:
            log("tcp", **rec, hex=first[:64].hex())
        # Keep the socket open: a peer that only checks "connected" stays happy.
        s.settimeout(HOLD)
        try:
            while s.recv(4096):
                pass
        except OSError:
            pass
        log("close", src=src, port=port, secs=round(time.time() - start, 1))


class UDPLog(socketserver.BaseRequestHandler):
    def handle(self):
        data, sock = self.request
        _ports["udp/%d" % sock.getsockname()[1]] += 1
        log("udp", src=self.client_address[0], port=sock.getsockname()[1],
            bytes=len(data), hex=data[:64].hex())

# ---------------------------------------------------------------- main


class TUDP(socketserver.ThreadingUDPServer):
    allow_reuse_address = True
    daemon_threads = True


class TTCP(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


def serve(cls, port, handler):
    try:
        srv = cls((BIND, port), handler)
    except OSError as e:
        log("bind-fail", port=port, server=cls.__name__, err=str(e))
        return
    threading.Thread(target=srv.serve_forever, daemon=True).start()


def summary(*_):
    log("summary", names=dict(_names.most_common()), ports={str(k): v for k, v in _ports.items()})
    sys.exit(0)


def main():
    signal.signal(signal.SIGINT, summary)
    signal.signal(signal.SIGTERM, summary)
    serve(TUDP, 53, DNSUDP)
    serve(TTCP, 53, DNSTCP)
    serve(TUDP, 123, NTP)
    for p in TCP_PORTS:
        serve(TTCP, p, Sink)
    for p in UDP_PORTS:
        serve(TUDP, p, UDPLog)
    log("start", ip=FAKE_IP, bind=BIND, tcp=TCP_PORTS, udp=UDP_PORTS)
    while True:
        time.sleep(3600)


if __name__ == "__main__":
    main()
