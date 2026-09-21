#!/usr/bin/env python3
"""Unit checks for live-suite network denial evidence; no VMs or network required."""
import contextlib
import errno
import io
import json
import socket
import subprocess
import unittest
from unittest.mock import patch, MagicMock
import test_lima_live as live


class GuestTcpProbeTests(unittest.TestCase):
    def reply(self, argv, **kwargs):
        address, port, nonce = argv[-3:]
        report = dict(version=1, nonce=nonce, address=address, port=int(port), attempted=True,
                      status='denied', reason='timeout')
        return subprocess.CompletedProcess(argv, 0, 'HORDE_TCP_PROBE:' + json.dumps(report) + '\n', '')

    def test_accepts_only_successful_transport_and_matching_guest_result(self):
        with patch.object(live, 'run', side_effect=self.reply) as run:
            live.assert_guest_denied(['sudo', 'limactl', 'shell', 'guest'], '192.0.2.10', 22)
        self.assertTrue(run.call_args.kwargs['check'])
        self.assertEqual(30, run.call_args.kwargs['timeout'])
        self.assertEqual('python3', run.call_args.args[0][-6])

    def test_transport_failures_cannot_count_as_denial(self):
        for code in (1, 124, 255):
            with self.subTest(code=code), patch.object(live, 'run', return_value=subprocess.CompletedProcess([], code, '', 'guest transport failed')):
                with self.assertRaises(RuntimeError):
                    live.assert_guest_denied(['sudo'], '192.0.2.10', 22)
        with patch.object(live, 'run', side_effect=subprocess.TimeoutExpired(['limactl'], 30)):
            with self.assertRaises(subprocess.TimeoutExpired):
                live.assert_guest_denied(['sudo'], '192.0.2.10', 22)

    def test_missing_malformed_wrong_nonce_and_connected_reports_fail(self):
        def altered(change):
            def reply(argv, **kwargs):
                original = self.reply(argv)
                report = json.loads(original.stdout.split(':', 1)[1])
                report.update(change)
                original.stdout = 'HORDE_TCP_PROBE:' + json.dumps(report) + '\n'
                return original
            return reply
        for data in ('', 'HORDE_TCP_PROBE:not-json\n'):
            with self.subTest(data=data), patch.object(live, 'run', return_value=subprocess.CompletedProcess([], 0, data, '')):
                with self.assertRaises(RuntimeError):
                    live.assert_guest_denied(['sudo'], '192.0.2.10', 22)
        for change in [dict(nonce='wrong'), dict(address='different'), dict(port=23), dict(version=2), dict(attempted=False),
                       dict(status='connected', reason=None), dict(status='inconclusive'), dict(reason='unknown')]:
            with self.subTest(change=change), patch.object(live, 'run', side_effect=altered(change)):
                with self.assertRaises(RuntimeError):
                    live.assert_guest_denied(['sudo'], '192.0.2.10', 22)

    def test_multiple_guest_results_are_ambiguous(self):
        def duplicate(argv, **kwargs):
            reply = self.reply(argv)
            reply.stdout += reply.stdout
            return reply
        with patch.object(live, 'run', side_effect=duplicate):
            with self.assertRaises(RuntimeError):
                live.assert_guest_denied(['sudo'], '192.0.2.10', 22)

    def test_guest_program_reports_actual_socket_outcomes(self):
        cases = [(None, 'connected', None), (socket.timeout(), 'denied', 'timeout'),
                 (OSError(errno.EACCES, 'denied'), 'denied', 'permission'),
                 (OSError(errno.ECONNREFUSED, 'refused'), 'denied', 'refused'),
                 (OSError(errno.ENETUNREACH, 'unreachable'), 'denied', 'unreachable'),
                 (OSError(errno.EBADF, 'bad descriptor'), 'inconclusive', 'socket-error')]
        for error, status, reason in cases:
            output = io.StringIO()
            with self.subTest(error=error), patch('sys.argv', ['probe', '192.0.2.10', '22', 'nonce']), \
                 patch('socket.create_connection', side_effect=error, return_value=MagicMock()) as connect, \
                 contextlib.redirect_stdout(output):
                exec(live.GUEST_TCP_PROBE, {})
            connect.assert_called_once_with(('192.0.2.10', 22), timeout=5)
            report = json.loads(output.getvalue().split(':', 1)[1])
            self.assertEqual(status, report['status'])
            self.assertEqual(reason, report['reason'])
            self.assertTrue(report['attempted'])


if __name__ == '__main__':
    unittest.main()
