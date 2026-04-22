# Darwin VM mount integration test
#
# Key security test: symlinks are dereferenced on HOST, /nix/store is NOT mounted
#
# This test will:
# - Run during `nix flake check` if container CLI is available
# - Skip gracefully if infrastructure is missing (with instructions)
#
# Prerequisites:
#   container system start    # Start container system first
{
  pkgs,
  treefmt,
}:

let
  inherit (pkgs) runCommandLocal;

  inherit (pkgs.stdenv) isDarwin;

  # Use Linux packages for image building (requires remote builder on Darwin)
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
  profiles = import ../../lib/sandbox/profiles.nix {
    pkgs = linuxPkgs;
    inherit treefmt;
  };
  profileImage = import ../../lib/sandbox/image.nix {
    pkgs = linuxPkgs;
    profile = profiles.base;
    entrypointPkg = linuxPkgs.claude-code;
    entrypointSh = ../../lib/sandbox/darwin/entrypoint.sh;
    claudeConfig = { };
    claudeSettings = { };
  };

  # Test script that runs inside the container
  containerTestScript = ./mount-test.sh;

in
{
  # Integration test that runs a VM and tests mounts
  # Skips gracefully if infrastructure is not available
  darwin-mount-integration =
    runCommandLocal "test-darwin-mounts"
      {
        nativeBuildInputs = [ pkgs.skopeo ];
      }
      ''
        # Nix-safe exit 77 handler: treat skip (77) as build success
        trap '_ec=$?; if [ "$_ec" -eq 77 ]; then mkdir -p $out; exit 0; fi' EXIT

        set -euo pipefail

        # Ensure we're on Darwin
        if [ "$(uname)" != "Darwin" ]; then
          echo "SKIP: Darwin-only test" >&2
          exit 77
        fi

        # Check macOS version
        MACOS_VERSION=$(/usr/bin/sw_vers -productVersion | cut -d. -f1)
        if [ "$MACOS_VERSION" -lt 26 ]; then
          echo "SKIP: Requires macOS 26+ (current: $(/usr/bin/sw_vers -productVersion))" >&2
          exit 77
        fi

        echo "=== Darwin Mount Integration Test ==="
        echo ""

        # Get the real console user
        REAL_USER=$(/usr/bin/stat -f %Su /dev/console 2>/dev/null || echo "")
        if [ -z "$REAL_USER" ]; then
          echo "SKIP: Could not determine console user" >&2
          exit 77
        fi

        REAL_HOME="/Users/$REAL_USER"

        # Check if container CLI is available
        if ! command -v container >/dev/null 2>&1; then
          echo "SKIP: container CLI not found" >&2
          exit 77
        fi

        # Check if container system is running
        if ! container system status >/dev/null 2>&1; then
          echo "SKIP: container system not running (start with: container system start)" >&2
          exit 77
        fi

        # Check if we can access the container storage
        CONTAINER_STORAGE="$REAL_HOME/Library/Application Support/com.apple.container"
        if [ ! -d "$CONTAINER_STORAGE" ] || [ ! -w "$CONTAINER_STORAGE" ]; then
          echo "SKIP: Cannot access container storage (running in nix build sandbox; run manually with: nix run .#test-darwin)" >&2
          exit 77
        fi

        # Set HOME so container uses the right storage directory
        export HOME="$REAL_HOME"

        # Load test image
        TEST_IMAGE="wrapix-mount-test:latest"
        echo "Loading test image..."
        container image delete "$TEST_IMAGE" 2>/dev/null || true
        OCI_TAR=$(mktemp)
        skopeo --insecure-policy copy --quiet "docker-archive:${profileImage}" "oci-archive:$OCI_TAR"
        LOAD_OUTPUT=$(container image load --input "$OCI_TAR" 2>&1)
        LOADED_REF=$(echo "$LOAD_OUTPUT" | grep -oE 'untagged@sha256:[a-f0-9]+' | head -1)
        if [ -n "$LOADED_REF" ]; then
          container image tag "$LOADED_REF" "$TEST_IMAGE"
        fi
        rm -f "$OCI_TAR"

        echo "Using image: $TEST_IMAGE"
        echo ""

        # Create temporary test directory
        TEST_DIR=$(mktemp -d)
        cleanup() { rm -rf "$TEST_DIR"; }
        trap cleanup EXIT

        echo "Test directory: $TEST_DIR"

        # Set up test workspace
        WORKSPACE="$TEST_DIR/workspace"
        mkdir -p "$WORKSPACE"
        echo "workspace-file-content" > "$WORKSPACE/workspace-test.txt"

        # Copy the container test script into workspace
        cp ${containerTestScript} "$WORKSPACE/mount-test.sh"
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

        echo "Test setup: symlink in $CLAUDE_DIR -> $SYMLINK_TARGET"
        echo ""

        # Dereference symlinks on host (security: avoids mounting /nix/store)
        CLAUDE_DIR_DEREF="$TEST_DIR/claude-config-deref"
        mkdir -p "$CLAUDE_DIR_DEREF"
        cp -rL "$CLAUDE_DIR/." "$CLAUDE_DIR_DEREF/"

        # Run the container with our test script
        # Note: Using dereferenced directory, no /nix/store mount needed
        set +e
        container run --rm \
          -w / \
          -v "$WORKSPACE:/workspace" \
          -v "$CLAUDE_DIR_DEREF:/mnt/wrapix/dir0" \
          -v "$CLAUDE_JSON_DIR:/mnt/wrapix/file0" \
          -e BD_DB=/tmp/beads.db \
          -e BD_NO_DAEMON=1 \
          -e HOST_UID=$(id -u "$REAL_USER") \
          -e WRAPIX_DIR_MOUNTS="$DIR_MOUNTS" \
          -e WRAPIX_FILE_MOUNTS="$FILE_MOUNTS" \
          -e WRAPIX_NOTIFY_TCP=1 \
          -e WRAPIX_PROMPT="test" \
          --network default \
          --entrypoint /bin/bash \
          "$TEST_IMAGE" /workspace/mount-test.sh
        EXIT_CODE=$?
        set -e

        echo ""
        if [ "$EXIT_CODE" -eq 0 ]; then
          echo "=== MOUNT INTEGRATION TEST PASSED ==="
          mkdir -p $out
        else
          echo "=== MOUNT INTEGRATION TEST FAILED ==="
          exit 1
        fi
      '';
}
