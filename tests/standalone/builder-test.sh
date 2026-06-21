#!/usr/bin/env bash
# wrix-builder integration test
# Tests the Linux builder functionality on macOS 26+
# Use with: nix run .#test-builder (when added to flake.nix)
set -euo pipefail

skip() {
  local reason="$1"
  echo "SKIP: $reason"
  exit 77
}

macos_major_version() {
  local version="$1"
  printf '%s\n' "${version%%.*}"
}

echo "=== wrix-builder Integration Test ==="
echo "Date: $(date)"
echo ""

# Ensure we're on Darwin with macOS 26+
if [[ "$(uname)" != "Darwin" ]]; then
  skip "This test only runs on Darwin"
fi

MACOS_VERSION="$(sw_vers -productVersion)"
MACOS_MAJOR="$(macos_major_version "$MACOS_VERSION")"
if ! [[ "$MACOS_MAJOR" =~ ^[0-9]+$ ]]; then
  echo "FAIL: Could not parse macOS version: $MACOS_VERSION" >&2
  exit 1
fi
if [[ "$MACOS_MAJOR" -lt 26 ]]; then
  skip "Requires macOS 26+ (current: $MACOS_VERSION)"
fi

FAILED=0

# Build wrix-builder
echo "=== Building wrix-builder ==="
nix build .#wrix-builder
BUILDER="./result/bin/wrix-builder"

# Test 1: Start builder
echo ""
echo "Test 1: Start builder"
if "$BUILDER" start; then
  echo "  PASS: Builder started"
else
  echo "  FAIL: Failed to start builder"
  FAILED=1
fi

# Test 2: Check status
echo ""
echo "Test 2: Check status"
STATUS_OUTPUT=$("$BUILDER" status)
if echo "$STATUS_OUTPUT" | grep -q "running"; then
  echo "  PASS: Builder is running"
else
  echo "  FAIL: Builder not running"
  FAILED=1
fi
if echo "$STATUS_OUTPUT" | grep -q "Nix store:"; then
  echo "  PASS: Status shows Nix store path"
else
  echo "  FAIL: Status missing Nix store info"
  FAILED=1
fi

# Test 3: Test SSH connection
echo ""
echo "Test 3: SSH connection"
if "$BUILDER" ssh "whoami" 2>/dev/null | grep -q "builder"; then
  echo "  PASS: SSH works, user is builder"
else
  echo "  FAIL: SSH connection failed"
  FAILED=1
fi

# Test 4: Verify nix-daemon is running inside container
echo ""
echo "Test 4: nix-daemon running"
if "$BUILDER" ssh "pgrep -x nix-daemon" >/dev/null 2>&1; then
  echo "  PASS: nix-daemon is running"
else
  echo "  FAIL: nix-daemon not running"
  FAILED=1
fi

# Test 5: Verify nix commands work
echo ""
echo "Test 5: Nix commands work"
if "$BUILDER" ssh "nix --version" >/dev/null 2>&1; then
  NIX_VERSION=$("$BUILDER" ssh "nix --version" 2>/dev/null)
  echo "  PASS: Nix available ($NIX_VERSION)"
else
  echo "  FAIL: Nix commands not working"
  FAILED=1
fi

# Test 6: Remote build test
echo ""
echo "Test 6: Remote build (nixpkgs#hello)"
KEYS_DIR="$HOME/.local/share/wrix/builder-keys"
if nix build \
  --builders "ssh-ng://builder@localhost:2222 aarch64-linux $KEYS_DIR/builder_ed25519 4 1" \
  --max-jobs 0 \
  --no-link \
  nixpkgs#hello 2>/dev/null; then
  echo "  PASS: Remote build succeeded"
else
  echo "  FAIL: Remote build failed"
  echo "  (This may fail if no remote Linux builder is available for aarch64-linux)"
  # Don't fail the whole test for this - it requires a working Linux builder chain
fi

# Test 7: Store persistence across restart
echo ""
echo "Test 7: Store persistence"
echo "  Building a test derivation..."
# Build something and capture a store path
TEST_STORE_PATH=$("$BUILDER" ssh "nix build --no-link --print-out-paths nixpkgs#hello" 2>/dev/null) || TEST_STORE_PATH=""
if [[ -z "$TEST_STORE_PATH" ]]; then
  echo "  WARNING: Could not build test derivation (skipping persistence check)"
else
  echo "  Built: $TEST_STORE_PATH"
  echo "  Stopping builder..."
  "$BUILDER" stop
  sleep 2
  echo "  Starting builder..."
  if "$BUILDER" start; then
    sleep 5
    echo "  Checking if store path persisted..."
    if "$BUILDER" ssh "test -e '$TEST_STORE_PATH'" 2>/dev/null; then
      echo "  PASS: Store path persisted across restart"
    else
      echo "  FAIL: Store path lost after restart"
      FAILED=1
    fi
  else
    echo "  FAIL: Failed to restart builder"
    FAILED=1
  fi
fi

# Test 8: Config output
echo ""
echo "Test 8: Config command"
if "$BUILDER" config | grep -q 'protocol = "ssh-ng"'; then
  echo "  PASS: Config outputs valid nix.conf snippet"
else
  echo "  FAIL: Config command failed"
  FAILED=1
fi

# Cleanup
echo ""
echo "=== Cleanup ==="
"$BUILDER" stop
echo "Builder stopped"

# Summary
echo ""
echo "========================================"
if [[ "$FAILED" -eq 0 ]]; then
  echo "ALL BUILDER TESTS PASSED"
  exit 0
else
  echo "SOME BUILDER TESTS FAILED"
  exit 1
fi
