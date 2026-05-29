# shellcheck shell=bash
# pre-push-checks <command> [args…]
#
# Wraps a slow pre-push check with a marker-aware short-circuit. Consults
# `loom gate verify-marker` to decide whether to skip the wrapped command;
# the wrapper never reads or interprets `.wrapix/loom/marker.json` itself.
#
# Resolution order (per specs/pre-commit.md § pre-push-checks):
#   1. marker absent in PWD          → exec "$@"
#   2. `loom` missing from PATH      → exec "$@"
#   3. `loom gate verify-marker` 0   → exit 0 (short-circuit)
#   4. `loom gate verify-marker` !=0 → exec "$@"
#
# Self-reference caveat: downstream MUST NOT wire `loom gate verify-marker`
# itself through this wrapper. The marker check is the canonical decision
# input the wrapper consults; routing it through `pre-push-checks` would be
# self-referential. Position it as a plain hook
# (`entry: loom gate verify-marker`).
set -euo pipefail

if [[ $# -eq 0 ]]; then
    echo "pre-push-checks: usage: pre-push-checks <command> [args…]" >&2
    exit 2
fi

if [[ ! -f .wrapix/loom/marker.json ]]; then
    exec "$@"
fi

if ! command -v loom >/dev/null; then
    exec "$@"
fi

if loom gate verify-marker; then
    exit 0
fi

exec "$@"
