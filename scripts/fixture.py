"""Deterministic loopback HTTP origin shared by integration tests and benchmarks.

No external Python packages. Each ranged connection can have its own bandwidth
cap, deliberately modeling servers with per-connection limits, not the internet.
"""
import hashlib
import http.server
import socket
import threading
import time
from urllib.parse import parse_qs, urlsplit


class Origin(http.server.ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True
    request_queue_size = 128

    def __init__(self):
        super().__init__(("127.0.0.1", 0), Handler)
        self.hits = {}
        self.guard = threading.Lock()
        self.version = 1

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server_port}"


def payload(size):
    block = bytes(range(256)) * 256
    return (block * (size // len(block) + 1))[:size]


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def handle(self):
        # Deliberate cancellation tests also close keep-alive sockets while idle.
        try:
            super().handle()
        except (ConnectionResetError, ConnectionAbortedError, BrokenPipeError):
            pass

    def log_message(self, *_):
        pass

    def do_GET(self):
        try:
            self.serve()
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError, OSError):
            pass

    def serve(self):
        parsed = urlsplit(self.path)
        query = parse_qs(parsed.query)
        size = int(query.get("size", [2 * 1024 * 1024])[0])
        cap = float(query.get("cap", [0])[0]) * 1024 * 1024
        latency = float(query.get("latency", [0])[0])
        route = parsed.path
        version = self.server.version
        tag = f'"fixture-{version}-{size}"'
        if route == "/redirect":
            self.send_response(302)
            self.send_header("Location", "/file.bin" + ("?" + parsed.query if parsed.query else ""))
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if route == "/crossredirect":
            self.send_response(302)
            self.send_header("Location", self.server.url.replace("127.0.0.1", "localhost") + "/file.bin")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if route in ("/page", "/ambiguous"):
            body = b'<html><a href="/file.bin" download>Download</a>'
            if route == "/ambiguous":
                body += b'<a href="/other.zip">Other</a>'
            body += b"</html>"
            self.send_response(200)
            self.send_header("Content-Type", "text/html")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if route == "/encoded":
            self.send_response(200)
            self.send_header("Content-Encoding", "gzip")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if self.headers.get("If-Match") not in (None, tag):
            self.send_response(412)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        span = self.headers.get("Range")
        start, end = 0, size - 1
        ranged = span is not None and route not in ("/norange", "/unknown")
        if ranged:
            start, end = map(int, span.removeprefix("bytes=").split("-"))
            end = min(end, size - 1)
            if start >= size:
                self.send_response(416)
                self.send_header("Content-Range", f"bytes */{size}")
                self.send_header("Content-Length", "0")
                self.end_headers()
                return
        key = (self.path, span)
        with self.server.guard:
            self.server.hits[key] = self.server.hits.get(key, 0) + 1
            attempt = self.server.hits[key]
        # Probes always succeed; actual chunks can experience failures.
        actual_chunk = ranged and end > start
        if actual_chunk and route == "/rate" and attempt == 1:
            self.send_response(429)
            self.send_header("Retry-After", "1")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if route == "/missing":
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if latency and actual_chunk:
            time.sleep(latency)
        self.send_response(206 if ranged else 200)
        if route != "/noetag":
            self.send_header("ETag", 'W/' + tag if route == "/weak" else tag)
        self.send_header("Content-Type", "application/octet-stream")
        length = max(0, end - start + 1)
        if route == "/unknown":
            self.send_header("Connection", "close")
            self.close_connection = True
        else:
            self.send_header("Content-Length", str(length))
        if ranged:
            reported_start = start + 1 if actual_chunk and route == "/badrange" else start
            self.send_header("Content-Range", f"bytes {reported_start}-{end}/{size}")
        self.end_headers()
        if actual_chunk and route in ("/truncate", "/always-truncate") and (attempt == 1 or route == "/always-truncate"):
            data = payload(min(length, 1024))
            self.wfile.write(data)
            self.wfile.flush()
            self.close_connection = True
            self.connection.shutdown(socket.SHUT_RDWR)
            return
        data = bytes(range(256)) * 256
        sent = 0
        began = time.perf_counter()
        while sent < length:
            count = min(64 * 1024, length - sent)
            offset = (start + sent) % len(data)
            block = (data + data)[offset:offset + count]
            if version != 1:
                block = bytes(b ^ 1 for b in block)
            self.wfile.write(block)
            self.wfile.flush()
            sent += count
            if cap:
                time.sleep(max(0, sent / cap - (time.perf_counter() - began)))


def start():
    origin = Origin()
    threading.Thread(target=origin.serve_forever, daemon=True).start()
    return origin


if __name__ == "__main__":
    server = start()
    print(server.url, flush=True)
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        server.shutdown()
