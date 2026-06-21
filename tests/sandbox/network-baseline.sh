#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
LINUX_ENTRYPOINT="$REPO_ROOT/lib/sandbox/linux/entrypoint.sh"
DARWIN_ENTRYPOINT="$REPO_ROOT/lib/sandbox/darwin/entrypoint.sh"
BASH_BIN="${BASH:-$(command -v bash)}"
AWK_BIN="$(command -v awk)"
SORT_BIN="$(command -v sort)"
GREP_BIN="$(command -v grep)"
TEST_TMP="$(mktemp -d -t wrix-network-baseline.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

PASSED=0
FAILED=0

pass() {
  printf '  PASS: %s\n' "$1"
  PASSED=$((PASSED + 1))
}

fail() {
  printf '  FAIL: %s\n' "$1" >&2
  FAILED=$((FAILED + 1))
  return 1
}

extract_policy_block() {
  local source="$1"
  local out="$2"
  awk '
    /^# BEGIN wrix network policy$/ { capture = 1 }
    capture { print }
    /^# END wrix network policy$/ { exit }
  ' "$source" >"$out"
}

write_firewall_stub() {
  local path="$1"
  local family="$2"
  cat >"$path" <<STUB
#!$BASH_BIN
set -euo pipefail
log="\${WRIX_STUB_FIREWALL_LOG:?}"
printf '%s %s\\n' '$family' "\$*" >>"\$log"
if [[ "\${WRIX_STUB_NET_ADMIN_DROPPED:-0}" = "1" && "\$*" = *"-A OUTPUT -j ACCEPT"* ]]; then
  exit 1
fi
if [[ "\${WRIX_STUB_FIREWALL_FAIL:-}" = "1" ]]; then
  exit 1
fi
args=" \$* "
if [[ "\$args" = *" -S INPUT "* ]]; then
  printf '%s\\n' '-P INPUT DROP'
  exit 0
fi
if [[ "\$args" = *" -S OUTPUT "* ]]; then
  printf '%s\\n' '-P OUTPUT DROP'
  exit 0
fi
if [[ "\$args" = *" -C OUTPUT -d 10.0.0.0/8 -j REJECT "* ]]; then
  exit 0
fi
exit 0
STUB
  chmod +x "$path"
}

write_capsh_stub() {
  local path="$1"
  local drops_net_admin="$2"
  cat >"$path" <<STUB
#!$BASH_BIN
set -euo pipefail
printf 'capsh %s\\n' "\$*" >>"\${WRIX_STUB_FIREWALL_LOG:?}"
if [[ "\$1" != "--drop=cap_net_admin" || "\$2" != "--" || "\$3" != "-c" ]]; then
  echo "unexpected capsh invocation: \$*" >&2
  exit 64
fi
script="\$4"
shift 4
if [[ "$drops_net_admin" = "1" ]]; then
  if [[ "\${WRIX_STUB_CAPSH_RESET_PATH:-0}" = "1" ]]; then
    PATH=/no-such-path WRIX_STUB_NET_ADMIN_DROPPED=1 "$BASH_BIN" -c "\$script" "\$@"
  else
    WRIX_STUB_NET_ADMIN_DROPPED=1 "$BASH_BIN" -c "\$script" "\$@"
  fi
else
  "$BASH_BIN" -c "\$script" "\$@"
fi
STUB
  chmod +x "$path"
}

write_getent_stub() {
  local path="$1"
  cat >"$path" <<STUB
#!$BASH_BIN
set -euo pipefail
if [[ "\$1" != "ahostsv4" ]]; then
  exit 2
fi
case "\$2" in
  example.com)
    printf '93.184.216.34 STREAM example.com\\n'
    ;;
  cache.nixos.org)
    printf '151.101.2.217 STREAM cache.nixos.org\\n'
    ;;
  private.test)
    printf '10.42.0.9 STREAM private.test\\n'
    ;;
  unresolvable.test)
    exit 2
    ;;
  *)
    printf '198.51.100.9 STREAM %s\\n' "\$2"
    ;;
esac
STUB
  chmod +x "$path"
}

prepare_stubs() {
  local dir="$1"
  local drops_net_admin="${2:-1}"
  mkdir -p "$dir"
  write_firewall_stub "$dir/iptables" iptables
  write_firewall_stub "$dir/ip6tables" ip6tables
  write_capsh_stub "$dir/capsh" "$drops_net_admin"
  write_getent_stub "$dir/getent"
}

run_policy() {
  local platform="$1"
  local mode="$2"
  local stub_dir="$3"
  local block="$TEST_TMP/$platform-policy.sh"
  case "$platform" in
    linux) extract_policy_block "$LINUX_ENTRYPOINT" "$block" ;;
    darwin) extract_policy_block "$DARWIN_ENTRYPOINT" "$block" ;;
    *) fail "unknown platform: $platform" ;;
  esac
  WRIX_IPTABLES_BIN="$stub_dir/iptables" \
  WRIX_IP6TABLES_BIN="$stub_dir/ip6tables" \
  WRIX_CAPSH_BIN="$stub_dir/capsh" \
  WRIX_GETENT_BIN="$stub_dir/getent" \
  WRIX_AWK_BIN="$AWK_BIN" \
  WRIX_SORT_BIN="$SORT_BIN" \
  WRIX_GREP_BIN="$GREP_BIN" \
  WRIX_STUB_FIREWALL_LOG="$stub_dir/firewall.log" \
  WRIX_NETWORK="$mode" \
  WRIX_NETWORK_ALLOWLIST="${WRIX_NETWORK_ALLOWLIST:-}" \
  WRIX_STUB_FIREWALL_FAIL="${WRIX_STUB_FIREWALL_FAIL:-}" \
  WRIX_NETWORK_DNS_SERVERS="1.1.1.1,100.100.100.100" \
  WRIX_NETWORK_LOCAL_ENDPOINTS="192.168.64.1:21000/tcp" \
  "$BASH_BIN" -c ". '$block'; apply_wrix_network_policy" \
    >"$stub_dir/$platform.out" 2>"$stub_dir/$platform.err"
}

assert_log_contains() {
  local label="$1"
  local log="$2"
  local needle="$3"
  if ! grep -qF -- "$needle" "$log"; then
    fail "$label missing log entry: $needle"
  fi
}

assert_log_absent() {
  local label="$1"
  local log="$2"
  local needle="$3"
  if grep -qF -- "$needle" "$log"; then
    fail "$label had forbidden log entry: $needle"
  fi
}

test_open_blocks_lan() {
  local platform stub_dir log
  for platform in linux darwin; do
    stub_dir="$TEST_TMP/open-$platform"
    prepare_stubs "$stub_dir"
    run_policy "$platform" open "$stub_dir"
    log="$stub_dir/firewall.log"
    assert_log_contains "$platform open" "$log" "iptables -w -P INPUT DROP"
    assert_log_contains "$platform open" "$log" "iptables -w -P OUTPUT DROP"
    assert_log_contains "$platform open" "$log" "iptables -w -A OUTPUT -p tcp -d 100.100.100.100 --dport 53 -j ACCEPT"
    assert_log_contains "$platform open" "$log" "iptables -w -A OUTPUT -p tcp -d 192.168.64.1 --dport 21000 -j ACCEPT"
    assert_log_contains "$platform open" "$log" "iptables -w -A OUTPUT -d 10.0.0.0/8 -j REJECT"
    assert_log_contains "$platform open" "$log" "iptables -w -A OUTPUT -d 192.168.0.0/16 -j REJECT"
    assert_log_contains "$platform open" "$log" "iptables -w -A OUTPUT -j ACCEPT"
  done
  pass "open mode keeps the LAN/private baseline on both platforms"
}

test_limit_allowlist() {
  local platform stub_dir log
  for platform in linux darwin; do
    stub_dir="$TEST_TMP/limit-$platform"
    prepare_stubs "$stub_dir"
    WRIX_NETWORK_ALLOWLIST="example.com,cache.nixos.org" run_policy "$platform" limit "$stub_dir"
    log="$stub_dir/firewall.log"
    assert_log_contains "$platform limit" "$log" "iptables -w -A OUTPUT -d 93.184.216.34 -j ACCEPT"
    assert_log_contains "$platform limit" "$log" "iptables -w -A OUTPUT -d 151.101.2.217 -j ACCEPT"
    assert_log_absent "$platform limit" "$log" "iptables -w -A OUTPUT -j ACCEPT"

    stub_dir="$TEST_TMP/unresolvable-$platform"
    prepare_stubs "$stub_dir"
    if WRIX_NETWORK_ALLOWLIST="unresolvable.test" run_policy "$platform" limit "$stub_dir"; then
      fail "$platform limit accepted an unresolvable allowlist domain"
    fi
  done
  pass "limit mode resolves allowlists once and fails closed"
}

test_ipv6_blocked() {
  local platform stub_dir log
  for platform in linux darwin; do
    stub_dir="$TEST_TMP/ipv6-$platform"
    prepare_stubs "$stub_dir"
    run_policy "$platform" open "$stub_dir"
    log="$stub_dir/firewall.log"
    assert_log_contains "$platform ipv6" "$log" "ip6tables -w -P INPUT DROP"
    assert_log_contains "$platform ipv6" "$log" "ip6tables -w -P OUTPUT DROP"
    assert_log_absent "$platform ipv6" "$log" "ip6tables -w -A OUTPUT"
  done
  pass "IPv6 output is blocked on both platforms"
}

test_fail_closed() {
  local platform stub_dir
  for platform in linux darwin; do
    stub_dir="$TEST_TMP/fail-$platform"
    prepare_stubs "$stub_dir"
    if WRIX_STUB_FIREWALL_FAIL=1 run_policy "$platform" open "$stub_dir"; then
      fail "$platform continued after firewall setup failed"
    fi

    stub_dir="$TEST_TMP/private-allowlist-$platform"
    prepare_stubs "$stub_dir"
    if WRIX_NETWORK_ALLOWLIST="private.test" run_policy "$platform" limit "$stub_dir"; then
      fail "$platform accepted an allowlist domain resolving to a private address"
    fi
  done
  pass "firewall and allowlist failures stop launch"
}

test_agent_lacks_net_admin() {
  local platform stub_dir block
  for platform in linux darwin; do
    stub_dir="$TEST_TMP/drop-$platform"
    prepare_stubs "$stub_dir" 1
    block="$TEST_TMP/$platform-policy.sh"
    case "$platform" in
      linux) extract_policy_block "$LINUX_ENTRYPOINT" "$block" ;;
      darwin) extract_policy_block "$DARWIN_ENTRYPOINT" "$block" ;;
    esac
    WRIX_IPTABLES_BIN="$stub_dir/iptables" \
    WRIX_IP6TABLES_BIN="$stub_dir/ip6tables" \
    WRIX_CAPSH_BIN="$stub_dir/capsh" \
    WRIX_GETENT_BIN="$stub_dir/getent" \
    WRIX_AWK_BIN="$AWK_BIN" \
    WRIX_SORT_BIN="$SORT_BIN" \
    WRIX_GREP_BIN="$GREP_BIN" \
    WRIX_STUB_FIREWALL_LOG="$stub_dir/firewall.log" \
    "$BASH_BIN" -c ". '$block'; run_without_net_admin '$BASH_BIN' -c 'exit 0'"
    assert_log_contains "$platform capsh" "$stub_dir/firewall.log" "capsh --drop=cap_net_admin -- -c PATH=\"\$1\"; shift; export PATH; exec \"\$@\" wrix-no-net-admin"

    stub_dir="$TEST_TMP/drop-fail-$platform"
    prepare_stubs "$stub_dir" 0
    if WRIX_IPTABLES_BIN="$stub_dir/iptables" \
      WRIX_IP6TABLES_BIN="$stub_dir/ip6tables" \
      WRIX_CAPSH_BIN="$stub_dir/capsh" \
      WRIX_GETENT_BIN="$stub_dir/getent" \
      WRIX_AWK_BIN="$AWK_BIN" \
      WRIX_SORT_BIN="$SORT_BIN" \
      WRIX_GREP_BIN="$GREP_BIN" \
      WRIX_STUB_FIREWALL_LOG="$stub_dir/firewall.log" \
      "$BASH_BIN" -c ". '$block'; run_without_net_admin '$BASH_BIN' -c 'exit 0'"; then
      fail "$platform did not fail when capsh failed to drop NET_ADMIN"
    fi
  done
  pass "agent commands run under a verified NET_ADMIN drop"
}

test_capsh_shell_receives_entrypoint_path() {
  local platform stub_dir block agent_dir agent_log
  for platform in linux darwin; do
    stub_dir="$TEST_TMP/path-$platform"
    prepare_stubs "$stub_dir" 1
    block="$TEST_TMP/$platform-policy.sh"
    agent_dir="$stub_dir/agent-bin"
    agent_log="$stub_dir/agent.log"
    mkdir -p "$agent_dir"
    cat >"$agent_dir/pi" <<STUB
#!$BASH_BIN
set -euo pipefail
printf 'pi argv:' >"\${WRIX_STUB_AGENT_LOG:?}"
for arg in "\$@"; do
  printf ' <%s>' "\$arg" >>"\$WRIX_STUB_AGENT_LOG"
done
printf '\n' >>"\$WRIX_STUB_AGENT_LOG"
STUB
    chmod +x "$agent_dir/pi"
    case "$platform" in
      linux) extract_policy_block "$LINUX_ENTRYPOINT" "$block" ;;
      darwin) extract_policy_block "$DARWIN_ENTRYPOINT" "$block" ;;
    esac
    WRIX_IPTABLES_BIN="$stub_dir/iptables" \
    WRIX_IP6TABLES_BIN="$stub_dir/ip6tables" \
    WRIX_CAPSH_BIN="$stub_dir/capsh" \
    WRIX_GETENT_BIN="$stub_dir/getent" \
    WRIX_AWK_BIN="$AWK_BIN" \
    WRIX_SORT_BIN="$SORT_BIN" \
    WRIX_GREP_BIN="$GREP_BIN" \
    WRIX_STUB_FIREWALL_LOG="$stub_dir/firewall.log" \
    WRIX_STUB_AGENT_BIN_DIR="$agent_dir" \
    WRIX_STUB_AGENT_LOG="$agent_log" \
    WRIX_STUB_CAPSH_RESET_PATH=1 \
    "$BASH_BIN" -c '. "$1"; PATH="$WRIX_STUB_AGENT_BIN_DIR:/bin:/usr/bin"; export -n PATH; wrix_verify_net_admin_drop() { :; }; run_without_net_admin pi alpha beta' _ "$block"
    assert_log_contains "$platform agent argv" "$agent_log" "pi argv: <alpha> <beta>"
  done
  pass "capsh shell receives the entrypoint PATH for agent lookup"
}

ALL_TESTS=(
  test_open_blocks_lan
  test_limit_allowlist
  test_ipv6_blocked
  test_fail_closed
  test_agent_lacks_net_admin
  test_capsh_shell_receives_entrypoint_path
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
  "$fn"
fi
