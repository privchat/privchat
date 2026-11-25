#!/usr/bin/env python3
"""Collect real Alertmanager notifications in a disposable test directory."""
import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

events = Path(sys.argv[2])


class Receiver(BaseHTTPRequestHandler):
    def do_POST(self):
        try:
            size = int(self.headers["Content-Length"])
            if self.path != "/alert" or not 0 < size <= 65536:
                raise ValueError("invalid webhook request")
            payload = json.loads(self.rfile.read(size))
            if payload["receiver"] != "sd06-acceptance":
                raise ValueError("unexpected receiver")
            with events.open("a", encoding="utf-8") as output:
                for alert in payload["alerts"]:
                    labels = alert["labels"]
                    if labels["alertname"] == "PrivChatDependencyUnavailable" and labels["job"] == "privchat-sd06":
                        output.write(f"{labels['dependency']} {alert['status']} {alert['fingerprint']}\n")
        except (ValueError, TypeError, KeyError):
            self.send_error(400)
            return
        self.send_response(204)
        self.end_headers()


with HTTPServer(("127.0.0.1", int(sys.argv[1])), Receiver) as server:
    events.with_suffix(".ready").touch()
    server.serve_forever()
