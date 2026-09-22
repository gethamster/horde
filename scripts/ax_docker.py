"""Optional Docker daemon supervised inside an AX gVisor worker.

Activate only in an image deployed with the documented Docker-capable sandbox
configuration. Every mount and network operation below stays inside that guest.
"""
import ipaddress
import json
import os
from pathlib import Path
import signal
import subprocess
import threading
import time

PATH = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
CAPABILITIES = {
    "CHOWN": 0, "DAC_OVERRIDE": 1, "FOWNER": 3, "FSETID": 4, "KILL": 5,
    "SETGID": 6, "SETUID": 7, "SETPCAP": 8, "NET_BIND_SERVICE": 10,
    "NET_ADMIN": 12, "NET_RAW": 13, "SYS_CHROOT": 18, "SYS_PTRACE": 19,
    "SYS_ADMIN": 21, "MKNOD": 27, "AUDIT_WRITE": 29, "SETFCAP": 31,
}


class Cancelled(Exception):
    """The worker is stopping; no new managed processes may start."""


def terminate_group(child, grace=5):
    try:
        os.killpg(child.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        child.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    child.wait(timeout=5)


def prepare_cgroups(raw, mountinfo, execute):
    names = [parts[0] for line in raw.splitlines()
             if (parts := line.split()) and not parts[0].startswith("#")
             and len(parts) == 4 and parts[3] == "1"]
    if "devices" not in names:
        raise RuntimeError("gVisor devices cgroup controller is unavailable")
    mounted = {line.split(" - ", 1)[0].split()[4]
               for line in mountinfo.splitlines() if " - cgroup " in line}
    if all("/sys/fs/cgroup/" + name in mounted for name in names):
        return
    if not mounted:
        execute(["mkdir", "-p", "/sys/fs/cgroup"])
        execute(["mount", "-t", "tmpfs", "-o", "mode=755", "tmpfs", "/sys/fs/cgroup"])
    for name in names:
        target = "/sys/fs/cgroup/" + name
        if target not in mounted:
            execute(["mkdir", "-p", target])
            execute(["mount", "-t", "cgroup", "-o", name, "cgroup", target])


def prepare_network(execute):
    routes = json.loads(execute(["ip", "-j", "route", "show", "default"]))
    if not routes or not routes[0].get("dev"):
        raise RuntimeError("gVisor guest has no default network interface")
    interface = routes[0]["dev"]
    links = json.loads(execute(["ip", "-j", "address", "show", "dev", interface]))
    addresses = [item["local"] for item in links[0]["addr_info"] if item["family"] == "inet"]
    if not addresses:
        raise RuntimeError("gVisor guest has no IPv4 address")
    address = str(ipaddress.IPv4Address(addresses[0]))
    mtu = int(links[0]["mtu"])
    Path("/proc/sys/net/ipv4/ip_forward").write_text("1\n")
    for protocol in ("tcp", "udp"):
        rule = ["POSTROUTING", "-o", interface, "-j", "SNAT", "--to-source", address, "-p", protocol]
        try:
            execute(["iptables-legacy", "-t", "nat", "-C", *rule])
        except subprocess.CalledProcessError as error:
            if error.returncode != 1:
                raise
            execute(["iptables-legacy", "-t", "nat", "-A", *rule])
    return mtu


class DockerService:
    def __init__(self, workspace):
        self.base = Path(workspace) / ".horde" / "docker"
        self.state = "idle"
        self.error = None
        self.daemon = None
        self.thread = None
        self.lock = threading.Lock()
        self.cancel = threading.Event()
        self.children = set()

    def environment(self):
        return {"PATH": PATH, "LANG": "C.UTF-8", "HOME": str(self.base / "home"),
                "DOCKER_HOST": "unix:///var/run/docker.sock", "DOCKER_CONFIG": str(self.base / "home" / ".docker")}

    def daemon_args(self, mtu):
        return ["dockerd", "--config-file=" + str(self.base / "daemon.json"),
                "--host=unix:///var/run/docker.sock", "--data-root=" + str(self.base / "data"),
                "--exec-root=/run/horde-docker", "--pidfile=/run/horde-docker.pid",
                "--feature=containerd-snapshotter=false", "--storage-driver=vfs",
                "--iptables=false", "--ip6tables=false", "--ip-masq=false",
                "--exec-opt=native.cgroupdriver=cgroupfs", "--mtu=" + str(mtu)]

    def spawn(self, args, **kwargs):
        with self.lock:
            if self.cancel.is_set():
                raise Cancelled()
            child = subprocess.Popen(args, env=self.environment(), stdin=subprocess.DEVNULL,
                                     start_new_session=True, **kwargs)
            self.children = self.children | {child}
            return child

    def execute(self, args, timeout=20):
        child = self.spawn(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        try:
            try:
                output, _ = child.communicate(timeout=timeout)
            except subprocess.TimeoutExpired:
                terminate_group(child, 1)
                raise
            if child.returncode:
                raise subprocess.CalledProcessError(child.returncode, args, output=output)
            return output
        finally:
            with self.lock:
                self.children = self.children - {child}

    def start(self):
        with self.lock:
            if self.state != "idle":
                return
            self.state = "starting"
            self.thread = threading.Thread(target=self._prepare, daemon=True)
            self.thread.start()

    def _prepare(self):
        try:
            self.prepare()
        except Cancelled:
            return
        except Exception as error:
            self.cleanup()
            with self.lock:
                if not self.cancel.is_set():
                    self.state = "failed"
                    self.error = "Docker startup failed: " + str(error)
                    print(self.error, flush=True)

    def cleanup(self):
        with self.lock:
            children = tuple(self.children)
            self.children = set()
        for child in children:
            terminate_group(child)

    def prepare(self):
        self.base.mkdir(parents=True, exist_ok=True, mode=0o700)
        (self.base / "home").mkdir(exist_ok=True, mode=0o700)
        status = Path("/proc/self/status").read_text()
        mask = int(next(line.split()[1] for line in status.splitlines() if line.startswith("CapEff:")), 16)
        missing = [name for name, bit in CAPABILITIES.items() if not mask & (1 << bit)]
        if missing:
            raise RuntimeError("gVisor actor lacks capabilities: " + ", ".join(missing))
        prepare_cgroups(Path("/proc/cgroups").read_text(), Path("/proc/self/mountinfo").read_text(), self.execute)
        mtu = prepare_network(self.execute)
        (self.base / "daemon.json").write_text("{}\n")
        with (self.base / "dockerd.log").open("a") as log:
            child = self.spawn(self.daemon_args(mtu), stdout=log, stderr=log)
        with self.lock:
            self.daemon = child
        self.wait_ready(child)

    def wait_ready(self, child):
        deadline = time.monotonic() + 25
        while not self.cancel.is_set() and time.monotonic() < deadline:
            if child.poll() is not None:
                raise RuntimeError("dockerd exited before readiness; see .horde/docker/dockerd.log")
            try:
                self.execute(["docker", "info", "--format", "{{.ServerVersion}}"], timeout=2)
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
                self.cancel.wait(0.25)
            else:
                with self.lock:
                    if not self.cancel.is_set():
                        self.state = "ready"
                return
        if self.cancel.is_set():
            raise Cancelled()
        raise RuntimeError("dockerd was not ready within 25 seconds")

    def check(self):
        with self.lock:
            if self.state == "ready" and self.daemon.poll() is not None:
                self.state = "failed"
                self.error = "Managed Docker daemon exited; stop and start this runtime to recover"
                print(self.error, flush=True)

    def stop(self):
        self.cancel.set()
        with self.lock:
            self.state = "stopped"
        self.cleanup()
        if self.thread is not None:
            self.thread.join(timeout=5)
