#!/usr/bin/env bash
set -euo pipefail

main() {
  local joined_targets="${1:-}"
  local filter

  if [[ "$joined_targets" == "--print-filter" ]]; then
    nextest_filter "${2:-}"
    return
  fi

  filter="$(nextest_filter "$joined_targets")"
  if [[ -z "$filter" ]]; then
    printf 'loom nextest target adapter: no test targets supplied\n' >&2
    return 2
  fi

  NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1 cargo nextest run -E "$filter" --message-format=libtest-json
}

nextest_filter() {
  local joined_targets="$1"
  local old_ifs="$IFS"
  local raw_targets=()
  local raw_target
  local target
  local name
  local filter=""

  IFS='|'
  read -r -a raw_targets <<< "$joined_targets"
  IFS="$old_ifs"

  for raw_target in "${raw_targets[@]}"; do
    target="${raw_target//[[:space:]]/}"
    if [[ -z "$target" ]]; then
      continue
    fi
    name="$target"
    if [[ "$target" =~ ^(\.\./)?crates/[^/]+/tests/[^/]+\.rs::(.+)$ ]]; then
      name="${BASH_REMATCH[2]}"
    fi
    if [[ -n "$filter" ]]; then
      filter+=" + "
    fi
    filter+="test($name)"
  done

  printf '%s\n' "$filter"
}

main "$@"
