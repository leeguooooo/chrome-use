#!/usr/bin/env python3
"""Loopback fixtures, including one intentionally slow load resource."""
from functools import partial
from http.server import ThreadingHTTPServer, SimpleHTTPRequestHandler
from pathlib import Path
import json
import time

class Handler(SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path.split('?', 1)[0] == '/slow-resource.svg':
            time.sleep(2)
            body = b'<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><rect width="1" height="1"/></svg>'
            self.send_response(200)
            self.send_header('Content-Type', 'image/svg+xml')
            self.send_header('Content-Length', str(len(body)))
            self.send_header('Cache-Control', 'no-store')
            self.end_headers()
            self.wfile.write(body)
        else:
            super().do_GET()

server = ThreadingHTTPServer(('127.0.0.1', 0), partial(Handler,
    directory=str(Path(__file__).resolve().parent / 'fixtures')))
print(json.dumps({'port': server.server_port}), flush=True)
server.serve_forever()
