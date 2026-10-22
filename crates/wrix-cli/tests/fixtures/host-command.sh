#!/usr/bin/env bash
set -euo pipefail
# Only explicit host fixtures use this adapter. Never change the outer worker
# marker, or turn normal worker-denial commands into host commands.
if [[ "${1:-}" == --private-host ]]; then
  shift
  mount -t tmpfs -o mode=755 tmpfs /etc/wrix
  exec "$@"
fi
if [[ -f /etc/wrix/image-agent ]]; then
  exec unshare --user --map-root-user --mount --propagation private -- \
    bash "${BASH_SOURCE[0]}" --private-host "$@"
fi
exec "$@"
