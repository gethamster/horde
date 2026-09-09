#!/usr/bin/env python3
"""Offline checks for publishing a verified multi-platform release image index."""
import contextlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import merge_release_image as merge

IMAGE = 'ghcr.io/example/horde'
AMD64 = 'sha256:' + 'a' * 64
ARM64 = 'sha256:' + 'b' * 64
MERGED = 'sha256:' + 'c' * 64


def descriptor(architecture, attestation=False):
    return dict(digest=AMD64, platform=dict(os='unknown' if architecture == 'unknown' else 'linux', architecture=architecture),
                annotations={'vnd.docker.reference.type': 'attestation-manifest'} if attestation else {})


def manifest(digest, *architectures):
    return dict(digest=digest, manifests=[descriptor(arch) for arch in architectures])


class MergeImageTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='horde-image-merge-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / 'amd64').write_text(AMD64 + '\n')
        (self.root / 'arm64').write_text(ARM64 + '\n')
        self.documents = [manifest(AMD64, 'amd64'), manifest(ARM64, 'arm64'), manifest(MERGED, 'amd64', 'arm64')]
        self.inspections = iter(self.documents)
        self.docker = patch.object(merge.subprocess, 'run', side_effect=self.run_docker).start()
        self.addCleanup(patch.stopall)

    def run_docker(self, command, **kwargs):
        self.assertEqual(command[:3], ['docker', 'buildx', 'imagetools'])
        self.assertTrue(kwargs['check'])
        data = next(self.inspections) if command[3] == 'inspect' else None
        return subprocess.CompletedProcess(command, 0, json.dumps(data) if data else 'created\n', '')

    def publish(self):
        return merge.merge(IMAGE, 'v1.2.3', self.root)

    def assert_no_create(self):
        self.assertFalse(any(call.args[0][3] == 'create' for call in self.docker.call_args_list))

    def test_missing_or_invalid_digest_prevents_all_docker_calls(self):
        for contents in [None, '', 'sha256:abc', 'sha256:' + 'g' * 64, AMD64 + '\n' + ARM64]:
            with self.subTest(contents=contents):
                path = self.root / 'arm64'
                if path.exists():
                    path.unlink()
                if contents is not None:
                    path.write_text(contents)
                with self.assertRaises((ValueError, OSError)):
                    self.publish()
                self.docker.assert_not_called()

    def test_wrong_extra_or_duplicate_platform_prevents_publication(self):
        for platforms in [('arm64',), ('amd64', 'arm64'), ('amd64', 'amd64')]:
            with self.subTest(platforms=platforms):
                self.inspections = iter([manifest(AMD64, *platforms)])
                with self.assertRaises(ValueError):
                    self.publish()
                self.assert_no_create()

    def test_unknown_platform_requires_attestation_annotation(self):
        self.documents[0]['manifests'].append(descriptor('unknown'))
        with self.assertRaises(ValueError):
            self.publish()
        self.assert_no_create()

    def test_single_manifest_without_verified_platform_is_rejected(self):
        self.inspections = iter([dict(digest=AMD64)])
        with self.assertRaises(ValueError):
            self.publish()
        self.assert_no_create()

    def test_source_inspection_must_match_requested_digest(self):
        self.documents[0]['digest'] = MERGED
        with self.assertRaises(ValueError):
            self.publish()
        self.assert_no_create()

    def test_attestations_are_allowed_and_final_index_digest_is_emitted(self):
        for document in self.documents:
            document['manifests'].append(descriptor('unknown', attestation=True))
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            merge.main([IMAGE, 'v1.2.3', str(self.root)])
        self.assertEqual(output.getvalue(), f'digest={MERGED}\n')
        self.assertNotIn(AMD64, output.getvalue())
        self.assertNotIn(ARM64, output.getvalue())
        commands = [call.args[0] for call in self.docker.call_args_list]
        self.assertEqual(commands[2], ['docker', 'buildx', 'imagetools', 'create', '--tag', f'{IMAGE}:v1.2.3', f'{IMAGE}@{AMD64}', f'{IMAGE}@{ARM64}'])
        self.assertEqual(commands[3][-1], f'{IMAGE}:v1.2.3')
        self.assertEqual(commands[0][4:6], ['--format', '{{json .Manifest}}'])

    def test_final_index_requires_both_platforms_once_and_valid_digest(self):
        for document in [manifest(MERGED, 'amd64'), manifest(MERGED, 'amd64', 'arm64', 'arm64'), manifest('invalid', 'amd64', 'arm64')]:
            with self.subTest(document=document):
                self.inspections = iter([manifest(AMD64, 'amd64'), manifest(ARM64, 'arm64'), document])
                output = io.StringIO()
                with contextlib.redirect_stdout(output), self.assertRaises(ValueError):
                    merge.main([IMAGE, 'v1.2.3', str(self.root)])
                self.assertEqual(output.getvalue(), '')

    def test_same_platform_with_different_content_cannot_be_signed(self):
        self.documents[2]['manifests'][0]['digest'] = 'sha256:' + 'd' * 64
        output = io.StringIO()
        with contextlib.redirect_stdout(output), self.assertRaises(ValueError):
            merge.main([IMAGE, 'v1.2.3', str(self.root)])
        self.assertEqual(output.getvalue(), '')

    def test_docker_failure_propagates_without_digest_output(self):
        self.docker.side_effect = subprocess.CalledProcessError(1, ['docker'])
        output = io.StringIO()
        with contextlib.redirect_stdout(output), self.assertRaises(subprocess.CalledProcessError):
            merge.main([IMAGE, 'v1.2.3', str(self.root)])
        self.assertEqual(output.getvalue(), '')
        self.assert_no_create()


if __name__ == '__main__':
    unittest.main()
