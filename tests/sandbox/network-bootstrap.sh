#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TEST_TMP="$(mktemp -d -t wrix-network-bootstrap.XXXXXX)"
trap 'rm -rf "$TEST_TMP"' EXIT

# Exercise real firewall and capability tools in a disposable user/network namespace.
# Only image paths are remapped; no host firewall or workspace is modified.
if [[ "$(uname -s)" != Linux ]]; then
  printf 'SKIP: Linux user/network namespaces required\n' >&2
  exit 77
fi
for tool in unshare nft iptables ip6tables capsh cc jq python3; do
  if ! command -v "$tool" >/dev/null; then
    printf 'SKIP: %s is unavailable\n' "$tool" >&2
    exit 77
  fi
done
if ! unshare --user --map-root-user --net true; then
  printf 'SKIP: user/network namespaces are unavailable\n' >&2
  exit 77
fi

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

prepare_case() {
  local root="$1" agent="$2" tool real
  mkdir -p "$root"/{tools,home,run,etc/wrix/pi-agent,workspace/bin,lib,tmp}
  printf '%s\n' "$agent" >"$root/etc/wrix/image-agent"
  printf '{}\n' >"$root/etc/wrix/claude-config.json"
  printf '{}\n' >"$root/etc/wrix/claude-settings.json"
  printf '{}\n' >"$root/etc/wrix/pi-agent/settings.json"
  printf '{"schema":1,"runtime_selection":false,"servers":[]}\n' >"$root/etc/wrix/mcp-available.json"
  : >"$root/etc/resolv.conf"
  cc -shared -fPIC -o "$root/lib/libfakeuid.so" "$REPO_ROOT/lib/sandbox/linux/fakeuid.c" -ldl

  for tool in bash nft iptables ip6tables capsh getent awk sort grep nc sleep; do
    ln -s "$(command -v "$tool")" "$root/tools/$tool"
  done
  real="$(command -v capsh)"
  rm "$root/tools/capsh"
  cat >"$root/tools/capsh" <<EOF
#!$(command -v bash)
set -euo pipefail
"$(command -v nft)" list ruleset >"$root/rules.nft"
"$(command -v iptables)" -S >"$root/rules.v4"
"$(command -v ip6tables)" -S >"$root/rules.v6"
printf 'drop\n' >>"$root/events"
exec "$real" "\$@"
EOF
  chmod +x "$root/tools/capsh"

  python3 - "$REPO_ROOT" "$root" <<'PY'
from pathlib import Path
import shutil
import sys
repo, root = map(Path, sys.argv[1:])
sources = {
    'entrypoint.sh': 'lib/sandbox/linux/entrypoint.sh',
    'network-bootstrap.sh': 'lib/sandbox/network-bootstrap.sh',
    'network-ready.sh': 'lib/sandbox/network-ready.sh',
    'krun-init.sh': 'lib/sandbox/linux/krun-init.sh',
    'beads-sandbox.sh': 'lib/beads/sandbox.sh',
    'git-ssh-setup.sh': 'lib/util/git-ssh-setup.sh',
    'mcp-manifest.sh': 'lib/sandbox/mcp-manifest.sh',
}
for name, source in sources.items():
    path = repo / source
    text = path.read_text()
    for old, new in [
        ('#!/bin/bash', '#!' + shutil.which('bash')),
        ('/tmp/wrix-', str(root / 'tmp/wrix-')),
        ('/workspace', str(root / 'workspace')),
        ('/etc/', str(root / 'etc') + '/'),
        ('/home/wrix', str(root / 'home')),
        ('/run/wrix-network-ready', str(root / 'run/wrix-network-ready')),
        ('/usr/local/libexec/wrix-network', str(root / 'tools')),
        ('/lib/libfakeuid.so', str(root / 'lib/libfakeuid.so')),
    ] + [('/' + script, str(root / script)) for script in sources]:
        text = text.replace(old, new)
    dest = root / name
    dest.write_text(text)
    dest.chmod(0o755)
PY

  for tool in mkdir cp jq; do
    real="$(command -v "$tool")"
    cat >"$root/workspace/bin/$tool" <<EOF
#!$(command -v bash)
set -euo pipefail
phase=startup
[[ ! -f "$root/agent-ran" ]] || phase=exit
while read -r field value _rest; do
  case "\$field" in
    CapInh:|CapPrm:|CapEff:|CapBnd:|CapAmb:)
      low="\${value: -8}"
      if (( (16#\$low & 16#1000) != 0 )); then
        printf '%s %s %s\n' "\$phase" '$tool' "\$field" >>"$root/privileged"
      fi
      ;;
  esac
done < /proc/self/status
printf '%s %s\n' "\$phase" '$tool' >>"$root/events"
exec "$real" "\$@"
EOF
    chmod +x "$root/workspace/bin/$tool"
  done
  for tool in nft iptables ip6tables capsh getent awk sort grep; do
    cat >"$root/workspace/bin/$tool" <<EOF
#!$(command -v bash)
set -euo pipefail
printf '%s\n' '$tool' >>"$root/poison"
exit 97
EOF
    chmod +x "$root/workspace/bin/$tool"
  done
  cat >"$root/workspace/bin/probe" <<EOF
#!$(command -v bash)
set -euo pipefail
[[ "\${LD_PRELOAD:-}" == "\${WRIX_TEST_PRELOAD:-}" ]]
if "$(command -v nft)" flush ruleset >"$root/firewall-probe" 2>&1; then
  echo 'agent modified the firewall' >&2
  exit 1
fi
printf '%s\n' "\$@" >"$root/argv"
printf 'agent\n' >>"$root/events"
: >"$root/agent-ran"
exit "\${WRIX_TEST_AGENT_EXIT:-0}"
EOF
  chmod +x "$root/workspace/bin/probe"
  for tool in claude pi loom-direct-runner; do
    ln -s probe "$root/workspace/bin/$tool"
  done
}

run_case() {
  local root="$1" agent="$2" boundary="$3" backend="$4" mode="$5"
  shift 5
  local start="$root/network-bootstrap.sh" preload="" cmd=""
  if [[ "$boundary" == krun ]]; then
    start="$root/krun-init.sh"
    preload="$root/lib/libfakeuid.so"
    if [[ "$#" -gt 0 ]]; then
      printf -v cmd '%q ' "$@"
    fi
    set --
  fi
  env -i HOME="$root/home" PATH="$root/tools:$PATH" \
    WRIX_AGENT="$agent" WRIX_STDIO=1 WRIX_TEST_PRELOAD="$preload" \
    WRIX_KRUN_CMD="$cmd" WRIX_TEST_AGENT_EXIT="${WRIX_TEST_AGENT_EXIT:-0}" \
    WRIX_FIREWALL_BACKEND="$backend" WRIX_NETWORK="$mode" \
    WRIX_NETWORK_ALLOWLIST="${WRIX_TEST_ALLOWLIST:-93.184.216.34}" \
    WRIX_NETWORK_DNS_SERVERS=10.0.0.53 \
    WRIX_NETWORK_LOCAL_ENDPOINTS=10.1.0.2:3307/tcp,10.1.0.3:8080/tcp \
    unshare --user --map-root-user --net bash "$start" "$@" \
    >"$root/stdout" 2>"$root/stderr"
}

test_setup_and_exit_lack_net_admin() {
  local boundary agent root status
  for boundary in container krun; do
    for agent in claude pi direct; do
      root="$TEST_TMP/$boundary-$agent"
      prepare_case "$root" "$agent"
      status=0
      WRIX_TEST_AGENT_EXIT=23 run_case "$root" "$agent" "$boundary" nft open || status=$?
      [[ "$status" -eq 23 ]] || fail "$boundary/$agent: expected agent exit 23, got $status: $(<"$root/stderr")"
      [[ ! -e "$root/privileged" ]] || fail "$boundary/$agent ran workspace tools with NET_ADMIN: $(<"$root/privileged")"
      [[ ! -e "$root/poison" ]] || fail "network setup used workspace tools"
      grep -q '^startup jq$' "$root/events" || fail "$boundary/$agent startup shim was not exercised"
      grep -q '^exit mkdir$' "$root/events" || fail 'exit shim was not exercised'
      [[ "$(head -1 "$root/events")" == drop ]] || fail 'workspace ran before capability drop'
      jq -e '.exit_code == 23' "$root/workspace/.wrix/log/"*.json >/dev/null
    done
  done
  printf 'PASS: startup and exit shims lack NET_ADMIN for every Linux agent/init path\n'
}

test_policy_and_argv() {
  local backend mode boundary root
  for backend in nft iptables; do
    for mode in open limit; do
      for boundary in container krun; do
        root="$TEST_TMP/$backend-$mode-$boundary"
        prepare_case "$root" direct
        # shellcheck disable=SC2016 # The command substitution must remain a literal argument.
        run_case "$root" direct "$boundary" "$backend" "$mode" probe alpha 'two words' '$(exit 98)' || fail "bootstrap failed: $(<"$root/stderr")"
        # shellcheck disable=SC2016
        diff -u <(printf '%s\n' alpha 'two words' '$(exit 98)') "$root/argv"
        [[ ! -e "$root/privileged" && ! -e "$root/poison" ]] || fail 'privilege or tool boundary violated'
        if [[ "$backend" == nft ]]; then
          grep -q 'ip daddr 10.0.0.0/8 reject' "$root/rules.nft"
          grep -q 'ip daddr 10.0.0.53 udp dport 53 accept' "$root/rules.nft"
          grep -q 'ip daddr 10.1.0.2 tcp dport 3307 accept' "$root/rules.nft"
          grep -q 'ip daddr 10.1.0.3 tcp dport 8080 accept' "$root/rules.nft"
          [[ "$(grep -c 'policy drop' "$root/rules.nft")" -eq 3 ]]
          if [[ "$mode" == open ]]; then
            grep -q 'meta nfproto ipv4 accept' "$root/rules.nft"
          else
            grep -q 'ip daddr 93.184.216.34 accept' "$root/rules.nft"
            if grep -q 'meta nfproto ipv4 accept' "$root/rules.nft"; then
              fail 'limit mode permits all public egress'
            fi
          fi
        else
          grep -q -- '-A OUTPUT -d 10.0.0.0/8 -j REJECT' "$root/rules.v4"
          grep -q -- '-A OUTPUT -d 10.1.0.2/32 -p tcp -m tcp --dport 3307 -j ACCEPT' "$root/rules.v4"
          grep -qx -- '-P OUTPUT DROP' "$root/rules.v6"
          if [[ "$mode" == open ]]; then
            grep -qx -- '-A OUTPUT -j ACCEPT' "$root/rules.v4"
          else
            grep -q -- '-A OUTPUT -d 93.184.216.34/32 -j ACCEPT' "$root/rules.v4"
            if grep -qx -- '-A OUTPUT -j ACCEPT' "$root/rules.v4"; then
              fail 'limit mode permits all public egress'
            fi
          fi
        fi
      done
    done
  done
  printf 'PASS: open/limit, IPv6 drop, endpoint exceptions and literal argv survive both init paths\n'
}

test_fail_closed() {
  local failure root mode backend
  for failure in firewall ipv6 capsh capability marker allowlist; do
    root="$TEST_TMP/fail-$failure"
    prepare_case "$root" claude
    backend=nft
    case "$failure" in
      firewall) rm "$root/tools/nft"; ln -s "$(command -v false)" "$root/tools/nft" ;;
      ipv6) backend=iptables; rm "$root/tools/ip6tables"; ln -s "$(command -v false)" "$root/tools/ip6tables" ;;
      capsh) rm "$root/tools/capsh"; ln -s "$(command -v false)" "$root/tools/capsh" ;;
      capability) printf '#!%s\nset -euo pipefail\nexec "%s"\n' "$(command -v bash)" "$root/entrypoint.sh" >"$root/tools/capsh" ;;
      marker) : >"$root/run/wrix-network-ready" ;;
      allowlist) ;;
    esac
    mode=open
    [[ "$failure" != allowlist ]] || mode=limit
    if WRIX_TEST_ALLOWLIST=127.0.0.1 run_case "$root" claude container "$backend" "$mode"; then
      fail "$failure did not abort startup"
    fi
    [[ ! -e "$root/events" || "$(<"$root/events")" == drop ]] || fail "$failure executed workspace setup/exit handling"
    [[ ! -e "$root/agent-ran" ]] || fail "$failure executed the agent"
  done
  printf 'PASS: bootstrap failures never enter workspace setup or exit handling\n'
}

if [[ "$#" -gt 0 ]]; then
  "$1"
else
  test_setup_and_exit_lack_net_admin
  test_policy_and_argv
  test_fail_closed
fi
