# wrapix-builder: CLI wrapper for Linux remote builder
#
# Manages a container that serves as an ssh-ng:// remote builder
# for Nix on macOS. Uses Apple's container CLI (macOS 26+).
#
# Usage:
#   wrapix-builder start   - Start the builder container
#   wrapix-builder stop    - Stop and remove the container
#   wrapix-builder status  - Show builder status
#   wrapix-builder ssh     - Connect to builder via SSH
#   wrapix-builder config  - Print nix.conf snippet for remote builder
#
{ pkgs, linuxPkgs }:

let
  shellLib = import ../util/shell.nix { };

  builderImage = import ../sandbox/builder/image.nix {
    pkgs = linuxPkgs;
  };

  # SSH keys in nix store (stable, accessible to nix-darwin and root)
  keys = import ./hostkey.nix { inherit pkgs; };

  script = pkgs.writeShellScriptBin "wrapix-builder" ''
      set -euo pipefail

      # XDG-compliant directories
      XDG_DATA_HOME="''${XDG_DATA_HOME:-$HOME/.local/share}"
      XDG_CACHE_HOME="''${XDG_CACHE_HOME:-$HOME/.cache}"
      WRAPIX_DATA="$XDG_DATA_HOME/wrapix"
      WRAPIX_CACHE="$XDG_CACHE_HOME/wrapix"

      # Builder-specific paths
      KEYS_DIR="${keys}"
      NIX_STORE="$WRAPIX_DATA/builder-nix"
      CONTAINER_NAME="wrapix-builder"
      BUILDER_IMAGE="wrapix-builder:latest"
      SSH_PORT=2222

      usage() {
        echo "Usage: wrapix-builder <command>"
        echo ""
        echo "Commands:"
        echo "  start        - Start the builder container"
        echo "  stop         - Stop and remove the container"
        echo "  status       - Show builder status and SSH connection info"
        echo "  ssh          - Connect to builder via SSH (or run a command)"
        echo "  config       - Print nix-darwin configuration for remote builder"
        echo "  setup        - Run all setup steps (routes + ssh, requires sudo)"
        echo "  setup-routes - Fix container network routes (requires sudo)"
        echo "  setup-ssh    - Add host key to root's known_hosts (requires sudo)"
        exit 1
      }

      container_exists() {
        # container inspect returns [] with exit 0 even when container doesn't exist
        local output
        output=$(container inspect "$CONTAINER_NAME" 2>/dev/null) || return 1
        [ "$output" != "[]" ]
      }

      check_macos_version() {
        if [ "$(sw_vers -productVersion | cut -d. -f1)" -lt 26 ]; then
          echo "Error: macOS 26+ required (current: $(sw_vers -productVersion))"
          exit 1
        fi
      }

      ensure_container_system() {
        if ! container system status >/dev/null 2>&1; then
          echo "Starting container system..."
          container system start
          sleep 2
        fi
      }


      load_builder_image() {
        # Load image into container registry if not present or outdated
        # Use the store version file to detect if image changed (same source of truth as store init)
        STORE_VERSION_FILE="$NIX_STORE/.image-version"
        CURRENT_IMAGE="${builderImage}"

        needs_load=false
        if ! container image inspect "$BUILDER_IMAGE" >/dev/null 2>&1; then
          needs_load=true
        elif [ ! -f "$STORE_VERSION_FILE" ] || [ "$(cat "$STORE_VERSION_FILE")" != "$CURRENT_IMAGE" ]; then
          # Image changed or no version file - need to reload to ensure consistency
          needs_load=true
        fi

        if [ "$needs_load" = true ]; then
          echo "Loading builder image..."
          # Delete old image if exists
          container image delete "$BUILDER_IMAGE" 2>/dev/null || true
          # Convert Docker-format tar to OCI-archive format
          OCI_TAR="$WRAPIX_CACHE/builder-image-oci.tar"
          mkdir -p "$WRAPIX_CACHE"
          ${pkgs.skopeo}/bin/skopeo --insecure-policy copy --quiet "docker-archive:${builderImage}" "oci-archive:$OCI_TAR"
          # Load and capture the digest from output
          LOAD_OUTPUT=$(container image load --input "$OCI_TAR" 2>&1)
          LOADED_REF=$(echo "$LOAD_OUTPUT" | grep -oE 'untagged@sha256:[a-f0-9]+' | head -1)
          if [ -n "$LOADED_REF" ]; then
            container image tag "$LOADED_REF" "$BUILDER_IMAGE"
          fi
          rm -f "$OCI_TAR"
          container image prune
        fi
      }

      cmd_start() {
        check_macos_version
        ensure_container_system

        # Check if container exists and is running
        if container_exists; then
          local state
          state=$(container inspect "$CONTAINER_NAME" 2>/dev/null | grep -o '"status":"[^"]*"' | head -1 | cut -d'"' -f4)
          if [ "$state" = "running" ]; then
            echo "Builder container is already running"
            echo "Use 'wrapix-builder ssh' to connect"
            exit 0
          fi
          # Container exists but not running - remove and recreate
          echo "Cleaning up stale container..."
          container rm "$CONTAINER_NAME" 2>/dev/null || true
        fi

        load_builder_image

        # Ensure persistent Nix store directory exists
        mkdir -p "$NIX_STORE"

        # Track image version inside the persistent store
        # Re-initialize if store is empty OR if image has changed
        STORE_VERSION_FILE="$NIX_STORE/.image-version"
        CURRENT_IMAGE="${builderImage}"

        needs_init=false
        if [ ! -d "$NIX_STORE/store" ] || [ -z "$(ls -A "$NIX_STORE/store" 2>/dev/null)" ]; then
          needs_init=true
          echo "Initializing persistent Nix store (first run)..."
        elif [ ! -f "$STORE_VERSION_FILE" ] || [ "$(cat "$STORE_VERSION_FILE")" != "$CURRENT_IMAGE" ]; then
          needs_init=true
          echo "Builder image changed, re-initializing persistent Nix store..."
          chmod -R u+w "$NIX_STORE" 2>/dev/null || true
          rm -rf "$NIX_STORE"
          mkdir -p "$NIX_STORE"
        fi

        if [ "$needs_init" = true ]; then
          echo "This may take a few minutes..."

          # Run temp container without volume mount to export /nix
          TEMP_CONTAINER="wrapix-builder-init-$$"
          container run --name "$TEMP_CONTAINER" -d "$BUILDER_IMAGE" >/dev/null 2>&1
          sleep 3

          # Copy /nix from container to host
          # Use container exec with tar to stream the content
          # Permission warnings are expected (some store paths are 000) and harmless
          # Tar returns exit 2 for these warnings, so we ignore it and verify success below
          container exec "$TEMP_CONTAINER" tar -cf - -C / nix 2>/dev/null | tar -xf - -C "$NIX_STORE" --strip-components=1 2>/dev/null || true

          # Verify the store was populated
          if [ ! -d "$NIX_STORE/store" ] || [ -z "$(ls -A "$NIX_STORE/store" 2>/dev/null)" ]; then
            echo "Error: Failed to initialize Nix store"
            container stop "$TEMP_CONTAINER" >/dev/null 2>&1 || true
            container rm "$TEMP_CONTAINER" >/dev/null 2>&1 || true
            exit 1
          fi

          # Cleanup temp container
          container stop "$TEMP_CONTAINER" >/dev/null 2>&1 || true
          container rm "$TEMP_CONTAINER" >/dev/null 2>&1 || true

          # Record which image this store was initialized from
          echo "$CURRENT_IMAGE" > "$STORE_VERSION_FILE"

          echo "Nix store initialized ($(du -sh "$NIX_STORE" 2>/dev/null | cut -f1))"
        fi

        echo "Creating builder container..."

        # Mount keys (from nix store) and nix store (for persistence)
        container run \
          --name "$CONTAINER_NAME" \
          -d \
          -c 4 \
          -m 4096M \
          --network default \
          -p "127.0.0.1:$SSH_PORT:22" \
          -v "$KEYS_DIR:/run/keys:ro" \
          -v "$NIX_STORE:/nix" \
          "$BUILDER_IMAGE"

        # Wait for nix-daemon to be ready
        echo "Waiting for services to start..."
        for i in $(seq 1 120); do
          if container exec "$CONTAINER_NAME" pgrep -x nix-daemon >/dev/null 2>&1; then
            break
          fi
          if [ "$i" -eq 120 ]; then
            echo "Warning: nix-daemon did not start within 120 seconds"
          fi
          sleep 1
        done

        echo ""
        echo "Builder started successfully!"
        echo ""
        echo "Next steps:"
        echo "  wrapix-builder setup   - Configure routes and SSH for nix-daemon (requires sudo)"
        echo "  wrapix-builder ssh     - Connect to builder via SSH"
        echo "  wrapix-builder config  - Print nix-darwin configuration"
      }

      cmd_stop() {
        echo "Stopping builder container..."
        container stop "$CONTAINER_NAME" 2>/dev/null || true
        container rm "$CONTAINER_NAME" 2>/dev/null || true
        echo "Builder stopped"
      }

      cmd_status() {
        if container_exists; then
          local state
          state=$(container inspect "$CONTAINER_NAME" 2>/dev/null | grep -o '"status":"[^"]*"' | head -1 | cut -d'"' -f4)
          echo "Builder: $state"
          echo ""
          echo "SSH connection:"
          echo "  ssh -p $SSH_PORT -i $KEYS_DIR/builder_ed25519 builder@localhost"
          echo ""
          echo "SSH keys: $KEYS_DIR"
          echo "Nix store: $NIX_STORE"
          if [ "$state" = "running" ] && [ -d "$NIX_STORE" ]; then
            store_size=$(du -sh "$NIX_STORE" 2>/dev/null | cut -f1) || store_size="unknown"
            echo "Store size: $store_size"
          fi
        else
          echo "Builder: not created"
          echo ""
          echo "Nix store: $NIX_STORE"
          echo "Run 'wrapix-builder start' to create and start the builder"
        fi
      }

      cmd_ssh() {
        if ! container_exists; then
          echo "Error: Builder is not running"
          echo "Run 'wrapix-builder start' first"
          exit 1
        fi

        local state
        state=$(container inspect "$CONTAINER_NAME" 2>/dev/null | grep -o '"status":"[^"]*"' | head -1 | cut -d'"' -f4)
        if [ "$state" != "running" ]; then
          echo "Error: Builder container is not running (state: $state)"
          echo "Run 'wrapix-builder start' first"
          exit 1
        fi

        # Use container exec for reliable access (--uid 1000 = builder user)
        if [ $# -eq 0 ]; then
          exec container exec -it --uid 1000 "$CONTAINER_NAME" /bin/bash -l
        else
          exec container exec --uid 1000 "$CONTAINER_NAME" /bin/bash -c "$*"
        fi
      }

      cmd_config() {
        cat <<NIXCONFIG
    # Add to nix-darwin configuration:

    # SSH config (environment.etc):
    "ssh/ssh_config.d/100-wrapix-builder.conf".text = '''
      Host wrapix-builder
        Hostname localhost
        Port 2222
        User builder
        HostKeyAlias wrapix-builder
        IdentityFile $KEYS_DIR/builder_ed25519
    ''';

    # buildMachines (no sshKey needed, uses SSH config):
    {
      hostName = "wrapix-builder";
      systems = [ "aarch64-linux" ];
      protocol = "ssh-ng";
      maxJobs = 4;
      supportedFeatures = [ "big-parallel" "benchmark" ];
      publicHostKey = builtins.readFile $KEYS_DIR/public_host_key_base64;
    }

    # Or import from wrapix flake:
    # sshKey: inputs.wrapix.packages.<system>.wrapix-builder.sshKey
    # publicHostKey: inputs.wrapix.packages.<system>.wrapix-builder.publicHostKey
    NIXCONFIG
      }

      cmd_setup_routes() {
        ${shellLib.fixVmnetRoute}
      }

      cmd_setup_ssh() {
        # Container commands must run as original user when script is run with sudo
        local container_cmd="container"
        if [ -n "''${SUDO_USER:-}" ]; then
          container_cmd="sudo -u $SUDO_USER container"
        fi

        # Check if container exists
        local output
        output=$($container_cmd inspect "$CONTAINER_NAME" 2>/dev/null) || true
        if [ -z "$output" ] || [ "$output" = "[]" ]; then
          echo "Error: Builder container is not running"
          echo "Run 'wrapix-builder start' first"
          exit 1
        fi

        local state
        state=$(echo "$output" | grep -o '"status":"[^"]*"' | head -1 | cut -d'"' -f4)
        if [ "$state" != "running" ]; then
          echo "Error: Builder container is not running (state: $state)"
          echo "Run 'wrapix-builder start' first"
          exit 1
        fi

        # Copy SSH key with proper permissions (SSH rejects world-readable keys)
        # Can't symlink because nix store has 444 permissions
        echo "Installing SSH key at /etc/nix/wrapix_builder_ed25519..."
        rm -f /etc/nix/wrapix_builder_ed25519
        cp "$KEYS_DIR/builder_ed25519" /etc/nix/wrapix_builder_ed25519
        chmod 600 /etc/nix/wrapix_builder_ed25519
        chown root:nixbld /etc/nix/wrapix_builder_ed25519

        # Symlink for host key is fine (not a private key)
        echo "Creating host key symlink at /etc/nix/wrapix_builder_host_key_base64..."
        ln -sf "$KEYS_DIR/public_host_key_base64" /etc/nix/wrapix_builder_host_key_base64

        # Add host key to root's known_hosts (nix-daemon runs as root and reads this)
        echo "Adding to root's known_hosts..."
        local host_key
        host_key=$(cat "$KEYS_DIR/ssh_host_ed25519_key.pub")
        local root_known_hosts="/var/root/.ssh/known_hosts"
        mkdir -p /var/root/.ssh
        chmod 700 /var/root/.ssh

        # Remove old entries
        if [ -f "$root_known_hosts" ]; then
          grep -v "^wrapix-builder " "$root_known_hosts" > "$root_known_hosts.tmp" 2>/dev/null || true
          mv "$root_known_hosts.tmp" "$root_known_hosts"
        fi

        # Add entry using the raw public key (format: hostname key-type key)
        echo "wrapix-builder $host_key" >> "$root_known_hosts"
        chmod 600 "$root_known_hosts"

        # Verify SSH works
        ssh -o BatchMode=yes wrapix-builder true 2>/dev/null || {
          echo "Error: Failed to connect to wrapix-builder"
          echo "Check that routes are configured: wrapix-builder setup-routes"
          exit 1
        }
        echo "Host key added to known_hosts"
      }

      cmd_setup() {
        echo "=== Setting up wrapix-builder ==="
        echo ""
        echo "Step 1: Configuring network routes..."
        cmd_setup_routes
        echo ""
        echo "Step 2: Adding SSH host key for root..."
        cmd_setup_ssh
        echo ""
        echo "=== Setup complete ==="
        echo "You can now use 'nix run' with wrapix-builder as a remote builder"
      }

      # Main command dispatch
      case "''${1:-}" in
        start)        cmd_start ;;
        stop)         cmd_stop ;;
        status)       cmd_status ;;
        ssh)          shift; cmd_ssh "$@" ;;
        config)       cmd_config ;;
        setup)        cmd_setup ;;
        setup-routes) cmd_setup_routes ;;
        setup-ssh)    cmd_setup_ssh ;;
        *)            usage ;;
      esac
  '';

in
# Expose the script with passthru attributes for nix-darwin integration
script.overrideAttrs (old: {
  passthru = (old.passthru or { }) // {
    # Public host key for nix-darwin buildMachines
    publicHostKey = builtins.readFile "${keys}/public_host_key_base64";
    # SSH key path for nix-darwin buildMachines or SSH config IdentityFile
    sshKey = "${keys}/builder_ed25519";
    # Path to keys directory (for reference)
    keysPath = keys;
  };
})
