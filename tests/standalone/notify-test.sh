#!/usr/bin/env bash
# Notification connectivity test - run inside container
#
# Tests notification connectivity to host daemon.
# - Darwin containers: uses TCP to gateway (VirtioFS can't pass Unix sockets)
# - Linux containers: uses mounted Unix socket
#
# Skips gracefully if daemon not running on host
set -euo pipefail

echo "=== Notification Connectivity Test ==="

TCP_PORT=5959    # Must match daemon.nix
SOCKET="/run/wrix/notify.sock"

# Detect transport mechanism (WRIX_NOTIFY_TCP=1 set by Darwin sandbox)
if [ "${WRIX_NOTIFY_TCP:-}" = "1" ]; then
  echo ""
  echo "Transport: TCP to gateway (Darwin container)"

  # Get gateway IP
  GATEWAY=$(ip route | awk '/default/ {print $3; exit}')
  if [ -z "$GATEWAY" ]; then
    echo "  FAIL: Could not determine gateway IP"
    exit 1
  fi
  echo "  Gateway: $GATEWAY"

  # Test 1: TCP connectivity
  echo ""
  echo "Test 1: TCP connectivity to host ($GATEWAY:$TCP_PORT)"
  if echo '{"title":"test","message":"tcp test"}' | socat - TCP:"$GATEWAY":"$TCP_PORT" 2>/dev/null; then
    echo "  PASS: Successfully sent notification via TCP"
  else
    echo "  FAIL: Could not connect via TCP"
    echo ""
    echo "  This usually means:"
    echo "    1. Daemon not running - wrix-notifyd is not running on the host."
    echo "       Fix: Run 'nix run .#wrix-notifyd' on the host."
    echo "    2. Firewall blocking - port $TCP_PORT may be blocked."
    echo "       Fix: Check firewall settings on the host."
    exit 1
  fi
else
  echo ""
  echo "Transport: Unix socket (Linux container)"

  # Test 1: Check if socket exists
  echo ""
  echo "Test 1: Socket existence"
  if [ -S "$SOCKET" ]; then
    echo "  PASS: Socket exists at $SOCKET"
  else
    echo "  SKIP: Socket not mounted (daemon may not be running on host)"
    exit 77
  fi

  # Test 2: Check socket permissions
  echo ""
  echo "Test 2: Socket permissions"
  PERMS=$(stat -c '%a' "$SOCKET" 2>/dev/null || stat -f '%Lp' "$SOCKET" 2>/dev/null)
  if [ "$PERMS" = "777" ] || [ "$PERMS" = "755" ] || [ "$PERMS" = "700" ] || [ "$PERMS" = "666" ]; then
    echo "  PASS: Socket has accessible permissions ($PERMS)"
  else
    echo "  FAIL: Socket has inaccessible permissions ($PERMS)"
    exit 1
  fi

  # Test 3: Write test (verifies daemon is listening)
  echo ""
  echo "Test 3: Socket writability"
  if echo '{"title":"test","message":"socket test"}' | socat -u STDIN UNIX-CONNECT:$SOCKET 2>/dev/null; then
    echo "  PASS: Successfully wrote to socket"
  else
    echo "  FAIL: Could not write to socket"
    echo ""
    echo "  This usually means:"
    echo "    1. Stale socket mount - the daemon was restarted after the container started."
    echo "       Fix: Restart the container to pick up the new socket."
    echo "    2. Daemon not running - wrix-notifyd is not running on the host."
    echo "       Fix: Run 'nix run .#wrix-notifyd' on the host."
    exit 1
  fi
fi

echo ""
echo "=== ALL TESTS PASSED ==="
