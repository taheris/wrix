#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

skip() {
  local reason="$1"

  echo "SKIP: $reason" >&2
  exit 77
}

command -v nix >/dev/null 2>&1 || skip "nix not on PATH"
command -v jq >/dev/null 2>&1 || skip "jq not on PATH"
command -v nix-store >/dev/null 2>&1 || skip "nix-store not on PATH"

SYSTEM="$(nix eval --raw --impure --no-warn-dirty --expr 'builtins.currentSystem')"
HEAVY_DERIVATION_RE='(^|-)stream-wrix-|(^|-)wrix-base-image|(^|-)wrix-stable-profile-|(^|-)wrix-agent-|(^|-)wrix-base($|[. -])|(^|-)wrix-(0[.]1[.]0|deps|clippy|nextest)'
WORK_DIR="$(mktemp -d -t wrix-ci-only-heavy.XXXXXX)"
cleanup() {
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

CHECKS_MAP="$WORK_DIR/checks.json"
CI_CHECKS_MAP="$WORK_DIR/ci-checks.json"
APPS_MAP="$WORK_DIR/apps.json"
LISTED_TARGETS="$WORK_DIR/test-ci-list.txt"
LISTED_CHECKS="$WORK_DIR/listed-checks.txt"
LISTED_APPS="$WORK_DIR/listed-apps.txt"
HEAVY_CHECKS="$WORK_DIR/heavy-checks.txt"
HEAVY_CI_CHECKS="$WORK_DIR/heavy-ci-checks.txt"
HEAVY_APPS="$WORK_DIR/heavy-apps.txt"
MISSING_CI_CHECKS="$WORK_DIR/missing-ci-checks.txt"
MISSING_APPS="$WORK_DIR/missing-apps.txt"

write_derivation_map() {
  local flake_ref="$1"
  local output="$2"

  nix eval --json --no-warn-dirty \
    --apply 'attrs: builtins.mapAttrs (name: drv: drv.drvPath) attrs' \
    "$flake_ref" > "$output"
}

write_app_program_map() {
  local flake_ref="$1"
  local output="$2"

  nix eval --json --no-warn-dirty \
    --apply 'apps: builtins.mapAttrs (name: app: app.program) apps' \
    "$flake_ref" > "$output"
}

is_heavy_derivation() {
  local drv="$1"

  nix-store -q --requisites --include-outputs "$drv" \
    | sed 's|.*/||' \
    | awk -v re="$HEAVY_DERIVATION_RE" '$0 ~ re { found = 1 } END { exit found ? 0 : 1 }'
}

write_heavy_derivations() {
  local map_file="$1"
  local output="$2"
  local name drv

  : > "$output"
  while IFS=$'\t' read -r name drv; do
    if is_heavy_derivation "$drv"; then
      printf '%s\n' "$name" >> "$output"
    fi
  done < <(jq -r 'to_entries[] | [.key, .value] | @tsv' "$map_file")
  sort -u -o "$output" "$output"
}

app_root_from_program() {
  local program="$1"

  if [[ "$program" != */bin/* ]]; then
    return 1
  fi
  printf '%s\n' "${program%/bin/*}"
}

write_heavy_apps() {
  local map_file="$1"
  local output="$2"
  local name program root drv

  : > "$output"
  while IFS=$'\t' read -r name program; do
    [[ "$name" = "test-ci" ]] && continue
    if ! root="$(app_root_from_program "$program")"; then
      echo "FAIL: apps.$SYSTEM.$name program is not under /bin: $program" >&2
      return 1
    fi
    drv="$(nix path-info --derivation "$root")"
    if is_heavy_derivation "$drv"; then
      printf '%s\n' "$name" >> "$output"
    fi
  done < <(jq -r 'to_entries[] | [.key, .value] | @tsv' "$map_file")
  sort -u -o "$output" "$output"
}

write_listed_targets() {
  local listed_targets="$1"
  local listed_checks="$2"
  local listed_apps="$3"

  nix run --no-warn-dirty "$REPO_ROOT#test-ci" -- --list > "$listed_targets"
  awk '$1 == "check" { print $2 }' "$listed_targets" | sort -u > "$listed_checks"
  awk '$1 == "app" { print $2 }' "$listed_targets" | sort -u > "$listed_apps"
}

fail_if_file_nonempty() {
  local message="$1"
  local file="$2"

  if [[ -s "$file" ]]; then
    echo "FAIL: $message: $(paste -sd ', ' "$file")" >&2
    return 1
  fi
  return 0
}

failed=0

if ! jq -e --arg app "test-ci" 'index($app)' \
  < <(nix eval --json --no-warn-dirty --apply 'builtins.attrNames' "$REPO_ROOT#apps.$SYSTEM") \
  >/dev/null; then
  echo "FAIL: apps.$SYSTEM.test-ci is missing" >&2
  failed=$((failed + 1))
fi

write_derivation_map "$REPO_ROOT#checks.$SYSTEM" "$CHECKS_MAP"
write_derivation_map "$REPO_ROOT#legacyPackages.$SYSTEM.ciChecks" "$CI_CHECKS_MAP"
write_app_program_map "$REPO_ROOT#apps.$SYSTEM" "$APPS_MAP"
write_heavy_derivations "$CHECKS_MAP" "$HEAVY_CHECKS"
write_heavy_derivations "$CI_CHECKS_MAP" "$HEAVY_CI_CHECKS"
write_heavy_apps "$APPS_MAP" "$HEAVY_APPS" || failed=$((failed + 1))
write_listed_targets "$LISTED_TARGETS" "$LISTED_CHECKS" "$LISTED_APPS"

if ! fail_if_file_nonempty "checks.$SYSTEM contains heavy realization checks" "$HEAVY_CHECKS"; then
  failed=$((failed + 1))
fi

comm -23 "$HEAVY_CI_CHECKS" "$LISTED_CHECKS" > "$MISSING_CI_CHECKS"
if ! fail_if_file_nonempty "test-ci does not build CI-only heavy checks" "$MISSING_CI_CHECKS"; then
  failed=$((failed + 1))
fi

comm -23 "$HEAVY_APPS" "$LISTED_APPS" > "$MISSING_APPS"
if ! fail_if_file_nonempty "test-ci does not run CI-only heavy apps" "$MISSING_APPS"; then
  failed=$((failed + 1))
fi

if [[ ! -s "$HEAVY_CI_CHECKS" && ! -s "$HEAVY_APPS" ]]; then
  echo "FAIL: no heavy CI-only targets were detected from derivation input closures" >&2
  failed=$((failed + 1))
fi

if [[ "$failed" -ne 0 ]]; then
  exit 1
fi

echo "PASS: heavy realization checks are CI-only apps/checks"
