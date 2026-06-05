# shellcheck shell=bash
# skip-if-missing <tool> -- <command> [args…]
#
# Renders a hook inert in contexts where its runtime dependency is absent:
# if `<tool>` resolves on PATH, exec the command; otherwise exit 0 silently.
# Tagging the dependency at the hook's `entry:` line keeps the knowledge
# co-located with the command instead of in a wrix-curated skip list.
#
# The literal `--` separator is required to disambiguate the tool name
# from the wrapped command's leading arguments.
set -euo pipefail

if [[ $# -lt 3 ]]; then
    echo "skip-if-missing: usage: skip-if-missing <tool> -- <command> [args…]" >&2
    exit 2
fi

tool="$1"
shift

if [[ "$1" != "--" ]]; then
    echo "skip-if-missing: missing '--' separator between <tool> and <command>" >&2
    exit 2
fi
shift

if ! command -v "$tool" >/dev/null; then
    exit 0
fi

exec "$@"
