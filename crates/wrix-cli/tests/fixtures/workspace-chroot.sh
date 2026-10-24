#!/usr/bin/env bash
set -euo pipefail

mount_bin="$1"
root="$2"
shift 2

for device in urandom null; do
  "$mount_bin" --bind "/dev/$device" "$root/dev/$device"
done

exec "$@"
