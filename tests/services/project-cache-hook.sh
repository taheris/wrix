#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"

TEST_TMP="$(mktemp -d -t wrix-services-cache-hook.XXXXXX)"
cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  local message="$1"
  printf 'FAIL: %s\n' "$message" >&2
  return 1
}

build_hook() {
  cargo build --quiet -p wrix-cache --bin wrix-cache-hook
  printf '%s\n' "$REPO_ROOT/target/debug/wrix-cache-hook"
}

write_publisher() {
  local publisher="$1"
  cat >"$publisher" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

{
  printf 'uid=%s\n' "$(id -u)"
  printf 'gid=%s\n' "$(id -g)"
  printf 'args=%s\n' "$*"
} >>"${WRIX_PUBLISH_LOG:?}"
EOF
  chmod +x "$publisher"
}

assert_contains() {
  local label="$1"
  local haystack="$2"
  local needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    fail "$label missing '$needle'"
  fi
}

assert_no_publish() {
  local label="$1"
  local log="$2"
  if [[ -e "$log" ]]; then
    fail "$label published unexpectedly: $(cat "$log")"
  fi
}

test_hook_manifest_scope() {
  local hook_bin state_root cache_root workspace workspace_hash publisher log allowed_drv skipped_drv output
  hook_bin="$(build_hook)"
  state_root="$TEST_TMP/state"
  cache_root="$TEST_TMP/cache"
  workspace="$TEST_TMP/workspace"
  workspace_hash="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
  publisher="$TEST_TMP/publisher"
  log="$TEST_TMP/publish.log"
  allowed_drv="/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-allowed.drv"
  skipped_drv="/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-skipped.drv"
  mkdir -p "$state_root" "$cache_root" "$workspace"
  write_publisher "$publisher"
  cat >"$state_root/publish-roots.json" <<JSON
{
  "roots": [
    {
      "name": "package",
      "drv_path": "$allowed_drv"
    }
  ]
}
JSON
  cat >"$workspace/publish-roots.json" <<JSON
{
  "roots": [
    {
      "name": "malicious-workspace-copy",
      "drv_path": "$skipped_drv"
    }
  ]
}
JSON

  WRIX_PUBLISH_LOG="$log" DRV_PATH="$allowed_drv" OUT_PATHS="/nix/store/out" "$hook_bin" \
    --workspace-hash "$workspace_hash" \
    --owner-uid "$(id -u)" \
    --owner-gid "$(id -g)" \
    --state-root "$state_root" \
    --cache-root "$cache_root" \
    --manifest "$state_root/publish-roots.json" \
    --publisher-helper "$publisher"

  [[ -f "$log" ]] || fail "matching manifest derivation did not invoke publisher"
  assert_contains "publisher uid" "$(cat "$log")" "uid=$(id -u)"
  assert_contains "publisher gid" "$(cat "$log")" "gid=$(id -g)"
  assert_contains "publisher args" "$(cat "$log")" "--workspace-hash $workspace_hash"
  assert_contains "publisher args" "$(cat "$log")" "--state-root $state_root"
  assert_contains "publisher args" "$(cat "$log")" "--cache-root $cache_root"
  assert_contains "publisher args" "$(cat "$log")" "--manifest $state_root/publish-roots.json"
  assert_contains "publisher args" "$(cat "$log")" "--drv-path $allowed_drv"
  assert_contains "publisher args" "$(cat "$log")" "--out-paths /nix/store/out"

  rm -f "$log"
  output="$(WRIX_PUBLISH_LOG="$log" DRV_PATH="$skipped_drv" OUT_PATHS="/nix/store/out" "$hook_bin" \
    --workspace-hash "$workspace_hash" \
    --owner-uid "$(id -u)" \
    --owner-gid "$(id -g)" \
    --state-root "$state_root" \
    --cache-root "$cache_root" \
    --manifest "$state_root/publish-roots.json" \
    --publisher-helper "$publisher")"
  assert_contains "skip output" "$output" "skipping non-project derivation"
  assert_no_publish "workspace manifest copy" "$log"
}

ALL_TESTS=(
  test_hook_manifest_scope
)

run_all() {
  local failed=0
  local fn
  for fn in "${ALL_TESTS[@]}"; do
    printf '=== %s ===\n' "$fn"
    if "$fn"; then
      printf 'PASS: %s\n' "$fn"
    else
      printf 'FAIL: %s\n' "$fn" >&2
      failed=$((failed + 1))
    fi
  done
  if [[ "$failed" -ne 0 ]]; then
    printf '%s test(s) failed\n' "$failed" >&2
    return 1
  fi
}

if [[ "$#" -eq 0 ]]; then
  run_all
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    printf 'Unknown function: %s\n' "$fn" >&2
    exit 1
  fi
  "$fn"
fi
