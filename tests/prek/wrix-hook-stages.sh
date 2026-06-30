#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  local reason="$1"

  echo "SKIP: $reason" >&2
  exit 77
}

require_tool() {
  local tool="$1"

  command -v "$tool" >/dev/null 2>&1 || skip "$tool not on PATH"
}

test_wrix_config_stage_set() {
  require_tool jq
  require_tool prek

  local actual expected
  expected=$'pre-commit:check-merge-conflict,end-of-file-fixer,shell-reexec-explicit-interpreter,trailing-whitespace,treefmt\npre-push:loom-gate-verify,nix-flake-check'
  actual="$(PREK_COLOR=never prek -C "$REPO_ROOT" -c "$REPO_ROOT/.pre-commit-config.yaml" list --output-format json --no-progress \
    | jq -r '[.[] as $hook | $hook.stages[] | { stage: ., id: $hook.id }] | group_by(.stage)[] | "\(.[0].stage):\(map(.id) | sort | join(","))"')"

  if [[ "$actual" != "$expected" ]]; then
    echo "FAIL: .pre-commit-config.yaml declares an unexpected stage→hook mapping" >&2
    echo "expected:" >&2
    printf '%s\n' "$expected" >&2
    echo "actual:" >&2
    printf '%s\n' "$actual" >&2
    return 1
  fi

  echo "PASS: Wrix hook config stage→hook mapping matches the spec"
}

if [[ $# -eq 0 ]]; then
  test_wrix_config_stage_set
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    echo "Unknown function: $fn" >&2
    exit 1
  fi
  "$fn"
fi
