#!/bin/bash
set -euo pipefail

# krun-init.sh: initialization for krun microVM
#
# Called by krun-relay inside a real PTY. Sets up LD_PRELOAD for UID
# spoofing (krun maps host user to root; claude refuses root) and
# configures git before handing off to entrypoint.sh.

# Pre-configure git safe.directory while still real root (before LD_PRELOAD).
# After LD_PRELOAD, git sees uid 1000 but /workspace is owned by root,
# which triggers git's "dubious ownership" check.
git config --global safe.directory /workspace

# Reconstruct container command args from launcher env var
ARGS=()
if [[ -n "${WRIX_KRUN_CMD:-}" ]]; then
  eval "ARGS=($WRIX_KRUN_CMD)"
fi

# Activate UID spoofing (terminal handled by krun-relay's real PTY)
export LD_PRELOAD=/lib/libfakeuid.so

exec /entrypoint.sh "${ARGS[@]}"
