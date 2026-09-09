#!/usr/bin/env python3
"""Run the same repository task against a single frontier worker and a composed workflow.
Opt-in: uses configured executors and can consume subscription/API capacity.
Results include missing-usage counts; unknown spend is never interpreted as zero spend.
"""
import argparse
import json
import os
import shlex
from pathlib import Path
import signal
import subprocess
import tempfile
import time

p = argparse.ArgumentParser()
p.add_argument("repo", type=Path)
p.add_argument("objective")
p.add_argument("--template", default="local-implementation")
p.add_argument("--timeout", type=int, default=3600)
p.add_argument("--output", type=Path, default=Path("benchmark-results.json"))
p.add_argument("--verify", help="Independent verification argv, parsed with shlex (no shell)")
p.add_argument("--preserve", action="append", default=[], help="Repository file that must remain unchanged")
a = p.parse_args()
binary = Path(__file__).resolve().parents[1] / "target/debug/horde"
root = Path(tempfile.mkdtemp(prefix="horde-bench-", dir="/tmp"))
log = (root / "daemon.log").open("w")
base = [str(binary), "--data-dir", str(root / "data")]
daemon = subprocess.Popen([*base, "daemon"], stdout=log, stderr=log)
results = []
try:
    for _ in range(100):
        if (root / "data/daemon.sock").exists():
            break
        time.sleep(.05)
    for label, template in [("frontier_only", "frontier-only"), ("composed", a.template)]:
        repo = root / label
        subprocess.run(["git", "clone", "--local", str(a.repo.resolve()), str(repo)], check=True, capture_output=True)
        # Fixture author identity is local to the benchmark clone.
        for key, value in [("user.name", "Horde benchmark"), ("user.email", "benchmark@example.invalid")]:
            subprocess.run(["git", "config", key, value], cwd=repo, check=True)
        for relative in (".horde.toml", ".horde/horde.toml"):
            config = a.repo / relative
            if config.exists():
                destination = repo / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(config.read_bytes())
        templates = repo / ".horde/templates"
        templates.mkdir(parents=True, exist_ok=True)
        (templates / "frontier-only.toml").write_text('''name="frontier-only"
version="1"
inputs=["task"]
[[steps]]
id="solve"
role="planner"
scope=["."]
tools=["read_file","search","write_file","apply_patch","command"]
instructions="Implement {{task}}. Inspect the repository, infer acceptance criteria, implement, test and review the result. Commit changes."
''')
        oid = json.loads(subprocess.check_output([*base, "submit", a.objective, "--repo", str(repo), "--template", template]))["id"]
        start = time.monotonic()
        while True:
            value = json.loads(subprocess.check_output([*base, "inspect", oid]))
            if value["task"]["status"] in {"succeeded", "failed", "blocked", "waiting"}:
                break
            if time.monotonic() - start > a.timeout:
                subprocess.run([*base, "cancel", oid], check=True, capture_output=True)
                break
            time.sleep(.5)
        metrics = json.loads(subprocess.check_output([*base, "metrics", oid]))
        independent = None
        integrated = root / "data/workspaces" / oid / "integrated"
        if a.verify and integrated.exists():
            env = {k: os.environ[k] for k in ["PATH", "HOME", "USER", "TMPDIR", "LANG"] if k in os.environ}
            env["CI"] = "true"
            check = subprocess.run(shlex.split(a.verify), cwd=integrated, env=env, capture_output=True, text=True, timeout=a.timeout)
            preserved = all((integrated / name).read_bytes() == (a.repo / name).read_bytes() for name in a.preserve)
            independent = {"passed": check.returncode == 0 and preserved, "exit_code": check.returncode, "preserved_files": preserved, "stdout": check.stdout[-16000:], "stderr": check.stderr[-16000:]}
        results.append({"strategy": label, "independent_verification": independent, "wall_seconds": round(time.monotonic() - start, 3), "metrics": metrics})
        print(json.dumps(results[-1]), flush=True)
    a.output.write_text(json.dumps({"repository": str(a.repo.resolve()), "objective": a.objective, "working_directory": str(root), "results": results}, indent=2) + "\n")
finally:
    daemon.send_signal(signal.SIGINT)
    try:
        daemon.wait(timeout=15)
    except subprocess.TimeoutExpired:
        daemon.kill()
        daemon.wait()
    log.close()
