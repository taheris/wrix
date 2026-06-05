#!/usr/bin/env bash
# Container mount verification test script
# This runs INSIDE the container to verify mounts are working
#
# Key security test: symlinks are dereferenced on HOST, /nix/store is NOT mounted
set -euo pipefail

# Darwin-only test: uses darwin-specific mounts (VirtioFS, WRIX_DIR_MOUNTS staging).
# Platform gating is at the Nix level (tests/darwin/default.nix); this script runs
# inside a Linux container on a Darwin host, so uname returns "Linux" here.

# Precondition: test fixtures must be set up by the darwin test harness
# (tests/darwin/default.nix). It creates workspace-test.txt, staging directories
# at /mnt/wrix/dir0, and sets WRIX_DIR_MOUNTS. When running without the
# harness (e.g., via wrix-mcp on Linux), these fixtures are absent.
if [ ! -f /workspace/workspace-test.txt ] && [ -z "${WRIX_DIR_MOUNTS:-}" ]; then
  echo "SKIP: Darwin test fixtures not present (workspace-test.txt missing, WRIX_DIR_MOUNTS unset)"
  echo "This test requires the test harness from tests/darwin/default.nix."
  exit 77
fi

echo "=== Container Mount Verification ==="
echo "Running as: $(id)"
echo "HOME: $HOME"
echo "PWD: $(pwd)"
echo ""

# Fixed home directory for wrix user
# (we bypass entrypoint with --entrypoint /bin/bash, so HOME is not set up)
TEST_HOME="/home/wrix"

FAILED=0

# Test 1: Workspace mount
echo "Test 1: Workspace mount at /workspace"
if [ -f /workspace/workspace-test.txt ]; then
  CONTENT=$(cat /workspace/workspace-test.txt)
  if [ "$CONTENT" = "workspace-file-content" ]; then
    echo "  PASS: Content matches"
  else
    echo "  FAIL: Content mismatch: $CONTENT"
    FAILED=1
  fi
else
  echo "  FAIL: File not found"
  ls -la /workspace/ || true
  FAILED=1
fi

# Test 2: Directory mount environment variable
echo ""
echo "Test 2: WRIX_DIR_MOUNTS env var"
if [ -n "${WRIX_DIR_MOUNTS:-}" ]; then
  echo "  PASS: WRIX_DIR_MOUNTS=$WRIX_DIR_MOUNTS"
else
  echo "  FAIL: WRIX_DIR_MOUNTS not set"
  FAILED=1
fi

# Test 3: File mount environment variable
echo ""
echo "Test 3: WRIX_FILE_MOUNTS env var"
if [ -n "${WRIX_FILE_MOUNTS:-}" ]; then
  echo "  PASS: WRIX_FILE_MOUNTS=$WRIX_FILE_MOUNTS"
else
  echo "  FAIL: WRIX_FILE_MOUNTS not set"
  FAILED=1
fi

# Test 4: Directory mount staging location
# VirtioFS maps files as root, so directories are staged and copied
# Symlinks are dereferenced on HOST before mounting (security)
echo ""
echo "Test 4: Directory mount staging"
if [ -d /mnt/wrix/dir0 ]; then
  echo "  PASS: Staging directory exists at /mnt/wrix/dir0"
  if [ -e /mnt/wrix/dir0/settings.json ]; then
    # Verify it's a regular file (symlinks dereferenced on host)
    if [ -L /mnt/wrix/dir0/settings.json ]; then
      echo "  FAIL: settings.json is still a symlink (should be dereferenced on host)"
      readlink /mnt/wrix/dir0/settings.json
      FAILED=1
    else
      echo "  PASS: settings.json is a regular file (dereferenced on host)"
    fi
    # Verify content is readable
    if grep -q "settings-value" /mnt/wrix/dir0/settings.json; then
      echo "  PASS: settings.json content correct in staging"
    else
      echo "  FAIL: settings.json content wrong in staging"
      cat /mnt/wrix/dir0/settings.json 2>&1 || echo "  (failed to read)"
      FAILED=1
    fi
  else
    echo "  FAIL: settings.json not found in staging"
    ls -la /mnt/wrix/dir0/ || true
    FAILED=1
  fi
  if [ -f /mnt/wrix/dir0/mcp/config.json ]; then
    echo "  PASS: Nested mcp/config.json exists in staging"
  else
    echo "  FAIL: mcp/config.json not found in staging"
    ls -la /mnt/wrix/dir0/ 2>/dev/null || true
    FAILED=1
  fi
else
  echo "  FAIL: Staging directory /mnt/wrix/dir0 not found"
  ls -la /mnt/wrix/ 2>/dev/null || echo "  /mnt/wrix does not exist"
  FAILED=1
fi

# Test 5: Directory copy from staging to destination
# Note: We bypass entrypoint for testing, so manually simulate the copy
echo ""
echo "Test 5: Directory copy from staging to destination"
if [ -n "${WRIX_DIR_MOUNTS:-}" ]; then
  # Parse WRIX_DIR_MOUNTS and copy directories (simulating entrypoint behavior)
  IFS=',' read -ra MOUNTS <<< "$WRIX_DIR_MOUNTS"
  for mapping in "${MOUNTS[@]}"; do
    src="${mapping%%:*}"
    dst="${mapping#*:}"
    if [ -d "$src" ]; then
      mkdir -p "$(dirname "$dst")"
      cp -r "$src" "$dst"  # Symlinks already dereferenced on host
      echo "  Copied $src -> $dst"
    fi
  done
fi

if [ -d "$TEST_HOME/.claude" ]; then
  echo "  PASS: Directory copied to $TEST_HOME/.claude"
  if [ -f "$TEST_HOME/.claude/settings.json" ]; then
    if grep -q "settings-value" "$TEST_HOME/.claude/settings.json"; then
      echo "  PASS: settings.json content correct"
    else
      echo "  FAIL: settings.json content wrong"
      cat "$TEST_HOME/.claude/settings.json"
      FAILED=1
    fi
    # Verify it's a regular file (symlinks dereferenced on host before mount)
    if [ -L "$TEST_HOME/.claude/settings.json" ]; then
      echo "  FAIL: settings.json is still a symlink"
      ls -la "$TEST_HOME/.claude/settings.json"
      FAILED=1
    else
      echo "  PASS: settings.json is a regular file"
    fi
  else
    echo "  FAIL: settings.json not found in copy"
    ls -la "$TEST_HOME/.claude/" || true
    FAILED=1
  fi
  if [ -f "$TEST_HOME/.claude/mcp/config.json" ]; then
    echo "  PASS: Nested mcp/config.json exists in copy"
  else
    echo "  FAIL: mcp/config.json not found in copy"
    ls -la "$TEST_HOME/.claude/" 2>/dev/null || true
    FAILED=1
  fi
else
  echo "  FAIL: Directory not copied to $TEST_HOME/.claude"
  ls -la "$TEST_HOME/" 2>/dev/null || true
  FAILED=1
fi

# Test 6: File mount staging location
# VirtioFS only supports directory mounts, so files are staged via parent dir
echo ""
echo "Test 6: File mount staging"
if [ -f /mnt/wrix/file0/claude.json ]; then
  if grep -q "test-api-key-12345" /mnt/wrix/file0/claude.json; then
    echo "  PASS: claude.json content correct at /mnt/wrix/file0/claude.json"
  else
    echo "  FAIL: claude.json content wrong"
    cat /mnt/wrix/file0/claude.json
    FAILED=1
  fi
else
  echo "  FAIL: claude.json not found at /mnt/wrix/file0/"
  ls -la /mnt/wrix/ 2>/dev/null || echo "  /mnt/wrix does not exist"
  FAILED=1
fi

# Test 7: File copy from staging to destination
# Note: We bypass entrypoint for testing, so manually simulate the copy
echo ""
echo "Test 7: File copy from staging to destination"
if [ -n "${WRIX_FILE_MOUNTS:-}" ]; then
  # Parse WRIX_FILE_MOUNTS and copy files (simulating entrypoint behavior)
  IFS=',' read -ra MOUNTS <<< "$WRIX_FILE_MOUNTS"
  for mapping in "${MOUNTS[@]}"; do
    src="${mapping%%:*}"
    dst="${mapping#*:}"
    if [ -f "$src" ]; then
      mkdir -p "$(dirname "$dst")"
      cp "$src" "$dst"
      echo "  Copied $src -> $dst"
    fi
  done
fi

if [ -f "$TEST_HOME/.claude.json" ]; then
  if grep -q "test-api-key-12345" "$TEST_HOME/.claude.json"; then
    echo "  PASS: claude.json copied to $TEST_HOME/.claude.json"
  else
    echo "  FAIL: claude.json content wrong at destination"
    cat "$TEST_HOME/.claude.json"
    FAILED=1
  fi
else
  echo "  FAIL: claude.json not copied to $TEST_HOME/.claude.json"
  ls -la "$TEST_HOME/" 2>/dev/null || true
  FAILED=1
fi

# Test 8: Security - /nix/store must NOT be mounted
echo ""
echo "Test 8: Security - /nix/store must NOT be mounted"
if mount | grep -q "/nix/store"; then
  echo "  FAIL: /nix/store is mounted (security violation)"
  mount | grep /nix/store
  FAILED=1
else
  echo "  PASS: /nix/store is not mounted"
fi

# Test 9: Workspace is writable
echo ""
echo "Test 9: Workspace is writable"
echo "test-content" > /workspace/test-output.txt
if [ -f /workspace/test-output.txt ]; then
  echo "  PASS: Wrote to workspace"
else
  echo "  FAIL: Could not write to workspace"
  FAILED=1
fi

# Test 10: Notification transport configuration (Darwin uses TCP, not mounted sockets)
echo ""
echo "Test 10: Notification transport"
if [ "${WRIX_NOTIFY_TCP:-}" = "1" ]; then
  echo "  PASS: WRIX_NOTIFY_TCP=1 (TCP transport enabled)"
  GATEWAY=$(ip route | awk '/default/ {print $3; exit}')
  if [ -n "$GATEWAY" ]; then
    echo "  PASS: Gateway found at $GATEWAY (notifications use TCP port 5959)"
  else
    echo "  FAIL: No gateway found"
    FAILED=1
  fi
else
  echo "  FAIL: WRIX_NOTIFY_TCP not set"
  echo "        Darwin containers should have WRIX_NOTIFY_TCP=1"
  FAILED=1
fi

# Legacy socket mounts (for user-defined mounts, not notifications)
if [ -n "${WRIX_SOCK_MOUNTS:-}" ]; then
  echo "  INFO: WRIX_SOCK_MOUNTS=$WRIX_SOCK_MOUNTS (user-defined)"
  IFS=',' read -ra SOCKETS <<< "$WRIX_SOCK_MOUNTS"
  for sock in "${SOCKETS[@]}"; do
    if [ -S "$sock" ]; then
      PERMS=$(stat -c '%a' "$sock" 2>/dev/null || stat -f '%Lp' "$sock" 2>/dev/null || echo "unknown")
      if [ "$PERMS" = "000" ]; then
        echo "  WARN: Socket $sock has 0000 permissions (VirtioFS limitation)"
      else
        echo "  PASS: Socket $sock has accessible permissions ($PERMS)"
      fi
    else
      echo "  WARN: Socket $sock in SOCK_MOUNTS but not present"
    fi
  done
fi

echo ""
if [ "$FAILED" -eq 0 ]; then
  echo "=== ALL TESTS PASSED ==="
  exit 0
else
  echo "=== SOME TESTS FAILED ==="
  exit 1
fi
