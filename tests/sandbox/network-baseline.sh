#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

wrix_require_live_sandbox_linux
cd "$REPO_ROOT"

TEST_TMP=$(mktemp -d -t wrix-network-baseline.XXXXXX)
cleanup() {
  rm -rf "$TEST_TMP"
  wrix_remove_image_ref "$IMAGE_REF"
}
trap cleanup EXIT

PASSED=0
FAILED=0

pass() {
  local message="$1"
  printf '  PASS: %s\n' "$message"
  PASSED=$((PASSED + 1))
}

fail() {
  local message="$1"
  printf '  FAIL: %s\n' "$message" >&2
  FAILED=$((FAILED + 1))
  return 1
}

LAUNCHER=$(wrix_build_live_launcher)
IMAGE_SOURCE=$(wrix_realize_test_image_source claude)
IMAGE_REF="localhost/wrix-network-baseline-$$:latest"
DEPLOY_KEY="$TEST_TMP/deploy-key"
HOME_DIR="$TEST_TMP/home"
XDG_CACHE_HOME="$TEST_TMP/cache"
mkdir -p "$HOME_DIR" "$XDG_CACHE_HOME"
wrix_make_ed25519_key "$DEPLOY_KEY" "network-baseline-test"

run_sandbox_probe() {
  local label="$1"
  local mode="$2"
  local allowlist_csv="$3"
  local probe="$4"
  local workspace="$TEST_TMP/workspace-$label"
  local profile_config="$TEST_TMP/profile-$label.json"
  local spawn_config="$TEST_TMP/spawn-$label.json"
  local out="$TEST_TMP/$label.out"
  local err="$TEST_TMP/$label.err"

  mkdir -p "$workspace"
  wrix_write_profile_config "$profile_config" "$IMAGE_REF" "$IMAGE_SOURCE" claude "$allowlist_csv"
  wrix_write_spawn_config "$spawn_config" "$workspace" bash -lc "$probe"

  HOME="$HOME_DIR" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
    WRIX_DEPLOY_KEY="$DEPLOY_KEY" WRIX_GIT_SIGN=0 WRIX_NETWORK="$mode" \
    wrix_run_spawn "$LAUNCHER" "$profile_config" "$spawn_config" >"$out" 2>"$err"
}

dump_probe_error() {
  local label="$1"
  local err="$TEST_TMP/$label.err"

  [[ -f "$err" ]] && sed 's/^/    /' "$err" >&2
}

test_open_blocks_lan() {
  local label="open-blocks-lan"
  local rc=0
  local probe

  probe=$(cat <<'PROBE'
set -euo pipefail
curl --fail --silent --show-error --connect-timeout 10 --max-time 30 https://cache.nixos.org/nix-cache-info >/tmp/wrix-public-egress
for target in 10.0.0.1 172.16.0.1 192.168.0.1 169.254.169.254; do
  if nc -z -w 2 "$target" 80 >/tmp/wrix-private-probe 2>&1; then
    printf 'private target reachable despite baseline block: %s\n' "$target" >&2
    exit 1
  fi
done
PROBE
)

  run_sandbox_probe "$label" open "" "$probe" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    fail "open mode did not allow public egress while blocking private ranges"
    dump_probe_error "$label"
    return 1
  fi
  pass "open mode public egress works and private-range probes fail in a live sandbox"
}

test_limit_allowlist() {
  local label="limit-allowlist"
  local rc=0
  local probe

  probe=$(cat <<'PROBE'
set -euo pipefail
curl --fail --silent --show-error --connect-timeout 10 --max-time 30 https://cache.nixos.org/nix-cache-info >/tmp/wrix-allowlisted-egress
if curl --fail --silent --show-error --connect-timeout 5 --max-time 8 https://example.com >/tmp/wrix-nonallowlisted-egress 2>&1; then
  echo 'non-allowlisted public egress succeeded in limit mode' >&2
  exit 1
fi
PROBE
)

  run_sandbox_probe "$label" limit "cache.nixos.org" "$probe" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    fail "limit mode did not enforce the public allowlist"
    dump_probe_error "$label"
    return 1
  fi

  label="limit-unresolvable"
  run_sandbox_probe "$label" limit "unresolvable.invalid" 'exit 0' && rc=0 || rc=$?
  if [[ "$rc" -eq 0 ]]; then
    fail "limit mode accepted an unresolvable allowlist domain"
    return 1
  fi
  if ! grep -qF "allowlist domain is unresolvable" "$TEST_TMP/$label.err"; then
    fail "limit mode unresolvable failure did not name the allowlist domain"
    dump_probe_error "$label"
    return 1
  fi

  pass "limit mode allows only resolved allowlist destinations and fails closed"
}

test_ipv6_blocked() {
  local label="ipv6-blocked"
  local rc=0
  local probe

  probe=$(cat <<'PROBE'
set -euo pipefail
if curl -6 --fail --silent --show-error --connect-timeout 5 --max-time 8 https://cache.nixos.org/nix-cache-info >/tmp/wrix-ipv6-egress 2>&1; then
  echo 'IPv6 egress succeeded despite v1 block' >&2
  exit 1
fi
PROBE
)

  run_sandbox_probe "$label" open "" "$probe" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    fail "IPv6 probe did not fail closed as expected"
    dump_probe_error "$label"
    return 1
  fi
  pass "IPv6 egress is blocked in a live sandbox"
}

test_fail_closed() {
  local label="fail-closed-special-allowlist"
  local rc=0

  run_sandbox_probe "$label" limit "localhost" 'exit 0' && rc=0 || rc=$?
  if [[ "$rc" -eq 0 ]]; then
    fail "limit mode accepted an allowlist domain resolving to a blocked address"
    return 1
  fi
  if ! grep -qF "allowlist domain resolves to blocked local/special address" "$TEST_TMP/$label.err"; then
    fail "fail-closed error did not identify the blocked allowlist address"
    dump_probe_error "$label"
    return 1
  fi
  pass "allowlist domains resolving to local/special addresses fail closed"
}

test_agent_lacks_net_admin() {
  local label="agent-lacks-net-admin"
  local rc=0
  local probe

  probe=$(cat <<'PROBE'
set -euo pipefail
if command -v nft >/dev/null 2>&1; then
  if nft flush ruleset >/tmp/wrix-net-admin-probe 2>&1; then
    echo 'agent retained NET_ADMIN after startup' >&2
    exit 1
  fi
elif command -v iptables >/dev/null 2>&1; then
  if iptables -A OUTPUT -j ACCEPT >/tmp/wrix-net-admin-probe 2>&1; then
    echo 'agent retained NET_ADMIN after startup' >&2
    exit 1
  fi
else
  echo 'no firewall backend command found in sandbox' >&2
  exit 1
fi
PROBE
)

  run_sandbox_probe "$label" open "" "$probe" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    fail "agent NET_ADMIN drop was not enforced"
    dump_probe_error "$label"
    return 1
  fi
  pass "agent commands run without NET_ADMIN in a live sandbox"
}

ALL_TESTS=(
  test_open_blocks_lan
  test_limit_allowlist
  test_ipv6_blocked
  test_fail_closed
  test_agent_lacks_net_admin
)

run_all() {
  local fn rc

  for fn in "${ALL_TESTS[@]}"; do
    echo "=== $fn ==="
    rc=0
    "$fn" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
      fail "$fn returned $rc"
    fi
  done
  echo
  echo "Results: $PASSED passed, $FAILED failed"
  [[ "$FAILED" -eq 0 ]]
}

if [[ $# -eq 0 ]]; then
  run_all
else
  fn="$1"
  if ! declare -f "$fn" >/dev/null 2>&1; then
    echo "Unknown function: $fn" >&2
    exit 1
  fi
  rc=0
  "$fn" || rc=$?
  [[ "$rc" -eq 0 && "$FAILED" -eq 0 ]]
fi
