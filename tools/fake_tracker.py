#!/usr/bin/env python3
"""Fake brainsolpi for testing solmon without the tracker or the brainsolpi simulation.

Serves GET /api/history and /api/info in the same format as brainsolpi
(brainsolpi/brainsolpi/server.py:75-104, history.py:13). Standard library only.

It runs through every state the display can show, in a 60 s cycle:
   0- 3 s  empty history           -> LCD "no data yet"
   3-40 s  sunny, the panel sweeps -> angles change, LED bar mostly full
  40-50 s  a cloud passes          -> light, power and LED bar drop
  50-55 s  HTTP 503                -> after 3 failures: "brainSOL offline", red LED
  55-60 s  sunny again             -> recovers

Usage:
  python3 fake_tracker.py                 # listens on 0.0.0.0:8080
  python3 fake_tracker.py --port 8099
  python3 fake_tracker.py --phase sunny   # stay in one phase: empty|sunny|cloud|down
"""

import argparse
import json
import math
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

COLS = ["pan", "tilt", "lvl", "lux", "pmw", "off", "gain"]
START = time.monotonic()
FIXED_PHASE = None


def phase(t: float) -> str:
    if FIXED_PHASE:
        return FIXED_PHASE
    s = t % 60
    if s < 3:
        return "empty"
    if s < 40 or s >= 55:
        return "sunny"
    if s < 50:
        return "cloud"
    return "down"


def row(t: float) -> list:
    """One history row at time t (seconds since start)."""
    pan = 60 * math.sin(t / 20)  # slow sweep between -60 and 60 degrees
    tilt = 45 + 15 * math.sin(t / 13)  # 30..60 degrees
    lvl = 0.85 + 0.1 * math.sin(t / 3)  # bright, a little flicker
    if phase(t) == "cloud":
        s = (t % 60) - 40  # 0..10 s into the cloud: dip down and back up
        lvl *= 1 - 0.8 * math.sin(math.pi * s / 10)
    lvl = max(0.0, min(1.0, lvl))
    lux = 1100 * lvl
    pmw = 16.0 * lvl
    return [round(pan, 1), round(tilt, 1), round(lvl, 3), round(lux, 1), round(pmw, 2), 2.5, 1.48]


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        t = time.monotonic() - START
        p = phase(t)
        path = self.path.split("?")[0]

        if p == "down":
            return self.send(503, {"error": "simulated outage"})
        if path == "/api/info":
            return self.send(200, {"name": "fake", "version": "0", "proto": 1, "driver": "fake",
                                   "caps": {"lux": True, "pv": False, "simlight": False},
                                   "viewers": 0, "uptime": int(t), "brain": False})
        if path == "/api/history":
            rows = [] if p == "empty" else [row(t - 1), row(t)]
            return self.send(200, {"t0": int(time.time()) - len(rows), "dt": 1, "cols": COLS, "rows": rows})
        self.send(404, {"error": "not found"})

    def send(self, code: int, obj: dict):
        body = json.dumps(obj, separators=(",", ":")).encode()  # compact, like brainsolpi
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        t = time.monotonic() - START
        print(f"[{t:6.1f}s {phase(t):5}] {self.client_address[0]} {fmt % args}", flush=True)


def main():
    global FIXED_PHASE
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--host", default="0.0.0.0")
    ap.add_argument("--port", type=int, default=8080)
    ap.add_argument("--phase", choices=["empty", "sunny", "cloud", "down"])
    args = ap.parse_args()
    FIXED_PHASE = args.phase
    print(f"fake brainsolpi on http://{args.host}:{args.port}/api/history  (ctrl c to stop)", flush=True)
    ThreadingHTTPServer((args.host, args.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
