#!/usr/bin/env python3
"""Opt-in real Docker/Compose/Git probe in a disposable, sandbox-owned daemon.

Does not mount the host Docker socket, user credentials, or host repositories.
Pass the already-published local runner image; this probe never builds Horde.
"""
import argparse
import json
import subprocess
import time
import uuid

DIND = "docker:28.3.3-dind@sha256:a56b3bdde89315ed2cc0e4906e582b5033d93bf20d9cb9510c2cdd4e7f7690b1"


def docker(*args, check=True):
    return subprocess.run(["docker", *args], check=check, capture_output=True, text=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    args = parser.parse_args()
    scope = "horde-local-probe-" + uuid.uuid4().hex[:12]
    daemon = scope + "-docker"
    worker = scope + "-worker"
    volumes = [scope + "-socket", scope + "-workspace", scope + "-docker-data"]
    started = time.monotonic()
    try:
        for volume in volumes:
            docker("volume", "create", "--label", "io.horde.probe=" + scope, volume)
        docker("run", "-d", "--name", daemon, "--label", "io.horde.probe=" + scope,
               "--privileged", "-e", "DOCKER_TLS_CERTDIR=", "--mount",
               "type=volume,src=" + volumes[0] + ",dst=/var/run", "--mount",
               "type=volume,src=" + volumes[1] + ",dst=/workspace", "--mount",
               "type=volume,src=" + volumes[2] + ",dst=/var/lib/docker", DIND,
               "--host=unix:///var/run/docker.sock", "--group=10001")
        docker("run", "-d", "--name", worker, "--label", "io.horde.probe=" + scope,
               "--group-add", "10001", "--mount", "type=volume,src=" + volumes[0] + ",dst=/var/run",
               "--mount", "type=volume,src=" + volumes[1] + ",dst=/workspace",
               "--entrypoint", "sleep", args.image, "300")
        for _ in range(60):
            if docker("exec", worker, "docker", "info", check=False).returncode == 0:
                break
            time.sleep(1)
        else:
            raise RuntimeError("sandbox-owned Docker daemon did not become ready")
        mounts = json.loads(docker("inspect", worker).stdout)[0]["Mounts"]
        if any(m["Type"] != "volume" for m in mounts):
            raise RuntimeError("probe worker unexpectedly contains a host bind mount")
        docker("exec", "--user", "0", worker, "chown", "-R", "10001:10001", "/workspace")
        script = r'''set -eu
cd /workspace
mkdir app
cd app
git init -b main
git config user.email probe@example.invalid
git config user.name Probe
printf 'sandbox ready\n' > index.html
printf 'FROM busybox:1.37\nCOPY index.html /www/index.html\nCMD ["httpd","-f","-p","8080","-h","/www"]\n' > Dockerfile
printf 'services:\n  web:\n    image: horde-probe:verified\n' > compose.yaml
git add .
git commit -m seed
git switch -c horde/probe
printf 'branch iteration\n' > index.html
git commit -am iteration
docker build -t horde-probe:verified .
digest=$(docker image inspect horde-probe:verified --format '{{.Id}}')
docker compose -p probe up -d --no-build
container=$(docker compose -p probe ps -q web)
docker exec "$container" wget -qO- http://127.0.0.1:8080 | grep 'branch iteration'
test "$digest" = "$(docker inspect "$container" --format '{{.Image}}')"
docker compose -p probe down
'''
        result = docker("exec", worker, "sh", "-c", script)
        print(json.dumps({"scope": scope, "passed": True,
                          "elapsed_seconds": round(time.monotonic() - started, 2),
                          "checks": ["private_daemon", "docker_build", "compose_no_build",
                                     "http", "git_branch_commits", "same_image_identity"]}))
    finally:
        for container in (worker, daemon):
            docker("rm", "-f", container, check=False)
        for volume in reversed(volumes):
            docker("volume", "rm", volume, check=False)


if __name__ == "__main__":
    main()
