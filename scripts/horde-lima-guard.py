#!/usr/bin/python3 -I
"""Privileged, bounded network setup for project-owned Lima host identities.

Install root-owned as /usr/local/libexec/horde-lima-guard. Each root-owned,
non-writable /etc/horde-lima/projects/PROJECT.json contains project, user, home,
and egress. The dedicated non-root user must exist and may belong to only one
project. Horde invokes only `apply PROJECT` and `verify PROJECT`; arbitrary
commands, UID selection, and firewall text are never accepted from the caller.
"""
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import pwd
import re
import stat
import subprocess
import sys

REGISTRY = Path("/etc/horde-lima/projects")


def private_root(path):
    info = path.lstat()
    if info.st_uid != 0 or info.st_mode & 0o022 or stat.S_ISLNK(info.st_mode):
        raise ValueError("guard and policy paths must be root-owned without writable aliases")


def load_policy(project):
    if not re.fullmatch(r"[a-zA-Z0-9_-]{1,64}", project):
        raise ValueError("invalid project")
    for path in [REGISTRY.parent, REGISTRY, REGISTRY / (project + ".json")]:
        private_root(path)
    policy = json.loads((REGISTRY / (project + ".json")).read_text())
    if set(policy) != {"project", "user", "home", "egress"} or policy["project"] != project:
        raise ValueError("invalid project policy")
    user = pwd.getpwnam(policy["user"])
    if user.pw_uid < 1 or user.pw_uid == int(os.environ.get("SUDO_UID", "0")) or not re.fullmatch(r"[a-zA-Z0-9_-]{1,64}", user.pw_name):
        raise ValueError("dedicated non-root user required")
    home = Path(policy["home"])
    if home != Path("/var/lib/horde-lima") / project:
        raise ValueError("project home must be /var/lib/horde-lima/PROJECT")
    if not isinstance(policy["egress"], list) or not policy["egress"]:
        raise ValueError("explicit egress CIDRs required")
    for network in policy["egress"]:
        if not isinstance(network, str) or not re.fullmatch(r"[a-fA-F0-9:./]+", network):
            raise ValueError("invalid CIDR")
        ipaddress.ip_network(network, strict=False)
    for other in REGISTRY.glob("*.json"):
        private_root(other)
        other_policy = json.loads(other.read_text())
        if other_policy.get("project") != project and pwd.getpwnam(other_policy["user"]).pw_uid == user.pw_uid:
            raise ValueError("host UID cannot be shared between projects")
    return policy, user


def run(argv, source=None):
    return subprocess.run(argv, input=source, text=True, capture_output=True,
                          check=True, timeout=30, env={"PATH": "/usr/sbin:/usr/bin:/sbin:/bin"}).stdout


def nft_rules(policy, uid):
    name = "horde_lima_" + hashlib.sha256(policy["project"].encode()).hexdigest()[:16]
    rules = [{"add": {"table": {"family": "inet", "name": name}}},
             {"add": {"chain": {"family": "inet", "table": name, "name": "output",
                                  "type": "filter", "hook": "output", "prio": -100,
                                  "policy": "accept"}}}]
    for network in policy["egress"]:
        prefix = ipaddress.ip_network(network, strict=False)
        expr = [{"match": {"op": "==", "left": {"meta": {"key": "skuid"}}, "right": uid}},
                {"match": {"op": "==", "left": {"payload": {"protocol": "ip" if prefix.version == 4 else "ip6", "field": "daddr"}},
                           "right": {"prefix": {"addr": str(prefix.network_address), "len": prefix.prefixlen}}}},
                {"accept": None}]
        rules.append({"add": {"rule": {"family": "inet", "table": name, "chain": "output", "expr": expr}}})
    rules.append({"add": {"rule": {"family": "inet", "table": name, "chain": "output", "expr": [
        {"match": {"op": "==", "left": {"meta": {"key": "skuid"}}, "right": uid}}, {"drop": None}]}}})
    return name, rules


def require_unused_uid(uid):
    try:
        processes = run(["/bin/ps", "-u", str(uid), "-o", "pid="])
    except subprocess.CalledProcessError as error:
        if error.returncode != 1:
            raise
        processes = error.stdout or ""
    if processes.strip():
        raise ValueError("initial firewall installation requires an unused dedicated host UID")


def canonical_nft_expression(expression):
    # nft readback may collapse /32 and /128 destinations to bare IP strings.
    # Normalize only that equivalent form; preserve every other key and value.
    match = expression.get("match") if isinstance(expression, dict) else None
    if not isinstance(match, dict) or match.get("op") != "==" or not isinstance(match.get("right"), str):
        return expression
    for protocol, version in [("ip", 4), ("ip6", 6)]:
        if match.get("left") != {"payload": {"protocol": protocol, "field": "daddr"}}:
            continue
        try:
            address = ipaddress.ip_address(match["right"])
        except ValueError:
            return expression
        if address.version == version:
            return dict(expression, match=dict(match, right={"prefix": {
                "addr": str(address), "len": address.max_prefixlen}}))
    return expression


def nft(policy, uid, action):
    name, commands = nft_rules(policy, uid)
    try:
        current = json.loads(run(["/usr/sbin/nft", "--json", "list", "table", "inet", name]))
    except subprocess.CalledProcessError:
        current = None
    if action == "apply" and current is None:
        require_unused_uid(uid)
        run(["/usr/sbin/nft", "--json", "--file", "-"], json.dumps({"nftables": commands}))
    actual = json.loads(run(["/usr/sbin/nft", "--json", "list", "table", "inet", name]))["nftables"]
    chains = [entry["chain"] for entry in actual if "chain" in entry]
    if len(chains) != 1 or any(chains[0].get(k) != v for k, v in {"name": "output", "hook": "output", "type": "filter", "prio": -100, "policy": "accept"}.items()):
        raise ValueError("host firewall chain differs from project policy")
    expressions = [[canonical_nft_expression(expression) for expression in entry["rule"]["expr"]]
                   for entry in actual if "rule" in entry]
    expected = [entry["add"]["rule"]["expr"] for entry in commands if "rule" in entry["add"]]
    if expressions != expected:
        raise ValueError("host firewall rules differ from project policy")


def pf_rules(policy, uid):
    allowed = "".join("pass out quick proto { tcp udp } from any to %s user %d no state\n" % (network, uid)
                      for network in policy["egress"])
    return allowed + "block drop out quick proto { tcp udp } from any to any user %d\n" % uid


def pf(policy, uid, action):
    anchor = "horde-lima/" + hashlib.sha256(policy["project"].encode()).hexdigest()[:16]
    if "Status: Enabled" not in run(["/sbin/pfctl", "-s", "info"]):
        raise ValueError("host PF must be enabled before provisioning")
    active = run(["/sbin/pfctl", "-sr"])
    # A dedicated first anchor prevents other anchors' quick-pass rules from
    # bypassing project filtering. Never replace the user's main configuration.
    if re.search(r"\bskip\b", run(["/sbin/pfctl", "-s", "Interfaces", "-v"])):
        raise ValueError("PF interfaces must not bypass filtering with skip")
    # macOS includes packet-normalization rules in `-sr`, ahead of filtering.
    # They cannot accept a packet and must not hide an earlier quick-pass rule.
    lines = [line.strip() for line in active.splitlines()
             if line.strip() and not line.strip().startswith(("scrub-anchor ", "scrub ", "no scrub "))]
    if not lines or lines[0].strip() != 'anchor "horde-lima/*" all':
        raise ValueError("horde-lima/* must be the first active PF filter anchor")
    rules = pf_rules(policy, uid)
    # Keep explicit rules: the optimizer otherwise creates tables whose members
    # are omitted by `-sr`, preventing a complete comparison of the allowlist.
    expected = run(["/sbin/pfctl", "-a", anchor, "-o", "none", "-nvf", "-"], rules).strip()
    actual = run(["/sbin/pfctl", "-a", anchor, "-sr"]).strip()
    if action == "apply" and not actual:
        require_unused_uid(uid)
        run(["/sbin/pfctl", "-a", anchor, "-o", "none", "-f", "-"], rules)
        actual = run(["/sbin/pfctl", "-a", anchor, "-sr"]).strip()
    if not expected or actual != expected:
        raise ValueError("PF readback does not match approved project policy")


def main():
    if os.geteuid() != 0 or len(sys.argv) != 3 or sys.argv[1] not in {"apply", "verify"}:
        raise ValueError("usage: root-owned horde-lima-guard apply|verify PROJECT")
    private_root(Path(__file__))
    policy, user = load_policy(sys.argv[2])
    root = Path("/var/lib/horde-lima")
    if not root.exists():
        if sys.argv[1] != "apply":
            raise ValueError("Lima host directory has not been prepared")
        root.mkdir(mode=0o755)
        # Root bootstrap commonly uses umask 077; project UIDs must traverse
        # this shared parent while their own homes remain private.
        root.chmod(0o755)
    private_root(root)
    home = Path(policy["home"])
    if not home.exists() and sys.argv[1] == "apply":
        home.mkdir(mode=0o700)
        os.chown(home, user.pw_uid, user.pw_gid)
    info = home.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != user.pw_uid or info.st_mode & 0o077:
        raise ValueError("Lima home must be a private directory owned by the dedicated user")
    if sys.platform == "linux":
        nft(policy, user.pw_uid, sys.argv[1])
    elif sys.platform == "darwin":
        pf(policy, user.pw_uid, sys.argv[1])
    else:
        raise ValueError("unsupported Lima host")
    print(json.dumps(dict(policy, uid=user.pw_uid, enforced=True)))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print("Lima guard refused operation: %s" % error, file=sys.stderr)
        sys.exit(1)
