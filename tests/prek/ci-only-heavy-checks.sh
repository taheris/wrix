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
HEAVY_SANDBOX_VERIFY_TARGETS="$WORK_DIR/heavy-sandbox-verify-targets.txt"
ANNOTATED_CI_APPS="$WORK_DIR/annotated-ci-apps.txt"
UNKNOWN_ANNOTATED_CI_APPS="$WORK_DIR/unknown-annotated-ci-apps.txt"
FAKE_NIX_LOG="$WORK_DIR/fake-nix.log"

write_derivation_map() {
  local flake_ref="$1"
  local output="$2"

  nix eval --json --no-warn-dirty \
    --apply 'attrs: builtins.mapAttrs (name: drv: drv.drvPath) attrs' \
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
  grep -E '^verify:sandbox\.(agent-settings|wrix-cli-in-profile)$' "$VERIFY_TARGETS" \
    > "$HEAVY_SANDBOX_VERIFY_TARGETS" || true
  cat > "$EXPECTED_IMAGE_VERIFY_TARGETS" <<'TARGETS'
verify:images.darwin-entrypoint-core-hooks-path
verify:images.linux-entrypoint-core-hooks-path
TARGETS

  grep -rhoE '\[(check|system)\]\(test-ci:[^)]+\)' "$REPO_ROOT/specs" \
    | sed -E -e 's/^\[(check|system)\]\(test-ci://' -e 's/\)$//' \
    | sort -u > "$ANNOTATED_CI_APPS"
  comm -23 "$ANNOTATED_CI_APPS" "$LISTED_APPS" > "$UNKNOWN_ANNOTATED_CI_APPS"
}

test_darwin_pre_push_skips_test_ci() {
  local output="$WORK_DIR/darwin-pre-push.jsonl"

  WRIX_PRE_PUSH=1 \
    WRIX_TEST_CI_FAKE_PLATFORM=Darwin \
    WRIX_TEST_CI_FAKE_NIX_LOG="$FAKE_NIX_LOG" \
    PATH="$WORK_DIR/bin:$PATH" \
    "$REPO_ROOT/bin/test-ci-verifiers" \
      test-image-tier-graph \
      test-image-nix-config > "$output"

  if [[ -e "$FAKE_NIX_LOG" ]]; then
    echo "FAIL: Darwin pre-push invoked test-ci: $(<"$FAKE_NIX_LOG")" >&2
    return 1
  fi
  if [[ "$(jq -s 'length' "$output")" -ne 2 ]]; then
    echo "FAIL: Darwin pre-push did not emit one verdict per test-ci target" >&2
    return 1
  fi
  if ! jq -e -s 'all(.[]; .pass == true and (.evidence | contains("disabled by default for Darwin pre-push")))' "$output" >/dev/null; then
    echo "FAIL: Darwin pre-push verdicts do not report the test-ci policy skip" >&2
    return 1
  fi
}

test_manual_darwin_keeps_test_ci() {
  WRIX_PRE_PUSH=0 \
    WRIX_TEST_CI_FAKE_PLATFORM=Darwin \
    WRIX_TEST_CI_FAKE_NIX_LOG="$FAKE_NIX_LOG" \
    PATH="$WORK_DIR/bin:$PATH" \
    "$REPO_ROOT/bin/test-ci-verifiers" test-image-tier-graph

  if [[ "$(<"$FAKE_NIX_LOG")" != "run .#test-ci -- --json test-image-tier-graph" ]]; then
    echo "FAIL: manual Darwin verification did not retain test-ci: $(<"$FAKE_NIX_LOG")" >&2
    return 1
  fi
  rm -f "$FAKE_NIX_LOG"
}

test_linux_pre_push_keeps_test_ci() {
  WRIX_PRE_PUSH=1 \
    WRIX_TEST_CI_FAKE_PLATFORM=Linux \
    WRIX_TEST_CI_FAKE_NIX_LOG="$FAKE_NIX_LOG" \
    PATH="$WORK_DIR/bin:$PATH" \
    "$REPO_ROOT/bin/test-ci-verifiers" test-image-tier-graph

  if [[ "$(<"$FAKE_NIX_LOG")" != "run .#test-ci -- --json test-image-tier-graph" ]]; then
    echo "FAIL: Linux pre-push did not retain test-ci: $(<"$FAKE_NIX_LOG")" >&2
    return 1
  fi
  rm -f "$FAKE_NIX_LOG"
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
write_derivation_map "$REPO_ROOT#legacyPackages.$SYSTEM.ciApps" "$CI_APPS_MAP"
write_heavy_derivations "$CHECKS_MAP" "$HEAVY_CHECKS"
write_heavy_derivations "$CI_CHECKS_MAP" "$HEAVY_CI_CHECKS"
write_heavy_derivations "$CI_APPS_MAP" "$HEAVY_APPS"
write_listed_targets "$LISTED_TARGETS" "$LISTED_CHECKS" "$LISTED_APPS"
write_verify_targets

mkdir -p "$WORK_DIR/bin"
cat > "$WORK_DIR/bin/nix" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" > "$WRIX_TEST_CI_FAKE_NIX_LOG"
SCRIPT
chmod +x "$WORK_DIR/bin/nix"
cat > "$WORK_DIR/bin/uname" <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$WRIX_TEST_CI_FAKE_PLATFORM"
SCRIPT
chmod +x "$WORK_DIR/bin/uname"

if ! test_darwin_pre_push_skips_test_ci; then
  failed=$((failed + 1))
fi
if ! test_manual_darwin_keeps_test_ci; then
  failed=$((failed + 1))
fi
if ! test_linux_pre_push_keeps_test_ci; then
  failed=$((failed + 1))
fi

if ! fail_if_file_nonempty "checks.$SYSTEM contains heavy realization checks" "$HEAVY_CHECKS"; then
  failed=$((failed + 1))
fi

if ! cmp -s "$EXPECTED_IMAGE_VERIFY_TARGETS" "$IMAGE_VERIFY_TARGETS"; then
  echo "FAIL: full image realizations remain exposed through the generic verify registry" >&2
  diff -u "$EXPECTED_IMAGE_VERIFY_TARGETS" "$IMAGE_VERIFY_TARGETS" >&2 || true # diagnostic: diff is expected to report the mismatch already handled above.
  failed=$((failed + 1))
fi

if ! fail_if_file_nonempty "sandbox realization checks remain exposed through the generic verify registry" "$HEAVY_SANDBOX_VERIFY_TARGETS"; then
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
