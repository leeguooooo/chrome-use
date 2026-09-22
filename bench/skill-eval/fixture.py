#!/usr/bin/env python3
"""Loopback-only skill evaluation fixture with server-side outcome validation."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import threading
from urllib.parse import urlparse

PAGE = """<!doctype html><meta charset="utf-8"><title>Supply request</title>
<style>body{font:18px system-ui;max-width:700px;margin:30px auto}article{border:1px solid #aaa;padding:12px;margin:10px}label{display:block;margin:12px}button,input,select{font:inherit}output{display:block}</style>
<h1>Request office supplies</h1><p>This is a local test. No purchase or email is sent.</p>
<section id="products">
<article><h2>Notebook</h2><p>Price: $12.00. In stock.</p><button data-product="notebook">Choose Notebook</button></article>
<article><h2>Pencil</h2><p>Price: $4.00. Out of stock.</p><button disabled>Choose Pencil</button></article>
<article><h2>Folder</h2><p>Price: $9.00. In stock.</p><button data-product="folder">Choose Folder</button></article></section>
<section id="details" hidden><h2>Request details</h2><p id="choice"></p>
<form id="form"><label>Name <input name="name" required></label>
<label>Email <input name="email" type="email" required></label>
<label>Team <input name="team" required></label>
<label>Delivery <select name="delivery"><option value="express">Express</option><option value="economy">Economy</option></select></label>
<label><input name="agreed" type="checkbox" required> I confirm the request details</label>
<button type="submit">Submit request</button></form><output role="status" id="receipt"></output></section>
<script>
let product=null;
document.querySelectorAll('[data-product]').forEach(b=>b.onclick=()=>{product=b.dataset.product;document.querySelector('#products').hidden=true;document.querySelector('#details').hidden=false;document.querySelector('#choice').textContent='Selected: '+product;});
document.querySelector('#form').onsubmit=async e=>{e.preventDefault();const data=Object.fromEntries(new FormData(e.target));data.product=product;const r=await fetch(location.pathname+'/submit',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(data)});const v=await r.json();document.querySelector('#receipt').textContent=v.accepted?'Request saved. Receipt: '+v.receipt:'Request rejected: '+v.reason;};
</script>"""
EXPECTED = {
    "name": "Casey Sample",
    "email": "casey@example.test",
    "team": "Research",
    "delivery": "economy",
    "agreed": "on",
    "product": "folder",
}
RECORDS = {}
LOCK = threading.Lock()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def reply(self, value, status=200, html=False):
        body = value.encode() if html else json.dumps(value).encode()
        self.send_response(status)
        self.send_header(
            "Content-Type", "text/html; charset=utf-8" if html else "application/json"
        )
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        parts = urlparse(self.path).path.strip("/").split("/")
        if len(parts) == 2 and parts[0] == "case":
            self.reply(PAGE, html=True)
        elif len(parts) == 2 and parts[0] == "result":
            with LOCK:
                records = list(RECORDS.get(parts[1], []))
            # Application acceptance is not a browser-benchmark verdict.
            # The coordinator must separately review the recorded UI trace.
            self.reply(
                {
                    "submissions": len(records),
                    "accepted": len(records) == 1 and records[0] == EXPECTED,
                    "records": records,
                    "receipt": "fixture-" + parts[1],
                }
            )
        else:
            self.reply({"error": "not found"}, 404)

    def do_POST(self):
        parts = urlparse(self.path).path.strip("/").split("/")
        if len(parts) != 3 or parts[0] != "case" or parts[2] != "submit":
            return self.reply({"error": "not found"}, 404)
        value = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
        with LOCK:
            RECORDS.setdefault(parts[1], []).append(value)
        self.reply(
            {
                "accepted": value == EXPECTED,
                "receipt": "fixture-" + parts[1],
                "reason": "Check the selected product and request details.",
            }
        )


if __name__ == "__main__":
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    print(json.dumps({"port": server.server_port}), flush=True)
    server.serve_forever()
