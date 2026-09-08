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
            binary = b'#!/bin/sh\necho "horde 0.2.1"\n'
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

    def test_bad_signature_creates_no_install(self):
        (self.dist/'manifest.json').write_text('{}')
        self.assertNotEqual(self.install().returncode,0)
        self.assertFalse((self.home/'.local/bin/horde').exists())

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
        self.assertTrue(all(a['url'].startswith('https://horde.sh/releases/v0.2.1/') for a in manifest['artifacts']))


if __name__ == '__main__':
    unittest.main()
