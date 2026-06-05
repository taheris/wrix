# Darwin integration tests - runs container tests on macOS
# Use with: nix run .#test-darwin
{
  pkgs,
}:

let
  inherit (pkgs.stdenv) isDarwin;

  # Use Linux packages for building the container image (requires remote builder on Darwin)
  linuxPkgs =
    if isDarwin then
      import pkgs.path {
        system = "aarch64-linux";
        config.allowUnfree = true;
        inherit (pkgs) overlays;
      }
    else
      pkgs;

  # Build profile image for testing
  profiles = import ../../lib/sandbox/profiles.nix { pkgs = linuxPkgs; };
  profileImage = import ../../lib/sandbox/image.nix {
    pkgs = linuxPkgs;
    profile = profiles.base;
    agent = "claude";
    agentPkg = linuxPkgs.claude-code;
    entrypointSh = ../../lib/sandbox/darwin/entrypoint.sh;
    claudeConfig = { };
    claudeSettings = { };
  };

  # Test scripts that run inside the container
  networkTestScript = ./network-test.sh;
  mountTestScript = ./mount-test.sh;
  uidTestScript = ./uid-test.sh;

in
pkgs.writeShellScriptBin "test-darwin" ''
  set -euo pipefail

  # Ensure we're on Darwin
  if [ "$(uname)" != "Darwin" ]; then
    echo "SKIP: Darwin-only integration tests" >&2
    exit 77
  fi

  # Check macOS version
  MACOS_VERSION=$(sw_vers -productVersion | cut -d. -f1)
  if [ "$MACOS_VERSION" -lt 26 ]; then
    echo "SKIP: Requires macOS 26+ (current: $(sw_vers -productVersion))" >&2
    exit 77
  fi

  echo "=== Darwin Integration Tests ==="
  echo ""

  # Ensure container system is running
  if ! container system status >/dev/null 2>&1; then
    echo "Starting container system..."
    container system start
    sleep 2
  fi

  TEST_IMAGE="wrapix-integration-test:latest"
  XDG_CACHE_HOME="''${XDG_CACHE_HOME:-$HOME/.cache}"
  WRAPIX_CACHE="$XDG_CACHE_HOME/wrapix"
  mkdir -p "$WRAPIX_CACHE"

  # Load test image
  echo "Loading test image..."
  container image delete "$TEST_IMAGE" 2>/dev/null || true
  OCI_TAR="$WRAPIX_CACHE/integration-test-image.tar"
  ${pkgs.skopeo}/bin/skopeo --insecure-policy copy --quiet "docker-archive:${profileImage}" "oci-archive:$OCI_TAR"
  LOAD_OUTPUT=$(container image load --input "$OCI_TAR" 2>&1)
  LOADED_REF=$(echo "$LOAD_OUTPUT" | grep -oE 'untagged@sha256:[a-f0-9]+' | head -1)
  if [ -n "$LOADED_REF" ]; then
    container image tag "$LOADED_REF" "$TEST_IMAGE"
  fi
  rm -f "$OCI_TAR"

  echo "Using container CLI for tests"
  echo ""

  FAILED=0

  # ============================================
  # Network Integration Test
  # ============================================
  echo "----------------------------------------"
  echo "Running: Network Integration Test"
  echo "----------------------------------------"

  TEST_DIR=$(mktemp -d)
  trap "rm -rf $TEST_DIR" EXIT

  WORKSPACE="$TEST_DIR/workspace"
  mkdir -p "$WORKSPACE"
  cp ${networkTestScript} "$WORKSPACE/network-test.sh"
  chmod +x "$WORKSPACE/network-test.sh"

  set +e
  container run --rm \
    -w / \
    -v "$WORKSPACE:/workspace" \
    -e BD_NO_DB=1 \
    -e HOST_UID=$(id -u) \
    -e WRAPIX_PROMPT="test" \
    --network default \
    --dns 100.100.100.100 \
    --dns 1.1.1.1 \
    --entrypoint /bin/bash \
    "$TEST_IMAGE" /workspace/network-test.sh
  NETWORK_EXIT=$?
  set -e

  if [ "$NETWORK_EXIT" -eq 0 ]; then
    echo "PASS: Network test"
  else
    echo "FAIL: Network test (exit code: $NETWORK_EXIT)"
    FAILED=1
  fi
  echo ""

  # ============================================
  # Mount Integration Test
  # ============================================
  echo "----------------------------------------"
  echo "Running: Mount Integration Test"
  echo "----------------------------------------"

  rm -rf "$TEST_DIR"
  TEST_DIR=$(mktemp -d)
  WORKSPACE="$TEST_DIR/workspace"
  mkdir -p "$WORKSPACE"
  echo "workspace-file-content" > "$WORKSPACE/workspace-test.txt"
  cp ${mountTestScript} "$WORKSPACE/mount-test.sh"
  chmod +x "$WORKSPACE/mount-test.sh"

  # Set up directory mount (simulating ~/.claude)
  # VirtioFS maps files as root, so we use staging + WRAPIX_DIR_MOUNTS
  CLAUDE_DIR="$TEST_DIR/claude-config"
  mkdir -p "$CLAUDE_DIR/mcp"

  # Create symlink to simulate home-manager (points outside the directory)
  SYMLINK_TARGET="$TEST_DIR/symlink-target"
  mkdir -p "$SYMLINK_TARGET"
  echo '{"test": "settings-value"}' > "$SYMLINK_TARGET/settings.json"
  ln -s "$SYMLINK_TARGET/settings.json" "$CLAUDE_DIR/settings.json"

  # Create regular file for nested config
  echo '{"server": "mcp-config"}' > "$CLAUDE_DIR/mcp/config.json"

  # Set up file mount (simulating ~/.claude.json)
  # VirtioFS only supports directory mounts, so we mount parent dir to staging
  CLAUDE_JSON_DIR="$TEST_DIR/claude-json"
  mkdir -p "$CLAUDE_JSON_DIR"
  echo '{"apiKey": "test-api-key-12345"}' > "$CLAUDE_JSON_DIR/claude.json"

  # Build mount environment variables in same format as production:
  # DIR_MOUNTS:  /staging/path:/destination/path
  # FILE_MOUNTS: /staging/path/filename:/destination/path
  DIR_MOUNTS="/mnt/wrapix/dir0:/home/wrapix/.claude"
  FILE_MOUNTS="/mnt/wrapix/file0/claude.json:/home/wrapix/.claude.json"

  # Dereference symlinks on host (security: avoids mounting /nix/store)
  CLAUDE_DIR_DEREF="$TEST_DIR/claude-config-deref"
  mkdir -p "$CLAUDE_DIR_DEREF"
  cp -rL "$CLAUDE_DIR/." "$CLAUDE_DIR_DEREF/"

  set +e
  container run --rm \
    -w / \
    -v "$WORKSPACE:/workspace" \
    -v "$CLAUDE_DIR_DEREF:/mnt/wrapix/dir0" \
    -v "$CLAUDE_JSON_DIR:/mnt/wrapix/file0" \
    -e BD_NO_DB=1 \
    -e HOST_UID=$(id -u) \
    -e WRAPIX_DIR_MOUNTS="$DIR_MOUNTS" \
    -e WRAPIX_FILE_MOUNTS="$FILE_MOUNTS" \
    -e WRAPIX_NOTIFY_TCP=1 \
    -e WRAPIX_PROMPT="test" \
    --network default \
    --dns 100.100.100.100 \
    --dns 1.1.1.1 \
    --entrypoint /bin/bash \
    "$TEST_IMAGE" /workspace/mount-test.sh
  MOUNT_EXIT=$?
  set -e

  if [ "$MOUNT_EXIT" -eq 0 ]; then
    echo "PASS: Mount test"
  else
    echo "FAIL: Mount test (exit code: $MOUNT_EXIT)"
    FAILED=1
  fi
  echo ""

  # ============================================
  # UID Mapping Integration Test
  # ============================================
  echo "----------------------------------------"
  echo "Running: UID Mapping Integration Test"
  echo "----------------------------------------"

  rm -rf "$TEST_DIR"
  TEST_DIR=$(mktemp -d)
  WORKSPACE="$TEST_DIR/workspace"
  mkdir -p "$WORKSPACE"
  git -C "$WORKSPACE" init --quiet
  git -C "$WORKSPACE" commit --allow-empty -m "init" --quiet
  cp ${uidTestScript} "$WORKSPACE/uid-test.sh"
  chmod +x "$WORKSPACE/uid-test.sh"

  set +e
  container run --rm \
    -w / \
    -v "$WORKSPACE:/workspace" \
    -e BD_NO_DB=1 \
    -e HOST_UID=$(id -u) \
    -e WRAPIX_PROMPT="test" \
    --network default \
    --entrypoint /bin/bash \
    "$TEST_IMAGE" -c '
      # Simulate entrypoint passwd setup (test bypasses entrypoint)
      sed -i "s/^wrapix:x:1000:1000:/wrapix:x:'"$(id -u)"':'"$(id -u)"':/" /etc/passwd
      sed -i "s/^wrapix:x:1000:/wrapix:x:'"$(id -u)"':/" /etc/group
      export HOME="/home/wrapix"
      exec /workspace/uid-test.sh
    '
  UID_EXIT=$?
  set -e

  if [ "$UID_EXIT" -eq 0 ]; then
    echo "PASS: UID mapping test"
  else
    echo "FAIL: UID mapping test (exit code: $UID_EXIT)"
    FAILED=1
  fi
  echo ""

  # ============================================
  # Summary
  # ============================================
  echo "========================================"
  if [ "$FAILED" -eq 0 ]; then
    echo "ALL INTEGRATION TESTS PASSED"
    exit 0
  else
    echo "SOME INTEGRATION TESTS FAILED"
    exit 1
  fi
''
