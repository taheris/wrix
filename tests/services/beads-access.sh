#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TEST_TMP=$(mktemp -d -t wrix-beads-access.XXXXXX)
SERVER_PID=''
cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true # best-effort: failed servers may already have exited.
    wait "$SERVER_PID" 2>/dev/null || true # best-effort: termination by signal is expected.
  fi
  rm -rf "$TEST_TMP"
}
trap cleanup EXIT

BD_BIN="${WRIX_TEST_BD_BIN:-${WRIX_REAL_BD_BIN:-$(command -v bd)}}"
export WRIX_REAL_BD_BIN="$BD_BIN"
export HOME="$TEST_TMP/home"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export BD_DISABLE_METRICS=1
mkdir -p "$HOME" "$TEST_TMP/repo" "$TEST_TMP/data"
cd "$TEST_TMP/repo"
git init -q
git config user.name 'Wrix Test'
git config user.email 'test@example.invalid'
PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
SOCKET="$TEST_TMP/dolt.sock"
dolt sql-server --data-dir "$TEST_TMP/data" --host 127.0.0.1 --port "$PORT" --socket "$SOCKET" >"$TEST_TMP/server.log" 2>&1 &
SERVER_PID=$!
for _ in {1..100}; do
  [[ -S "$SOCKET" ]] && break
  sleep 0.05
done
export BEADS_DOLT_SERVER_HOST=127.0.0.1 BEADS_DOLT_SERVER_PORT="$PORT" BEADS_DOLT_AUTO_START=0
unset BEADS_DOLT_SERVER_SOCKET BEADS_DIR BEADS_DB
"$BD_BIN" init --server --prefix wx --skip-hooks --quiet
"$BD_BIN" config set import.auto false
"$BD_BIN" config set export.auto false
"$BD_BIN" config set dolt.auto-start false
"$BD_BIN" config set sync.mode dolt-native
mkdir -p .git/beads-worktrees/beads/.beads/dolt-remote
"$BD_BIN" dolt remote add origin "file://$PWD/.git/beads-worktrees/beads/.beads/dolt-remote"
for i in {1..21}; do
  "$BD_BIN" create --title="preserved issue $i" --type=task --priority=2 >/dev/null
done
"$BD_BIN" dolt commit >/dev/null
"$BD_BIN" dolt push >/dev/null
"$BD_BIN" dolt remote list >"$TEST_TMP/remote-before"

# shellcheck source=/dev/null
source "$REPO_ROOT/lib/beads/sandbox.sh"
for transport in tcp unix; do
  if [[ "$transport" == tcp ]]; then
    export BEADS_DOLT_SERVER_HOST=127.0.0.1 BEADS_DOLT_SERVER_PORT="$PORT"
    unset BEADS_DOLT_SERVER_SOCKET
  else
    unset BEADS_DOLT_SERVER_HOST BEADS_DOLT_SERVER_PORT
    export BEADS_DOLT_SERVER_SOCKET="$SOCKET"
  fi
  wrix_configure_beads_endpoint "$PWD"
  wrix_wait_for_beads_endpoint
  env BEADS_DOLT_SERVER_SOCKET='' BEADS_DOLT_SERVER_HOST=127.0.0.1 BEADS_DOLT_SERVER_PORT="$PORT" \
    "$BD_BIN" --readonly sql 'SELECT COUNT(*) FROM issues' >"$TEST_TMP/host-$transport" &
  HOST_PID=$!
  "$BD_BIN" --readonly sql 'SELECT COUNT(*) FROM issues' >"$TEST_TMP/guest-$transport"
  wait "$HOST_PID"
  grep -q '21' "$TEST_TMP/host-$transport"
  grep -q '21' "$TEST_TMP/guest-$transport"
  "$BD_BIN" dolt remote list >"$TEST_TMP/remote-after"
  cmp "$TEST_TMP/remote-before" "$TEST_TMP/remote-after"
  [[ ! -f .beads/dolt-server.pid ]]
done

export BEADS_DOLT_SERVER_HOST=127.0.0.1
export BEADS_DOLT_SERVER_PORT
BEADS_DOLT_SERVER_PORT=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
unset BEADS_DOLT_SERVER_SOCKET
wrix_configure_beads_endpoint "$PWD"
if wrix_wait_for_beads_endpoint 2>"$TEST_TMP/unreachable"; then
  echo 'FAIL: stale TCP endpoint was accepted' >&2
  exit 1
fi
grep -q 'from this sandbox' "$TEST_TMP/unreachable"
[[ ! -f .beads/dolt-server.pid ]]
printf 'PASS: TCP/socket clients share 21 issues and a file remote; stale endpoints fail without another server\n'
