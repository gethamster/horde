"""Offline AX Docker preparation and supervision contracts."""
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("ax_docker", Path(__file__).with_name("ax_docker.py"))
docker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(docker)


class DockerTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.service = docker.DockerService(Path(self.directory.name))

    def test_client_environment_excludes_ambient_credentials_and_context(self):
        with patch.dict(os.environ, {"OPENAI_API_KEY": "secret", "DOCKER_HOST": "tcp://outside", "DOCKER_CONTEXT": "outside"}):
            env = self.service.environment()
        self.assertEqual(env["DOCKER_HOST"], "unix:///var/run/docker.sock")
        self.assertNotIn("OPENAI_API_KEY", env)
        self.assertNotIn("DOCKER_CONTEXT", env)
        self.assertIn("/usr/sbin", env["PATH"].split(":"))
        self.assertIn("/sbin", env["PATH"].split(":"))

    def test_daemon_uses_owned_storage_socket_and_classic_vfs(self):
        args = self.service.daemon_args(1500)
        self.assertIn("--host=unix:///var/run/docker.sock", args)
        self.assertIn("--exec-root=/run/horde-docker", args)
        self.assertIn("--data-root=" + str(self.service.base / "data"), args)
        self.assertIn("--feature=containerd-snapshotter=false", args)
        self.assertIn("--storage-driver=vfs", args)
        self.assertIn("--mtu=1500", args)
        self.assertNotIn("--debug", args)

    def test_start_is_once_and_nonblocking(self):
        gate = threading.Event()
        with patch.object(self.service, "prepare", side_effect=lambda: gate.wait(2)) as prepare:
            self.service.start()
            self.service.start()
            self.assertEqual(self.service.state, "starting")
            gate.set()
            self.service.thread.join(3)
        prepare.assert_called_once()

    def test_startup_failure_is_visible_and_not_retried_implicitly(self):
        with patch.object(self.service, "prepare", side_effect=RuntimeError("missing capabilities")) as prepare:
            self.service.start()
            self.service.thread.join(3)
            self.service.start()
        self.assertEqual(self.service.state, "failed")
        self.assertIn("missing capabilities", self.service.error)
        prepare.assert_called_once()

    def test_failed_preparation_reaps_daemon_before_reporting_failure(self):
        child = Mock(pid=123)
        def fail():
            self.service.daemon = child
            self.service.children = {child}
            raise IndexError("network changed")
        with patch.object(self.service, "prepare", side_effect=fail), \
                patch.object(docker, "terminate_group") as terminate:
            self.service.start()
            self.service.thread.join(3)
        terminate.assert_called_once_with(child)
        self.assertEqual(self.service.state, "failed")
        self.assertEqual(self.service.children, set())

    def test_ready_requires_actual_daemon_probe(self):
        child = Mock(poll=Mock(return_value=None))
        with patch.object(self.service, "execute", return_value="29.1.3") as execute:
            self.service.wait_ready(child)
        self.assertEqual(self.service.state, "ready")
        self.assertEqual(execute.call_args.args[0][0:2], ["docker", "info"])

    def test_readiness_deadline_is_bounded(self):
        with patch.object(docker.time, "monotonic", side_effect=[0, 26]):
            with self.assertRaisesRegex(RuntimeError, "25 seconds"):
                self.service.wait_ready(Mock())

    def test_preparation_starts_daemon_and_probes_its_private_socket(self):
        child = Mock(pid=123, poll=Mock(return_value=None))
        values = {"/proc/self/status": "CapEff:\tffffffff\n", "/proc/cgroups": "devices 0 1 1\n",
                  "/proc/self/mountinfo": ""}
        with patch.object(Path, "read_text", autospec=True, side_effect=lambda path: values[str(path)]), \
                patch.object(docker, "prepare_cgroups") as cgroups, \
                patch.object(docker, "prepare_network", return_value=1400), \
                patch.object(docker.subprocess, "Popen", return_value=child) as spawn, \
                patch.object(self.service, "execute", return_value="29.1.3") as execute:
            self.service.prepare()
        cgroups.assert_called_once()
        self.assertEqual(self.service.state, "ready")
        self.assertIn("--mtu=1400", spawn.call_args.args[0])
        self.assertEqual(spawn.call_args.kwargs["env"]["DOCKER_CONFIG"], str(self.service.base / "home" / ".docker"))
        execute.assert_called_once()
        self.assertEqual(json.loads((self.service.base / "daemon.json").read_text()), {})

    def test_missing_capabilities_fail_before_any_mount(self):
        with patch.object(Path, "read_text", return_value="CapEff:\t20000420\n"), \
                patch.object(docker, "prepare_cgroups") as cgroups:
            with self.assertRaisesRegex(RuntimeError, "SYS_ADMIN"):
                self.service.prepare()
        cgroups.assert_not_called()

    def test_probe_retries_until_docker_is_actually_ready(self):
        with patch.object(self.service, "execute", side_effect=[subprocess.CalledProcessError(1, "docker"), "29"]), \
                patch.object(self.service.cancel, "wait") as wait:
            self.service.wait_ready(Mock(poll=Mock(return_value=None)))
        self.assertEqual(self.service.state, "ready")
        wait.assert_called_once_with(0.25)

    def test_preparation_cancellation_does_not_mark_failed(self):
        self.service.cancel.set()
        self.service.state = "stopped"
        with patch.object(self.service, "prepare", side_effect=docker.Cancelled):
            self.service._prepare()
        self.assertEqual(self.service.state, "stopped")

    def test_stopping_joins_preparation_and_reaps_remaining_children(self):
        child = Mock(pid=123)
        self.service.children = {child}
        self.service.thread = Mock()
        with patch.object(docker, "terminate_group") as terminate:
            self.service.stop()
        terminate.assert_called_once_with(child)
        self.service.thread.join.assert_called_once_with(timeout=5)
        self.assertEqual(self.service.state, "stopped")

    def test_daemon_loss_fails_without_restarting(self):
        self.service.state = "ready"
        self.service.daemon = Mock(poll=Mock(return_value=2))
        self.service.check()
        self.assertEqual(self.service.state, "failed")

    def test_stop_kills_descendants_even_when_leader_exited(self):
        child = Mock(pid=123, poll=Mock(return_value=1))
        with patch.object(docker.os, "killpg") as kill:
            docker.terminate_group(child, 1)
        self.assertEqual([call.args for call in kill.call_args_list], [(123, signal.SIGTERM), (123, signal.SIGKILL)])

    def test_stop_prevents_later_preparation_commands(self):
        self.service.stop()
        with patch.object(docker.subprocess, "Popen") as popen:
            with self.assertRaises(docker.Cancelled):
                self.service.execute(["mount", "-t", "tmpfs", "tmpfs", "/sys/fs/cgroup"])
        popen.assert_not_called()

    def test_command_timeout_cleans_entire_owned_process_group(self):
        child = Mock(pid=123)
        child.communicate.side_effect = subprocess.TimeoutExpired("ip", 2)
        with patch.object(docker.subprocess, "Popen", return_value=child) as popen, \
                patch.object(docker, "terminate_group") as terminate:
            with self.assertRaises(subprocess.TimeoutExpired):
                self.service.execute(["ip", "-j", "route"], timeout=2)
        self.assertTrue(popen.call_args.kwargs["start_new_session"])
        terminate.assert_called_once_with(child, 1)

    def test_prepare_cgroups_preserves_existing_controller_mount(self):
        mountinfo = "73 71 0:39 / /sys/fs/cgroup/devices rw - cgroup cgroup rw,devices"
        calls = []
        docker.prepare_cgroups("devices 0 1 1\n", mountinfo, calls.append)
        self.assertEqual(calls, [])

    def test_prepare_cgroups_creates_only_guest_emulated_mounts(self):
        calls = []
        docker.prepare_cgroups("devices 0 1 1\nmemory 0 1 1\n", "", calls.append)
        self.assertIn(["mount", "-t", "cgroup", "-o", "devices", "cgroup", "/sys/fs/cgroup/devices"], calls)
        self.assertFalse(any("--bind" in call for call in calls))

    def test_network_uses_guest_interface_mtu_and_idempotent_snat(self):
        calls = []
        def execute(command, **_):
            calls.append(command)
            if command == ["ip", "-j", "route", "show", "default"]:
                return '[{"dev":"eth0"}]'
            if command == ["ip", "-j", "address", "show", "dev", "eth0"]:
                return '[{"mtu":1400,"addr_info":[{"family":"inet","local":"10.0.0.2"}]}]'
            return ""
        with patch.object(Path, "write_text") as write:
            self.assertEqual(docker.prepare_network(execute), 1400)
        write.assert_called_once_with("1\n")
        rules = [call for call in calls if call[0] == "iptables-legacy"]
        self.assertEqual(len(rules), 2)
        self.assertTrue(all("-C" in rule for rule in rules))
        self.assertTrue(all("10.0.0.2" in rule for rule in rules))

    def test_missing_network_rule_is_added(self):
        def execute(command, **_):
            if command[0] == "ip":
                return '[{"dev":"eth0"}]' if "route" in command else '[{"mtu":1500,"addr_info":[{"family":"inet","local":"10.0.0.2"}]}]'
            if "-C" in command:
                raise subprocess.CalledProcessError(1, command)
            return ""
        with patch.object(Path, "write_text"), patch.object(self.service, "execute", side_effect=execute) as run:
            docker.prepare_network(run)
        self.assertEqual(sum("-A" in call.args[0] for call in run.call_args_list), 2)


if __name__ == "__main__":
    unittest.main()
