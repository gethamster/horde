#!/usr/bin/env python3
"""Package editable default skills without a compiled name list."""
import argparse
from pathlib import Path
import tarfile


def package_skills(skills, output):
    if not skills.is_dir() or skills.is_symlink():
        raise ValueError('Skills must be a regular directory')
    paths = sorted(skills.rglob('*'))
    if len(paths) + 2 > 4096:
        raise ValueError('Too many release skill entries')
    total = 0
    seen = set()
    for path in paths:
        if path.is_symlink() or not (path.is_dir() or path.is_file()):
            raise ValueError(f'Only regular skill files and directories are allowed: {path}')
        name = path.relative_to(skills).as_posix()
        if (name.lower() in seen or any(c in name for c in '\\:')
                or any(ord(c) < 32 or ord(c) == 127 for c in name)):
            raise ValueError(f'Unsafe or duplicate skill path: {path}')
        seen.add(name.lower())
        if path.is_file():
            size = path.stat().st_size
            total += size
            if size > 1024*1024 or total > 8*1024*1024:
                raise ValueError('Release skills exceed size limits')
    with tarfile.open(output, 'w', format=tarfile.USTAR_FORMAT) as archive:
        for path, name in [(skills, 'skills')] + [
                (path, 'skills/' + path.relative_to(skills).as_posix()) for path in paths]:
            info = archive.gettarinfo(str(path), arcname=name)
            info.uid = info.gid = info.mtime = 0
            info.uname = info.gname = ''
            info.mode = 0o755 if info.isdir() or name == 'horde' or info.mode & 0o111 else 0o644
            if info.isfile():
                with path.open('rb') as source:
                    archive.addfile(info, source)
            else:
                archive.addfile(info)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('skills', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    package_skills(args.skills, args.output)
