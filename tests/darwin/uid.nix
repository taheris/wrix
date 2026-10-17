# Darwin VM UID mapping integration test
# Verifies real guest-root ownership for VirtioFS and private Nix together.
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

  inherit (pkgs.stdenv.hostPlatform) isDarwin;
  runIntegration = isDarwin && builtins.getEnv "WRIX_DARWIN_INTEGRATION" != "";

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
    hostPkgs = pkgs;
    asTarball = true;
    profile = profiles.base;
    agent = "direct";
    agentPkg = linuxPkgs.writeShellApplication {
      name = "wrix-uid-probe";
      text = "exec /workspace/uid-test.sh";
    };
    entrypointSh = ../../lib/sandbox/darwin/entrypoint.sh;
    claudeConfig = { };
    claudeSettings = { };
  };

  # Test script that runs inside the container
  containerTestScript = ./uid-test.sh;

in
{
  # Integration test that runs a VM and tests UID mapping
  # Skips gracefully if infrastructure is not available
  darwin-uid-integration =
    if !runIntegration then
      runCommandLocal "test-darwin-uid-skipped" { } ''
        trap '_ec=$?; if [ "$_ec" -eq 77 ]; then mkdir -p $out; exit 0; fi' EXIT
        echo "SKIP: Darwin UID integration test requires WRIX_DARWIN_INTEGRATION=1" >&2
        exit 77
      ''
    else
      runCommandLocal "test-darwin-uid"
        {
          nativeBuildInputs = [
            pkgs.skopeo
            pkgs.git
          ];
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

          echo "=== Darwin UID Mapping Integration Test ==="
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
          TEST_IMAGE="wrix-uid-test:latest"
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

          # Set up test workspace with a git repo (needed for git ownership tests)
          WORKSPACE="$TEST_DIR/workspace"
          mkdir -p "$WORKSPACE"
          git -C "$WORKSPACE" init --quiet
          git -C "$WORKSPACE" -c user.name=test -c user.email=test@test commit --allow-empty -m "init" --quiet

          # Copy the container test script into workspace
          cp ${containerTestScript} "$WORKSPACE/uid-test.sh"
          chmod +x "$WORKSPACE/uid-test.sh"

          WORKSPACE_UID=$(/usr/bin/stat -f %u "$WORKSPACE")
          echo "Running real image entrypoint with ownership and Nix checks..."
          set +e
          container run --rm \
            --cap-add CAP_NET_ADMIN \
            -w / \
            -v "$WORKSPACE:/workspace" \
            -e BD_NO_DB=1 \
            -e WRIX_AGENT=direct \
            -e WRIX_NETWORK=open \
            --network default \
            "$TEST_IMAGE"
          EXIT_CODE=$?
          set -e

          echo ""
          if [ "$EXIT_CODE" -eq 0 ]; then
            [[ "$(/usr/bin/stat -f %u "$WORKSPACE/.uid-mapping-test")" == "$WORKSPACE_UID" ]]
            echo "=== UID MAPPING INTEGRATION TEST PASSED ==="
            mkdir -p $out
          else
            echo "=== UID MAPPING INTEGRATION TEST FAILED ==="
            exit 1
          fi
        '';
}
