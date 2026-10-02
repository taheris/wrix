#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=tests/lib/live-sandbox.sh
source "$SCRIPT_DIR/../lib/live-sandbox.sh"

require_host_public_ipv4_egress() {
  command -v curl >/dev/null 2>&1 || wrix_live_skip "curl not on PATH for host egress preflight"
  if ! curl -4 --fail --silent --show-error --connect-timeout 10 --max-time 30 \
    https://cache.nixos.org/nix-cache-info >/dev/null; then
    wrix_live_skip "host public IPv4 egress to cache.nixos.org is unavailable"
  fi
}

wrix_require_live_sandbox
require_host_public_ipv4_egress
cd "$REPO_ROOT"

TEST_TMP=$(mktemp -d -t wrix-network-baseline.XXXXXX)
IMAGE_REF=""
SERVICE_WORKSPACE=""
cleanup() {
  local status="$?"
  trap - EXIT
  if [[ -n "$SERVICE_WORKSPACE" ]]; then
    if ! (
      cd "$SERVICE_WORKSPACE"
      HOME="$HOME_DIR" XDG_STATE_HOME="$XDG_STATE_HOME" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
        WRIX_SERVICE_ALLOW_TEMP_CACHE=1 "$LAUNCHER/bin/wrix" service stop
    ); then
      printf 'WARN: could not stop the network baseline service container\n' >&2
      status=1
    fi
  fi
  if [[ -n "$IMAGE_REF" ]] && ! wrix_remove_image_ref "$IMAGE_REF"; then
    printf 'WARN: could not remove test image %s\n' "$IMAGE_REF" >&2
  fi
  rm -rf "$TEST_TMP"
  exit "$status"
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
IMAGE_REF=$(wrix_live_image_ref "network-baseline-$$")
DEPLOY_KEY="$TEST_TMP/deploy-key"
HOME_DIR="$TEST_TMP/home"
XDG_CACHE_HOME="$TEST_TMP/cache"
XDG_STATE_HOME="$TEST_TMP/state"
mkdir -p "$HOME_DIR" "$XDG_CACHE_HOME" "$XDG_STATE_HOME"
wrix_make_ed25519_key "$DEPLOY_KEY" "network-baseline-test"

configure_cache_service_image() {
  local digest_path

  export WRIX_SERVICE_IMAGE_SOURCE WRIX_SERVICE_IMAGE WRIX_SERVICE_IMAGE_SOURCE_KIND WRIX_SERVICE_IMAGE_DIGEST
  WRIX_SERVICE_IMAGE_SOURCE=$(nix build --no-link --print-out-paths --no-warn-dirty \
    .#wrix-service-image.source) || return
  WRIX_SERVICE_IMAGE=$(nix eval --raw --no-warn-dirty .#wrix-service-image.ref) || return
  WRIX_SERVICE_IMAGE_SOURCE_KIND=$(nix eval --raw --no-warn-dirty \
    .#wrix-service-image.source_kind) || return
  digest_path=$(nix eval --raw --no-warn-dirty .#wrix-service-image.digest) || return
  [[ -f "$digest_path" ]] || {
    fail "service image digest was not realized: $digest_path"
    return 1
  }
  WRIX_SERVICE_IMAGE_DIGEST=$(<"$digest_path")

  export WRIX_PROJECT_CACHE_SANDBOX_HOST
  case "$(uname -s)" in
    Linux) WRIX_PROJECT_CACHE_SANDBOX_HOST="169.254.1.2" ;;
    Darwin)
      WRIX_PROJECT_CACHE_SANDBOX_HOST=$(container network inspect default | jq -er '
        (if type == "array" then .[0] else . end)
        | .status.ipv4Gateway
        | strings
        | select(test("^[0-9]+(\\.[0-9]+){3}$"))
      ') || return
      ;;
    *) fail "unsupported cache-network host: $(uname -s)"; return 1 ;;
  esac
}

run_sandbox_probe() {
  local label="$1"
  local mode="$2"
  local allowlist_csv="$3"
  local probe="$4"
  local workspace="${5:-$TEST_TMP/workspace-$label}"
  local cache_enabled="${6:-false}"
  local profile_config="$TEST_TMP/profile-$label.json"
  local profile_config_tmp="$TEST_TMP/profile-$label.tmp.json"
  local spawn_config="$TEST_TMP/spawn-$label.json"
  local out="$TEST_TMP/$label.out"
  local err="$TEST_TMP/$label.err"

  mkdir -p "$workspace" || return
  wrix_write_profile_config "$profile_config" "$IMAGE_REF" "$IMAGE_SOURCE" claude "$allowlist_csv" || return
  jq --argjson enabled "$cache_enabled" '.services.nix_cache.enable = $enabled' \
    "$profile_config" >"$profile_config_tmp" || return
  mv "$profile_config_tmp" "$profile_config" || return
  wrix_write_spawn_config "$spawn_config" "$workspace" bash -lc "$probe" || return

  HOME="$HOME_DIR" XDG_STATE_HOME="$XDG_STATE_HOME" XDG_CACHE_HOME="$XDG_CACHE_HOME" \
    WRIX_SERVICE_ALLOW_TEMP_CACHE=1 \
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
  local control_label="open-lan-exception-control"
  local rc=0
  local control_probe probe lan_target lan_port

  configure_cache_service_image || return 1
  SERVICE_WORKSPACE="$TEST_TMP/workspace-$control_label"
  mkdir -p "$SERVICE_WORKSPACE" || return 1
  git -C "$SERVICE_WORKSPACE" init -q -b main || return 1
  control_probe=$(cat <<'PROBE'
set -euo pipefail
host="${WRIX_PROJECT_CACHE_HOST:?}"
port="${WRIX_PROJECT_CACHE_PORT:?}"
curl --noproxy '*' --fail --silent --show-error --connect-timeout 5 --max-time 10 \
  "http://$host:$port/nix-cache-info" | grep -F 'WantMassQuery: 1' >/dev/null
printf '%s %s\n' "$host" "$port" >/workspace/cache-endpoint
PROBE
)
  run_sandbox_probe "$control_label" open "" "$control_probe" \
    "$SERVICE_WORKSPACE" true || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    fail "sandbox could not reach its live project cache through the Wrix-owned endpoint exception"
    dump_probe_error "$control_label"
    return 1
  fi
  read -r lan_target lan_port <"$SERVICE_WORKSPACE/cache-endpoint" || return 1
  if [[ ! "$lan_target" =~ ^[0-9]+(\.[0-9]+){3}$ || ! "$lan_port" =~ ^[0-9]+$ ]]; then
    fail "live project cache published an invalid endpoint: $lan_target:$lan_port"
    return 1
  fi

  probe=$(cat <<PROBE
set -euo pipefail
curl -4 --fail --silent --show-error --connect-timeout 10 --max-time 30 https://cache.nixos.org/nix-cache-info >/tmp/wrix-public-egress
if curl --noproxy '*' --fail --silent --show-error --connect-timeout 2 --max-time 5 http://$lan_target:$lan_port/nix-cache-info >/tmp/wrix-private-probe 2>&1; then
  echo 'project cache reachable without its endpoint exception despite baseline block' >&2
  exit 1
fi
PROBE
)

  rc=0
  run_sandbox_probe "$label" open "" "$probe" "$SERVICE_WORKSPACE" || rc=$?
  if [[ "$rc" -ne 0 ]]; then
    fail "open mode did not allow public egress while blocking the cache endpoint without an exception"
    dump_probe_error "$label"
    return 1
  fi
  if ! curl --noproxy '*' --fail --silent --show-error --connect-timeout 5 --max-time 10 \
    "http://127.0.0.1:$lan_port/nix-cache-info" | grep -F 'WantMassQuery: 1' >/dev/null; then
    fail "project cache stopped before the blocked-endpoint probe completed"
    return 1
  fi
  pass "open mode permits public egress and blocks a live cache endpoint when its exception is absent"
}

test_limit_allowlist() {
  local label="limit-allowlist"
  local rc=0
  local probe

  probe=$(cat <<'PROBE'
set -euo pipefail
curl -4 --fail --silent --show-error --connect-timeout 10 --max-time 30 https://cache.nixos.org/nix-cache-info >/tmp/wrix-allowlisted-egress
if curl -4 --fail --silent --show-error --connect-timeout 5 --max-time 8 https://example.com >/tmp/wrix-nonallowlisted-egress 2>&1; then
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
  local mode label rc probe

  probe=$(cat <<'PROBE'
set -euo pipefail
if curl -6 --fail --silent --show-error --connect-timeout 5 --max-time 8 https://cache.nixos.org/nix-cache-info >/tmp/wrix-ipv6-egress 2>&1; then
  echo 'IPv6 egress succeeded despite v1 block' >&2
  exit 1
fi
PROBE
)

  for mode in open limit; do
    label="ipv6-blocked-$mode"
    rc=0
    run_sandbox_probe "$label" "$mode" "" "$probe" || rc=$?
    if [[ "$rc" -ne 0 ]]; then
      fail "IPv6 probe did not fail closed in $mode mode"
      dump_probe_error "$label"
      return 1
    fi
    if ! grep -F -- "Network policy verified: IPv6 output default-drop" "$TEST_TMP/$label.err" >/dev/null; then
      fail "assembled $mode-mode sandbox did not attest its inspected IPv6 output-drop policy"
      dump_probe_error "$label"
      return 1
    fi
  done
  pass "assembled sandbox verifies IPv6 output-drop policy in open and limit modes"
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
seen=0
while read -r field value _rest; do
  case "$field" in
    CapInh:|CapPrm:|CapEff:|CapBnd:|CapAmb:)
      [[ "$value" =~ ^[0-9A-Fa-f]+$ ]] || {
        printf 'invalid capability value for %s: %s\n' "$field" "$value" >&2
        exit 1
      }
      low="${value: -8}"
      if (( (16#$low & 16#1000) != 0 )); then
        printf 'agent retained NET_ADMIN in %s\n' "$field" >&2
        exit 1
      fi
      seen=$((seen + 1))
      ;;
  esac
done </proc/self/status
[[ "$seen" -eq 5 ]] || {
  echo 'agent capability sets could not be verified' >&2
  exit 1
}
if command -v nft >/dev/null 2>&1; then
  if nft flush ruleset >/tmp/wrix-net-admin-probe 2>&1; then
    echo 'agent modified nft policy without NET_ADMIN' >&2
    exit 1
  fi
elif command -v iptables >/dev/null 2>&1; then
  if iptables -A OUTPUT -j ACCEPT >/tmp/wrix-net-admin-probe 2>&1; then
    echo 'agent modified iptables policy without NET_ADMIN' >&2
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
  pass "assembled sandbox agent lacks NET_ADMIN in every capability set and cannot modify firewall policy"
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
