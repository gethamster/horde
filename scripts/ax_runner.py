#!/usr/bin/env python3
"""Horde's specialized runner for AX's private, trusted-fleet Gateway.

AX prepares no repositories or model sessions here. Horde owns that work after
its existing bootstrap packet arrives. HTTP readiness means the durable workspace
is available; only Horde's authenticated heartbeat makes the worker schedulable.
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import time
from urllib.parse import urlsplit

MAX_PACKET = 128 * 1024


class Conflict(Exception):
    """A retry attempted to replace an existing worker's bootstrap packet."""


class Stopping(Exception):
    """The worker is draining and cannot accept bootstrap requests."""


class Worker:
    def __init__(self, workspace, environ, popen=subprocess.Popen):
        self.workspace = Path(workspace)
        self.identity = environ.get("HORDE_AX_RUNTIME_ID")
        self.project = environ.get("HORDE_AX_PROJECT_ID")
        if not self.identity or not self.project:
            raise ValueError("AX runtime and project identity are required")
        self.base = self.workspace / ".horde"
        self.base.mkdir(parents=True, exist_ok=True, mode=0o700)
        os.chmod(self.base, 0o700)
        self.bootstrap = self.base / "bootstrap.json"
        self.child = None
        self.popen = popen
        self.lock = threading.Lock()
        self.stopping = False
        self.reported_exit = False
        self.restart_at = None

    def validate(self, raw):
        if len(raw) > MAX_PACKET:
            raise ValueError("bootstrap exceeds size limit")
        value = json.loads(raw)
        if not isinstance(value, dict) or value.get("id") != self.identity:
            raise ValueError("bootstrap runtime mismatch")
        if not isinstance(value.get("project"), dict) or value["project"].get("id") != self.project:
            raise ValueError("bootstrap project mismatch")
        if not isinstance(value.get("network"), dict):
            raise ValueError("bootstrap network is required")
        for key in ("ca", "certificate", "key"):
            if not isinstance(value.get(key), str) or not value[key]:
                raise ValueError("bootstrap credentials are required")
        return value

    def save(self, raw):
        descriptor, name = tempfile.mkstemp(prefix=".bootstrap-", dir=self.base)
        try:
            with os.fdopen(descriptor, "wb") as stream:
                stream.write(raw)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(name, self.bootstrap)
            directory = os.open(self.base, os.O_RDONLY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            if os.path.exists(name):
                os.unlink(name)

    def launch(self, raw):
        if self.child is not None:
            return
        locations = {"HOME": "home", "XDG_CONFIG_HOME": "config", "XDG_CACHE_HOME": "cache"}
        env = {"PATH": "/usr/local/bin:/usr/bin:/bin", "LANG": "C.UTF-8",
               "HORDE_SUPERVISED": "1", "HORDE_ISOLATION": "ax-gvisor",
               "HORDE_BOOTSTRAP_JSON": raw.decode("utf-8"),
               "AX_METADATA_URL": "http://127.0.0.1:80"}
        for key, directory in locations.items():
            path = self.base / directory
            path.mkdir(exist_ok=True, mode=0o700)
            env[key] = str(path)
        self.child = self.popen(
            ["/usr/local/bin/horde", "--data-dir", str(self.base / "state"), "daemon"],
            cwd=str(self.workspace), env=env, stdin=subprocess.DEVNULL,
            start_new_session=True,
        )

    def accept(self, raw):
        value = self.validate(raw)
        with self.lock:
            if self.stopping:
                raise Stopping()
            existed = self.bootstrap.exists()
            if existed:
                stored = self.bootstrap.read_bytes()
                if self.validate(stored) != value:
                    raise Conflict()
                raw = stored
            else:
                self.save(raw)
            self.launch(raw)
            return 200 if existed else 202

    def restore(self):
        if self.bootstrap.exists():
            self.accept(self.bootstrap.read_bytes())

    def observe_exit(self):
        with self.lock:
            if self.stopping or self.child is None:
                return
            code = self.child.poll()
            if code is None:
                return
            if not self.reported_exit:
                print(f"Horde daemon exited with status {code}", flush=True)
                self.reported_exit = True
                # Match Horde's existing sandbox supervisor. Restarting the
                # daemon does not replay workflows; durable recovery decides
                # which attempts are uncertain before scheduling more work.
                self.restart_at = time.monotonic() + 2 if code != 0 else None
            if self.restart_at is not None and time.monotonic() >= self.restart_at:
                previous = self.child
                self.terminate_group(previous, 0)
                self.child = None
                try:
                    raw = self.bootstrap.read_bytes()
                    self.validate(raw)
                    self.launch(raw)
                except (OSError, ValueError):
                    self.child = previous
                    self.restart_at = time.monotonic() + 2
                    print("Horde daemon restart failed; retrying after backoff", flush=True)
                else:
                    self.reported_exit = False
                    self.restart_at = None

    def stop(self, grace=10):
        with self.lock:
            self.stopping = True
            child = self.child
        if child is None:
            return
        self.terminate_group(child, grace)

    @staticmethod
    def terminate_group(child, grace):
        try:
            os.killpg(child.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            child.wait(timeout=grace)
        except subprocess.TimeoutExpired:
            pass
        # Descendants may still be alive after the group leader has exited.
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()


def handler_for(worker):
    class Handler(http.server.BaseHTTPRequestHandler):
        def setup(self):
            super().setup()
            self.connection.settimeout(15)

        def log_message(self, *_args):
            pass  # Never record caller-controlled request paths or bootstrap data.

        def respond(self, status):
            body = json.dumps({"ok": 200 <= status < 300}).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if urlsplit(self.path).path not in ("/healthz", "/readyz"):
                self.respond(404)
            else:
                self.respond(503 if worker.stopping else 200)

        def do_POST(self):
            if self.path != "/bootstrap":
                self.respond(404)
                return
            try:
                if self.headers.get("Transfer-Encoding"):
                    raise ValueError("chunked bootstrap is unsupported")
                size = int(self.headers.get("Content-Length", "0"))
                if size > MAX_PACKET:
                    self.respond(413)
                    return
                if size <= 0:
                    raise ValueError("empty bootstrap")
                raw = self.rfile.read(size)
                if len(raw) != size:
                    raise ValueError("truncated bootstrap")
                self.respond(worker.accept(raw))
            except Conflict:
                self.respond(409)
            except Stopping:
                self.respond(503)
            except (ValueError, UnicodeError):
                self.respond(400)
            except OSError:
                self.respond(503)
    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=80)
    parser.add_argument("--workspace", type=Path, default=Path("/workspace"))
    args = parser.parse_args()
    os.umask(0o077)
    worker = Worker(args.workspace, os.environ)
    worker.restore()
    server = http.server.ThreadingHTTPServer(("0.0.0.0", args.port), handler_for(worker))
    stopped = threading.Event()
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, lambda *_args: stopped.set())
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    while not stopped.wait(0.5):
        worker.observe_exit()
    worker.stop()
    server.shutdown()
    server.server_close()
    thread.join()


if __name__ == "__main__":
    main()
