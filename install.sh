#!/bin/sh
# Source checkout entry point; release CI publishes the rendered standalone installer.
set -eu
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
exec sh "$script_dir/website/scripts/install-template.sh" "$@"
