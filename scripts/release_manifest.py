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
schema_declarations = re.findall(r'^pub const SCHEMA_VERSION: u32 = ([0-9]+);$',
                                (Path(__file__).resolve().parent.parent/'src/store.rs').read_text(), re.MULTILINE)
if len(schema_declarations) != 1:
    raise ValueError('Cannot identify the authoritative runtime schema')
source_schema = int(schema_declarations[0])
artifacts = []
compatibility = None
# The public release repository's verifier derives every artifact's file name
# as horde-<target>.tar, so the skills pack is horde-skills.tar (0.6.2 shipped
# skills.tar and the mirror refused the release). Installers select it by
# target, never by file name.
skills = root/'horde-skills.tar'
for archive in sorted(root.glob('horde-*.tar')):
    if archive == skills:
        continue
    target = archive.stem.removeprefix('horde-')
    record = json.loads((root/f'horde-{target}.compatibility.json').read_text())
    if (not isinstance(record, dict) or set(record) != {'version', 'protocol', 'schema_min', 'schema_max'}
            or record['version'] != version or record['protocol'] != 1
            or any(type(record[k]) is not int for k in ('protocol', 'schema_min', 'schema_max'))
            or record['schema_min'] != 2 or record['schema_max'] != source_schema
            or not 2 <= record['schema_max'] <= 0xffffffff):
        raise ValueError('Invalid compiled release compatibility')
    if compatibility is not None and record != compatibility:
        raise ValueError('Platform binaries disagree on release compatibility')
    compatibility = record
    artifacts.append(dict(target=target, sha256=hashlib.sha256(archive.read_bytes()).hexdigest(),
                          url=f'https://horde.sh/releases/v{version}/{archive.name}'))
assert len(artifacts) == 4, 'All four platform archives are required'
if skills.exists():
    artifacts.append(dict(target='skills', sha256=hashlib.sha256(skills.read_bytes()).hexdigest(),
                          url=f'https://horde.sh/releases/v{version}/horde-skills.tar'))
manifest = root/'manifest.json'
manifest.write_text(json.dumps(dict(**compatibility, artifacts=artifacts, image=os.environ["HORDE_RELEASE_IMAGE"]), sort_keys=True))
key = os.environ['HORDE_RELEASE_PRIVATE_KEY_FILE']
subprocess.run(['openssl','pkeyutl','-sign','-inkey',key,'-rawin','-in',str(manifest),'-out',str(root/'manifest.sig')], check=True)
pem = subprocess.check_output(['openssl','pkey','-in',key,'-pubout']).decode()
der = subprocess.check_output(['openssl','pkey','-in',key,'-pubout','-outform','DER'])
assert der[-32:].hex() == os.environ['HORDE_RELEASE_PUBLIC_KEY'], 'Signing key does not match embedded verification key'
(root/'install.sh').write_text(Path('website/scripts/install-template.sh').read_text().replace('@HORDE_RELEASE_PUBLIC_KEY_PEM@',pem.strip()))
(root/'SHA256SUMS').write_text(''.join(f"{a['sha256']}  {a['url'].rsplit('/', 1)[-1]}\n" for a in artifacts))

(root/'release-key.pem').write_text(pem)
(root/'release-key.hex').write_text(der[-32:].hex()+'\n')
