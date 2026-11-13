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
CI_APPS_MAP="$WORK_DIR/ci-apps.json"
LISTED_TARGETS="$WORK_DIR/test-ci-list.txt"
LISTED_CHECKS="$WORK_DIR/listed-checks.txt"
LISTED_APPS="$WORK_DIR/listed-apps.txt"
HEAVY_CHECKS="$WORK_DIR/heavy-checks.txt"
HEAVY_CI_CHECKS="$WORK_DIR/heavy-ci-checks.txt"
HEAVY_APPS="$WORK_DIR/heavy-apps.txt"
MISSING_CI_CHECKS="$WORK_DIR/missing-ci-checks.txt"
MISSING_APPS="$WORK_DIR/missing-apps.txt"
VERIFY_TARGETS="$WORK_DIR/verify-targets.txt"
IMAGE_VERIFY_TARGETS="$WORK_DIR/image-verify-targets.txt"
EXPECTED_IMAGE_VERIFY_TARGETS="$WORK_DIR/expected-image-verify-targets.txt"
HEAVY_GENERIC_VERIFY_TARGETS="$WORK_DIR/heavy-generic-verify-targets.txt"
ANNOTATED_CI_APPS="$WORK_DIR/annotated-ci-apps.txt"
UNKNOWN_ANNOTATED_CI_APPS="$WORK_DIR/unknown-annotated-ci-apps.txt"

prepare_heavy_graph() {
  local roots=() drv
  while IFS= read -r drv; do roots+=("$drv"); done < <(jq -r '.[]' "$CHECKS_MAP" "$CI_CHECKS_MAP" "$CI_APPS_MAP" | sort -u)
  nix-store -q --requisites --include-outputs "${roots[@]}" > "$WORK_DIR/nodes"
  nix path-info --json --json-format 1 --stdin < "$WORK_DIR/nodes" > "$WORK_DIR/references.json"
  grep '\.drv$' "$WORK_DIR/nodes" > "$WORK_DIR/derivations"
  nix derivation show --stdin < "$WORK_DIR/derivations" > "$WORK_DIR/derivations.json"
  jq -nr --slurpfile paths "$WORK_DIR/references.json" --slurpfile derivations "$WORK_DIR/derivations.json" \
    -f "$SCRIPT_DIR/heavy-derivation-graph.jq" \
    | awk -v heavy="$HEAVY_DERIVATION_RE" -f "$SCRIPT_DIR/heavy-derivation-graph.awk" > "$WORK_DIR/heavy-paths"
}

write_heavy_derivations() {
  local map_file="$1"
  local output="$2"
  jq -r --rawfile heavy "$WORK_DIR/heavy-paths" '
    (reduce ($heavy | split("\n")[]) as $path ({}; .[$path] = true)) as $paths |
    to_entries[] | select($paths[.value] // false) | .key
  ' "$map_file" | sort -u > "$output"
}

write_listed_targets() {
  local listed_targets="$1"
  local listed_checks="$2"
  local listed_apps="$3"

  nix run --no-warn-dirty "$REPO_ROOT#test-ci" -- --list > "$listed_targets"
  awk '$1 == "check" { print $2 }' "$listed_targets" | sort -u > "$listed_checks"
  awk '$1 == "app" { print $2 }' "$listed_targets" | sort -u > "$listed_apps"
}

write_verify_targets() {
  nix run --no-warn-dirty "$REPO_ROOT#verify" -- --list > "$VERIFY_TARGETS"
  grep '^verify:images\.' "$VERIFY_TARGETS" | sort -u > "$IMAGE_VERIFY_TARGETS"
  awk '/^verify:(sandbox\.(agent-settings|wrix-cli-in-profile)|prek\.container-pre-(commit|push))$/ { print }' \
    "$VERIFY_TARGETS" > "$HEAVY_GENERIC_VERIFY_TARGETS"
  cat > "$EXPECTED_IMAGE_VERIFY_TARGETS" <<'TARGETS'
verify:images.darwin-entrypoint-core-hooks-path
verify:images.inspection-artifacts
verify:images.linked-worktree-core-hooks-path
verify:images.linux-entrypoint-core-hooks-path
verify:images.missing-hook-runtime-blocks-agent
TARGETS

  grep -rhoE '\[(check|system)\]\(test-ci:[^)]+\)' "$REPO_ROOT/specs" \
    | sed -E -e 's/^\[(check|system)\]\(test-ci://' -e 's/\)$//' \
    | sort -u > "$ANNOTATED_CI_APPS"
  comm -23 "$ANNOTATED_CI_APPS" "$LISTED_APPS" > "$UNKNOWN_ANNOTATED_CI_APPS"
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

RECIPE=$(nix eval --raw --no-warn-dirty "$REPO_ROOT#legacyPackages.$SYSTEM.verificationInventoryRecipe")
if ! nix-store --check-validity "$RECIPE"; then
  RECIPE=$(nix eval --raw --option eval-cache false --no-warn-dirty "$REPO_ROOT#legacyPackages.$SYSTEM.verificationInventoryRecipe")
fi
INVENTORY=$(nix build --no-link --print-out-paths --no-warn-dirty "$RECIPE^*")
cp "$INVENTORY" "$WORK_DIR/inventory.json"
if ! jq -e '.apps | index("test-ci")' "$WORK_DIR/inventory.json" > /dev/null; then
  echo "FAIL: apps.$SYSTEM.test-ci is missing" >&2
  failed=$((failed + 1))
fi

jq '.checks' "$WORK_DIR/inventory.json" > "$CHECKS_MAP"
jq '.ciChecks' "$WORK_DIR/inventory.json" > "$CI_CHECKS_MAP"
jq '.ciApps' "$WORK_DIR/inventory.json" > "$CI_APPS_MAP"
prepare_heavy_graph
write_heavy_derivations "$CHECKS_MAP" "$HEAVY_CHECKS"
write_heavy_derivations "$CI_CHECKS_MAP" "$HEAVY_CI_CHECKS"
write_heavy_derivations "$CI_APPS_MAP" "$HEAVY_APPS"
write_listed_targets "$LISTED_TARGETS" "$LISTED_CHECKS" "$LISTED_APPS"
write_verify_targets

if ! fail_if_file_nonempty "checks.$SYSTEM contains heavy realization checks" "$HEAVY_CHECKS"; then
  failed=$((failed + 1))
fi

if ! cmp -s "$EXPECTED_IMAGE_VERIFY_TARGETS" "$IMAGE_VERIFY_TARGETS"; then
  echo "FAIL: full image realizations remain exposed through the generic verify registry" >&2
  diff -u "$EXPECTED_IMAGE_VERIFY_TARGETS" "$IMAGE_VERIFY_TARGETS" >&2 || true # diagnostic: diff is expected to report the mismatch already handled above.
  failed=$((failed + 1))
fi

if ! fail_if_file_nonempty "full realization checks remain exposed through the generic verify registry" "$HEAVY_GENERIC_VERIFY_TARGETS"; then
  failed=$((failed + 1))
fi

if ! fail_if_file_nonempty "spec annotations reference unknown test-ci apps" "$UNKNOWN_ANNOTATED_CI_APPS"; then
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
