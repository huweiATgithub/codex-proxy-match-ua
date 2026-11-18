"""Run an official host with disposable PostgreSQL, Redis and a recording model upstream."""

from contextlib import ExitStack, contextmanager
from functools import partial
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import threading
import time
from urllib.error import HTTPError, URLError
from urllib.request import ProxyHandler, Request, build_opener


def wait(check, *, timeout=45):
    deadline = time.monotonic() + timeout
    while True:
        try:
            return check()
        except (AssertionError, URLError, ConnectionError) as error:
            if time.monotonic() >= deadline:
                raise TimeoutError(str(error) or "fixture condition did not become true") from error
            time.sleep(0.2)


def docker(*args):
    return subprocess.check_output(["docker", *args], text=True, timeout=120).strip()


def database(stack, image, port, args, command=()):
    container = docker("run", "-d", "--rm", "-p", f"127.0.0.1::{port}", *args, image, *command)
    stack.callback(docker, "stop", "--time", "2", container)
    mapping = docker("port", container, f"{port}/tcp")
    address, host_port = mapping.split(":")
    assert address == "127.0.0.1"
    return container, int(host_port)


class Upstream(BaseHTTPRequestHandler):
    def __init__(self, *args, records, **kwargs):
        self.records = records
        super().__init__(*args, **kwargs)

    def log_message(self, *_):
        pass

    def reply(self, kind, payload):
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        self.reply("application/json", b'{"data":[]}')

    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", "0")))
        if not self.path.endswith("/responses"):
            self.do_GET()
            return
        self.records.append(self.headers.get("User-Agent"))
        item = {"id": "msg_test", "type": "message", "role": "assistant", "status": "completed",
                "content": [{"type": "output_text", "text": "OK", "annotations": []}]}
        result = {"id": f"resp_test_{len(self.records)}", "object": "response", "created_at": 1700000000,
                  "status": "completed", "model": "gpt-5.4", "output": [item],
                  "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}}
        events = [
            {"type": "response.created", "response": {**result, "status": "in_progress", "output": []}},
            {"type": "response.output_item.added", "output_index": 0, "item": item},
            {"type": "response.output_text.delta", "response_id": result["id"], "item_id": item["id"],
             "output_index": 0, "content_index": 0, "delta": "OK"},
            {"type": "response.output_item.done", "output_index": 0, "item": item},
            {"type": "response.completed", "response": result},
        ]
        frames = [f"event: {e['type']}\ndata: {json.dumps({**e, 'sequence_number': n})}\n\n"
                  for n, e in enumerate(events)]
        self.reply("text/event-stream", ("".join(frames) + "data: [DONE]\n\n").encode())


class Host:
    def __init__(self, url, directory, upstream, records):
        self.url = url
        self.directory = directory
        self.upstream = upstream
        self.records = records
        self.cookie = ""
        self.client = build_opener(ProxyHandler({}))

    def request(self, path, body=None, headers=None):
        extra = {"Origin": self.url, **(headers or {})}
        if self.cookie and path.startswith("/api/"):
            extra["Cookie"] = self.cookie
        if isinstance(body, bytes):
            extra["Content-Type"] = "application/gzip"
        elif body is not None:
            extra["Content-Type"] = "application/json"
            body = json.dumps(body).encode()
        try:
            response = self.client.open(Request(self.url + path, body, extra), timeout=12)
        except HTTPError as error:
            response = error
        with response:
            return response.status, response.headers, json.load(response)

    def admin(self, path, body=None):
        status, _, result = self.request(path, body)
        if status not in (200, 201) or result.get("code", 200) != 200:
            raise RuntimeError(f"{path} returned {status}: {result.get('message', '')}")
        return result.get("data", result)


@contextmanager
def run_host(release):
    release = Path(release).resolve()
    with ExitStack() as stack:
        scratch = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="match-ua-host-")))
        secret = secrets.token_hex(24)
        pg, pg_port = database(stack, "postgres:17-alpine", 5432, [
            "-e", "POSTGRES_USER=ua_test", "-e", "POSTGRES_DB=ua_test", "-e", f"POSTGRES_PASSWORD={secret}"])
        _, redis_port = database(stack, "redis:7-alpine", 6379, [], ["redis-server", "--requirepass", secret])

        def ready_pg():
            probe = subprocess.run(["docker", "exec", pg, "pg_isready", "-h", "127.0.0.1",
                                    "-U", "ua_test", "-d", "ua_test"], capture_output=True)
            assert probe.returncode == 0, "PostgreSQL TCP listener is starting"
        wait(ready_pg)

        records = []
        server = ThreadingHTTPServer(("127.0.0.1", 0), partial(Upstream, records=records))
        stack.callback(server.server_close)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        stack.callback(server.shutdown)
        upstream = f"http://127.0.0.1:{server.server_port}/v1"
        with socket.socket() as port_finder:
            port_finder.bind(("127.0.0.1", 0))
            port = port_finder.getsockname()[1]
        config = {
            "schema_version": 1,
            "host": {"timezone": "UTC", "listen": {"host": "127.0.0.1", "port": port},
                     "runtime_data_dir": "../runtime", "logging": {"level": "info", "stdout": True,
                     "file": {"enabled": False, "directory": "../logs", "retention_days": 1, "max_file_size_mb": 20}}},
            "store": {"database": {"url": f"postgres://ua_test@127.0.0.1:{pg_port}/ua_test", "password": secret},
                      "redis": {"url": f"redis://127.0.0.1:{redis_port}/", "password": secret}},
            "admin": {"default_username": "admin@cpr.local", "default_password": secret,
                      "session_ttl_minutes": 10080, "session_absolute_ttl_minutes": 43200},
            "api": {"asset_directory": str(release / "web/dist"), "cors_allowed_origins": [],
                    "request_timeout_seconds": None, "request_id_header": "x-request-id"},
            "openai": {"api": {"base_url": upstream}, "stream_max_retries": 0},
        }
        (scratch / "deploy").mkdir()
        (scratch / "deploy/config.yaml").write_text(json.dumps(config))
        # The child only needs local databases and the loopback upstream.
        env = {k: os.environ[k] for k in ("PATH", "HOME", "LANG", "TMPDIR") if k in os.environ}
        env.update(HTTP_PROXY="http://127.0.0.1:9", HTTPS_PROXY="http://127.0.0.1:9", NO_PROXY="127.0.0.1,localhost")
        log_path = scratch / "host.log"
        log = stack.enter_context(log_path.open("wb"))
        process = subprocess.Popen([str(release / "codex-proxy-rs"), "serve"], cwd=scratch,
                                   env=env, stdout=log, stderr=subprocess.STDOUT)

        def stop_host():
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        stack.callback(stop_host)
        host = Host(f"http://127.0.0.1:{port}", scratch, upstream, records)

        def ready_host():
            if process.poll() is not None:
                tail = log_path.read_text(errors="replace")[-1400:].replace(secret, "[redacted]")
                raise RuntimeError(f"host exited during startup: {tail}")
            host.admin("/api/auth/status")
        wait(ready_host)
        status, headers, _ = host.request("/api/auth/login", {"mode": "admin", "password": secret})
        assert status == 200, "fixture administrator login failed"
        host.cookie = headers["Set-Cookie"].split(";", 1)[0]
        yield host
