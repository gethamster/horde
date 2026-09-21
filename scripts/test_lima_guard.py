import importlib.util
import copy
import contextlib
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("lima_guard", Path(__file__).with_name("horde-lima-guard.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

POLICY = {"project": "hamster", "user": "horde-hamster", "home": "/var/lib/horde-lima/hamster", "egress": ["203.0.113.9/32", "2001:db8::1/128"]}


def nft_state(policy=POLICY):
    _, commands = guard.nft_rules(policy, 501)
    return json.dumps({"nftables": [c["add"] for c in commands]})


def nft_kernel_state():
    # nft 1.0.9 readback represents single-host destinations as address strings.
    # Keep this independent of nft_rules so the fixture exercises the wire shape.
    return {"nftables": [
        {"metainfo": {"version": "1.0.9", "json_schema_version": 1}},
        {"table": {"family": "inet", "name": "fixture", "handle": 4}},
        {"chain": {"family": "inet", "table": "fixture", "name": "output", "handle": 1,
                   "type": "filter", "hook": "output", "prio": -100, "policy": "accept"}},
        {"rule": {"family": "inet", "table": "fixture", "chain": "output", "handle": 2, "expr": [
            {"match": {"op": "==", "left": {"meta": {"key": "skuid"}}, "right": 501}},
            {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "203.0.113.9"}},
            {"accept": None}]}},
        {"rule": {"family": "inet", "table": "fixture", "chain": "output", "handle": 3, "expr": [
            {"match": {"op": "==", "left": {"meta": {"key": "skuid"}}, "right": 501}},
            {"match": {"op": "==", "left": {"payload": {"protocol": "ip6", "field": "daddr"}}, "right": "2001:db8::1"}},
            {"accept": None}]}},
        {"rule": {"family": "inet", "table": "fixture", "chain": "output", "handle": 4, "expr": [
            {"match": {"op": "==", "left": {"meta": {"key": "skuid"}}, "right": 501}},
            {"drop": None}]}}
    ]}


class GuardTests(unittest.TestCase):
    def test_nft_accepts_exact_host_address_readback_without_replacing_rules(self):
        state = nft_kernel_state()
        for action in ["apply", "verify"]:
            with self.subTest(action=action), patch.object(guard, "run", return_value=json.dumps(state)) as runner:
                guard.nft(POLICY, 501, action)
                self.assertEqual(runner.call_count, 2)
                self.assertTrue(all("list" in call.args[0] for call in runner.call_args_list))
        self.assertEqual(state, nft_kernel_state(), "verification must not mutate its input")

    def test_nft_host_normalization_cannot_hide_rule_changes(self):
        base = nft_kernel_state()
        changes = [
            (0, 1, {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "203.0.113.10"}}),
            (0, 1, {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": {"prefix": {"addr": "203.0.113.0", "len": 24}}}}),
            (1, 1, {"match": {"op": "==", "left": {"payload": {"protocol": "ip6", "field": "daddr"}}, "right": {"prefix": {"addr": "2001:db8::", "len": 64}}}}),
            (0, 1, {"match": {"op": "==", "left": {"payload": {"protocol": "ip6", "field": "daddr"}}, "right": "203.0.113.9"}}),
            (0, 1, {"match": {"op": "!=", "left": {"payload": {"protocol": "ip", "field": "daddr"}}, "right": "203.0.113.9"}}),
            (0, 1, {"match": {"op": "==", "left": {"payload": {"protocol": "ip", "field": "saddr"}}, "right": "203.0.113.9"}}),
            (0, 0, {"match": {"op": "==", "left": {"meta": {"key": "skuid"}}, "right": 502}}),
            (0, 2, {"drop": None}),
            (2, 1, {"accept": None}),
        ]
        for rule, expression, replacement in changes:
            state = copy.deepcopy(base)
            state["nftables"][3 + rule]["rule"]["expr"][expression] = replacement
            with self.subTest(replacement=replacement), patch.object(guard, "run", return_value=json.dumps(state)):
                with self.assertRaisesRegex(ValueError, "rules differ"):
                    guard.nft(POLICY, 501, "verify")
        for change in ["extra expression", "missing UID", "extra match field"]:
            state = copy.deepcopy(base)
            expressions = state["nftables"][3]["rule"]["expr"]
            if change == "extra expression":
                expressions.append({"accept": None})
            elif change == "missing UID":
                expressions.pop(0)
            else:
                expressions[1]["match"]["extra"] = True
            with self.subTest(change=change), patch.object(guard, "run", return_value=json.dumps(state)):
                with self.assertRaisesRegex(ValueError, "rules differ"):
                    guard.nft(POLICY, 501, "verify")

    def test_nft_preserves_non_host_prefixes(self):
        policy = dict(POLICY, egress=["203.0.113.0/24", "2001:db8::/64"])
        state = json.loads(nft_state(policy))
        with patch.object(guard, "run", return_value=json.dumps(state)):
            guard.nft(policy, 501, "verify")
        for index, address in [(2, "203.0.113.0"), (3, "2001:db8::")]:
            changed = copy.deepcopy(state)
            changed["nftables"][index]["rule"]["expr"][1]["match"]["right"] = address
            with self.subTest(address=address), patch.object(guard, "run", return_value=json.dumps(changed)):
                with self.assertRaisesRegex(ValueError, "rules differ"):
                    guard.nft(policy, 501, "verify")

    def test_nft_verifies_every_rule_and_output_hook(self):
        with patch.object(guard, "run", return_value=nft_state()):
            guard.nft(POLICY, 501, "verify")
        state = json.loads(nft_state())
        state["nftables"][-1]["rule"]["expr"][-1] = {"accept": None}
        with patch.object(guard, "run", return_value=json.dumps(state)):
            with self.assertRaisesRegex(ValueError, "rules differ"):
                guard.nft(POLICY, 501, "verify")

    def test_nft_create_is_atomic_and_does_not_replace_other_tables(self):
        missing = subprocess.CalledProcessError(1, ["nft"])
        with patch.object(guard, "run", side_effect=[missing, "", "", nft_state()]) as runner:
            guard.nft(POLICY, 501, "apply")
        written = json.loads(runner.call_args_list[2].args[1])
        self.assertNotIn("flush", str(written))
        self.assertEqual(len(written["nftables"]), 5)

    def test_existing_different_nft_policy_is_never_silently_replaced(self):
        state = json.loads(nft_state())
        state["nftables"][1]["chain"]["hook"] = "forward"
        with patch.object(guard, "run", return_value=json.dumps(state)) as runner:
            with self.assertRaisesRegex(ValueError, "chain differs"):
                guard.nft(POLICY, 501, "apply")
            self.assertEqual(runner.call_count, 2)

    def test_pf_requires_first_anchor_and_rejects_skip_bypass(self):
        with patch.object(guard, "run", side_effect=["Status: Enabled", 'anchor "horde-lima/*" all', 'lo0 (skip)']):
            with self.assertRaisesRegex(ValueError, "skip"):
                guard.pf(POLICY, 501, "verify")
        with patch.object(guard, "run", side_effect=["Status: Enabled", 'pass quick all\nanchor "horde-lima/*" all', 'lo0']):
            with self.assertRaisesRegex(ValueError, "first"):
                guard.pf(POLICY, 501, "verify")

    def test_pf_directional_or_interface_anchor_cannot_claim_enforcement(self):
        for anchor in ['anchor "horde-lima/*" in all', 'anchor "horde-lima/*" out on en0 all', 'anchor "horde-lima/*" proto tcp all']:
            with patch.object(guard,"run",side_effect=["Status: Enabled",anchor,"lo0"]):
                with self.assertRaisesRegex(ValueError,"first"):
                    guard.pf(POLICY,501,"verify")

    def test_pf_apple_scrub_anchors_do_not_precede_the_first_filter(self):
        scrub = 'scrub-anchor "com.apple/*" all fragment reassemble\nscrub-anchor "com.apple.internet-sharing" all fragment reassemble\n'
        active = scrub + 'anchor "horde-lima/*" all\nanchor "com.apple/*" all\nanchor "com.apple.internet-sharing" all'
        with patch.object(guard, "run", side_effect=["Status: Enabled", active, "lo0", "canonical block", "canonical block"]):
            guard.pf(POLICY, 501, "verify")
        active = scrub + 'pass out quick all\nanchor "horde-lima/*" all'
        with patch.object(guard, "run", side_effect=["Status: Enabled", active, "lo0"]):
            with self.assertRaisesRegex(ValueError, "first"):
                guard.pf(POLICY, 501, "verify")

    def test_initial_firewall_refuses_preexisting_uid_processes(self):
        with patch.object(guard,"run",return_value="1234\n"):
            with self.assertRaisesRegex(ValueError,"unused"):
                guard.require_unused_uid(501)
        with patch.object(guard,"run",side_effect=subprocess.CalledProcessError(1,["ps"],output="")):
            guard.require_unused_uid(501)

    def test_pf_readback_is_mandatory(self):
        with patch.object(guard, "run", side_effect=["Status: Enabled", 'anchor "horde-lima/*" all', 'lo0', 'canonical block', '']):
            with self.assertRaisesRegex(ValueError, "readback"):
                guard.pf(POLICY, 501, "verify")
        with patch.object(guard, "run", side_effect=["Status: Enabled", 'anchor "horde-lima/*" all', 'lo0', 'canonical block', 'canonical block']):
            guard.pf(POLICY, 501, "verify")

    def test_pf_disables_automatic_tables_for_exact_rule_readback(self):
        with patch.object(guard, "run", side_effect=["Status: Enabled", 'anchor "horde-lima/*" all', 'lo0', 'canonical block', '', '', '', 'canonical block']) as runner:
            guard.pf(POLICY, 501, "apply")
        for index in [3, 6]:
            argv = runner.call_args_list[index].args[0]
            self.assertIn("-o", argv)
            self.assertEqual(argv[argv.index("-o") + 1], "none")

    def test_verify_does_not_create_host_directories(self):
        with patch.object(guard.os,"geteuid",return_value=0), patch.object(guard.sys,"argv",["guard","verify","hamster"]), patch.object(guard,"private_root"), patch.object(guard,"load_policy",return_value=(POLICY,None)), patch.object(guard.Path,"exists",return_value=False), patch.object(guard.Path,"mkdir") as mkdir:
            with self.assertRaisesRegex(ValueError,"has not been prepared"):
                guard.main()
            mkdir.assert_not_called()

    def test_new_managed_parent_is_traversable_under_private_bootstrap_umask(self):
        self.check_managed_parent_mode(existing=False, expected=0o755)

    def test_existing_managed_parent_permissions_are_not_silently_changed(self):
        self.check_managed_parent_mode(existing=True, expected=0o700)

    def check_managed_parent_mode(self, existing, expected):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "horde-lima"
            if existing:
                root.mkdir(mode=0o700)
            policy = dict(POLICY, home=str(root / POLICY["project"]))
            user = SimpleNamespace(pw_uid=os.getuid(), pw_gid=os.getgid())
            def path(value):
                return root if str(value) == "/var/lib/horde-lima" else Path(value)
            previous = os.umask(0o077)
            try:
                with patch.object(guard, "Path", side_effect=path), \
                     patch.object(guard.os, "geteuid", return_value=0), \
                     patch.object(guard.os, "chown"), \
                     patch.object(guard.sys, "argv", ["guard", "apply", POLICY["project"]]), \
                     patch.object(guard.sys, "platform", "linux"), \
                     patch.object(guard, "private_root"), \
                     patch.object(guard, "load_policy", return_value=(policy, user)), \
                     patch.object(guard, "nft"), contextlib.redirect_stdout(io.StringIO()):
                    guard.main()
            finally:
                os.umask(previous)
            self.assertEqual(stat.S_IMODE(root.stat().st_mode), expected)
            self.assertEqual(stat.S_IMODE(Path(policy["home"]).stat().st_mode), 0o700)

    def test_pf_disabled_host_fails_closed(self):
        with patch.object(guard, "run", return_value="Status: Disabled"):
            with self.assertRaisesRegex(ValueError, "enabled"):
                guard.pf(POLICY, 501, "apply")


if __name__ == "__main__":
    unittest.main()
