#!/usr/bin/env python3
"""Build release metadata; private signing material stays in CI's temporary directory."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys

version = sys.argv[1]
assert re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?", version)
root = Path(sys.argv[2])
artifacts = []
for archive in sorted(root.glob('horde-*.tar')):
    target = archive.stem.removeprefix('horde-')
    artifacts.append(dict(target=target, sha256=hashlib.sha256(archive.read_bytes()).hexdigest(),
                          url=f'https://horde.sh/releases/v{version}/{archive.name}'))
assert len(artifacts) == 4, 'All four platform archives are required'
manifest = root/'manifest.json'
manifest.write_text(json.dumps(dict(version=version, protocol=1, schema_min=2, schema_max=2, artifacts=artifacts, image=os.environ["HORDE_RELEASE_IMAGE"]), sort_keys=True))
key = os.environ['HORDE_RELEASE_PRIVATE_KEY_FILE']
subprocess.run(['openssl','pkeyutl','-sign','-inkey',key,'-rawin','-in',str(manifest),'-out',str(root/'manifest.sig')], check=True)
pem = subprocess.check_output(['openssl','pkey','-in',key,'-pubout']).decode()
der = subprocess.check_output(['openssl','pkey','-in',key,'-pubout','-outform','DER'])
assert der[-32:].hex() == os.environ['HORDE_RELEASE_PUBLIC_KEY'], 'Signing key does not match embedded verification key'
(root/'install.sh').write_text(Path('website/scripts/install-template.sh').read_text().replace('@HORDE_RELEASE_PUBLIC_KEY_PEM@',pem.strip()))
(root/'SHA256SUMS').write_text(''.join(f"{a['sha256']}  horde-{a['target']}.tar\n" for a in artifacts))

(root/'release-key.pem').write_text(pem)
(root/'release-key.hex').write_text(der[-32:].hex()+'\n')
