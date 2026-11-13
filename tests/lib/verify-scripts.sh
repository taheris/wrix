#!/usr/bin/env bash
set -euo pipefail

verify_script_directory=""
verify_script_evaluations=""
declare -A verify_script_derivations=()

verify_scripts_cleanup() {
  if [[ -n "$verify_script_directory" ]]; then
    rm -rf "$verify_script_directory"
  fi
}

verify_scripts_prepare() {
  local source="$1" system="$2" target index names
  shift 2
  local selected=() installables=() derivations=()
  local -A seen=()
  for target in "$@"; do
    if [[ ! -v "seen[$target]" ]]; then
      seen["$target"]=1
      selected+=("$target")
      installables+=("$source#legacyPackages.$system.verifyScripts.\"$target\"")
    fi
  done
  if [[ "${#selected[@]}" -lt 2 ]]; then
    return 0
  fi
  verify_script_directory=$(mktemp -d -t wrix-verify-scripts.XXXXXX)
  names=$(printf '%s\n' "${selected[@]}" | jq -Rn '[inputs | select(test("^[a-z0-9.-]+$"))] | map(tojson) | join(" ")')
  if nix eval --json --no-warn-dirty "$source#legacyPackages.$system.verifyEvaluations" --apply "values: builtins.listToAttrs (map (name: { inherit name; value = builtins.hasAttr name values && (builtins.tryEval values.\${name}).success; }) [$names])" > "$verify_script_directory/evaluations.json" &&
    jq -e 'type == "object" and all(.[]; type == "boolean")' "$verify_script_directory/evaluations.json" > /dev/null; then
    verify_script_evaluations="$verify_script_directory/evaluations.json"
  fi
  if ! nix build --dry-run --json --no-link --no-warn-dirty "${installables[@]}" > "$verify_script_directory/plan.json"; then
    return 0 # Planning failure preserves independent original script builds.
  fi
  if ! jq -e 'type == "array" and all(.[]; (.drvPath | type) == "string")' "$verify_script_directory/plan.json" > /dev/null; then
    return 0 # Unsupported metadata retains the original per-target diagnostics.
  fi
  if ! WRIX_VERIFY_DERIVATIONS="$(jq -c 'map(.drvPath)' "$verify_script_directory/plan.json")" \
    nix-instantiate --impure --add-root "$verify_script_directory/derivation" --indirect --expr '
      map (path: import (builtins.storePath path))
        (builtins.fromJSON (builtins.getEnv "WRIX_VERIFY_DERIVATIONS"))
    ' > "$verify_script_directory/derivations"; then
    return 0 # Missing metadata roots retain the original independent path.
  fi
  mapfile -t derivations < "$verify_script_directory/derivations"
  if [[ "${#derivations[@]}" -ne "${#selected[@]}" ]]; then
    return 0 # Ambiguous cardinality cannot select a verifier script.
  fi
  for index in "${!selected[@]}"; do
    verify_script_derivations["${selected[$index]}"]="$(readlink -f "${derivations[$index]}")"
  done
}

verify_script_path() {
  local source="$1" system="$2" target="$3" script
  if [[ -v "verify_script_derivations[$target]" ]]; then
    rm -f "$verify_script_directory"/current*
    if nix build --no-warn-dirty --out-link "$verify_script_directory/current" "${verify_script_derivations[$target]}^*"; then
      script=$(readlink -f "$verify_script_directory/current")
      if [[ -f "$script" && "$script" == /nix/store/*-verify-"$target".sh ]]; then
        printf '%s\n' "$script"
        return 0
      fi
    fi
  fi
  nix build --no-link --print-out-paths --no-warn-dirty "$source#legacyPackages.$system.verifyScripts.\"$target\""
}

verify_evaluation_succeeded() {
  [[ -n "${verify_evaluation_results:-}" ]] &&
    jq -e --arg target "$1" '.[$target] == true' "$verify_evaluation_results" > /dev/null
}

verify_script_run() {
  local script definitions self="${SELF:-$0}"
  script="$(verify_script_path "$1" "$2" "$3")" || return 1
  definitions="$(declare -f)"
  SELF="$self" bash -euo pipefail -c "$definitions"$'\nverify_execute_source() { local verify_evaluation_results="$2"; source "$1"; }\nverify_execute_source "$1" "$2"' "$self" "$script" "$verify_script_evaluations"
}
