#!/usr/bin/env bash
set -euo pipefail

parallel_verifier_directory=""
parallel_verifier_pid=""
parallel_verifier_work=""
parallel_verifier_total=0
declare -a parallel_verifier_targets=()
declare -A parallel_verifier_counts=() parallel_verifier_indices=() parallel_verifier_batches=() parallel_verifier_jobs=()

parallel_verifiers_cleanup() {
  if [[ -n "$parallel_verifier_pid" ]]; then
    touch "$parallel_verifier_work/cancel"
    # Cancellation is expected to fail; cleanup does not convert it into acceptance.
    if wait "$parallel_verifier_pid"; then :; else :; fi
    parallel_verifier_pid=""
  fi
  if [[ -n "$parallel_verifier_directory" ]]; then
    rm -rf "$parallel_verifier_directory"
  fi
}

parallel_verifiers_prepare() {
  local raw target
  for raw in "$@"; do
    target="$(normalize_target "$raw")"
    if parallel_verifier_target "$target"; then
      parallel_verifier_counts["$target"]=$((${parallel_verifier_counts[$target]:-0} + 1))
      parallel_verifier_jobs["$target"]+="$parallel_verifier_total "
      parallel_verifier_targets+=("$target")
      parallel_verifier_total=$((parallel_verifier_total + 1))
    fi
  done
}

parallel_verifiers_batch() {
  local source="$1" system="$2" target tools script cache parent="$BASHPID"
  parallel_verifier_directory="$(mktemp -d -t wrix-parallel-verifiers.XXXXXX)"
  parallel_verifier_work="$parallel_verifier_directory/all"
  mkdir "$parallel_verifier_work"
  tools="$(nix build --max-jobs 1 --cores 2 --no-link --print-out-paths --no-warn-dirty "$source#legacyPackages.$system.verifierParallelTools")" || return 1
  printf '%s\n' "${SELF%/bin/*}" "$tools" > "$parallel_verifier_work/manifest"
  # Each distinct script keeps its own temporary GC root until the batch lease
  # has adopted every script. Only explicitly parallel-safe owners enter here.
  for target in "${!parallel_verifier_counts[@]}"; do
    script="$(verify_script_path "$source" "$system" "$target")" || return 1
    printf '%s\n' "$script" >> "$parallel_verifier_work/manifest"
  done
  printf '%s\n' "${parallel_verifier_targets[@]}" | jq -Rn '[inputs]' > "$parallel_verifier_work/targets.json"
  cache="${XDG_CACHE_HOME:-$HOME/.cache}/wrix/parallel-verifiers"
  (umask 077; mkdir -p "${cache%/*}")
  if [[ ! -e "$cache" ]]; then
    "$tools/bin/wrix-verification-roots" init "$cache" || return 1
  fi
  "$tools/bin/wrix-verification-roots" run "$cache" "$parallel_verifier_work/manifest" -- \
    "$tools/bin/wrix-parallel-verifiers" "$parallel_verifier_work" "$SELF" '@targets' \
    "$parallel_verifier_total" "$parent" &
  parallel_verifier_pid="$!"
  local status=0
  wait "$parallel_verifier_pid" || status="$?"
  parallel_verifier_pid=""
  return "$status"
}

parallel_verifier_run() {
  local source="$1" system="$2" target="$3" index work status=0 ordinal
  local -a indices
  if [[ ! -v 'parallel_verifier_batches[all]' ]]; then
    parallel_verifiers_batch "$source" "$system" || status="$?"
    parallel_verifier_batches[all]="$status"
  fi
  if [[ "${parallel_verifier_batches[all]}" -ne 0 ]]; then
    return "${parallel_verifier_batches[all]}"
  fi
  ordinal="${parallel_verifier_indices[$target]:-0}"
  read -r -a indices <<< "${parallel_verifier_jobs[$target]}"
  index="${indices[$ordinal]}"
  parallel_verifier_indices["$target"]=$((ordinal + 1))
  work="$parallel_verifier_work"
  cat "$work/$index.stdout"
  cat "$work/$index.stderr" >&2
  status="$(< "$work/$index.status")"
  [[ "$status" =~ ^[0-9]+$ && "$status" -le 255 ]] || return 1
  return "$status"
}
