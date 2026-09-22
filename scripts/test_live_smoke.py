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
import time
import unittest
from unittest.mock import patch

import live_smoke


class SmokeTests(unittest.TestCase):
    def test_decision_configuration_is_explicit_and_bounded(self):
        args = live_smoke.arguments(["--decision-base-url", "https://tuara.com/router", "--decision-review"])
        decision = live_smoke.configuration(args).split("[decision]", 1)[1]
        for setting in ['mode="shadow"', 'backend="tuara"', 'protocol="systemone-v1"',
                        'model="jev-1.13.0"', 'api_key_env="TUARA_API_KEY"',
                        'deadline_ms=30000', 'max_attempts=1', 'max_decisions_per_task=16',
                        'review_enabled=true', 'runtime="local"', 'capability="worker"']:
            self.assertIn(setting, decision)
        self.assertNotIn("[decision]", live_smoke.configuration(live_smoke.arguments([])))

    def test_decision_options_validate_endpoint_and_environment_name(self):
        cases = [["--decision-review"],
                 ["--decision-base-url", "https://name:key@tuara.com/router"],
                 ["--decision-base-url", "http://example.com"],
                 ["--decision-base-url", "https://tuara.com/router?token=secret"],
                 ["--decision-base-url", "https://tuara.com/router", "--decision-api-key-env", "not-a-variable"]]
        for case in cases:
            with self.subTest(case=case), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                live_smoke.arguments(case)

    def test_local_abstentions_do_not_count_as_provider_success(self):
        local = {"purpose": "routing", "state": "succeeded", "provider_attempts": 0, "abstention": True}
        with self.assertRaisesRegex(RuntimeError, "no successful provider routing"):
            live_smoke.validate_decisions([local], [], False)
        routing = {**local, "provider_attempts": 1}
        with self.assertRaisesRegex(RuntimeError, "no successful provider final_evidence review"):
            live_smoke.validate_decisions([routing], [local], True)
        # Provider abstention is valid evidence that the end-to-end request worked.
        live_smoke.validate_decisions([routing, local], [{**routing, "kind": "final_evidence"}], True)

    def test_earlier_review_success_does_not_replace_final_provider_review(self):
        routing = {"purpose": "routing", "state": "succeeded", "provider_attempts": 1}
        earlier = {"kind": "plan", "state": "succeeded", "provider_attempts": 1}
        for final in [{"kind": "final_evidence", "state": "skipped", "provider_attempts": 0},
                      {"kind": "final_evidence", "state": "succeeded", "provider_attempts": 0, "abstention": True}]:
            with self.subTest(final=final), self.assertRaisesRegex(RuntimeError, "no successful provider final_evidence review"):
                live_smoke.validate_decisions([routing], [earlier, final], True)

    def test_preparation_loads_current_settings_without_credentials_or_models(self):
        cases = [[], ["claude"], ["tuara", "--model", "test-model"],
                 ["--decision-base-url", "https://tuara.com/router", "--decision-review"],
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
                            self.assertIn("[providers.smoke]", (root / "config/horde/config.toml").read_text())
                            self.assertNotIn("[providers", (root / "repo/.horde/horde.toml").read_text())
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
        self.run_mock_smoke()

    def test_mock_decisions_and_delayed_reviews_drain_and_retain_evidence(self):
        self.run_mock_smoke(decisions=True)

    def test_mock_decision_provider_failure_fails_smoke_and_retains_evidence(self):
        self.run_mock_smoke(decisions=True, provider_failure=True)

    def run_mock_smoke(self, decisions=False, provider_failure=False):
        requests = []
        decision_requests = []

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
                if self.path == "/v1/systemone":
                    decision_requests.append(request)
                    if provider_failure:
                        self.send_error(503)
                        return
                    # The final review must finish before the harness stops its daemon.
                    if request["state"].get("checkpoint", {}).get("kind") == "final_evidence":
                        time.sleep(1)
                    answers = {}
                    for key, question in request["questions"].items():
                        kind = question["type"]
                        if kind == "choice":
                            labels = list(question["criteria"])
                            choice = "local/worker" if "local/worker" in labels else "none"
                            answers[key] = {"type": kind, "choice": choice, "confidence": 1,
                                            "probabilities": {label: int(label == choice) for label in labels}}
                        elif kind == "score":
                            labels = question["criteria"]
                            legend = {str(index): label for index, label in enumerate(labels)}
                            answers[key] = {"type": kind, "legend": legend, "score": 0, "confidence": 1,
                                            "probabilities": {str(index): int(index == 0) for index in range(len(labels))}}
                        else:
                            answers[key] = {"type": kind, "noul": 0, "confidence": 1}
                    self.reply({"model": request["model"], "answers": answers})
                    return
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
            with patch.dict(os.environ, {"HORDE_SMOKE_API_KEY": "offline-dummy", "TUARA_API_KEY": "decision-dummy"}):
                args = live_smoke.arguments([
                    "local", "--model", "offline-model", "--base-url",
                    f"http://127.0.0.1:{server.server_port}/v1", "--timeout", "30",
                    *(["--decision-base-url", f"http://127.0.0.1:{server.server_port}", "--decision-review"] if decisions else []),
                ])
                output = io.StringIO()
                with contextlib.redirect_stdout(output):
                    try:
                        if provider_failure:
                            with self.assertRaisesRegex(RuntimeError, "decision.*failed"):
                                live_smoke.run(args)
                        else:
                            live_smoke.run(args)
                    finally:
                        prepared = next((json.loads(line) for line in output.getvalue().splitlines() if line.startswith('{"directory"')), None)
                        if prepared:
                            root = Path(prepared["directory"])
            self.assertEqual(len(requests), 4)
            self.assertTrue(all(r["model"] == "offline-model" for r in requests))
            self.assertTrue(all(any(t["function"]["name"] == "complete_step" for t in r["tools"]) for r in requests))
            self.assertEqual(json.loads((root / "result.json").read_text())["task"]["status"], "succeeded")
            for name in ["events.json", "metrics.json"]:
                json.loads((root / name).read_text())
            self.assertNotIn("offline-dummy", (root / "repo/.horde/horde.toml").read_text())
            if decisions:
                self.assertTrue(decision_requests)
                self.assertTrue(all(row["model"] == "jev-1.13.0" for row in decision_requests))
                rows = json.loads((root / "decisions.json").read_text())
                reviews = json.loads((root / "reviews.json").read_text())
                self.assertFalse(any(row["state"] in {"queued", "running"} for row in rows))
                json.loads((root / "summary.json").read_text())
                if provider_failure:
                    self.assertTrue(any(row["state"] == "failed" for row in rows))
                else:
                    self.assertTrue(any(row["purpose"] == "routing" and row["state"] == "succeeded" and row["provider_attempts"] > 0 for row in rows))
                    self.assertTrue(any(row["kind"] == "final_evidence" and row["state"] == "succeeded" and row["provider_attempts"] > 0 for row in reviews))
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)
            if root:
                shutil.rmtree(root)


if __name__ == "__main__":
    unittest.main()
