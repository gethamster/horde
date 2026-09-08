#!/bin/sh
# Use as the start command of an E2B template or Daytona snapshot after installing
# an official release with install.sh --no-service. Storage must survive restarts.
set -u
export HORDE_SUPERVISED=1
horde_data_dir=${HORDE_DATA_DIR:-}
set --
[ -z "$horde_data_dir" ] || set -- --data-dir "$horde_data_dir"
child=''
stop() {
  if [ -n "$child" ]; then kill -TERM "$child" 2>/dev/null || true; wait "$child" 2>/dev/null || true; fi
  exit 0
}
trap stop TERM INT
while :; do
  "$HOME/.local/bin/horde" "$@" daemon &
  child=$!
  wait "$child"
  result=$?
  child=''
  [ "$result" -ne 0 ] || exit 0
  sleep 2
done
