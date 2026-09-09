#!/usr/bin/env python3
"""Opt-in isolated executor smoke; live runs may consume API/subscription capacity."""
import argparse
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import time
from urllib.parse import urlsplit

BINARY = Path(__file__).resolve().parents[1] / "target/debug/horde"
TEMPLATE = '''name="smoke"
version="1"
[[steps]]
id="hello"
scope=["hello.txt"]
tools=["read_file","write_file","command"]
instructions="Create hello.txt containing exactly hello followed by a newline. Check its contents with a command, then commit it. Do not create other files. Return JSON with result and accepted."
acceptance=["hello.txt contains hello plus newline", "Changes are committed"]
'''


def positive(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be positive")
    return number


def arguments(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind", nargs="?", default="codex", choices=["codex", "claude", "tuara", "local"])
    parser.add_argument("--model", help="exact endpoint model identifier; required for tuara/local")
    parser.add_argument("--base-url", help="native endpoint URL including /v1; required for local")
    parser.add_argument("--api-key-env", help="environment variable holding the key, never the key itself")
    parser.add_argument("--timeout", type=positive, default=180, help="worker timeout in seconds")
    parser.add_argument("--max-tool-rounds", type=positive, default=32)
    parser.add_argument("--binary", type=Path, default=BINARY)
    parser.add_argument("--prepare-only", action="store_true", help="validate configuration/template without starting a daemon or calling a model")
    args = parser.parse_args(argv)
    native = args.kind in {"tuara", "local"}
    if native and not args.model:
        parser.error("tuara/local requires --model with an exact catalog identifier")
    if args.kind == "local" and not args.base_url:
        parser.error("local requires --base-url")
    if not native and (args.base_url or args.api_key_env):
        parser.error("--base-url and --api-key-env require tuara/local; codex/claude use existing CLI login")
    if native:
        args.base_url = args.base_url or "https://tuara.com/router/v1"
        url = urlsplit(args.base_url)
        if url.scheme not in {"http", "https"} or not url.hostname or url.username or url.password or url.query or url.fragment:
            parser.error("--base-url must be an HTTP(S) endpoint without credentials, query, or fragment")
        args.api_key_env = args.api_key_env or ("HORDE_SMOKE_API_KEY" if args.kind == "local" else "TUARA_API_KEY")
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", args.api_key_env):
            parser.error("--api-key-env must name an environment variable")
    return args


def configuration(args):
    # JSON basic strings are compatible with TOML for these user-supplied values.
    quote = lambda value: json.dumps(value, ensure_ascii=False)
    lines = [f"timeout_seconds={args.timeout}", f"max_tool_rounds={args.max_tool_rounds}", "concurrency=1", "[delivery]", "enabled=false", "[providers.smoke]"]
    native = args.kind in {"tuara", "local"}
    lines += [f"kind={quote('tuara' if native else args.kind)}", f"auth_mode={quote('api' if native else 'login')}"]
    if native:
        lines += [f"base_url={quote(args.base_url)}", f"api_key_env={quote(args.api_key_env)}"]
    if args.model:
        lines += [f"model={quote(args.model)}"]
    for role in ["planner", "worker", "reviewer"]:
        lines += [f"[executors.{role}]", 'provider="smoke"']
    return "\n".join(lines) + "\n"


def run(args):
    binary = args.binary.resolve()
    if not binary.is_file():
        raise RuntimeError(f"Build Horde first with cargo build --locked: {binary}")
    if args.api_key_env and not args.prepare_only and not os.environ.get(args.api_key_env):
        raise RuntimeError(f"Export {args.api_key_env} before running (use a dummy value only for an endpoint that needs no authentication)")
    root = Path(tempfile.mkdtemp(prefix="horde-live-", dir="/tmp"))
    repo = root / "repo"
    repo.mkdir()
    env = os.environ.copy()
    # Isolate Horde's user config while retaining HOME and CLI credential stores.
    env["XDG_CONFIG_HOME"] = str(root / "config")
    Path(env["XDG_CONFIG_HOME"]).mkdir()
    for command in [["init", "-b", "main"], ["config", "user.email", "smoke@example.invalid"], ["config", "user.name", "Horde smoke"], ["commit", "--allow-empty", "-m", "initial"]]:
        subprocess.run(["git", *command], cwd=repo, env=env, check=True, capture_output=True, timeout=30)
    (repo / ".horde").mkdir(exist_ok=True)
    (repo / ".horde/horde.toml").write_text(configuration(args))
    templates = repo / ".horde/templates"
    templates.mkdir(parents=True)
    (templates / "smoke.toml").write_text(TEMPLATE)
    base = [str(binary), "--data-dir", str(root / "data")]

    def call(*command):
        return json.loads(subprocess.check_output([*base, *command], env=env, timeout=15))

    # Doctor loads the merged settings without probing the provider.
    call("doctor", "--repo", str(repo))
    call("validate", "smoke", "--repo", str(repo))
    print(json.dumps({"directory": str(root), "executor": args.kind, "prepared": True}), flush=True)
    if args.prepare_only:
        return root
    task = None
    with (root / "daemon.log").open("w") as log:
        daemon = subprocess.Popen([*base, "daemon"], env=env, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 10
            while not (root / "data/daemon.sock").exists():
                if daemon.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError(f"Daemon did not start; inspect {root / 'daemon.log'}")
                time.sleep(.05)
            task = call("submit", "Live executor smoke", "--repo", str(repo), "--template", "smoke")["id"]
            print(json.dumps({"task": task}), flush=True)
            deadline = time.monotonic() + args.timeout + 30
            while time.monotonic() < deadline:
                value = call("inspect", task)
                status = value["task"]["status"]
                if status in {"succeeded", "failed", "blocked", "cancelled"}:
                    print(json.dumps({"status": status}), flush=True)
                    if status != "succeeded":
                        raise RuntimeError(f"Smoke ended {status}; evidence: {root}")
                    integrated = root / f"data/workspaces/{task}/integrated/hello.txt"
                    if integrated.read_bytes() != b"hello\n" or (repo / "hello.txt").exists():
                        raise RuntimeError("Integrated result or original-checkout isolation failed")
                    return root
                time.sleep(.5)
            raise RuntimeError(f"Smoke timed out; evidence: {root}")
        finally:
            if task and daemon.poll() is None:
                for command, filename in [("inspect", "result.json"), ("events", "events.json"), ("metrics", "metrics.json")]:
                    try:
                        (root / filename).write_text(json.dumps(call(command, task), indent=2))
                    except (subprocess.SubprocessError, ValueError, OSError):
                        print(f"Could not capture {filename}; inspect daemon.log", flush=True)
            if daemon.poll() is None:
                daemon.send_signal(signal.SIGINT)
                try:
                    daemon.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait(timeout=10)


if __name__ == "__main__":
    run(arguments())
