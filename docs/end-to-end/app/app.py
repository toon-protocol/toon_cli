"""A payment-oblivious app for the end-to-end run: answers every request with 200.

`toon` starts an app's container with port 3100 published and waits for `GET /health`.
"""
from http.server import BaseHTTPRequestHandler, HTTPServer


class App(BaseHTTPRequestHandler):
    # The health check reads the status line and expects HTTP/1.1.
    protocol_version = "HTTP/1.1"

    def answer(self):
        length = int(self.headers.get("content-length") or 0)
        body = b'{"app":"e2e","path":"%s","received":%d}' % (self.path.encode(), len(self.rfile.read(length)))
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    do_GET = do_POST = answer


HTTPServer(("0.0.0.0", 3100), App).serve_forever()
