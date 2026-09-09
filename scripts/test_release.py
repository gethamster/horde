#!/usr/bin/env python3
"""Offline installer checks: signed fixtures, no provider requests or service changes."""
import io
import json
import os
from pathlib import Path
import platform
import subprocess
import tarfile
import tempfile
import unittest

REPO = Path(__file__).resolve().parent.parent


class InstallerTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='horde-release-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.dist = self.root/'dist'
        self.dist.mkdir()
        self.home = self.root/'home'
        self.home.mkdir()
        self.env = dict(os.environ, HOME=str(self.home))
        key = self.root/'key.pem'
        subprocess.run(['openssl','genpkey','-algorithm','ED25519','-out',str(key)], check=True, capture_output=True)
        der = subprocess.check_output(['openssl','pkey','-in',str(key),'-pubout','-outform','DER'])
        for target in ['x86_64-unknown-linux-musl','aarch64-unknown-linux-musl','x86_64-apple-darwin','aarch64-apple-darwin']:
            binary = b'#!/bin/sh\nif [ "$1" = update ]; then test "$2" = --version && test "$3" = 0.2.1 || exit 9; echo repair-update; exit 0; fi\necho "horde 0.2.1"\n'
            with tarfile.open(self.dist/f'horde-{target}.tar','w') as archive:
                info = tarfile.TarInfo('horde')
                info.size = len(binary)
                info.mode = 0o755
                archive.addfile(info,io.BytesIO(binary))
        self.env.update(HORDE_RELEASE_PRIVATE_KEY_FILE=str(key),HORDE_RELEASE_PUBLIC_KEY=der[-32:].hex(),
                        HORDE_RELEASE_IMAGE='ghcr.io/asomervell/horde@sha256:'+'0'*64)
        signed = subprocess.run(['python3','scripts/release_manifest.py','0.2.1',str(self.dist)],cwd=REPO,env=self.env,capture_output=True,text=True)
        self.assertEqual(signed.returncode, 0, signed.stderr)
        tools = self.root/'tools'
        tools.mkdir()
        curl = tools/'curl'
        curl.write_text('#!/usr/bin/env python3\nimport os,pathlib,shutil,sys\na=sys.argv[1:];url=next(x for x in a if x.startswith("https://"));assert url.startswith("https://horde.sh/releases/");shutil.copyfile(pathlib.Path(os.environ["FIXTURE_RELEASE"])/url.rsplit("/",1)[-1],a[a.index("-o")+1])\n')
        curl.chmod(0o755)
        self.env.update(PATH=str(tools)+os.pathsep+self.env['PATH'],FIXTURE_RELEASE=str(self.dist))

    def install(self):
        return subprocess.run(['sh',str(self.dist/'install.sh'),'--no-service'],env=self.env,capture_output=True,text=True)

    def test_signed_install_and_reinstall_preserves_existing_binary(self):
        result = self.install()
        self.assertEqual(result.returncode,0,result.stderr)
        launcher = self.home/'.local/bin/horde'
        self.assertEqual(subprocess.check_output([str(launcher),'--version'],env=self.env,text=True).strip(),'horde 0.2.1')
        current = self.home/'.local/share/horde-install/current'
        previous = current.resolve()
        self.assertNotEqual(self.install().returncode,0)
        self.assertEqual(current.resolve(),previous)

    def rewrite_archives(self, extra):
        for archive_path in self.dist.glob('horde-*.tar'):
            with tarfile.open(archive_path) as archive:
                binary = archive.extractfile('horde').read()
            with tarfile.open(archive_path, 'w') as archive:
                info = tarfile.TarInfo('horde'); info.size = len(binary); info.mode = 0o755
                archive.addfile(info, io.BytesIO(binary))
                for name, data, kind in extra:
                    info = tarfile.TarInfo(name); info.type = kind; info.mode = 0o6755
                    info.size = len(data) if kind == tarfile.REGTYPE else 0
                    if kind in (tarfile.SYMTYPE, tarfile.LNKTYPE): info.linkname = '../../outside'
                    archive.addfile(info, io.BytesIO(data))
        result = subprocess.run(['python3', 'scripts/release_manifest.py', '0.2.1', str(self.dist)],
                                cwd=REPO, env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_signed_skills_ship_with_binary_and_safe_permissions(self):
        body = b'---\nname: added-without-rust\ndescription: A new skill.\n---\nRun checks.\n'
        self.rewrite_archives([('skills/', b'', tarfile.DIRTYPE),
                               ('skills/added-without-rust/SKILL.md', body, tarfile.REGTYPE)])
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        installed = self.home/'.local/share/horde-install/current/skills/added-without-rust/SKILL.md'
        self.assertEqual(installed.read_bytes(), body)
        self.assertEqual(installed.stat().st_mode & 0o7777, 0o755)

    def test_unsafe_skill_archives_are_rejected(self):
        for name, kind in [('skills/../../outside', tarfile.REGTYPE),
                           ('skills/x/link', tarfile.SYMTYPE), ('skills/x/hard', tarfile.LNKTYPE),
                           ('unexpected', tarfile.REGTYPE), ('skills/x/device', tarfile.CHRTYPE),
                           ('skills/x/../alias', tarfile.REGTYPE), ('skills/x\\alias', tarfile.REGTYPE)]:
            with self.subTest(name=name):
                self.rewrite_archives([(name, b'bad', kind)])
                self.assertNotEqual(self.install().returncode, 0)
                self.assertFalse((self.home/'.local/bin/horde').exists())

    def test_duplicate_or_oversized_skill_files_are_rejected(self):
        item = ('skills/example/SKILL.md', b'body', tarfile.REGTYPE)
        for extra in [[item, item], [('skills/example/SKILL.md', b'x' * (1024 * 1024 + 1), tarfile.REGTYPE)]]:
            self.rewrite_archives(extra)
            self.assertNotEqual(self.install().returncode, 0)
            self.assertFalse((self.home/'.local/bin/horde').exists())

    def test_separate_signed_pack_preserves_old_updater_archive_contract(self):
        body = b'---\nname: example\ndescription: Example skill.\n---\nUse tools.\n'
        with tarfile.open(self.dist/'skills.tar', 'w') as archive:
            info = tarfile.TarInfo('skills/example/SKILL.md'); info.mode = 0o644; info.size = len(body)
            archive.addfile(info, io.BytesIO(body))
        self.rewrite_archives([])
        manifest = json.loads((self.dist/'manifest.json').read_text())
        self.assertEqual(len(manifest['artifacts']), 5)
        self.assertEqual(set(manifest), {'version', 'protocol', 'schema_min', 'schema_max', 'artifacts', 'image'})
        for artifact in manifest['artifacts']:
            self.assertEqual(set(artifact), {'target', 'url', 'sha256'})
            if artifact['target'] != 'skills':
                with tarfile.open(self.dist/artifact['url'].rsplit('/', 1)[-1]) as archive:
                    self.assertEqual([entry.name for entry in archive], ['horde'])
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stderr)
        installed = self.home/'.local/share/horde-install/current/skills/example/SKILL.md'
        self.assertEqual(installed.read_bytes(), body)

    def test_separate_pack_checksum_and_binary_injection_are_rejected(self):
        with tarfile.open(self.dist/'skills.tar', 'w') as archive:
            info = tarfile.TarInfo('horde'); info.size = 4
            archive.addfile(info, io.BytesIO(b'evil'))
        self.rewrite_archives([])
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Binary in skills artifact', result.stderr)
        (self.dist/'skills.tar').write_bytes(b'corrupt')
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('checksum mismatch', result.stderr)
        self.assertFalse((self.home/'.local/bin/horde').exists())

    def test_packaging_discovers_new_skill_without_binary_changes(self):
        source = self.root/'source-skills'
        (source/'new-skill').mkdir(parents=True)
        (source/'new-skill/SKILL.md').write_text('New instructions')
        binary = self.root/'horde'; binary.write_bytes(b'binary')
        output = self.root/'release.tar'
        result = subprocess.run(['python3', 'scripts/package_release.py', str(source), str(output)],
                                cwd=REPO, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        with tarfile.open(output) as archive:
            self.assertEqual(archive.extractfile('skills/new-skill/SKILL.md').read(), b'New instructions')
        (source/'new-skill/link').symlink_to(binary)
        result = subprocess.run(['python3', 'scripts/package_release.py', str(source), str(output)],
                                cwd=REPO, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)

    def test_bad_signature_creates_no_install(self):
        (self.dist/'manifest.json').write_text('{}')
        self.assertNotEqual(self.install().returncode,0)
        self.assertFalse((self.home/'.local/bin/horde').exists())

    def test_repair_uses_verified_new_updater(self):
        self.assertEqual(self.install().returncode, 0)
        result = subprocess.run(['sh',str(self.dist/'install.sh'),'--repair'],env=self.env,capture_output=True,text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('repair-update', result.stdout)
        (self.dist/'manifest.json').write_text('{}')
        result = subprocess.run(['sh',str(self.dist/'install.sh'),'--repair'],env=self.env,capture_output=True,text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('repair-update', result.stdout)

    def test_corrupt_archive_creates_no_install(self):
        for archive in self.dist.glob('*.tar'):
            archive.write_bytes(b'corrupt')
        self.assertNotEqual(self.install().returncode,0)
        self.assertFalse((self.home/'.local/bin/horde').exists())

    def test_optimized_python_cannot_disable_checksum_verification(self):
        self.env['PYTHONOPTIMIZE'] = '1'
        for archive in self.dist.glob('*.tar'):
            archive.write_bytes(b'corrupt')
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('checksum mismatch', result.stderr)
        self.assertFalse((self.home/'.local/bin/horde').exists())

    def test_public_key_assets_match_manifest_signer(self):
        self.assertEqual((self.dist/'release-key.hex').read_text().strip(), self.env['HORDE_RELEASE_PUBLIC_KEY'])
        result = subprocess.run(['openssl', 'pkeyutl', '-verify', '-pubin', '-inkey', str(self.dist/'release-key.pem'), '-rawin', '-in', str(self.dist/'manifest.json'), '-sigfile', str(self.dist/'manifest.sig')], capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads((self.dist/'manifest.json').read_text())
        self.assertEqual((manifest['schema_min'], manifest['schema_max']), (2, 5))
        self.assertTrue(all(a['url'].startswith('https://horde.sh/releases/v0.2.1/') for a in manifest['artifacts']))


if __name__ == '__main__':
    unittest.main()
