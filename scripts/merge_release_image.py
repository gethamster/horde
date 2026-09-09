#!/usr/bin/env python3
"""Verify architecture digests, publish their release index, and emit its digest."""
import argparse
import json
from pathlib import Path
import re
import subprocess
import sys

ARCHITECTURES = ('amd64', 'arm64')


def digest(value):
    if not isinstance(value, str) or not re.fullmatch(r'sha256:[0-9a-f]{64}', value):
        raise ValueError('expected a sha256 digest containing 64 lowercase hex digits')
    return value


def docker(*arguments):
    return subprocess.run(['docker', 'buildx', 'imagetools', *arguments],
                          check=True, capture_output=True, text=True).stdout


def inspect(reference, architectures):
    document = json.loads(docker('inspect', '--format', '{{json .Manifest}}', reference))
    if not isinstance(document, dict) or not isinstance(document.get('manifests'), list):
        raise ValueError(f'{reference}: expected a platform index; enable build provenance')
    platforms = []
    for entry in document['manifests']:
        if not isinstance(entry, dict) or not isinstance(entry.get('platform'), dict):
            raise ValueError(f'{reference}: descriptor is missing platform metadata')
        digest(entry.get('digest'))
        platform = entry['platform']
        pair = (platform.get('os'), platform.get('architecture'))
        annotations = entry.get('annotations')
        if pair == ('unknown', 'unknown') and isinstance(annotations, dict) and annotations.get(
                'vnd.docker.reference.type') == 'attestation-manifest':
            continue
        platforms.append(pair)
    expected = [('linux', architecture) for architecture in architectures]
    if len(platforms) != len(expected) or any(platforms.count(pair) != 1 for pair in expected):
        raise ValueError(f'{reference}: expected exactly {expected}, found {platforms}')
    members = frozenset(json.dumps({key: entry.get(key, {}) for key in
                                   ('digest', 'platform', 'annotations')}, sort_keys=True)
                        for entry in document['manifests'])
    return digest(document.get('digest')), members


def merge(image, tag, directory):
    if not re.fullmatch(r'[a-z0-9][a-z0-9._:/-]*', image) or ':' in image.rsplit('/', 1)[-1]:
        raise ValueError('image must be a repository name without a tag or digest')
    if not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}', tag):
        raise ValueError('invalid release image tag')
    # Read and validate every local result before invoking Docker.
    digests = [digest((Path(directory) / architecture).read_text().strip())
               for architecture in ARCHITECTURES]
    references = [f'{image}@{value}' for value in digests]
    source_members = set()
    for architecture, reference, expected in zip(ARCHITECTURES, references, digests):
        actual, members = inspect(reference, [architecture])
        if actual != expected:
            raise ValueError(f'{reference}: registry returned a different digest')
        source_members.update(members)
    target = f'{image}:{tag}'
    docker('create', '--tag', target, *references)
    merged, members = inspect(target, ARCHITECTURES)
    if members != source_members:
        raise ValueError('published index does not contain the verified source images')
    return merged


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('image')
    parser.add_argument('tag')
    parser.add_argument('digest_dir', type=Path)
    args = parser.parse_args(argv)
    print(f'digest={merge(args.image, args.tag, args.digest_dir)}')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f'Release image verification failed: {error}', file=sys.stderr)
        sys.exit(1)
