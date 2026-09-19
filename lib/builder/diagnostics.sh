#!/usr/bin/env bash
set -euo pipefail

wrix_builder_check_ssh() {
  local identity="$1" known_hosts="$2" port="$3" connect_timeout="$4"
  local deadline=$((connect_timeout + 1))

  "$WRIX_BUILDER_TIMEOUT" --kill-after=1 "$deadline" ssh \
    -p "$port" \
    -i "$identity" \
    -o BatchMode=yes \
    -o "ConnectTimeout=$connect_timeout" \
    -o ConnectionAttempts=1 \
    -o IdentitiesOnly=yes \
    -o StrictHostKeyChecking=yes \
    -o "UserKnownHostsFile=$known_hosts" \
    builder@localhost true
}

wrix_builder_diagnostic() {
  local label="$1" status
  shift

  printf '\n--- %s ---\n' "$label" >&2
  if "$WRIX_BUILDER_TIMEOUT" --kill-after=1 10 "$@" >&2; then
    return 0
  else
    status="$?"
    printf 'Diagnostic command failed (exit %s); continuing collection.\n' "$status" >&2
  fi
}

wrix_builder_startup_diagnostics() {
  local name="$1" identity="$2" known_hosts="$3" port="$4" status

  printf 'Collecting builder diagnostics before container cleanup (persistent volume retained).\n' >&2
  wrix_builder_diagnostic "Nix daemon process" container exec "$name" pgrep -x nix-daemon
  wrix_builder_diagnostic "SSH daemon process" container exec "$name" pgrep -x sshd
  wrix_builder_diagnostic "SSH daemon configuration" container exec "$name" /bin/sshd -t
  wrix_builder_diagnostic "Persistent store space" container exec "$name" /bin/df -h /nix

  printf '\n--- Host-to-builder SSH ---\n' >&2
  if wrix_builder_check_ssh "$identity" "$known_hosts" "$port" 5 >&2; then
    printf 'SSH probe succeeded during diagnostic collection.\n' >&2
  else
    status="$?"
    printf 'SSH probe failed (exit %s).\n' "$status" >&2
  fi

  printf '\n--- Container logs (last 80 lines) ---\n' >&2
  if "$WRIX_BUILDER_TIMEOUT" --kill-after=1 10 container logs "$name" 2>&1 | tail -n 80 >&2; then
    return 0
  else
    status="$?"
    printf 'Container logs unavailable (exit %s).\n' "$status" >&2
  fi
}
