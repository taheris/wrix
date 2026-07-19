#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
TEST_TMP="$(mktemp -d -t wrix-darwin-network-bootstrap.XXXXXX)"

cleanup() {
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

tools="$TEST_TMP/trusted-tools"
poison="$TEST_TMP/workspace-bin"
bootstrap="$TEST_TMP/network-bootstrap.sh"
stage_two="$TEST_TMP/entrypoint.sh"
ready_file="$TEST_TMP/run/wrix-network-ready"
nft_log="$TEST_TMP/nft.log"
capsh_log="$TEST_TMP/capsh.log"
stage_two_log="$TEST_TMP/stage-two.log"
poison_log="$TEST_TMP/poison.log"
mkdir -p "$tools" "$poison" "${ready_file%/*}"
: >"$nft_log"

sed \
  -e "s|/usr/local/libexec/wrix-network|$tools|g" \
  -e "s|/entrypoint.sh|$stage_two|g" \
  -e "s|/run/wrix-network-ready|$ready_file|g" \
  "$REPO_ROOT/lib/sandbox/darwin/network-bootstrap.sh" >"$bootstrap"
chmod +x "$bootstrap"

cat >"$tools/nft" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${WRIX_TEST_NFT_LOG:?}"
if [[ "${1:-}" == "-f" ]]; then
  /bin/cat >>"${WRIX_TEST_NFT_LOG:?}"
  exit 0
fi
if [[ "${1:-} ${2:-} ${3:-} ${4:-} ${5:-}" == "list chain inet wrix input" ]]; then
  printf 'chain input { policy drop; }\n'
elif [[ "${1:-} ${2:-} ${3:-} ${4:-} ${5:-}" == "list chain inet wrix output" ]]; then
  printf 'chain output { policy drop; ip daddr 10.0.0.0/8 reject; }\n'
fi
EOF

cat >"$tools/capsh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >"${WRIX_TEST_CAPSH_LOG:?}"
tool_dir="${0%/*}"
"$tool_dir/grep" -qF 'list chain inet wrix output' "${WRIX_TEST_NFT_LOG:?}" || {
  echo 'capsh ran before firewall verification' >&2
  exit 1
}
[[ "${1:-}" == "--drop=cap_net_admin" && "${2:-}" == "--" && "${3:-}" == "-c" ]] || {
  echo "unexpected capsh invocation: $*" >&2
  exit 1
}
script="$4"
shift 4
exec /bin/bash -c "$script" "$@"
EOF

cat >"$tools/getent" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '93.184.216.34 STREAM %s\n' "${2:-example.com}"
EOF

cat >"$tools/iptables" <<'EOF'
#!/usr/bin/env bash
exit 99
EOF
cat >"$tools/ip6tables" <<'EOF'
#!/usr/bin/env bash
exit 99
EOF
cat >"$tools/nc" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF

for tool in awk sort grep sleep; do
  ln -s "$(command -v "$tool")" "$tools/$tool"
done
chmod +x "$tools/nft" "$tools/capsh" "$tools/getent" "$tools/iptables" "$tools/ip6tables" "$tools/nc"

cat >"$stage_two" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ -f "${WRIX_TEST_READY_FILE:?}" ]]
printf '%s\n' "$@" >"${WRIX_TEST_STAGE_TWO_LOG:?}"
EOF
chmod +x "$stage_two"

for tool in nft capsh getent awk sort grep; do
  cat >"$poison/$tool" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
: >"${WRIX_TEST_POISON_LOG:?}"
exit 97
EOF
  chmod +x "$poison/$tool"
done

PATH="$poison:$PATH" \
WRIX_NETWORK=open \
WRIX_NETWORK_DNS_SERVERS=1.1.1.1 \
WRIX_TEST_CAPSH_LOG="$capsh_log" \
WRIX_TEST_NFT_LOG="$nft_log" \
WRIX_TEST_POISON_LOG="$poison_log" \
WRIX_TEST_READY_FILE="$ready_file" \
WRIX_TEST_STAGE_TWO_LOG="$stage_two_log" \
  /bin/bash "$bootstrap" alpha 'two words'

[[ ! -e "$poison_log" ]] || fail "bootstrap executed a workspace-controlled tool"
[[ -f "$ready_file" ]] || fail "bootstrap did not write its trusted completion marker"
grep -qF -- '--drop=cap_net_admin -- -c' "$capsh_log" || fail "bootstrap did not drop NET_ADMIN through capsh"
grep -qF 'flush ruleset' "$nft_log" || fail "bootstrap did not install the nft base policy"
[[ "$(sed -n '1p' "$stage_two_log")" == "alpha" ]] || fail "bootstrap did not preserve the first agent argument"
[[ "$(sed -n '2p' "$stage_two_log")" == "two words" ]] || fail "bootstrap did not preserve the quoted agent argument"

printf 'PASS: Darwin network bootstrap uses trusted tools and drops NET_ADMIN before stage two\n'
