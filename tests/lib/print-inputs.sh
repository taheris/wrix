#!/usr/bin/env bash
set -euo pipefail

verifier_print_inputs() {
  local definitions="$1"
  shift
  # Missing entries mean unknown, not an empty known declaration. Keep the
  # batch shape even for one target, so an opaque target cannot become [] by
  # accident. jq failures propagate to Loom's inputs-protocol audit.
  jq -cn --slurpfile definitions "$definitions" --args '
    {inputs: ($definitions[0] | with_entries(
      select(.key as $target | $ARGS.positional | length == 0 or index($target) != null)
    ))}
  ' "$@"
}
