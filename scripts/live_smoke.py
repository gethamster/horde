#!/usr/bin/env python3
"""Opt-in live executor smoke. Uses an isolated repository and the caller's existing login.
May consume subscription/API capacity. Never publishes or modifies the source repository.
Usage: python3 scripts/live_smoke.py [codex|claude|tuara]
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

kind = sys.argv[1] if len(sys.argv) > 1 else "codex"
assert kind in {"codex", "claude", "tuara"}
binary = Path(__file__).resolve().parents[1] / "target/debug/horde"
root = Path(tempfile.mkdtemp(prefix="horde-live-", dir="/tmp"))
repo = root / "repo"
repo.mkdir()
for args in [["init", "-b", "main"], ["config", "user.email", "smoke@example.invalid"], ["config", "user.name", "Horde smoke"], ["commit", "--allow-empty", "-m", "initial"]]:
    subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)
model = '\nmodel="z-ai/glm-5.3-flash"' if kind == "tuara" else ""
(repo / ".horde.toml").write_text(f'timeout_seconds=180\n[executors.worker]\nkind="{kind}"{model}\n')
templates = repo / ".horde/templates"
templates.mkdir(parents=True)
(templates / "smoke.toml").write_text('''name="smoke"
version="1"
[[steps]]
id="hello"
scope=["hello.txt"]
tools=["read_file","write_file","command"]
instructions="Create hello.txt containing exactly hello followed by a newline. Check its contents with a command, then commit it. Do not create other files. Return JSON with result and accepted."
acceptance=["hello.txt contains hello plus newline", "Changes are committed"]
''')
log = (root / "daemon.log").open("w")
daemon = subprocess.Popen([str(binary), "--data-dir", str(root / "data"), "daemon"], stdout=log, stderr=log)
base = [str(binary), "--data-dir", str(root / "data")]
try:
    for _ in range(100):
        if (root / "data/daemon.sock").exists():
            break
        time.sleep(.05)
    submitted = subprocess.check_output([*base, "submit", "Live executor smoke", "--repo", str(repo), "--template", "smoke"])
    oid = json.loads(submitted)["id"]
    print(json.dumps({"directory": str(root), "task": oid, "executor": kind}), flush=True)
    for _ in range(400):
        value = json.loads(subprocess.check_output([*base, "inspect", oid]))
        if value["task"]["status"] in {"succeeded", "failed", "blocked"}:
            (root / "result.json").write_text(json.dumps(value, indent=2))
            print(json.dumps({"status": value["task"]["status"], "results": [{"state": t["state"], "result": t["result"]} for t in value["steps"]]}), flush=True)
            assert value["task"]["status"] == "succeeded"
            assert (root / f"data/workspaces/{oid}/integrated/hello.txt").read_text() == "hello\n"
            assert not (repo / "hello.txt").exists()
            break
        time.sleep(.5)
    else:
        raise RuntimeError("smoke timeout")
finally:
    daemon.send_signal(signal.SIGINT)
    try:
        daemon.wait(timeout=10)
    except subprocess.TimeoutExpired:
        daemon.kill()
        daemon.wait()
    log.close()
