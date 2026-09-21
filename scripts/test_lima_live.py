#!/usr/bin/env python3
"""Opt-in Lima acceptance suite for a prepared macOS/Linux provisioning host.

Requires a running Horde controller, two existing isolated project profiles,
installed host guards, pinned Ubuntu guest images, and Linux Horde binaries.
No setup or provider login is performed. Creates only uniquely named test VMs;
uses Docker's scratch image so tests require no external container registry.

  python3 scripts/test_lima_live.py --allow-live --project-a UUID --profile-a a \
      --project-b UUID --profile-b b --blocked-ip 10.20.30.1 --blocked-port 443

The blocked address must be a reachable host/private-network service excluded
from both configured allowlists. A host-side TCP probe confirms reachability
before guest denial counts as a pass. Live runs consume local VM resources.
"""
import argparse
import json
import ipaddress
import os
from pathlib import Path
import socket
import subprocess
import time
import uuid


def run(argv, timeout=120, check=True):
    result = subprocess.run(argv, text=True, capture_output=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError("command failed: %s\n%s" % (argv[0], result.stderr))
    return result


GUEST_TCP_PROBE = r"""
import errno,json,socket,sys
address,port,nonce = sys.argv[1],int(sys.argv[2]),sys.argv[3]
report = dict(version=1,nonce=nonce,address=address,port=port,attempted=True,status='inconclusive',reason='socket-error')
try:
    with socket.create_connection((address,port),timeout=5):
        report.update(status='connected',reason=None)
except socket.timeout:
    report.update(status='denied',reason='timeout')
except OSError as error:
    reasons = {errno.EACCES:'permission',errno.EPERM:'permission',errno.ECONNREFUSED:'refused',
               errno.ENETUNREACH:'unreachable',errno.EHOSTUNREACH:'unreachable',
               errno.ECONNRESET:'reset',errno.ETIMEDOUT:'timeout'}
    if error.errno in reasons:
        report.update(status='denied',reason=reasons[error.errno])
print('HORDE_TCP_PROBE:'+json.dumps(report))
"""


def assert_guest_denied(base, address, port):
    nonce = uuid.uuid4().hex
    result = run(base + ['python3', '-c', GUEST_TCP_PROBE, address, str(port), nonce], timeout=30, check=True)
    if result.returncode != 0:
        raise RuntimeError('guest network probe transport failed; denial is unverified')
    reports = [line.removeprefix('HORDE_TCP_PROBE:') for line in result.stdout.splitlines()
               if line.startswith('HORDE_TCP_PROBE:')]
    if len(reports) != 1:
        raise RuntimeError('guest network probe result missing or ambiguous')
    try:
        report = json.loads(reports[0])
    except ValueError as error:
        raise RuntimeError('guest network probe result is malformed') from error
    if (not isinstance(report, dict) or report.get('version') != 1 or report.get('nonce') != nonce
            or report.get('address') != address or report.get('port') != port or report.get('attempted') is not True):
        raise RuntimeError('guest network probe result does not match this attempted connection')
    if report.get('status') != 'denied' or report.get('reason') not in {'timeout', 'permission', 'refused', 'unreachable', 'reset'}:
        raise RuntimeError('guest network access allowed or denial probe inconclusive')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--allow-live", action="store_true")
    parser.add_argument("--horde", default="horde")
    parser.add_argument("--data-dir", type=Path)
    for suffix in ("a", "b"):
        parser.add_argument("--project-" + suffix, required=True)
        parser.add_argument("--profile-" + suffix, required=True)
    parser.add_argument("--blocked-ip", required=True)
    parser.add_argument("--blocked-port", type=int, required=True)
    args = parser.parse_args()
    if not args.allow_live:
        parser.error("live VM provisioning requires explicit --allow-live")
    if args.project_a == args.project_b:
        parser.error("two distinct project identities are required")
    ipaddress.ip_address(args.blocked_ip)
    with socket.create_connection((args.blocked_ip, args.blocked_port), timeout=5):
        pass
    prefix = [args.horde]
    if args.data_dir:
        prefix += ["--data-dir", str(args.data_dir)]

    def call(project, method, payload):
        output = run(prefix + ["--project", project, "call", method, json.dumps(payload)])
        return json.loads(output.stdout)

    def wait(project, runtime, request):
        deadline = time.monotonic() + 1200
        while time.monotonic() < deadline:
            report = call(project, "runtime_inspect", {"id": runtime})
            operations = [op for op in report["operations"] if op["id"] == request]
            if operations and operations[0]["state"] == "succeeded":
                return report
            if operations and operations[0]["state"] in {"failed", "blocked", "uncertain"}:
                raise RuntimeError("owned VM operation requires inspection: " + json.dumps(operations[0]))
            time.sleep(2)
        raise RuntimeError("VM operation timed out; inspect " + runtime)

    def wait_ready(project,runtime):
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            report = call(project,"runtime_inspect",{"id":runtime})
            state = report["runtime"][0]
            if state["state"] == "ready":
                return report
            if state.get("error"):
                raise RuntimeError("guest enrollment failed: " + state["error"])
            time.sleep(2)
        raise RuntimeError("guest did not establish authenticated controller enrollment: " + runtime)

    resources = []
    guests = []
    try:
        for project, profile in [(args.project_a, args.profile_a), (args.project_b, args.profile_b)]:
            runtime = "live-" + uuid.uuid4().hex[:12]
            request = "create-" + runtime
            resources.append((project, runtime))
            payload = {"id": runtime, "profile": profile, "request_id": request}
            call(project, "runtime_create", payload)
            call(project, "runtime_create", payload)  # Exact retry must not clone resources.
            wait(project, runtime, request)
            report = wait_ready(project,runtime)
            spec = json.loads(report["runtime"][0]["spec"])
            resource = report["runtime"][0]["resource"]
            if not isinstance(resource, str) or not resource:
                raise RuntimeError("ready Lima runtime has no persisted guest resource")
            if spec.get("host"):
                raise RuntimeError("run this suite on the provisioning host with local profiles")
            # Lima opens its SSH control connection before Docker group creation.
            # Refresh the horde user's supplementary groups for these commands.
            base = ["sudo", "-n", "-H", "-u", spec["lima_user"], "--", "/usr/bin/env",
                    "LIMA_HOME=" + spec["lima_home"], "PATH=/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
                    "limactl", "shell", resource, "sudo", "-u", "horde", "--"]
            marker = runtime + "-private"
            guests.append((runtime,spec,base))
            script = """set -eu
marker="$1"
command -v docker >/dev/null
docker compose version
sudo systemctl is-active horde.service
mkdir -p /home/horde/acceptance
printf '%s' "$1" > /home/horde/acceptance/marker
cd /home/horde/acceptance
touch "$1"
printf 'FROM scratch\\nCOPY marker /marker\\n' > Dockerfile
docker build -t horde-acceptance:local .
# Launch another build from a container using only this VM's Docker daemon.
set -- --rm --mount type=bind,src=/var/run/docker.sock,dst=/var/run/docker.sock --mount type=bind,src="$PWD",dst=/workspace,readonly
for directory in /usr /lib /lib64; do
    if test -d "$directory"; then set -- "$@" --mount "type=bind,src=$directory,dst=$directory,readonly"; fi
done
docker run "$@" horde-acceptance:local /usr/bin/docker -H unix:///var/run/docker.sock build -t horde-acceptance:nested /workspace
printf 'services:\\n  check:\\n    image: horde-acceptance:local\\n    entrypoint: ["/usr/bin/sleep", "30"]\\n    volumes:\\n' > compose.yaml
for directory in /usr /lib /lib64; do
    if test -d "$directory"; then printf '      - %s:%s:ro\\n' "$directory" "$directory" >> compose.yaml; fi
done
docker compose -p "$marker" up -d
service=$(docker compose -p "$marker" ps -q check)
test "$(docker inspect --format '{{.State.Running}}' "$service")" = true
docker compose -p "$marker" down
container=$(docker create horde-acceptance:local /nonexistent)
docker cp "$container:/marker" copied-marker
docker rm "$container"
cmp marker copied-marker
# The VM's own daemon is the nested Docker environment; neither host socket
# nor a host-home mount may be present in its filesystem.
! mount | grep -E 'virtiofs|9p'
! test -e /run/host-services/docker.sock
"""
            run(base + ["/bin/sh", "-c", script, "acceptance", marker], timeout=180)
            # Require a matching guest-emitted result; a failed sudo, SSH, or
            # limactl command does not prove the guest was denied network access.
            assert_guest_denied(base, args.blocked_ip, args.blocked_port)
            for action in ("runtime_stop", "runtime_start"):
                request = action + "-" + runtime
                payload = {"id": runtime, "request_id": request}
                call(project, action, payload)
                call(project, action, payload)
                wait(project, runtime, request)
                if action == "runtime_start":
                    wait_ready(project, runtime)
            request = "reconcile-" + runtime
            call(project, "runtime_reconcile", {"id": runtime, "request_id": request, "resource": resource})
            wait(project, runtime, request)
            wait_ready(project, runtime)
            run(base + ["test", "-f", "/home/horde/acceptance/marker"])
        assert len({guest[1]["lima_home"] for guest in guests}) == 2, "projects share a Lima disk directory"
        assert len({guest[1]["lima_user"] for guest in guests}) == 2, "projects share a host identity"
        for runtime, _, base in guests:
            for other, _, _ in guests:
                if other != runtime:
                    run(base + ["/bin/sh", "-c", '! find /home /var/lib/horde -name "$1" -print -quit | grep .', "check", other + "-private"])
        print("PASS: separate VM disks, guest Docker/Compose, denied egress, lifecycle retries, reconcile, and restart persistence")
    finally:
        failures = []
        for project, runtime in reversed(resources):
            try:
                request = "destroy-" + runtime
                call(project, "runtime_destroy", {"id": runtime, "request_id": request})
                wait(project, runtime, request)
            except Exception as error:
                failures.append(runtime + ": " + str(error))
        if failures:
            raise RuntimeError("owned resources retained for inspection: " + "; ".join(failures))


if __name__ == "__main__":
    main()
