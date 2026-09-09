#!/usr/bin/env python3
"""Offline smoke-harness regressions. Build target/debug/horde first."""
import contextlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
import os
from pathlib import Path
import shutil
import tempfile
import threading
import unittest
from unittest.mock import patch

import live_smoke


class SmokeTests(unittest.TestCase):
    def test_preparation_loads_current_settings_without_credentials_or_models(self):
        cases = [[], ["claude"], ["tuara", "--model", "test-model"],
                 ["local", "--model", 'test-"model', "--base-url", "http://127.0.0.1:1/v1"]]
        with tempfile.TemporaryDirectory() as config:
            poison = Path(config) / "horde"
            poison.mkdir()
            (poison / "config.toml").write_text("not valid TOML")
            with patch.dict(os.environ, {"XDG_CONFIG_HOME": config}):
                for case in cases:
                    with self.subTest(case=case):
                        root = live_smoke.run(live_smoke.arguments([*case, "--prepare-only"]))
                        try:
                            self.assertFalse((root / "daemon.log").exists())
                            self.assertFalse((root / "data/daemon.sock").exists())
                            self.assertEqual((poison / "config.toml").read_text(), "not valid TOML")
                        finally:
                            shutil.rmtree(root)

    def test_local_requires_explicit_endpoint_and_model(self):
        for args in [["local"], ["local", "--model", "model"], ["tuara"],
                     ["codex", "--base-url", "http://localhost/v1"]]:
            with self.subTest(args=args), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit):
                    live_smoke.arguments(args)

    def test_mock_native_worker_commits_integrates_and_retains_evidence(self):
        requests = []

        class Provider(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def reply(self, data):
                body = json.dumps(data).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self):
                self.reply({"data": [{"id": "offline-model"}]})

            def do_POST(self):
                request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                requests.append(request)
                if len(requests) == 1:
                    name, args = "write_file", {"path": "hello.txt", "content": "hello\n"}
                elif len(requests) == 2:
                    name, args = "command", {"argv": ["git", "add", "hello.txt"]}
                elif len(requests) == 3:
                    name, args = "command", {"argv": ["git", "commit", "-m", "Add hello"]}
                else:
                    name, args = "complete_step", {"accepted": True, "result": "created and committed", "artifacts": ["hello.txt"]}
                self.reply({"choices": [{"message": {"role": "assistant", "content": "", "tool_calls": [{"id": str(len(requests)), "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}]}}]})

        server = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        root = None
        try:
            with patch.dict(os.environ, {"HORDE_SMOKE_API_KEY": "offline-dummy"}):
                root = live_smoke.run(live_smoke.arguments([
                    "local", "--model", "offline-model", "--base-url",
                    f"http://127.0.0.1:{server.server_port}/v1", "--timeout", "30",
                ]))
            self.assertEqual(len(requests), 4)
            self.assertTrue(all(r["model"] == "offline-model" for r in requests))
            self.assertTrue(all(any(t["function"]["name"] == "complete_step" for t in r["tools"]) for r in requests))
            self.assertEqual(json.loads((root / "result.json").read_text())["task"]["status"], "succeeded")
            for name in ["events.json", "metrics.json"]:
                json.loads((root / name).read_text())
            self.assertNotIn("offline-dummy", (root / "repo/.horde/horde.toml").read_text())
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
            if root:
                shutil.rmtree(root)


if __name__ == "__main__":
    unittest.main()
