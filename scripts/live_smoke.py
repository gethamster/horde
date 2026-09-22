#!/usr/bin/env python3
"""Opt-in isolated executor smoke; live runs may consume API/subscription capacity."""
import argparse
import ipaddress
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
    parser.add_argument("--decision-base-url", help="opt in to shadow Jev decisions at this explicit service root (Horde appends /v1/systemone)")
    parser.add_argument("--decision-model", default="jev-1.13.0")
    parser.add_argument("--decision-api-key-env", default="TUARA_API_KEY")
    parser.add_argument("--decision-review", action="store_true", help="also validate advisory work-product reviews; requires --decision-base-url")
    parser.add_argument("--timeout", type=positive, default=180, help="worker timeout in seconds")
    parser.add_argument("--max-tool-rounds", type=positive, default=32)
    parser.add_argument("--binary", type=Path, default=BINARY)
    parser.add_argument("--prepare-only", action="store_true", help="validate configuration/template without starting a daemon or calling a model")
    args = parser.parse_args(argv)
    if args.decision_review and not args.decision_base_url:
        parser.error("--decision-review requires --decision-base-url")
    if args.decision_base_url:
        url = urlsplit(args.decision_base_url)
        try:
            loopback = ipaddress.ip_address(url.hostname or "").is_loopback
        except ValueError:
            loopback = False
        if not url.hostname or url.username or url.password or url.query or url.fragment or not (
            url.scheme == "https" or url.scheme == "http" and loopback
        ):
            parser.error("--decision-base-url requires HTTPS (or literal loopback HTTP) without credentials, query, or fragment")
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", args.decision_api_key_env):
            parser.error("--decision-api-key-env must name an environment variable")
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
    if args.decision_base_url:
        lines += ["[decision]", 'mode="shadow"', 'backend="tuara"', 'protocol="systemone-v1"',
                  f"base_url={quote(args.decision_base_url)}", f"model={quote(args.decision_model)}",
                  f"api_key_env={quote(args.decision_api_key_env)}",
                  f"review_enabled={str(args.decision_review).lower()}",
                  "deadline_ms=30000", "max_attempts=1", "max_decisions_per_task=16",
                  "[[decision.capability_guidance]]", 'runtime="local"', 'capability="worker"',
                  'description="Create and commit the isolated hello.txt smoke fixture using the configured worker."']
    return "\n".join(lines) + "\n"


def drain_decisions(call, task, reviews_enabled, timeout):
    """Wait for durable final-review admission and completion of all admitted work."""
    deadline = time.monotonic() + timeout
    previous = None
    stable_since = time.monotonic()
    while time.monotonic() < deadline:
        # The fixture creates fewer than 200 rows, including locally skipped rows.
        decisions = call("decisions", task, "--limit", "200")
        reviews = call("reviews", task, "--limit", "200")
        snapshot = (decisions, reviews)
        if snapshot != previous:
            previous, stable_since = snapshot, time.monotonic()
        pending = any(row["state"] in {"queued", "running"} for row in decisions)
        final_seen = not reviews_enabled or any(row["kind"] == "final_evidence" for row in reviews)
        if decisions and final_seen and not pending and time.monotonic() - stable_since >= 1:
            return decisions, reviews
        time.sleep(.25)
    raise RuntimeError("Smoke decision drain timed out; inspect decisions.json and reviews.json")


def validate_decisions(decisions, reviews, reviews_enabled):
    failed = [row for row in decisions if row["state"] in {"failed", "interrupted", "cancelled"}]
    if failed:
        raise RuntimeError(f"Smoke decision validation failed: {len(failed)} failed/interrupted/cancelled decisions; inspect decisions.json")
    succeeded = lambda row: row["state"] == "succeeded" and row.get("provider_attempts", 0) > 0
    if not any(row["purpose"] == "routing" and succeeded(row) for row in decisions):
        raise RuntimeError("Smoke decision validation failed: no successful provider routing decision (local abstentions do not count)")
    if reviews_enabled and not any(row.get("kind") == "final_evidence" and succeeded(row) for row in reviews):
        raise RuntimeError("Smoke decision validation failed: no successful provider final_evidence review (local abstentions do not count)")


def run(args):
    binary = args.binary.resolve()
    if not binary.is_file():
        raise RuntimeError(f"Build Horde first with cargo build --locked: {binary}")
    if args.api_key_env and not args.prepare_only and not os.environ.get(args.api_key_env):
        raise RuntimeError(f"Export {args.api_key_env} before running (use a dummy value only for an endpoint that needs no authentication)")
    if args.decision_base_url and not args.prepare_only and not os.environ.get(args.decision_api_key_env):
        raise RuntimeError(f"Export {args.decision_api_key_env} before running decisions")
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
    # Provider connections belong to the isolated administrator configuration.
    admin_config = Path(env["XDG_CONFIG_HOME"]) / "horde"
    admin_config.mkdir()
    (admin_config / "config.toml").write_text(configuration(args))
    (repo / ".horde/horde.toml").write_text("[delivery]\nenabled=false\n")
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
                    if args.decision_base_url and status in {"succeeded", "failed"}:
                        decisions, reviews = drain_decisions(call, task, args.decision_review, args.timeout + 30)
                        validate_decisions(decisions, reviews, args.decision_review)
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
                commands = [("inspect", "result.json"), ("events", "events.json"), ("metrics", "metrics.json")]
                if args.decision_base_url:
                    commands += [("decisions", "decisions.json"), ("reviews", "reviews.json"), ("summary", "summary.json")]
                for command, filename in commands:
                    try:
                        options = ["--limit", "200"] if command in {"decisions", "reviews"} else []
                        (root / filename).write_text(json.dumps(call(command, task, *options), indent=2))
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
