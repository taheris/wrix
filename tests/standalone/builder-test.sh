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

print_output() {
  local output_file="$1"

  if [[ -s "$output_file" ]]; then
    echo "  Command output:"
    sed 's/^/    /' "$output_file"
  fi
}

run_builder_setup() {
  if [[ "$(id -u)" -eq 0 ]]; then
    "$BUILDER" setup
  else
    sudo -n "$BUILDER" setup
  fi
}

assert_builder_config_output() {
  local config_file="$1"
  local assertion_file="$2"

  cat >"$assertion_file" <<NIX
let
  module = import $config_file;
  machine = builtins.head module.nix.buildMachines;
  sshConfig = module.environment.etc."ssh/ssh_config.d/100-wrix-builder.conf".text;
in
  assert builtins.isAttrs module;
  assert builtins.isAttrs module.environment.etc;
  assert builtins.isString sshConfig;
  assert sshConfig != "";
  assert builtins.isList module.nix.buildMachines;
  assert builtins.length module.nix.buildMachines == 1;
  assert machine.hostName == "wrix-builder";
  assert machine.protocol == "ssh-ng";
  assert machine.systems == [ "aarch64-linux" ];
  assert machine.maxJobs == 4;
  assert machine.supportedFeatures == [ "big-parallel" "benchmark" ];
  assert machine ? publicHostKey;
  true
NIX

  nix-instantiate --parse "$config_file" >/dev/null
  nix-instantiate --eval --strict "$assertion_file" >/dev/null
}

REPO_ROOT="${REPO_ROOT:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}" # best-effort: allow direct script runs outside a git checkout.

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

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT
FAILED=0

# Build wrix-builder
echo "=== Building wrix-builder ==="
BUILDER_OUTPUT="$(nix build --no-link --print-out-paths --no-warn-dirty "$REPO_ROOT#wrix-builder")"
BUILDER="$BUILDER_OUTPUT/bin/wrix-builder"

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

# Test 6: Host setup for remote builds
echo ""
echo "Test 6: Host setup for remote builds"
SETUP_OUTPUT="$TMP_DIR/setup.log"
if run_builder_setup >"$SETUP_OUTPUT" 2>&1; then
  echo "  PASS: Host SSH setup completed"
else
  echo "  FAIL: Host SSH setup failed"
  print_output "$SETUP_OUTPUT"
  FAILED=1
fi

# Test 7: Remote build test
echo ""
echo "Test 7: Remote build (nixpkgs#hello)"
SYSTEM_CLIENT_KEY="/etc/nix/wrix_builder_ed25519"
REMOTE_BUILD_OUTPUT="$TMP_DIR/remote-build.log"
if nix build \
  --builders "ssh-ng://builder@localhost:2222 aarch64-linux $SYSTEM_CLIENT_KEY 4 1" \
  --max-jobs 0 \
  --no-link \
  nixpkgs#hello >"$REMOTE_BUILD_OUTPUT" 2>&1; then
  echo "  PASS: Remote build succeeded"
else
  echo "  FAIL: Remote build failed"
  print_output "$REMOTE_BUILD_OUTPUT"
  FAILED=1
fi

# Test 8: Store persistence across restart
echo ""
echo "Test 8: Store persistence"
echo "  Building a test derivation..."
PERSISTENCE_BUILD_OUTPUT="$TMP_DIR/persistence-build.log"
if TEST_STORE_PATH=$("$BUILDER" ssh "nix build --no-link --print-out-paths nixpkgs#hello" 2>"$PERSISTENCE_BUILD_OUTPUT"); then
  if [[ -z "$TEST_STORE_PATH" ]]; then
    echo "  FAIL: Build produced no store path"
    FAILED=1
  else
    echo "  Built: $TEST_STORE_PATH"
    echo "  Stopping builder..."
    if "$BUILDER" stop; then
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
    else
      echo "  FAIL: Failed to stop builder before persistence check"
      FAILED=1
    fi
  fi
else
  echo "  FAIL: Could not build test derivation"
  print_output "$PERSISTENCE_BUILD_OUTPUT"
  FAILED=1
fi

# Test 9: Config output
echo ""
echo "Test 9: Config command"
CONFIG_FILE="$TMP_DIR/builder-config.nix"
CONFIG_ASSERTION="$TMP_DIR/builder-config-assertion.nix"
if "$BUILDER" config >"$CONFIG_FILE" && assert_builder_config_output "$CONFIG_FILE" "$CONFIG_ASSERTION"; then
  echo "  PASS: Config outputs a parseable nix-darwin module snippet"
else
  echo "  FAIL: Config command did not output a valid nix-darwin module snippet"
  print_output "$CONFIG_FILE"
  FAILED=1
fi

# Cleanup
echo ""
echo "=== Cleanup ==="
if "$BUILDER" stop; then
  echo "Builder stopped"
else
  echo "  FAIL: Cleanup stop failed"
  FAILED=1
fi

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
