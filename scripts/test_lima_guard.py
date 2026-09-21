import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("lima_guard", Path(__file__).with_name("horde-lima-guard.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

POLICY = {"project": "hamster", "user": "horde-hamster", "home": "/var/lib/horde-lima/hamster", "egress": ["203.0.113.9/32", "2001:db8::1/128"]}


def nft_state(policy=POLICY):
    _, commands = guard.nft_rules(policy, 501)
    return json.dumps({"nftables": [c["add"] for c in commands]})


class GuardTests(unittest.TestCase):
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

    def test_pf_disabled_host_fails_closed(self):
        with patch.object(guard, "run", return_value="Status: Disabled"):
            with self.assertRaisesRegex(ValueError, "enabled"):
                guard.pf(POLICY, 501, "apply")


if __name__ == "__main__":
    unittest.main()
