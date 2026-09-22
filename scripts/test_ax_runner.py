"""Offline tests for the AX runner's persistent bootstrap and supervision."""
import importlib.util
import http.client
import http.server
import threading
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("ax_runner", Path(__file__).with_name("ax_runner.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


def packet(**updates):
    return dict(id="worker-1", project={"id": "project-1"}, network={},
                ca="certificate", certificate="certificate", key="private-test-key", **updates)


class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.spawn = Mock(return_value=Mock(pid=123, poll=Mock(return_value=None)))
        self.env = {"HORDE_AX_RUNTIME_ID": "worker-1", "HORDE_AX_PROJECT_ID": "project-1"}
        self.worker = runner.Worker(self.root, self.env, popen=self.spawn)

    def accept(self, value=None):
        return self.worker.accept(json.dumps(packet() if value is None else value).encode())

    def test_bootstrap_private_persistent_and_idempotent(self):
        self.assertEqual(self.accept(), 202)
        self.assertEqual(self.accept(), 200)
        self.assertEqual(self.spawn.call_count, 1)
        self.assertEqual(self.worker.bootstrap.stat().st_mode & 0o777, 0o600)
        restored = runner.Worker(self.root, self.env, popen=self.spawn)
        restored.restore()
        self.assertEqual(self.spawn.call_count, 2)
        self.assertEqual(restored.accept(json.dumps(packet()).encode()), 200)

    def test_conflicting_packet_and_wrong_ownership_rejected(self):
        self.accept()
        changed = packet()
        changed["key"] = "different"
        with self.assertRaises(runner.Conflict):
            self.accept(changed)
        for field in ("id", "project"):
            changed = packet()
            changed[field] = "wrong" if field == "id" else {"id": "wrong"}
            with self.assertRaises(ValueError):
                self.accept(changed)
        self.assertEqual(self.spawn.call_count, 1)

    def test_invalid_or_oversized_packet_never_starts_child(self):
        for raw in (b"[]", b"{}", b"{", b"x" * (128 * 1024 + 1)):
            with self.assertRaises(ValueError):
                self.worker.accept(raw)
        self.spawn.assert_not_called()
        self.assertFalse(self.worker.bootstrap.exists())

    def test_child_environment_is_project_local_without_ambient_credentials(self):
        with patch.dict(os.environ, {"OPENAI_API_KEY": "ambient", "GEMINI_API_KEY": "ambient", "AX_TASK_YAML": "secret"}):
            self.accept()
        kwargs = self.spawn.call_args.kwargs
        env = kwargs["env"]
        for key in ("OPENAI_API_KEY", "GEMINI_API_KEY", "AX_TASK_YAML"):
            self.assertNotIn(key, env)
        for key in ("HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME"):
            self.assertTrue(env[key].startswith(str(self.root)))
        self.assertEqual(env["HORDE_ISOLATION"], "ax-gvisor")
        self.assertEqual(json.loads(env["HORDE_BOOTSTRAP_JSON"]), packet())
        self.assertTrue(kwargs["start_new_session"])
        self.assertEqual(kwargs["cwd"], str(self.root))

    def test_failed_launch_can_retry_without_replacing_packet(self):
        self.spawn.side_effect = [OSError("spawn failed"), Mock(pid=123)]
        with self.assertRaises(OSError):
            self.accept()
        self.assertEqual(self.accept(), 200)
        self.assertEqual(self.spawn.call_count, 2)

    def test_child_exit_does_not_replay_on_bootstrap_retry(self):
        self.accept()
        self.worker.child.poll.return_value = 42
        self.assertEqual(self.accept(), 200)
        self.assertEqual(self.spawn.call_count, 1)

    def test_concurrent_retries_launch_once(self):
        barrier = threading.Barrier(4)
        statuses = []
        def deliver():
            barrier.wait()
            statuses.append(self.accept())
        threads = [threading.Thread(target=deliver) for _ in range(4)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        self.assertEqual(sorted(statuses), [200, 200, 200, 202])
        self.assertEqual(self.spawn.call_count, 1)

    def test_exit_status_is_reported_once_without_replay(self):
        self.accept()
        self.worker.child.poll.return_value = 23
        with patch("builtins.print") as output:
            self.worker.observe_exit()
            self.worker.observe_exit()
        output.assert_called_once_with("Horde daemon exited with status 23", flush=True)
        self.assertEqual(self.spawn.call_count, 1)

    def test_shutdown_signals_group_and_escalates_after_grace(self):
        self.accept()
        self.worker.child.wait.side_effect = [subprocess.TimeoutExpired("horde", 10), 0]
        with patch.object(runner.os, "killpg") as kill:
            self.worker.stop()
        self.assertEqual(kill.call_args_list[0].args, (123, signal.SIGTERM))
        self.assertEqual(kill.call_args_list[1].args, (123, signal.SIGKILL))
        with self.assertRaises(runner.Stopping):
            self.accept()

    def test_shutdown_kills_descendants_even_after_leader_exits(self):
        self.accept()
        self.worker.child.poll.return_value = 0
        with patch.object(runner.os, "killpg") as kill:
            self.worker.stop()
        self.assertIn(((123, signal.SIGTERM),), [tuple(call)[:1] for call in kill.call_args_list])

    def test_restored_packet_ownership_is_checked(self):
        self.accept()
        wrong = dict(self.env, HORDE_AX_PROJECT_ID="project-2")
        restored = runner.Worker(self.root, wrong, popen=self.spawn)
        with self.assertRaises(ValueError):
            restored.restore()
        self.assertEqual(self.spawn.call_count, 1)

    def test_nonzero_daemon_exit_restarts_after_backoff_using_persistent_state(self):
        self.accept()
        first = self.worker.child
        first.poll.return_value = 1  # runtime_restart deliberately exits nonzero.
        second = Mock(pid=124, poll=Mock(return_value=None))
        self.spawn.return_value = second
        with patch.object(runner.time, "monotonic", return_value=100), patch("builtins.print"):
            self.worker.observe_exit()
        self.assertEqual(self.spawn.call_count, 1)
        with patch.object(runner.time, "monotonic", return_value=103), patch.object(runner.os, "killpg"), patch("builtins.print"):
            self.worker.observe_exit()
        self.assertEqual(self.spawn.call_count, 2)
        self.assertIs(self.worker.child, second)
        self.assertEqual(json.loads(self.spawn.call_args.kwargs["env"]["HORDE_BOOTSTRAP_JSON"]), packet())

    def test_clean_daemon_exit_does_not_restart(self):
        self.accept()
        self.worker.child.poll.return_value = 0
        with patch.object(runner.time, "monotonic", side_effect=[100, 200]), patch("builtins.print"):
            self.worker.observe_exit()
            self.worker.observe_exit()
        self.assertEqual(self.spawn.call_count, 1)

    def test_runner_shutdown_cancels_pending_daemon_restart(self):
        self.accept()
        self.worker.child.poll.return_value = 1
        with patch.object(runner.time, "monotonic", return_value=100), patch("builtins.print"):
            self.worker.observe_exit()
        with patch.object(runner.os, "killpg"):
            self.worker.stop()
        with patch.object(runner.time, "monotonic", return_value=200), patch("builtins.print"):
            self.worker.observe_exit()
        self.assertEqual(self.spawn.call_count, 1)

    def test_real_child_ignoring_term_is_killed_after_grace(self):
        ready = self.root / "ready"
        code = "import pathlib,signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); pathlib.Path('ready').touch(); time.sleep(60)"
        def spawn(_command, **kwargs):
            return subprocess.Popen([sys.executable, "-c", code], **kwargs)
        worker = runner.Worker(self.root, self.env, popen=spawn)
        worker.accept(json.dumps(packet()).encode())
        self.addCleanup(worker.stop, 0.05)
        deadline = time.monotonic() + 5
        while not ready.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue(ready.exists())
        start = time.monotonic()
        worker.stop(grace=0.05)
        self.assertLess(time.monotonic() - start, 2)
        self.assertEqual(worker.child.returncode, -signal.SIGKILL)

    def test_missing_expected_identity_is_rejected(self):
        with self.assertRaises(ValueError):
            runner.Worker(self.root, {}, popen=self.spawn)


class HttpTests(unittest.TestCase):
    def setUp(self):
        RunnerTests.setUp(self)
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), runner.handler_for(self.worker))
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.addCleanup(self.close)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    def request(self, method, path, body=None, headers=None):
        connection = http.client.HTTPConnection(*self.server.server_address)
        try:
            connection.request(method, path, body, headers or {})
            response = connection.getresponse()
            return response.status, response.read()
        finally:
            connection.close()

    def test_ready_before_credentials_and_responses_never_echo_packet(self):
        self.assertEqual(self.request("GET", "/readyz")[0], 200)
        payload = json.dumps(packet()).encode()
        self.assertEqual(self.request("POST", "/bootstrap", payload), (202, b'{"ok": true}'))
        self.assertEqual(self.request("POST", "/bootstrap", payload), (200, b'{"ok": true}'))
        changed = packet()
        changed["key"] = "another-secret"
        self.assertEqual(self.request("POST", "/bootstrap", json.dumps(changed)), (409, b'{"ok": false}'))
        self.assertEqual(self.request("GET", "/bootstrap")[0], 404)
        self.assertEqual(self.request("GET", "/metadata/v1alpha1/ax/task")[0], 404)

    def test_stock_ax_workspace_readiness_query(self):
        self.assertEqual(self.request("GET", "/readyz?check=workspace")[0], 200)
        self.worker.stopping = True
        self.assertEqual(self.request("GET", "/readyz?check=workspace")[0], 503)

    def test_http_rejects_oversized_empty_and_chunked_requests(self):
        # Rejection happens from Content-Length, before reading the body. Sending
        # that body concurrently races the server closing the rejected request.
        self.assertEqual(self.request("POST", "/bootstrap", headers={
            "Content-Length": str(runner.MAX_PACKET + 1),
        })[0], 413)
        self.assertEqual(self.request("POST", "/bootstrap", b"")[0], 400)
        self.assertEqual(self.request("POST", "/bootstrap", b"{}", {"Transfer-Encoding": "chunked"})[0], 400)
        self.spawn.assert_not_called()


if __name__ == "__main__":
    unittest.main()
