#!/bin/sh
# Mirror skills/ and skills.sh.json into the public horde-skills repository.
# The public repository keeps its own README; skills/README.md stays here.
# Usage: scripts/sync_skills.sh /path/to/horde-skills-checkout
set -eu
target=${1:?usage: sync_skills.sh /path/to/horde-skills-checkout}
source_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
[ -d "$target/.git" ] || { echo "Not a Git checkout: $target" >&2; exit 1; }

rm -rf "$target/skills"
cp -R "$source_root/skills" "$target/skills"
rm -f "$target/skills/README.md"
cp "$source_root/skills.sh.json" "$target/skills.sh.json"

echo "Mirrored into $target. Review, then commit and push there."
git -C "$target" status --short
