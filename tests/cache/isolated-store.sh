#!/usr/bin/env bash
set -euo pipefail

mount --make-rprivate /
mount -t tmpfs tmpfs /nix/var/nix
root=/nix/var/nix/fixture
mkdir -p "$root/lower" "$root/upper" "$root/work"
mount --bind /nix/store "$root/lower"
mount -o remount,bind,ro "$root/lower"
mount -t overlay overlay -o "lowerdir=$root/lower,upperdir=$root/upper,workdir=$root/work" /nix/store
daemon_pid=""
cleanup() {
  if [[ -n "$daemon_pid" ]]; then
    kill "$daemon_pid"
    # SIGTERM is the expected exit status of the disposable daemon.
    wait "$daemon_pid" || [[ "$?" == 143 ]]
  fi
}
trap cleanup EXIT
export NIX_CONF_DIR=/nonexistent NIX_USER_CONF_FILES=/dev/null
export NIX_CONFIG='experimental-features = nix-command flakes
sandbox = false
build-users-group =
substituters ='
export NIX_DAEMON_SOCKET_PATH=/nix/var/nix/daemon-socket/socket
export NIX_STATE_DIR=/nix/var/nix NIX_STORE_DIR=/nix/store NIX_REMOTE=daemon
export NIX_LOG_DIR="$root/log"
export WRIX_TEST_STORE_ROOT="$root"
mkdir -p /nix/var/nix/daemon-socket
printf '%s' "$root" > /nix/var/nix/wrix-test-isolated-store
nix-daemon --store local >"$root/daemon.log" 2>&1 &
daemon_pid="$!"
for _attempt in {1..100}; do
  if [[ -S "$NIX_DAEMON_SOCKET_PATH" ]]; then
    "$@"
    exit "$?"
  fi
  sleep 0.05
done
printf '%s\n' 'disposable Nix daemon did not become ready' >&2
exit 1
