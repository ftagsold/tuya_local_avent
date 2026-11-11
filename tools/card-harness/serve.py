"""Static server for the card harness: serves only the harness page, its
script and the live card file (re-read on every request, so a browser reload
picks up edits). Bound to 127.0.0.1: localhost is a secure context, so the
browser grants the microphone for talk.

Usage: python3 -I serve.py [port]
"""

import http.server
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
FILES = {
    "/": (HERE / "index.html", "text/html; charset=utf-8"),
    "/index.html": (HERE / "index.html", "text/html; charset=utf-8"),
    "/harness.js": (HERE / "harness.js", "text/javascript; charset=utf-8"),
    "/tuya-local-avent-card.js": (HERE.parent.parent / "card" / "tuya-local-avent-card.js", "text/javascript; charset=utf-8"),
}


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        entry = FILES.get(self.path.split("?")[0])
        if not entry:
            self.send_error(404)
            return
        body = entry[0].read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", entry[1])
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        pass


port = int(sys.argv[1]) if len(sys.argv) > 1 else 8125
print(f"card harness: http://localhost:{port}/", flush=True)
http.server.ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
