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

expected_linux_system() {
  local machine

  machine="$(uname -m)"
  case "$machine" in
    arm64 | aarch64)
      printf 'aarch64-linux\n'
      ;;
    x86_64)
      printf 'x86_64-linux\n'
      ;;
    *)
      echo "FAIL: Unsupported macOS architecture: $machine" >&2
      return 1
      ;;
  esac
}

assert_builder_config_output() {
  local config_file="$1"
  local flake_file="$2"
  local config_dir
  local config_json
  local expected_system

  config_dir="$(dirname "$config_file")"
  mkdir -p "$config_dir/home"
  cat >"$flake_file" <<'NIX'
{
  outputs = { ... }: {
    builderConfig = import ./builder-config.nix;
  };
}
NIX

  config_json="$(HOME="$config_dir/home" \
    nix --extra-experimental-features "nix-command flakes" \
    eval --json --no-write-lock-file "path:$config_dir#builderConfig")"
  expected_system="$(expected_linux_system)"
  if ! jq -e --arg expected_system "$expected_system" '
    .nix.buildMachines[0] as $machine
    | (.nix.buildMachines | length) == 1
      and $machine.hostName == "localhost:2222"
      and $machine.protocol == "ssh-ng"
      and $machine.systems == [$expected_system]
      and $machine.sshUser == "builder"
      and $machine.sshKey == "/etc/nix/wrix_builder_ed25519"
      and $machine.maxJobs == 4
      and $machine.speedFactor == 1
      and $machine.supportedFeatures == ["big-parallel", "benchmark"]
      and ($machine | has("publicHostKey") | not)
  ' <<<"$config_json" >/dev/null; then
    jq . <<<"$config_json" >&2
    return 1
  fi
  printf '%s\n' "$config_json"
}

builder_spec_from_config() {
  jq -r '
    .nix.buildMachines[0]
    | "\(.protocol)://\(.sshUser)@\(.hostName) \(.systems | join(",")) \(.sshKey) \(.maxJobs) \(.speedFactor) \(.supportedFeatures | join(","))"
  '
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
TMP_DIR="$(cd "$TMP_DIR" && pwd -P)"
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
if echo "$STATUS_OUTPUT" | grep -q "Nix store volume:"; then
  echo "  PASS: Status shows the Nix store volume"
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

# Test 6: Verify the persistent store is internally consistent
echo ""
echo "Test 6: Nix store integrity"
if "$BUILDER" ssh "nix-store --verify --check-contents" >/dev/null 2>&1; then
  echo "  PASS: Nix store contents match the registered hashes"
else
  echo "  FAIL: Nix store integrity verification failed"
  FAILED=1
fi

# Test 7: Host setup for remote builds
echo ""
echo "Test 7: Host setup for remote builds"
SETUP_OUTPUT="$TMP_DIR/setup.log"
if run_builder_setup >"$SETUP_OUTPUT" 2>&1; then
  echo "  PASS: Host SSH setup completed"
else
  echo "  FAIL: Host SSH setup failed"
  print_output "$SETUP_OUTPUT"
  FAILED=1
fi

# Test 8: Config output
echo ""
echo "Test 8: Config command"
CONFIG_FILE="$TMP_DIR/builder-config.nix"
CONFIG_FLAKE="$TMP_DIR/flake.nix"
CONFIG_JSON=""
if "$BUILDER" config >"$CONFIG_FILE" && CONFIG_JSON="$(assert_builder_config_output "$CONFIG_FILE" "$CONFIG_FLAKE")"; then
  echo "  PASS: Config is a pure nix-darwin module for the native builder"
else
  echo "  FAIL: Config command did not output a pure nix-darwin module"
  print_output "$CONFIG_FILE"
  FAILED=1
fi

# Test 9: Remote build test
echo ""
echo "Test 9: Remote build (nixpkgs#hello)"
REMOTE_BUILD_OUTPUT="$TMP_DIR/remote-build.log"
if [[ -n "$CONFIG_JSON" ]]; then
  BUILDER_SPEC="$(builder_spec_from_config <<<"$CONFIG_JSON")"
  if nix build \
    --builders "$BUILDER_SPEC" \
    --max-jobs 0 \
    --no-link \
    nixpkgs#hello >"$REMOTE_BUILD_OUTPUT" 2>&1; then
    echo "  PASS: Remote build succeeded"
  else
    echo "  FAIL: Remote build failed"
    print_output "$REMOTE_BUILD_OUTPUT"
    FAILED=1
  fi
else
  echo "  FAIL: Remote build skipped because builder config was invalid"
  FAILED=1
fi

# Test 10: Store persistence across restart
echo ""
echo "Test 10: Store persistence"
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
