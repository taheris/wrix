#!/usr/bin/env bash
# Notification connectivity test - run inside container
#
# Tests notification connectivity to host daemon.
# - Darwin containers: uses TCP to gateway (VirtioFS can't pass Unix sockets)
# - Linux containers: uses mounted Unix socket
#
# Skips gracefully if daemon is not running on host.
set -euo pipefail

echo "=== Notification Connectivity Test ==="

TCP_PORT=5959
DARWIN_GATEWAY="192.168.64.1"
SOCKET="/run/wrix/notify.sock"
CONNECT_TIMEOUT_SECONDS=3
TCP_PAYLOAD='{"title":"test","message":"tcp test"}'
SOCKET_PAYLOAD='{"title":"test","message":"socket test"}'
CLIENT_TITLE="test"
CLIENT_MESSAGE="client test"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

print_output() {
  local output_file="$1"

  if [[ -s "$output_file" ]]; then
    echo "  Command output:"
    sed 's/^/    /' "$output_file"
  fi
}

fail_with_output() {
  local message="$1"
  local output_file="$2"

  echo "  FAIL: $message"
  print_output "$output_file"
  exit 1
}

fail() {
  local message="$1"

  echo "  FAIL: $message"
  exit 1
}

skip_daemon_not_running() {
  local detail="$1"

  echo "  SKIP: $detail"
  echo ""
  echo "  Start the host daemon with: nix run .#wrix-notifyd"
  exit 77
}

is_connection_refused() {
  local output_file="$1"

  grep -qi "connection refused" "$output_file"
}

get_default_gateway() {
  ip route | awk '/default/ {print $3; exit}'
}

probe_tcp_listener() {
  local gateway="$1"
  local output_file="$2"

  nc -nvz -w "$CONNECT_TIMEOUT_SECONDS" "$gateway" "$TCP_PORT" >"$output_file" 2>&1
}

send_tcp_notification() {
  local gateway="$1"
  local output_file="$2"

  printf '%s\n' "$TCP_PAYLOAD" | nc -N -w "$CONNECT_TIMEOUT_SECONDS" "$gateway" "$TCP_PORT" >"$output_file" 2>&1
}

probe_unix_listener() {
  local output_file="$1"

  nc -zUv -w "$CONNECT_TIMEOUT_SECONDS" "$SOCKET" >"$output_file" 2>&1
}

send_unix_notification() {
  local output_file="$1"

  printf '%s\n' "$SOCKET_PAYLOAD" | nc -U -N -w "$CONNECT_TIMEOUT_SECONDS" "$SOCKET" >"$output_file" 2>&1
}

run_client_command() {
  local output_file="$1"

  wrix-notify "$CLIENT_TITLE" "$CLIENT_MESSAGE" >"$output_file" 2>&1
}

check_client_command() {
  local test_number="$1"
  local output_file="$TMP_DIR/wrix-notify.log"

  echo ""
  echo "Test $test_number: wrix-notify client command"
  if ! command -v wrix-notify >"$output_file" 2>&1; then
    fail_with_output "wrix-notify is not available on PATH" "$output_file"
  fi
  if run_client_command "$output_file"; then
    echo "  PASS: wrix-notify command completed"
  else
    fail_with_output "wrix-notify command failed" "$output_file"
  fi
}

run_darwin_tcp_check() {
  local gateway
  local probe_output="$TMP_DIR/tcp-probe.log"
  local send_output="$TMP_DIR/tcp-send.log"

  echo ""
  echo "Transport: TCP to gateway (Darwin container)"

  if ! gateway="$(get_default_gateway)"; then
    fail "Could not inspect the default route"
  fi
  if [[ -z "$gateway" ]]; then
    fail "Could not determine gateway IP"
  fi
  if [[ "$gateway" != "$DARWIN_GATEWAY" ]]; then
    fail "Expected Darwin vmnet gateway $DARWIN_GATEWAY, got $gateway"
  fi
  echo "  Gateway: $gateway"

  echo ""
  echo "Test 1: TCP listener at host ($gateway:$TCP_PORT)"
  if probe_tcp_listener "$gateway" "$probe_output"; then
    echo "  PASS: TCP listener is reachable"
  elif is_connection_refused "$probe_output"; then
    skip_daemon_not_running "No TCP listener at $gateway:$TCP_PORT"
  else
    fail_with_output "TCP listener probe failed" "$probe_output"
  fi

  echo ""
  echo "Test 2: TCP notification write"
  if send_tcp_notification "$gateway" "$send_output"; then
    echo "  PASS: Successfully sent notification via TCP"
  else
    fail_with_output "Connected to TCP listener but notification write failed" "$send_output"
  fi

  check_client_command 3
}

run_linux_socket_check() {
  local perms
  local stat_output="$TMP_DIR/socket-stat.log"
  local probe_output="$TMP_DIR/socket-probe.log"
  local send_output="$TMP_DIR/socket-send.log"

  echo ""
  echo "Transport: Unix socket (Linux container)"

  echo ""
  echo "Test 1: Socket existence"
  if [[ -S "$SOCKET" ]]; then
    echo "  PASS: Socket exists at $SOCKET"
  else
    skip_daemon_not_running "Socket not mounted at $SOCKET"
  fi

  echo ""
  echo "Test 2: Socket permissions"
  if perms="$(stat -c '%a' "$SOCKET" 2>"$stat_output")"; then
    case "$perms" in
      777 | 755 | 700 | 666)
        echo "  PASS: Socket has accessible permissions ($perms)"
        ;;
      *)
        fail "Socket has inaccessible permissions ($perms)"
        ;;
    esac
  else
    fail_with_output "Could not read socket permissions" "$stat_output"
  fi

  echo ""
  echo "Test 3: Socket listener"
  if probe_unix_listener "$probe_output"; then
    echo "  PASS: Socket listener is reachable"
  elif is_connection_refused "$probe_output"; then
    skip_daemon_not_running "No daemon is listening on $SOCKET"
  else
    fail_with_output "Socket listener probe failed" "$probe_output"
  fi

  echo ""
  echo "Test 4: Socket notification write"
  if send_unix_notification "$send_output"; then
    echo "  PASS: Successfully wrote to socket"
  else
    fail_with_output "Connected to socket listener but notification write failed" "$send_output"
  fi

  check_client_command 5
}

if [[ "${WRIX_NOTIFY_TCP:-}" == "1" ]]; then
  run_darwin_tcp_check
else
  run_linux_socket_check
fi

echo ""
echo "=== ALL TESTS PASSED ==="
